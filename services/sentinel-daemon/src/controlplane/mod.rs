//! Controlplane-Kernel: observe/decide/act/verify Zyklus.
//!
//! Deterministischer, integrierter Controlplane-Kernel fuer den sentinel-daemon.
//! Laeuft auf dem ECS-Thread als Teil des Tick-Loops (alle N Ticks).
//!
//! Architektur:
//! ```text
//! ┌─────────────────────────────────────────────────┐
//! │              Controlplane Cycle                   │
//! │                                                   │
//! │  Observe ──> Decide ──> Act ──> Verify           │
//! │     │           │         │        │              │
//! │  ECS World   Policies  Store    Store             │
//! │  (read)      (config)  (write)  (read/write)     │
//! └─────────────────────────────────────────────────┘
//! ```
//!
//! - Kein LLM im Echtzeitpfad (AC-N1)
//! - Entscheidungslatenz < 200ms (AC-2)
//! - Jede Action hat TTL + rollback_condition + verify_outcome (AC-5)

pub mod act;
pub mod config;
pub mod decide;
pub mod observe;
pub mod store;
pub mod types;
pub mod verify;

use std::collections::HashSet;
use std::time::Instant;

use anyhow::Result;
use bevy_ecs::prelude::*;
use tracing::{debug, info, warn};

use self::config::ControlplaneConfig;
use self::store::ControlplaneStore;
use self::types::RuntimeState;

/// Controlplane-Kernel der den observe/decide/act/verify Zyklus ausfuehrt.
pub struct ControlplaneKernel {
    config: ControlplaneConfig,
    store: ControlplaneStore,
    state: RuntimeState,
    /// Cooldown-Tracker: Keys der kuerzlich ausgefuehrten Actions.
    recent_action_keys: HashSet<String>,
    /// Tick bei dem Cooldown-Keys zuletzt bereinigt wurden.
    last_cooldown_cleanup_tick: u64,
}

impl ControlplaneKernel {
    /// Erstellt einen neuen Controlplane-Kernel.
    pub fn new(config: ControlplaneConfig, store: ControlplaneStore) -> Result<Self> {
        let state = store.get_runtime_state()?;

        // Config als JSON im Store persistieren (fuer Runtime-Inspektion)
        let config_json = serde_json::to_vec(&config)?;
        store.set_config("active_config", &config_json)?;

        info!(
            cycle_interval = config.cycle_interval_ticks,
            guarded_mode = config.guarded_mode,
            total_cycles = state.total_cycles,
            "Controlplane-Kernel initialisiert"
        );

        Ok(Self {
            config,
            store,
            state,
            recent_action_keys: HashSet::new(),
            last_cooldown_cleanup_tick: 0,
        })
    }

    /// Prueft ob der Controlplane-Zyklus in diesem Tick laufen soll.
    pub fn should_run(&self, tick: u64) -> bool {
        tick > 0 && tick.is_multiple_of(self.config.cycle_interval_ticks)
    }

