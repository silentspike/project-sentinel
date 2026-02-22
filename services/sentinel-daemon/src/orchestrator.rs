//! ECS Tick-Loop Orchestrator mit Tokio/ECS Thread-Bridge.
//!
//! Thread-Modell:
//! ```text
//! ┌─────────────────────┐     mpsc channels     ┌──────────────────┐
//! │  std::thread         │ <──── actions ─────── │  tokio::Runtime  │
//! │  ECS Tick Loop       │ ────> perceptions ──> │  Zenoh Bus       │
//! │  (bevy_ecs World)    │                       │  Limbo EventStore│
//! │  (sentinel-runtime)  │                       │  redb StateStore │
//! └─────────────────────┘                       └──────────────────┘
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{error, info};

use sentinel_common::agent_config::load_all_agents;
use sentinel_common::{AgentId, Perception};
use sentinel_ecs::{
    attach_redb_store, create_simulation_world, spawn_agent, ActionReceiver, LimboEventStore,
    PerceptionSender, SimulationTime,
};
use sentinel_limbo::EventStore;
use sentinel_redb::StateStore;
use sentinel_runtime::RuntimeOrchestrator;

use crate::config::DaemonConfig;
use crate::controlplane::config::ControlplaneConfig;
use crate::controlplane::store::ControlplaneStore;
use crate::controlplane::ControlplaneKernel;
use crate::shift::{agents_for_shift, detect_current_shift};
use crate::signal::wait_for_shutdown;

