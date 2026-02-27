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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{error, info, warn};

use sentinel_common::agent_config::{load_all_agents, AgentConfig};
use sentinel_common::components::{AgentIdentity, ShiftInfo};
use sentinel_common::{AgentId, Perception};
use sentinel_ebpf::collector::MetricsSnapshot;
use sentinel_ebpf::EbpfCollector;
use sentinel_ecs::{
    attach_redb_store, create_simulation_world, despawn_agent_from_world, spawn_agent,
    ActionReceiver, EventBuffer, LimboEventStore, PerceptionSender, SimulationTime,
};
use sentinel_limbo::EventStore;
use sentinel_redb::StateStore;
use sentinel_runtime::RuntimeOrchestrator;
use sentinel_sandbox::{CgroupLimits, SandboxEnforcer, SandboxHandle, SandboxWarning};

use crate::config::DaemonConfig;
use crate::controlplane::config::ControlplaneConfig;
use crate::controlplane::store::ControlplaneStore;
use crate::controlplane::ControlplaneKernel;
use crate::episode_producer::EpisodeProducer;
use crate::shift::{agents_for_shift, detect_current_shift};
use crate::signal::wait_for_shutdown;

/// Mapping von shift_set auf (start_hour, end_hour).
fn shift_hours(shift_set: u8) -> (u8, u8) {
    match shift_set {
        1 => (6, 14),  // Fruehschicht
        2 => (14, 22), // Mittelschicht
        3 => (22, 6),  // Spaetschicht
        0 => (0, 0),   // Sonder (24/7)
        _ => (6, 14),  // Fallback
    }
}

/// Default cgroup limits fuer Agents (Issue #16 Spec).
/// CPU: 1 core, Memory: 256MB, IO: 300 IOPS + 10MB/s.
fn default_agent_limits() -> CgroupLimits {
    CgroupLimits::default()
}

/// Spawnt einen Agenten sowohl im RuntimeOrchestrator als auch in der ECS World.
/// Richtet die Sandbox (cgroup + home dir) ein wenn `sandbox` verfuegbar.
/// Gibt `true` zurueck wenn erfolgreich.
fn spawn_agent_full(
    runtime_orch: &mut RuntimeOrchestrator,
    world: &mut bevy_ecs::prelude::World,
    agent_cfg: &AgentConfig,
    sandbox: &SandboxEnforcer,
    sandbox_handles: &mut HashMap<AgentId, SandboxHandle>,
) -> bool {
    let agent_id = AgentId(agent_cfg.identity.id);
    let identity = AgentIdentity {
        agent_id,
        name: agent_cfg.identity.name.clone(),
        role: agent_cfg.identity.role.clone(),
    };
    let (start, end) = shift_hours(agent_cfg.identity.shift_set);
    let shift = ShiftInfo {
        shift_set: agent_cfg.identity.shift_set,
        shift_start_hour: start,
        shift_end_hour: end,
        is_on_duty: true,
    };
    if let Err(e) = runtime_orch.spawn_agent(identity, shift, "empfang") {
        warn!(agent_id = agent_cfg.identity.id, error = %e, "Agent-Spawn fehlgeschlagen");
        return false;
    }

    // Sandbox setup (cgroup + agent home) — AC-3: < 10ms, AC-4: bei jedem Spawn
    let t0 = Instant::now();
    match sandbox.setup_agent(&agent_cfg.identity.name, &default_agent_limits()) {
        Ok(handle) => {
            let elapsed = t0.elapsed();
            info!(
                agent = %agent_cfg.identity.name,
                cgroup = handle.cgroup_created,
                io = handle.io_available,
                elapsed_us = elapsed.as_micros(),
                "Sandbox setup abgeschlossen"
            );
            sandbox_handles.insert(agent_id, handle);
        }
        Err(e) => {
            warn!(
                agent = %agent_cfg.identity.name,
                error = %e,
                "Sandbox setup fehlgeschlagen (Agent laeuft ohne Isolation)"
            );
        }
    }

    spawn_agent(
        world,
        agent_id,
        &agent_cfg.identity.name,
        &agent_cfg.identity.role,
        agent_cfg.identity.shift_set,
    );
    true
}