    /// Fuehrt einen vollstaendigen observe/decide/act/verify Zyklus aus.
    ///
    /// Wird vom ECS Tick-Loop aufgerufen wenn `should_run()` true ist.
    /// Alle Phasen arbeiten in-memory, am Ende wird EINE redb-Transaktion
    /// ausgefuehrt (statt 4 separate — reduziert fsync-Kosten um ~75%).
    pub fn cycle(&mut self, world: &mut World, tick: u64) -> Result<()> {
        let start = Instant::now();
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // Phase 1: Observe (in-memory, kein I/O)
        let t_observe = Instant::now();
        let observation = observe::observe(world, tick, timestamp_ms);
        let incidents = observe::detect_incidents(&observation, &self.config);
        let observe_ms = t_observe.elapsed().as_micros() as f64 / 1000.0;

        // Phase 2: Decide (in-memory, kein I/O)
        let t_decide = Instant::now();
        self.cleanup_cooldowns(tick);
        let mut actions = decide::decide(&incidents, &self.config, &self.recent_action_keys);
        let decide_ms = t_decide.elapsed().as_micros() as f64 / 1000.0;

        // Phase 3: Act (in-memory: execute_single ist rein logging, kein Store-Write)
        let t_act = Instant::now();
        let executed = act::execute_actions_no_store(&mut actions)?;
        let act_ms = t_act.elapsed().as_micros() as f64 / 1000.0;

        // Cooldown-Keys fuer ausgefuehrte Actions registrieren
        for action in &actions {
            if action.status == types::ActionStatus::Executed {
                let key = match (&action.agent_id, &action.action_type) {
                    (Some(aid), _) => format!(
                        "{}:{}",
                        action.incident_id.split('-').nth(2).unwrap_or("unknown"),
                        aid
                    ),
                    _ => "system".into(),
                };
                self.recent_action_keys.insert(key);
            }
        }

        // Phase 4: Verify (liest Pending aus Store — 1 Read-Txn, kein Write)
        let t_verify = Instant::now();
        let pending = self.store.get_pending_actions()?;
        let (verify_stats, updated_actions) =
            verify::verify_actions_from_cache(pending, &observation, tick);
        let verify_ms = t_verify.elapsed().as_micros() as f64 / 1000.0;

        // Executed Actions fuer Batch-Write sammeln
        let new_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.status == types::ActionStatus::Executed)
            .cloned()
            .collect();

        // State aktualisieren
        self.state.last_cycle_tick = tick;
        self.state.total_cycles += 1;
        self.state.total_incidents += incidents.len() as u64;
        self.state.total_actions += executed as u64;

        // SINGLE WRITE TRANSACTION: alles in einem Commit
        let t_store = Instant::now();
        self.store
            .write_cycle_batch(&incidents, &new_actions, &updated_actions, &self.state)?;
        let store_ms = t_store.elapsed().as_micros() as f64 / 1000.0;

        let elapsed = start.elapsed();
        debug!(
            tick,
            incidents = incidents.len(),
            actions_decided = actions.len(),
            actions_executed = executed,
            verified = verify_stats.verified,
            expired = verify_stats.expired,
            observe_ms,
            decide_ms,
            act_ms,
            verify_ms,
            store_ms,
            elapsed_ms = elapsed.as_millis(),
            "Controlplane-Zyklus abgeschlossen"
        );

        // Warnung wenn Zykluszeit > 200ms (AC-2)
        if elapsed.as_millis() > 200 {
            warn!(
                elapsed_ms = elapsed.as_millis(),
                observe_ms,
                decide_ms,
                act_ms,
                verify_ms,
                store_ms,
                "Controlplane-Zyklus ueberschreitet 200ms Budget!"
            );
        }