/// Startet den Daemon-Hauptloop.
///
/// 1. Oeffnet EventStore + StateStore
/// 2. Spawnt ECS Tick-Loop auf dediziertem Thread
/// 3. Wartet auf Shutdown-Signal
/// 4. Setzt AtomicBool, joined ECS-Thread
pub async fn run(config: DaemonConfig) -> Result<()> {
    // -- Datenbanken oeffnen (sync) --
    let data_dir = &config.data_dir;
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("data_dir erstellen: {}", data_dir.display()))?;

    let events_path = data_dir.join("events.db");
    let state_path = data_dir.join("state.redb");

    let event_store = EventStore::open(events_path.to_str().context("events.db Pfad nicht UTF-8")?)
        .context("EventStore oeffnen")?;

    let state_store = StateStore::open(state_path.to_str().context("state.redb Pfad nicht UTF-8")?)
        .context("StateStore oeffnen")?;

    let event_store = Arc::new(event_store);

    // -- Agent-Definitionen laden --
    let agents_dir = config.config_dir.join("agents");
    let all_agents = load_all_agents(&agents_dir)
        .with_context(|| format!("Agents laden aus: {}", agents_dir.display()))?;
    info!(
        total_agents = all_agents.len(),
        "Agent-Definitionen geladen"
    );

    // -- Schicht erkennen + filtern --
    let current_shift = detect_current_shift();
    let shift_agents = agents_for_shift(&all_agents, current_shift);
    info!(
        shift_set = current_shift,
        active_agents = shift_agents.len(),
        "Schicht erkannt"
    );

    // -- Runtime Orchestrator --
    let runtime_orch =
        RuntimeOrchestrator::new(config.max_agents).with_event_store(Arc::clone(&event_store));

    // -- Controlplane-Kernel laden --
    let cp_config_path = config.config_dir.join("controlplane.toml");
    let cp_config = if cp_config_path.exists() {
        ControlplaneConfig::load(&cp_config_path)
            .with_context(|| format!("Controlplane-Config laden: {}", cp_config_path.display()))?
    } else {
        info!("Keine controlplane.toml gefunden, verwende Defaults");
        ControlplaneConfig::default_config()
    };

    let cp_store_path = data_dir.join("controlplane.redb");
    let cp_store = ControlplaneStore::open(&cp_store_path).context("ControlplaneStore oeffnen")?;

    let controlplane =
        ControlplaneKernel::new(cp_config, cp_store).context("Controlplane-Kernel erstellen")?;

    // -- Channels fuer ECS <-> Async Bridge --
    let (action_tx, action_rx) = mpsc::channel();
    let (perception_tx, perception_rx) = mpsc::sync_channel::<Perception>(64);

    // -- Shutdown Flag --
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_ecs = Arc::clone(&shutdown);

    // Werte fuer den ECS-Thread klonen
    let tick_rate = Duration::from_millis(config.tick_rate_ms);
    let shift_agent_ids: Vec<_> = shift_agents
        .iter()
        .map(|a| {
            (
                a.identity.id,
                a.identity.name.clone(),
                a.identity.role.clone(),
                a.identity.shift_set,
            )
        })
        .collect();

    // -- ECS Tick Loop (dedizierter Thread, bevy_ecs World ist Send+Sync) --
    let ecs_handle = std::thread::Builder::new()
        .name("ecs-tick-loop".into())
        .spawn(move || {
            ecs_tick_loop(
                state_store,
                event_store,
                action_rx,
                perception_tx,
                shift_agent_ids,
                tick_rate,
                shutdown_ecs,
                controlplane,
            )
        })
        .context("ECS Thread spawnen")?;

    // -- LLM Bridge starten (Perception → Cortex Gateway → Action) --
    #[cfg(feature = "llm")]
    let _llm_bridge_handle = {
        let bridge_config = crate::llm_bridge::bridge::LlmBridgeConfig {
            gateway_url: std::env::var("CORTEX_GATEWAY_URL")
                .unwrap_or_else(|_| "http://localhost:8080".to_string()),
            ..Default::default()
        };
        let bridge_telemetry =
            std::sync::Arc::new(crate::llm_bridge::bridge::BridgeTelemetry::default());
        let bridge_action_tx = action_tx.clone();
        let bridge_telem = std::sync::Arc::clone(&bridge_telemetry);
        info!(
            gateway_url = %bridge_config.gateway_url,
            "LLM Bridge wird gestartet"
        );
        tokio::spawn(crate::llm_bridge::bridge::run_llm_bridge(
            bridge_config,
            perception_rx,
            bridge_action_tx,
            bridge_telem,
        ))
    };

    info!(
        tick_rate_ms = config.tick_rate_ms,
        max_agents = config.max_agents,
        "Daemon gestartet, warte auf Shutdown-Signal"
    );

    // -- Auf Shutdown warten --
    wait_for_shutdown().await;

    // -- Graceful Shutdown --
    info!("Shutdown eingeleitet...");
    shutdown.store(true, Ordering::SeqCst);

    // Action-Channel schliessen damit ECS-Thread aufwacht falls er blockt
    drop(action_tx);

    match ecs_handle.join() {
        Ok(Ok(tick_count)) => {
            info!(total_ticks = tick_count, "ECS Thread sauber beendet");
        }
        Ok(Err(e)) => {
            error!(error = %e, "ECS Thread mit Fehler beendet");
        }
        Err(_) => {
            error!("ECS Thread panicked");
        }
    }

    // RuntimeOrchestrator State speichern
    drop(runtime_orch);
    info!("Daemon heruntergefahren");

    Ok(())
}