/// Startet den Daemon-Hauptloop.
///
/// 1. Oeffnet EventStore + StateStore
/// 2. RuntimeOrchestrator: Restore aus Snapshot oder frisch
/// 3. Spawnt ECS Tick-Loop auf dediziertem Thread (mit Orchestrator)
/// 4. Wartet auf Shutdown-Signal
/// 5. ECS-Thread speichert State-Snapshot vor Beendigung
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

    // -- Schicht erkennen --
    let current_shift = detect_current_shift();
    let shift_agents = agents_for_shift(&all_agents, current_shift);
    info!(
        shift_set = current_shift,
        active_agents = shift_agents.len(),
        "Schicht erkannt"
    );

    // -- Runtime Orchestrator (Restore oder Neu) --
    let runtime_orch =
        match RuntimeOrchestrator::restore(Arc::clone(&event_store), config.max_agents) {
            Ok(restored) => {
                info!(
                    agent_count = restored.agent_count(),
                    "Runtime State aus Snapshot wiederhergestellt"
                );
                restored
            }
            Err(_) => {
                info!("Kein Runtime-Snapshot vorhanden, starte frisch");
                RuntimeOrchestrator::new(config.max_agents)
                    .with_event_store(Arc::clone(&event_store))
            }
        };

    // -- Sandbox Enforcer (Landlock + cgroups v2 + bwrap) --
    let (sandbox, sandbox_warnings) = SandboxEnforcer::detect();
    for w in &sandbox_warnings {
        match w {
            SandboxWarning::LandlockNotAvailable => {
                warn!("Landlock LSM nicht verfuegbar — Agent-FS-Isolation eingeschraenkt");
            }
            SandboxWarning::CgroupNotDelegated(msg) => {
                warn!(detail = %msg, "cgroup v2 nicht delegiert — Resource-Limits deaktiviert");
            }
            SandboxWarning::BwrapUsernsDenied => {
                warn!("bwrap User-Namespaces blockiert — Agent-Namespace-Isolation deaktiviert");
            }
            SandboxWarning::IoNotDelegated => {
                warn!("IO-Controller nicht delegiert — IO-Limits nicht erzwingbar");
            }
            SandboxWarning::OomScoreFailed(msg) => {
                warn!(detail = %msg, "OOM-Score fuer ECS-Core konnte nicht gesetzt werden");
            }
            SandboxWarning::NetnsNotAvailable => {
                warn!("Network-Namespace nicht verfuegbar — Agent-Netzwerk-Isolation deaktiviert");
            }
        }
    }
    info!(
        landlock = sandbox.has_landlock(),
        cgroups = sandbox.has_cgroups(),
        bwrap = sandbox.has_bwrap(),
        netns = sandbox.has_netns(),
        warnings = sandbox_warnings.len(),
        "Sandbox Enforcer initialisiert"
    );

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

    // -- Hippocampus Memory Service oeffnen --
    let hippocampus_path = data_dir.join("hippocampus.redb");
    let hippocampus = sentinel_hippocampus::HippocampusService::open(
        hippocampus_path
            .to_str()
            .context("hippocampus.redb Pfad nicht UTF-8")?,
    )
    .context("HippocampusService oeffnen")?;

    // Agent-Name-Mapping fuer Episode Producer
    let agent_name_pairs: Vec<(u16, String)> = all_agents
        .iter()
        .map(|a| (a.identity.id, a.identity.name.clone()))
        .collect();
    let episode_producer = EpisodeProducer::new(hippocampus, &agent_name_pairs, &event_store);
    info!("Episode Producer initialisiert");

    // -- eBPF Monitoring initialisieren --
    let (ebpf_collector, ebpf_mode) = crate::ebpf::init_ebpf();
    info!(mode = %ebpf_mode, "eBPF Monitoring initialisiert");

    // -- eBPF Bridge: mpsc + shared Prometheus Text --
    let (ebpf_tx, ebpf_rx) = tokio::sync::mpsc::channel::<MetricsSnapshot>(4);
    let prometheus_text = Arc::new(RwLock::new(String::new()));

    // -- Channels fuer ECS <-> Async Bridge --
    let (action_tx, action_rx) = mpsc::channel();
    let (perception_tx, perception_rx) = mpsc::sync_channel::<Perception>(64);

    // -- Shutdown Flag --
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_ecs = Arc::clone(&shutdown);

    // Werte fuer den ECS-Thread
    let tick_rate = Duration::from_millis(config.tick_rate_ms);
    let all_agents_clone = all_agents.clone();

    // -- ECS Tick Loop (dedizierter Thread, bevy_ecs World ist Send+Sync) --
    let ecs_handle = std::thread::Builder::new()
        .name("ecs-tick-loop".into())
        .spawn(move || {
            ecs_tick_loop(
                state_store,
                event_store,
                action_rx,
                perception_tx,
                all_agents_clone,
                current_shift,
                tick_rate,
                shutdown_ecs,
                controlplane,
                runtime_orch,
                sandbox,
                ebpf_collector,
                ebpf_tx,
                episode_producer,
            )
        })
        .context("ECS Thread spawnen")?;

    // -- Prometheus eBPF Metrics Server (Port 9090) --
    let prom_text = Arc::clone(&prometheus_text);
    tokio::spawn(crate::ebpf::prometheus_server(prom_text, 9090));

    // -- eBPF Zenoh Publisher + Prometheus Text Renderer --
    let prom_text = Arc::clone(&prometheus_text);
    tokio::spawn(crate::ebpf::ebpf_publisher(ebpf_rx, prom_text));

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

    info!("Daemon heruntergefahren");

    Ok(())
}