        Ok(())
    }

    /// Bereinigt abgelaufene Cooldown-Keys.
    fn cleanup_cooldowns(&mut self, tick: u64) {
        if tick >= self.last_cooldown_cleanup_tick + self.config.cooldown_ticks {
            self.recent_action_keys.clear();
            self.last_cooldown_cleanup_tick = tick;
        }
    }

    /// Gibt den aktuellen Runtime-State zurueck (fuer Health-Checks).
    pub fn runtime_state(&self) -> &RuntimeState {
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_kernel() -> (tempfile::TempDir, ControlplaneKernel) {
        let tmp = tempfile::tempdir().unwrap();
        let store_path = tmp.path().join("controlplane.redb");
        let store = ControlplaneStore::open(&store_path).unwrap();
        let config = ControlplaneConfig::default_config();
        let kernel = ControlplaneKernel::new(config, store).unwrap();
        (tmp, kernel)
    }

    #[test]
    fn test_should_run_interval() {
        let (_tmp, kernel) = temp_kernel();
        assert!(!kernel.should_run(0)); // Tick 0: nicht laufen
        assert!(!kernel.should_run(5)); // Tick 5: nicht (interval=10)
        assert!(kernel.should_run(10)); // Tick 10: ja
        assert!(kernel.should_run(20)); // Tick 20: ja
        assert!(!kernel.should_run(15)); // Tick 15: nicht
    }

    #[test]
    fn test_cycle_with_empty_world() {
        let (_tmp, mut kernel) = temp_kernel();
        let mut world = World::new();

        // Leere World -> keine Agents -> keine Incidents -> keine Actions
        kernel.cycle(&mut world, 10).unwrap();

        assert_eq!(kernel.state.total_cycles, 1);
        assert_eq!(kernel.state.total_incidents, 0);
        assert_eq!(kernel.state.last_cycle_tick, 10);
    }

    #[test]
    fn test_cycle_with_healthy_agent() {
        use sentinel_common::components::*;
        use sentinel_common::AgentId;

        let (_tmp, mut kernel) = temp_kernel();
        let mut world = World::new();

        // Gesunden Agent spawnen
        world.spawn((
            AgentIdentity {
                agent_id: AgentId(1),
                name: "Test Agent".into(),
                role: "Tester".into(),
            },
            BioState {
                hunger: 0.3,
                energy: 0.8,
                caffeine_mg: 0.0,
                bladder: 0.2,
                stress: 0.1,
                social_need: 0.3,
                comfort: 0.7,
            },
            Position {
                room_id: "buero-dev-1".into(),
                in_transit: false,
                transit_target: None,
                transit_remaining_ms: 0,
                transit_correlation_id: None,
            },
            Mood {
                valence: 0.5,
                arousal: 0.3,
                dominant_emotion: sentinel_common::Emotion::Neutral,
            },
        ));

        kernel.cycle(&mut world, 10).unwrap();

        assert_eq!(kernel.state.total_cycles, 1);
        assert_eq!(kernel.state.total_incidents, 0); // Gesund = keine Incidents
    }

    #[test]
    fn test_cycle_detects_hunger_incident() {
        use sentinel_common::components::*;
        use sentinel_common::AgentId;

        let (_tmp, mut kernel) = temp_kernel();
        let mut world = World::new();

        // Agent mit kritischem Hunger
        world.spawn((
            AgentIdentity {
                agent_id: AgentId(1),
                name: "Hungry Agent".into(),
                role: "Tester".into(),
            },
            BioState {
                hunger: 0.95,
                energy: 0.8,
                caffeine_mg: 0.0,
                bladder: 0.2,
                stress: 0.1,
                social_need: 0.3,
                comfort: 0.7,
            },
            Position {
                room_id: "buero-dev-1".into(),
                in_transit: false,
                transit_target: None,
                transit_remaining_ms: 0,
                transit_correlation_id: None,
            },
            Mood {
                valence: 0.5,
                arousal: 0.3,
                dominant_emotion: sentinel_common::Emotion::Neutral,
            },
        ));

        kernel.cycle(&mut world, 10).unwrap();

        assert_eq!(kernel.state.total_cycles, 1);
        assert_eq!(kernel.state.total_incidents, 1);
        assert_eq!(kernel.state.total_actions, 1);
    }

    #[test]
    fn test_state_persists_across_cycles() {
        use sentinel_common::components::*;
        use sentinel_common::AgentId;

        let (_tmp, mut kernel) = temp_kernel();
        let mut world = World::new();

        world.spawn((
            AgentIdentity {
                agent_id: AgentId(1),
                name: "Agent".into(),
                role: "Tester".into(),
            },
            BioState {
                hunger: 0.3,
                energy: 0.8,
                caffeine_mg: 0.0,
                bladder: 0.2,
                stress: 0.1,
                social_need: 0.3,
                comfort: 0.7,
            },
            Position {
                room_id: "buero-dev-1".into(),
                in_transit: false,
                transit_target: None,
                transit_remaining_ms: 0,
                transit_correlation_id: None,
            },
            Mood {
                valence: 0.5,
                arousal: 0.3,
                dominant_emotion: sentinel_common::Emotion::Neutral,
            },
        ));

        kernel.cycle(&mut world, 10).unwrap();
        kernel.cycle(&mut world, 20).unwrap();
        kernel.cycle(&mut world, 30).unwrap();

        assert_eq!(kernel.state.total_cycles, 3);
        assert_eq!(kernel.state.last_cycle_tick, 30);
    }
}