/// ECS Tick-Loop auf dediziertem Thread.
/// Laeuft bis `shutdown` auf true gesetzt wird.
/// Gibt die Anzahl ausgefuehrter Ticks zurueck.
fn ecs_tick_loop(
    state_store: StateStore,
    event_store: Arc<EventStore>,
    action_rx: mpsc::Receiver<sentinel_common::AgentAction>,
    perception_tx: mpsc::SyncSender<Perception>,
    agents: Vec<(u16, String, String, u8)>,
    tick_rate: Duration,
    shutdown: Arc<AtomicBool>,
    mut controlplane: ControlplaneKernel,
) -> Result<u64> {
    // ECS World + Schedule erstellen
    let (mut world, mut schedule) = create_simulation_world();

    // Stores als Resources einfuegen
    attach_redb_store(&mut world, state_store);
    world.insert_resource(LimboEventStore(event_store));
    world.insert_resource(ActionReceiver(std::sync::Mutex::new(action_rx)));
    world.insert_resource(PerceptionSender(perception_tx));

    // Agents spawnen
    for (id, name, role, shift_set) in &agents {
        spawn_agent(&mut world, AgentId(*id), name, role, *shift_set);
    }
    info!(agent_count = agents.len(), "ECS World initialisiert");

    let mut tick_count: u64 = 0;

    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        // SimulationTime aktualisieren
        if let Some(mut time) = world.get_resource_mut::<SimulationTime>() {
            time.tick = sentinel_common::Tick(tick_count);
            time.tick_count = tick_count;
            time.delta_seconds = tick_rate.as_secs_f32();
        }

        // ECS Schedule ausfuehren (alle 10 Systems in Reihenfolge)
        schedule.run(&mut world);

        // Controlplane-Zyklus (alle N Ticks)
        if controlplane.should_run(tick_count) {
            if let Err(e) = controlplane.cycle(&mut world, tick_count) {
                error!(error = %e, tick = tick_count, "Controlplane-Zyklus fehlgeschlagen");
            }
        }

        tick_count += 1;

        if tick_count.is_multiple_of(60) {
            info!(tick = tick_count, "Tick Checkpoint");
        }

        std::thread::sleep(tick_rate);
    }

    Ok(tick_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controlplane::config::ControlplaneConfig;
    use crate::controlplane::store::ControlplaneStore;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn test_controlplane(tmp: &tempfile::TempDir) -> ControlplaneKernel {
        let cp_path = tmp.path().join("controlplane.redb");
        let cp_store = ControlplaneStore::open(&cp_path).unwrap();
        let cp_config = ControlplaneConfig::default_config();
        ControlplaneKernel::new(cp_config, cp_store).unwrap()
    }

    #[test]
    fn test_ecs_tick_loop_shutdown_immediate() {
        // Shutdown sofort setzen -> Loop sollte nach 0 Ticks beenden
        let shutdown = Arc::new(AtomicBool::new(true));

        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");
        let state_path = tmp.path().join("state.redb");

        let event_store = Arc::new(EventStore::open(events_path.to_str().unwrap()).unwrap());
        let state_store = StateStore::open(state_path.to_str().unwrap()).unwrap();

        let (_tx, rx) = mpsc::channel();
        let (ptx, _prx) = mpsc::sync_channel(64);

        let controlplane = test_controlplane(&tmp);

        let result = ecs_tick_loop(
            state_store,
            event_store,
            rx,
            ptx,
            vec![],
            Duration::from_millis(100),
            shutdown,
            controlplane,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_ecs_tick_loop_runs_ticks() {
        // Shutdown nach kurzer Zeit -> sollte mindestens 1 Tick laufen
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);

        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");
        let state_path = tmp.path().join("state.redb");

        let event_store = Arc::new(EventStore::open(events_path.to_str().unwrap()).unwrap());
        let state_store = StateStore::open(state_path.to_str().unwrap()).unwrap();

        let (_tx, rx) = mpsc::channel();
        let (ptx, _prx) = mpsc::sync_channel(64);

        let controlplane = test_controlplane(&tmp);

        // Shutdown nach 250ms
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            shutdown_clone.store(true, Ordering::SeqCst);
        });

        let result = ecs_tick_loop(
            state_store,
            event_store,
            rx,
            ptx,
            vec![(1, "Test Agent".into(), "Tester".into(), 1)],
            Duration::from_millis(50),
            shutdown,
            controlplane,
        );

        assert!(result.is_ok());
        let ticks = result.unwrap();
        assert!(ticks >= 1, "Mindestens 1 Tick erwartet, bekam {ticks}");
    }
}