/// ECS Tick-Loop auf dediziertem Thread.
///
/// Verwaltet den RuntimeOrchestrator (Lifecycle-Events, Shift-Wechsel, Snapshots)
/// UND die ECS World (Entity-Spawning, Simulation). Laeuft bis `shutdown` gesetzt wird.
/// Speichert Runtime-Snapshot vor Beendigung (AC-4).
#[allow(clippy::too_many_arguments)]
fn ecs_tick_loop(
    state_store: StateStore,
    event_store: Arc<EventStore>,
    action_rx: mpsc::Receiver<sentinel_common::AgentAction>,
    perception_tx: mpsc::SyncSender<Perception>,
    all_agents: Vec<AgentConfig>,
    initial_shift: u8,
    tick_rate: Duration,
    shutdown: Arc<AtomicBool>,
    mut controlplane: ControlplaneKernel,
    mut runtime_orch: RuntimeOrchestrator,
    sandbox: SandboxEnforcer,
    mut ebpf_collector: EbpfCollector,
    ebpf_tx: tokio::sync::mpsc::Sender<MetricsSnapshot>,
    mut episode_producer: EpisodeProducer,
) -> Result<u64> {
    // ECS World + Schedule erstellen
    let (mut world, mut schedule) = create_simulation_world();

    // Stores als Resources einfuegen
    attach_redb_store(&mut world, state_store);
    let event_store_for_episodes = Arc::clone(&event_store);
    world.insert_resource(LimboEventStore(event_store));
    world.insert_resource(ActionReceiver(std::sync::Mutex::new(action_rx)));
    world.insert_resource(PerceptionSender(perception_tx));

    // -- Sandbox Handles (cgroup + bwrap tracking pro Agent) --
    let mut sandbox_handles: HashMap<AgentId, SandboxHandle> = HashMap::new();

    // -- Agent-Spawning (Orchestrator + ECS + Sandbox) --
    let is_restored = runtime_orch.agent_count() > 0;
    let shift_agents = agents_for_shift(&all_agents, initial_shift);

    if is_restored {
        // Nach Restore: Shift-Transition durchfuehren falls Schicht gewechselt hat
        // (z.B. Daemon um 13:59 gestoppt, um 14:05 neu gestartet)
        let removed = runtime_orch.shift_transition(initial_shift);
        if !removed.is_empty() {
            info!(
                removed_count = removed.len(),
                "Stale Agents nach Restore entfernt (Schichtwechsel waehrend Downtime)"
            );
        }
    }

    // Agents spawnen: Orchestrator registriert (falls nicht via Restore), ECS erstellt Entity,
    // Sandbox Setup (cgroup + home dir) bei jedem Spawn (AC-4).
    for agent_cfg in &shift_agents {
        let agent_id = AgentId(agent_cfg.identity.id);

        if runtime_orch.get_agent_mut(agent_id).is_none() {
            // Nicht im Orchestrator → neu registrieren (emittiert Lifecycle-Events)
            let identity = AgentIdentity {
                agent_id,
                name: agent_cfg.identity.name.clone(),
                role: agent_cfg.identity.role.clone(),
            };
            let (start, end) = shift_hours(agent_cfg.identity.shift_set);
            let shift = ShiftInfo {
                shift_set: agent_cfg.identity.shift_set,
                shift_start_hour: start,
                shift_end_hour: end,
                is_on_duty: true,
            };
            if let Err(e) = runtime_orch.spawn_agent(identity, shift, "empfang") {
                warn!(agent_id = agent_cfg.identity.id, error = %e, "Agent-Spawn fehlgeschlagen");
                continue;
            }
        }

        // Sandbox setup (cgroup + agent home) — AC-3: < 10ms, AC-4: bei jedem Spawn
        let t0 = Instant::now();
        match sandbox.setup_agent(&agent_cfg.identity.name, &default_agent_limits()) {
            Ok(handle) => {
                let elapsed = t0.elapsed();
                info!(
                    agent = %agent_cfg.identity.name,
                    cgroup = handle.cgroup_created,
                    io = handle.io_available,
                    elapsed_us = elapsed.as_micros(),
                    "Sandbox setup abgeschlossen"
                );
                // eBPF Agent-Registrierung (cgroup_id fuer BPF Map Correlation)
                if handle.cgroup_created {
                    if let Some(cid) = sentinel_sandbox::cgroup_id(&agent_cfg.identity.name) {
                        ebpf_collector.register_agent(sentinel_ebpf::AgentCgroupMapping {
                            agent_name: agent_cfg.identity.name.clone(),
                            cgroup_path: sentinel_sandbox::cgroup_path(&agent_cfg.identity.name),
                            cgroup_id: cid,
                            pid: None,
                        });
                    }
                }
                sandbox_handles.insert(agent_id, handle);
            }
            Err(e) => {
                warn!(
                    agent = %agent_cfg.identity.name,
                    error = %e,
                    "Sandbox setup fehlgeschlagen (Agent laeuft ohne Isolation)"
                );
            }
        }

        // ECS Entity erstellen
        spawn_agent(
            &mut world,
            agent_id,
            &agent_cfg.identity.name,
            &agent_cfg.identity.role,
            agent_cfg.identity.shift_set,
        );
    }

    // EventBuffer leeren: ECS spawn_agent emittiert eigene Events, aber der
    // RuntimeOrchestrator ist SSOT fuer Lifecycle-Events (vermeidet Duplikate)
    if let Some(mut event_buffer) = world.get_resource_mut::<EventBuffer>() {
        event_buffer.events.clear();
    }

    info!(
        agent_count = shift_agents.len(),
        orchestrator_count = runtime_orch.agent_count(),
        restored = is_restored,
        shift_set = initial_shift,
        "ECS World initialisiert"
    );

    let mut tick_count: u64 = 0;
    let mut current_shift = initial_shift;

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

        // RuntimeOrchestrator Tick synchronisieren
        runtime_orch.set_tick(tick_count);

        // ECS Schedule ausfuehren (alle 10 Systems in Reihenfolge)
        schedule.run(&mut world);

        // Controlplane-Zyklus (alle N Ticks)
        if controlplane.should_run(tick_count) {
            if let Err(e) = controlplane.cycle(&mut world, tick_count) {
                error!(error = %e, tick = tick_count, "Controlplane-Zyklus fehlgeschlagen");
            }
        }

        // Shift-Erkennung (alle 60 Ticks = ~1 Minute bei 1s Tick-Rate)
        if tick_count > 0 && tick_count.is_multiple_of(60) {
            let new_shift = detect_current_shift();
            if new_shift != current_shift {
                info!(
                    old = current_shift,
                    new = new_shift,
                    "Schichtwechsel erkannt"
                );

                // Alte Schicht-Agents entfernen (Orchestrator entfernt + emittiert Events)
                let removed = runtime_orch.shift_transition(new_shift);
                for agent_id in &removed {
                    // Sandbox teardown VOR ECS despawn
                    if let Some(handle) = sandbox_handles.remove(agent_id) {
                        // eBPF Agent-Unregistrierung
                        if handle.cgroup_created {
                            if let Some(cid) = sentinel_sandbox::cgroup_id(&handle.agent_name) {
                                ebpf_collector.unregister_agent(cid);
                            }
                        }
                        if let Err(e) = sandbox.teardown_agent(&handle) {
                            warn!(agent_id = %agent_id, error = %e, "Sandbox teardown fehlgeschlagen");
                        }
                    }
                    if !despawn_agent_from_world(&mut world, *agent_id) {
                        warn!(agent_id = %agent_id, "ECS Entity fuer entfernten Agent nicht gefunden");
                    }
                }

                // Neue Schicht-Agents spawnen (mit Sandbox-Setup)
                let new_agents = agents_for_shift(&all_agents, new_shift);
                let mut spawned_count = 0u32;
                for agent_cfg in &new_agents {
                    let agent_id = AgentId(agent_cfg.identity.id);
                    // Set 0 (Sonder) bleibt, nicht nochmal spawnen
                    if runtime_orch.get_agent_mut(agent_id).is_some() {
                        continue;
                    }
                    if spawn_agent_full(
                        &mut runtime_orch,
                        &mut world,
                        agent_cfg,
                        &sandbox,
                        &mut sandbox_handles,
                    ) {
                        spawned_count += 1;
                    }
                }

                // EventBuffer leeren (spawn_agent_full → ECS spawn Events, Orchestrator ist SSOT)
                if let Some(mut event_buffer) = world.get_resource_mut::<EventBuffer>() {
                    event_buffer.events.clear();
                }

                info!(
                    removed = removed.len(),
                    spawned = spawned_count,
                    active = runtime_orch.agent_count(),
                    "Schichtwechsel abgeschlossen"
                );

                current_shift = new_shift;
            }
        }

        // eBPF Metrics Collection (alle 10 Ticks = ~10s bei 1s Tick-Rate)
        if tick_count > 0 && tick_count.is_multiple_of(10) {
            match ebpf_collector.collect() {
                Ok(snapshot) => {
                    // try_send: Non-blocking, dropped wenn Buffer voll (kein Backpressure)
                    let _ = ebpf_tx.try_send(snapshot);
                }
                Err(e) => {
                    warn!(error = %e, tick = tick_count, "eBPF collect fehlgeschlagen");
                }
            }
        }

        // Episode Producer (alle 30 Ticks = ~30s bei 1s Tick-Rate)
        if episode_producer.should_run(tick_count) {
            let tick_rate_s = tick_rate.as_secs_f64();
            episode_producer.tick(&event_store_for_episodes, tick_count, tick_rate_s);
        }

        tick_count += 1;

        if tick_count.is_multiple_of(60) {
            info!(tick = tick_count, "Tick Checkpoint");
        }

        std::thread::sleep(tick_rate);
    }

    // -- Graceful Shutdown: Sandbox teardown fuer alle Agents --
    let teardown_count = sandbox_handles.len();
    for (agent_id, handle) in sandbox_handles.drain() {
        if let Err(e) = sandbox.teardown_agent(&handle) {
            warn!(agent_id = %agent_id, error = %e, "Sandbox teardown bei Shutdown fehlgeschlagen");
        }
    }
    if teardown_count > 0 {
        info!(count = teardown_count, "Sandbox teardown abgeschlossen");
    }

    // -- Graceful Shutdown: Runtime-Snapshot speichern (AC-4 Issue #15) --
    if let Err(e) = runtime_orch.save_state() {
        error!(error = %e, "Runtime State Snapshot fehlgeschlagen");
    } else {
        info!(
            agent_count = runtime_orch.agent_count(),
            "Runtime State Snapshot gespeichert"
        );
    }

    Ok(tick_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controlplane::config::ControlplaneConfig;
    use crate::controlplane::store::ControlplaneStore;
    use sentinel_common::agent_config::{
        BackgroundConfig, IdentityConfig, PersonalityConfig, PreferencesConfig,
    };
    use sentinel_ebpf::loader::MonitoringMode;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    /// Erstellt EbpfCollector + tokio mpsc Sender fuer Tests (Userspace mode, kein tokio noetig).
    fn test_ebpf() -> (EbpfCollector, tokio::sync::mpsc::Sender<MetricsSnapshot>) {
        let collector = EbpfCollector::new(MonitoringMode::Userspace);
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        (collector, tx)
    }

    fn test_controlplane(tmp: &tempfile::TempDir) -> ControlplaneKernel {
        let cp_path = tmp.path().join("controlplane.redb");
        let cp_store = ControlplaneStore::open(&cp_path).unwrap();
        let cp_config = ControlplaneConfig::default_config();
        ControlplaneKernel::new(cp_config, cp_store).unwrap()
    }

    /// Erstellt SandboxEnforcer fuer Tests (degraded mode — keine Kernel-Features noetig).
    fn test_sandbox() -> SandboxEnforcer {
        let (enforcer, _warnings) = SandboxEnforcer::detect();
        enforcer
    }

    /// Erstellt EpisodeProducer fuer Tests (tempfile-basiert).
    fn test_episode_producer(tmp: &tempfile::TempDir, event_store: &EventStore) -> EpisodeProducer {
        let path = tmp.path().join("test-hippocampus.redb");
        let hippocampus =
            sentinel_hippocampus::HippocampusService::open(path.to_str().unwrap()).unwrap();
        EpisodeProducer::new(hippocampus, &[], event_store)
    }

    fn test_agent_config(id: u16, name: &str, role: &str, shift_set: u8) -> AgentConfig {
        AgentConfig {
            identity: IdentityConfig {
                id,
                name: name.to_string(),
                role: role.to_string(),
                department: "Test".to_string(),
                shift_set,
            },
            personality: PersonalityConfig {
                openness: 0.5,
                conscientiousness: 0.5,
                extraversion: 0.5,
                agreeableness: 0.5,
                neuroticism: 0.3,
                caffeine_tolerance: 0.5,
                morning_person: true,
            },
            preferences: PreferencesConfig {
                favorite_room: "empfang".to_string(),
                coffee_preference: "schwarz".to_string(),
                lunch_time: "12:00".to_string(),
            },
            background: BackgroundConfig {
                bio: "Test Agent".to_string(),
                quirks: vec!["testing".to_string()],
            },
        }
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
        let runtime_orch = RuntimeOrchestrator::new(10).with_event_store(Arc::clone(&event_store));

        let ep = test_episode_producer(&tmp, &event_store);
        let (ebpf_collector, ebpf_tx) = test_ebpf();
        let result = ecs_tick_loop(
            state_store,
            event_store,
            rx,
            ptx,
            vec![],
            1,
            Duration::from_millis(100),
            shutdown,
            controlplane,
            runtime_orch,
            test_sandbox(),
            ebpf_collector,
            ebpf_tx,
            ep,
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
        let runtime_orch = RuntimeOrchestrator::new(10).with_event_store(Arc::clone(&event_store));
        let all_agents = vec![test_agent_config(1, "Test Agent", "Tester", 1)];

        // Shutdown nach 500ms (genug Spielraum fuer Build-Server unter Last)
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            shutdown_clone.store(true, Ordering::SeqCst);
        });

        let (ebpf_collector, ebpf_tx) = test_ebpf();
        let ep = test_episode_producer(&tmp, &event_store);
        let result = ecs_tick_loop(
            state_store,
            event_store,
            rx,
            ptx,
            all_agents,
            1,
            Duration::from_millis(50),
            shutdown,
            controlplane,
            runtime_orch,
            test_sandbox(),
            ebpf_collector,
            ebpf_tx,
            ep,
        );

        assert!(result.is_ok());
        let ticks = result.unwrap();
        assert!(ticks >= 1, "Mindestens 1 Tick erwartet, bekam {ticks}");
    }

    #[test]
    fn test_save_state_on_shutdown() {
        // Verifiziert dass Runtime-Snapshot nach Loop-Exit existiert
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
        let runtime_orch = RuntimeOrchestrator::new(10).with_event_store(Arc::clone(&event_store));
        let all_agents = vec![
            test_agent_config(1, "Thomas", "CEO", 1),
            test_agent_config(2, "Lisa", "Designer", 1),
        ];

        // Shutdown nach 500ms (genug Spielraum fuer CI-Runner unter Last)
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            shutdown_clone.store(true, Ordering::SeqCst);
        });

        let es_clone = Arc::clone(&event_store);
        let ep = test_episode_producer(&tmp, &event_store);
        let (ebpf_collector, ebpf_tx) = test_ebpf();
        let result = ecs_tick_loop(
            state_store,
            event_store,
            rx,
            ptx,
            all_agents,
            1,
            Duration::from_millis(50),
            shutdown,
            controlplane,
            runtime_orch,
            test_sandbox(),
            ebpf_collector,
            ebpf_tx,
            ep,
        );

        assert!(result.is_ok());

        // Snapshot muss existieren
        let snapshot = es_clone.get_latest_snapshot("runtime");
        assert!(
            snapshot.is_ok() && snapshot.unwrap().is_some(),
            "Runtime-Snapshot muss nach Shutdown existieren"
        );
    }

    #[test]
    fn test_restore_on_startup() {
        // Verifiziert dass Agents aus Snapshot wiederhergestellt werden
        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");

        let event_store = Arc::new(EventStore::open(events_path.to_str().unwrap()).unwrap());

        // Orchestrator erstellen, 3 Agents spawnen, Snapshot speichern
        let mut orch = RuntimeOrchestrator::new(10).with_event_store(Arc::clone(&event_store));
        orch.set_tick(100);
        for i in 1..=3 {
            let identity = AgentIdentity {
                agent_id: AgentId(i),
                name: format!("Agent-{i}"),
                role: "Worker".to_string(),
            };
            let shift = ShiftInfo {
                shift_set: 1,
                shift_start_hour: 6,
                shift_end_hour: 14,
                is_on_duty: true,
            };
            orch.spawn_agent(identity, shift, "empfang").unwrap();
        }
        orch.save_state().unwrap();
        drop(orch);

        // Restore verifizieren
        let restored = RuntimeOrchestrator::restore(Arc::clone(&event_store), 10).unwrap();
        assert_eq!(
            restored.agent_count(),
            3,
            "Restored Orchestrator muss 3 Agents haben"
        );
    }

    #[test]
    fn test_shift_hours_mapping() {
        assert_eq!(shift_hours(0), (0, 0));
        assert_eq!(shift_hours(1), (6, 14));
        assert_eq!(shift_hours(2), (14, 22));
        assert_eq!(shift_hours(3), (22, 6));
        assert_eq!(shift_hours(99), (6, 14)); // Fallback
    }
}
