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
use tracing::{debug, error, info, warn};

use sentinel_common::agent_config::{load_all_agents, AgentConfig};
use sentinel_common::components::{AgentIdentity, ShiftInfo};
use sentinel_common::events::{DomainEvent, DomainEventPayload};
use sentinel_common::{AgentId, Perception};
use sentinel_ebpf::collector::MetricsSnapshot;
use sentinel_ebpf::EbpfCollector;
use sentinel_ecs::{
    apply_personality, create_simulation_world, despawn_agent_from_world, spawn_agent,
    ActionReceiver, EventBuffer, LimboEventStore, PerceptionSender, SimulationTime,
};
use sentinel_limbo::EventStore;
use sentinel_redb::StateStore;
use sentinel_runtime::RuntimeOrchestrator;
use sentinel_sandbox::{CgroupLimits, SandboxEnforcer, SandboxHandle, SandboxWarning};

use crate::adaptive_tick::AdaptiveTickRate;
use crate::config::DaemonConfig;
use crate::controlplane::config::ControlplaneConfig;
use crate::controlplane::store::ControlplaneStore;
use crate::controlplane::ControlplaneKernel;
use crate::episode_producer::EpisodeProducer;
use crate::shift::{agents_for_shift, detect_current_shift, detect_shift_from_sim_hour};
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
/// Richtet die Sandbox (cgroup + home dir + bwrap-Prozess) ein wenn verfuegbar.
/// Gibt `true` zurueck wenn erfolgreich.
#[allow(clippy::too_many_arguments)]
fn spawn_agent_full(
    runtime_orch: &mut RuntimeOrchestrator,
    world: &mut bevy_ecs::prelude::World,
    agent_cfg: &AgentConfig,
    sandbox: &SandboxEnforcer,
    sandbox_handles: &mut HashMap<AgentId, SandboxHandle>,
    ebpf_collector: &mut EbpfCollector,
    agent_processes: &mut HashMap<AgentId, sentinel_sandbox::AgentProcess>,
    agent_command: &[String],
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

            // Agent-Prozess in bwrap starten (TOGAF: agent-runtime)
            if let Some(handle) = sandbox_handles.get_mut(&agent_id) {
                if handle.cgroup_created {
                    match sandbox.start_agent_process(&agent_cfg.identity.name, agent_command) {
                        Ok(proc) => {
                            let pid = proc.pid;
                            info!(
                                agent = %agent_cfg.identity.name,
                                pid,
                                "Agent-Prozess in bwrap gestartet"
                            );
                            handle.bwrap_pid = Some(pid);

                            // PID im eBPF Mapping aktualisieren (fuer /proc/{pid}/io Tracking)
                            if let Some(cid) = sentinel_sandbox::cgroup_id(&agent_cfg.identity.name)
                            {
                                ebpf_collector.update_agent_pid(cid, pid);
                            }

                            // AgentProcess aufbewahren (haelt Child am Leben, Drop reaps Zombie)
                            agent_processes.insert(agent_id, proc);

                            // Network-Namespace Isolation (optional)
                            let agent_index = (agent_cfg.identity.id % 255) as u8;
                            match sandbox.setup_network(handle, pid, agent_index) {
                                Ok(true) => info!(
                                    agent = %agent_cfg.identity.name,
                                    "Network isolation aktiv"
                                ),
                                Ok(false) => {}
                                Err(e) => warn!(
                                    agent = %agent_cfg.identity.name,
                                    error = %e,
                                    "Netns setup fehlgeschlagen (Agent laeuft ohne Netzwerk-Isolation)"
                                ),
                            }
                        }
                        Err(e) => {
                            warn!(
                                agent = %agent_cfg.identity.name,
                                error = %e,
                                "Agent-Prozess konnte nicht gestartet werden (ECS-only Fallback)"
                            );
                        }
                    }
                }
            }
        }
        Err(e) => {
            warn!(
                agent = %agent_cfg.identity.name,
                error = %e,
                "Sandbox setup fehlgeschlagen (Agent laeuft ohne Isolation)"
            );
        }
    }

    let entity = spawn_agent(
        world,
        agent_id,
        &agent_cfg.identity.name,
        &agent_cfg.identity.role,
        agent_cfg.identity.shift_set,
    );
    apply_personality(world, entity, &agent_cfg.personality);
    sentinel_ecs::apply_capabilities(world, entity, &agent_cfg.capabilities);
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

    let state_store = Arc::new(
        StateStore::open(state_path.to_str().context("state.redb Pfad nicht UTF-8")?)
            .context("StateStore oeffnen")?,
    );

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

    // -- sentinel-fs FUSE Mount (optional, konfigurierbar) --
    #[cfg(feature = "fuse")]
    if let Some(ref fs_mount) = config.fs_mount {
        let mountpoint = std::path::PathBuf::from(fs_mount);
        let data_dir_clone = data_dir.clone();
        if !mountpoint.exists() {
            std::fs::create_dir_all(&mountpoint)
                .with_context(|| format!("FUSE mountpoint erstellen: {}", mountpoint.display()))?;
        }
        info!(
            mountpoint = %mountpoint.display(),
            data_dir = %data_dir_clone.display(),
            "sentinel-fs FUSE-Mount starten"
        );
        std::thread::spawn(move || {
            if let Err(e) = sentinel_fs::start_fuse(&data_dir_clone, &mountpoint) {
                error!(error = %e, "sentinel-fs FUSE-Mount fehlgeschlagen");
            }
        });
        // Kurz warten bis FUSE mounted ist
        std::thread::sleep(Duration::from_millis(200));
        if mountpoint.join("__BASE__").exists() || mountpoint.read_dir().is_ok() {
            info!(mountpoint = %mountpoint.display(), "sentinel-fs FUSE-Mount aktiv");
        } else {
            warn!(mountpoint = %mountpoint.display(), "sentinel-fs FUSE-Mount moeglicherweise nicht bereit");
        }
    }

    // -- Sandbox Enforcer (Landlock + cgroups v2 + bwrap) --
    let (mut sandbox, sandbox_warnings) = SandboxEnforcer::detect();

    // Wenn sentinel-fs FUSE konfiguriert: bwrap nutzt FUSE-Mount statt /ram/agents/
    if let Some(ref fs_mount) = config.fs_mount {
        sandbox.set_fs_mount(fs_mount.clone());
        info!(fs_mount = %fs_mount, "Sandbox nutzt sentinel-fs FUSE-Mount fuer Agent-Homes");
    }
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

    // -- Room Distance Map (BFS-vorberechnet fuer Transit-Dauer + Smell-Propagation) --
    let rooms_toml_path = config.config_dir.join("rooms.toml");
    let room_distances = if rooms_toml_path.exists() {
        match sentinel_common::room::BuildingConfig::load(&rooms_toml_path) {
            Ok(building_cfg) => {
                let map = sentinel_ecs::RoomDistanceMap::from_building_config(&building_cfg);
                info!(
                    rooms = building_cfg.rooms.len(),
                    "RoomDistanceMap aus rooms.toml geladen"
                );
                map
            }
            Err(e) => {
                warn!(error = %e, "rooms.toml konnte nicht geladen werden — Fallback auf Default-Distanzen");
                sentinel_ecs::RoomDistanceMap::default()
            }
        }
    } else {
        warn!("rooms.toml nicht gefunden — Transit-Dauer nutzt Default-Distanzen");
        sentinel_ecs::RoomDistanceMap::default()
    };

    // Werte fuer den ECS-Thread
    let tick_rate = Duration::from_millis(config.tick_rate_ms);
    let time_scale = config.time_scale;
    let adaptive_config = config.adaptive.clone();
    let agent_command_cfg = config.agent_command.clone();
    let all_agents_clone = all_agents.clone();

    // -- Arc::clone fuer Alert-Handler (NATS) bevor event_store in ECS-Thread moved wird --
    #[cfg(feature = "nats")]
    let alert_event_store = Arc::clone(&event_store);

    // -- ECS Tick Loop (dedizierter Thread, bevy_ecs World ist Send+Sync) --
    let ecs_state_store = Arc::clone(&state_store);
    let ecs_handle = std::thread::Builder::new()
        .name("ecs-tick-loop".into())
        .spawn(move || {
            ecs_tick_loop(
                ecs_state_store,
                event_store,
                action_rx,
                perception_tx,
                all_agents_clone,
                current_shift,
                tick_rate,
                time_scale,
                shutdown_ecs,
                controlplane,
                runtime_orch,
                sandbox,
                ebpf_collector,
                ebpf_tx,
                episode_producer,
                agent_command_cfg,
                adaptive_config,
                room_distances,
            )
        })
        .context("ECS Thread spawnen")?;

    // -- Prometheus eBPF Metrics Server (Port 9090) --
    let prom_text = Arc::clone(&prometheus_text);
    tokio::spawn(crate::ebpf::prometheus_server(prom_text, 9090));

    // -- eBPF Zenoh Publisher + NATS Bridge + Prometheus Text Renderer --
    let prom_text = Arc::clone(&prometheus_text);
    #[cfg(feature = "nats")]
    let ebpf_nats_url = Some(config.nats.url.clone());
    #[cfg(not(feature = "nats"))]
    let ebpf_nats_url: Option<String> = None;
    tokio::spawn(crate::ebpf::ebpf_publisher(
        ebpf_rx,
        prom_text,
        ebpf_nats_url,
    ));

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
            Arc::clone(&state_store),
        ))
    };

    // -- NATS Consumer fuer Judge-Alerts --
    #[cfg(feature = "nats")]
    {
        let nats_url = config.nats.url.clone();
        let (alert_tx, mut alert_rx) =
            tokio::sync::mpsc::channel::<crate::nats_consumer::JudgeAlert>(64);

        tokio::spawn(async move {
            crate::nats_consumer::run(&nats_url, alert_tx).await;
        });

        // Alert-Receiver: DomainEvent persistieren + Prometheus Counter + Log
        let es = Arc::clone(&alert_event_store);
        // Gateway URL for model-swap HTTP calls (ADR-001: swap via NATS alert → HTTP to Gateway)
        let gateway_url = std::env::var("CORTEX_GATEWAY_URL")
            .unwrap_or_else(|_| "http://localhost:8081".to_string());
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest Client build");

        tokio::spawn(async move {
            let counter = sentinel_telemetry::MetricsRegistry::global()
                .counter("sentinel_daemon_judge_alerts_total");

            while let Some(alert) = alert_rx.recv().await {
                counter.increment();

                // DomainEvent in Limbo persistieren
                // Parse "AGENT-XX" → u16 → AgentId
                let agent_num: u16 = alert
                    .agent_id
                    .strip_prefix("AGENT-")
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
                let agent_id = AgentId::new(agent_num).unwrap_or(AgentId(1));
                let payload = DomainEventPayload::JudgeAlertReceived {
                    agent_id,
                    alert_type: alert.alert_type.clone(),
                    severity: alert.severity.clone(),
                    score: alert.score,
                    details: alert.details.clone(),
                };
                let event = DomainEvent::new(
                    payload.event_type_str(),
                    &alert.agent_id,
                    &payload.to_json(),
                    &format!("judge_alert_{}", alert.agent_id),
                    0, // kein Simulations-Tick im async Context
                );

                if let Err(e) = es.append_event(&event) {
                    warn!(error = %e, agent = %alert.agent_id, "Judge Alert DomainEvent speichern fehlgeschlagen");
                }

                match alert.alert_type.as_str() {
                    "swap" => {
                        info!(
                            agent_id = %alert.agent_id,
                            severity = %alert.severity,
                            details = %alert.details,
                            alert_ref = "model_swap_requested",
                            "Model-Swap Alert empfangen — DomainEvent persistiert"
                        );

                        // ADR-001: HTTP POST to Gateway Control Plane for model-swap
                        let target_provider = extract_swap_provider(&alert.details);
                        let url = format!("{}/control/agent-provider", gateway_url);
                        let body = serde_json::json!({
                            "agent_id": alert.agent_id,
                            "provider": target_provider,
                        });
                        match http_client.post(&url).json(&body).send().await {
                            Ok(resp) => {
                                info!(
                                    status = %resp.status(),
                                    agent_id = %alert.agent_id,
                                    provider = %target_provider,
                                    "Model-Swap an Gateway gesendet"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    agent_id = %alert.agent_id,
                                    "Model-Swap Gateway-Call fehlgeschlagen"
                                );
                            }
                        }
                    }
                    "drift" => {
                        info!(
                            agent_id = %alert.agent_id,
                            score = alert.score,
                            severity = %alert.severity,
                            alert_ref = "judge_alert_received",
                            "Drift Alert empfangen — DomainEvent persistiert"
                        );
                    }
                    "fatigue" => {
                        info!(
                            agent_id = %alert.agent_id,
                            score = alert.score,
                            severity = %alert.severity,
                            alert_ref = "judge_alert_received",
                            "Fatigue Alert empfangen — DomainEvent persistiert"
                        );
                    }
                    _ => {
                        info!(
                            agent_id = %alert.agent_id,
                            alert_type = %alert.alert_type,
                            score = alert.score,
                            alert_ref = "judge_alert_received",
                            "Judge Alert empfangen — DomainEvent persistiert"
                        );
                    }
                }
            }
        });
    }

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

/// Extrahiert den Ziel-Provider aus den Swap-Alert Details.
///
/// Falls die Details einen bekannten Provider-Namen enthalten, wird dieser zurueckgegeben.
/// "claude-code" wird VOR "claude" geprueft (laengster Match zuerst).
/// Fallback: "claude-code" (Subscription-basiert, kein API Key noetig).
#[cfg(feature = "nats")]
fn extract_swap_provider(details: &str) -> String {
    let lower = details.to_lowercase();
    // Laengste Matches zuerst pruefen ("claude-code" vor "claude")
    for provider in ["claude-code", "claude", "ollama", "qwen3"] {
        if lower.contains(provider) {
            return provider.to_string();
        }
    }
    // Default: claude-code (Subscription-basiert, immer verfuegbar)
    "claude-code".to_string()
}

/// ECS Tick-Loop auf dediziertem Thread.
///
/// Verwaltet den RuntimeOrchestrator (Lifecycle-Events, Shift-Wechsel, Snapshots)
/// UND die ECS World (Entity-Spawning, Simulation). Laeuft bis `shutdown` gesetzt wird.
/// Speichert Runtime-Snapshot vor Beendigung (AC-4).
#[allow(clippy::too_many_arguments)]
fn ecs_tick_loop(
    state_store: Arc<StateStore>,
    event_store: Arc<EventStore>,
    action_rx: mpsc::Receiver<sentinel_common::AgentAction>,
    perception_tx: mpsc::SyncSender<Perception>,
    all_agents: Vec<AgentConfig>,
    initial_shift: u8,
    tick_rate: Duration,
    time_scale: f32,
    shutdown: Arc<AtomicBool>,
    mut controlplane: ControlplaneKernel,
    mut runtime_orch: RuntimeOrchestrator,
    sandbox: SandboxEnforcer,
    mut ebpf_collector: EbpfCollector,
    ebpf_tx: tokio::sync::mpsc::Sender<MetricsSnapshot>,
    mut episode_producer: EpisodeProducer,
    agent_command_cfg: Vec<String>,
    adaptive_config: crate::adaptive_tick::AdaptiveConfig,
    room_distances: sentinel_ecs::RoomDistanceMap,
) -> Result<u64> {
    // Adaptive Tick-Rate Controller (PSI-basiert, TOGAF Adaptive Scheduling)
    let mut adaptive_tick = AdaptiveTickRate::new(adaptive_config);

    // ECS World + Schedule erstellen
    let (mut world, mut schedule) = create_simulation_world();

    // Diegetisches HW-Mapping: PSI-Metriken als ECS Resource (bio_system liest diese)
    world.insert_resource(sentinel_ecs::PsiMetrics::default());
    // Room-Distanzen fuer Transit-Dauer und Smell-Propagation
    world.insert_resource(room_distances);

    // Stores als Resources einfuegen (Arc<StateStore> direkt verwenden)
    let state_store_for_sim = Arc::clone(&state_store);
    world.insert_resource(sentinel_ecs::RedbStateStore {
        store: state_store,
        persist_every_n_ticks: 20,
    });
    if let Some(mut telemetry) = world.get_resource_mut::<sentinel_ecs::PersistTelemetry>() {
        telemetry.enabled = true;
    }
    let event_store_for_episodes = Arc::clone(&event_store);
    world.insert_resource(LimboEventStore(event_store));
    world.insert_resource(ActionReceiver(std::sync::Mutex::new(action_rx)));
    world.insert_resource(PerceptionSender(perception_tx));

    // -- Tool Registry (sentinel-wasm native handlers) --
    let mut tool_runtime = sentinel_wasm::ToolRuntime::new();
    let _ = tool_runtime.register_tool(sentinel_wasm::ToolDefinition {
        name: "file_read".into(),
        description: "Liest Dateien aus dem Agent-Home".into(),
        wasm_path: None,
        tool_type: sentinel_wasm::ToolType::FileRead,
        required_capabilities: vec!["file_read".into()],
    });
    let _ = tool_runtime.register_tool(sentinel_wasm::ToolDefinition {
        name: "file_write".into(),
        description: "Schreibt Dateien ins Agent-Home".into(),
        wasm_path: None,
        tool_type: sentinel_wasm::ToolType::FileWrite,
        required_capabilities: vec!["file_write".into()],
    });
    let _ = tool_runtime.register_tool(sentinel_wasm::ToolDefinition {
        name: "chat".into(),
        description: "Sendet Nachricht an anderen Agent".into(),
        wasm_path: None,
        tool_type: sentinel_wasm::ToolType::Chat,
        required_capabilities: vec!["chat".into()],
    });
    let _ = tool_runtime.register_tool(sentinel_wasm::ToolDefinition {
        name: "calendar".into(),
        description: "Kalender-Verwaltung".into(),
        wasm_path: None,
        tool_type: sentinel_wasm::ToolType::Calendar,
        required_capabilities: vec!["calendar".into()],
    });
    let _ = tool_runtime.register_tool(sentinel_wasm::ToolDefinition {
        name: "search".into(),
        description: "Suche in Dokumenten/Agents/Raeumen".into(),
        wasm_path: None,
        tool_type: sentinel_wasm::ToolType::Search,
        required_capabilities: vec!["search".into()],
    });
    // -- WASM Plugin Auto-Load aus config/tools/ --
    #[cfg(feature = "wasm")]
    {
        let tools_dir = std::path::Path::new("config/tools");
        if tools_dir.is_dir() {
            match std::fs::read_dir(tools_dir) {
                Ok(entries) => {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().is_some_and(|ext| ext == "wasm") {
                            let config = sentinel_wasm::PluginConfig {
                                wasm_path: path.clone(),
                                ..Default::default()
                            };
                            match tool_runtime.plugin_host_mut().load(config) {
                                Ok(()) => {
                                    // Query plugin metadata (tool-name, tool-description)
                                    let agent_home =
                                        std::path::PathBuf::from("/tmp/plugin-meta-query");
                                    let _ = std::fs::create_dir_all(&agent_home);
                                    match tool_runtime
                                        .plugin_host()
                                        .query_meta(&path, agent_home)
                                    {
                                        Ok(meta) => {
                                            let tool_def = sentinel_wasm::ToolDefinition {
                                                name: meta.tool_name.clone(),
                                                description: meta.tool_description,
                                                wasm_path: Some(
                                                    path.to_string_lossy().to_string(),
                                                ),
                                                tool_type: sentinel_wasm::ToolType::Wasm,
                                                required_capabilities: Vec::new(),
                                            };
                                            match tool_runtime.register_tool(tool_def) {
                                                Ok(()) => info!(
                                                    tool = %meta.tool_name,
                                                    path = %path.display(),
                                                    "WASM Plugin geladen"
                                                ),
                                                Err(e) => warn!(
                                                    path = %path.display(),
                                                    error = %e,
                                                    "WASM Plugin Registrierung fehlgeschlagen"
                                                ),
                                            }
                                        }
                                        Err(e) => warn!(
                                            path = %path.display(),
                                            error = %e,
                                            "WASM Plugin Meta-Query fehlgeschlagen"
                                        ),
                                    }
                                }
                                Err(e) => warn!(
                                    path = %path.display(),
                                    error = %e,
                                    "WASM Plugin laden fehlgeschlagen"
                                ),
                            }
                        }
                    }
                }
                Err(e) => debug!(
                    path = %tools_dir.display(),
                    error = %e,
                    "config/tools/ nicht lesbar"
                ),
            }
        }
    }

    info!(
        tools = tool_runtime.tool_count(),
        "Tool Registry initialisiert"
    );
    world.insert_resource(sentinel_ecs::ToolRuntimeResource(tool_runtime));

    // -- Sandbox Handles (cgroup + bwrap tracking pro Agent) --
    let mut sandbox_handles: HashMap<AgentId, SandboxHandle> = HashMap::new();

    // -- Agent-Prozesse (bwrap Child Handles, Drop reaps Zombies) --
    let mut agent_processes: HashMap<AgentId, sentinel_sandbox::AgentProcess> = HashMap::new();
    let agent_command = agent_command_cfg;

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

                // Agent-Prozess in bwrap starten (TOGAF: agent-runtime)
                if let Some(h) = sandbox_handles.get_mut(&agent_id) {
                    if h.cgroup_created {
                        match sandbox.start_agent_process(&agent_cfg.identity.name, &agent_command)
                        {
                            Ok(proc) => {
                                let pid = proc.pid;
                                info!(
                                    agent = %agent_cfg.identity.name,
                                    pid,
                                    "Agent-Prozess in bwrap gestartet"
                                );
                                h.bwrap_pid = Some(pid);

                                // PID im eBPF Mapping aktualisieren
                                if let Some(cid) =
                                    sentinel_sandbox::cgroup_id(&agent_cfg.identity.name)
                                {
                                    ebpf_collector.update_agent_pid(cid, pid);
                                }

                                // AgentProcess aufbewahren (Drop reaps Zombie)
                                agent_processes.insert(agent_id, proc);

                                // Network-Namespace Isolation (optional)
                                let agent_index = (agent_cfg.identity.id % 255) as u8;
                                match sandbox.setup_network(h, pid, agent_index) {
                                    Ok(true) => info!(
                                        agent = %agent_cfg.identity.name,
                                        "Network isolation aktiv"
                                    ),
                                    Ok(false) => {}
                                    Err(e) => warn!(
                                        agent = %agent_cfg.identity.name,
                                        error = %e,
                                        "Netns setup fehlgeschlagen"
                                    ),
                                }
                            }
                            Err(e) => {
                                warn!(
                                    agent = %agent_cfg.identity.name,
                                    error = %e,
                                    "Agent-Prozess konnte nicht gestartet werden (ECS-only Fallback)"
                                );
                            }
                        }
                    }
                }
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
        let entity = spawn_agent(
            &mut world,
            agent_id,
            &agent_cfg.identity.name,
            &agent_cfg.identity.role,
            agent_cfg.identity.shift_set,
        );
        apply_personality(&mut world, entity, &agent_cfg.personality);
        sentinel_ecs::apply_capabilities(&mut world, entity, &agent_cfg.capabilities);
    }

    // GOLF: Default-Goals fuer alle gespawnten Agents erstellen
    for agent_cfg in &shift_agents {
        let existing = episode_producer
            .hippocampus()
            .get_goals(&agent_cfg.identity.name)
            .unwrap_or_default();
        if existing.is_empty() {
            let goals = sentinel_hippocampus::default_goals_for_role(
                &agent_cfg.identity.name,
                &agent_cfg.identity.role,
                0, // initial tick
            );
            if let Err(e) = episode_producer
                .hippocampus()
                .create_goals(&agent_cfg.identity.name, &goals)
            {
                warn!(
                    agent = %agent_cfg.identity.name,
                    error = %e,
                    "GOLF: Default-Goals konnten nicht erstellt werden"
                );
            } else {
                info!(
                    agent = %agent_cfg.identity.name,
                    goal_count = goals.len(),
                    "GOLF: Default-Goals erstellt"
                );
            }
        }
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

    // sim_hour aus redb restaurieren (Fallback: 8.0 fuer Erststart)
    let mut sim_hour: f32 = state_store_for_sim
        .get_sim_hour()
        .ok()
        .flatten()
        .unwrap_or(8.0);
    info!(
        restored_sim_hour = format!("{:.2}", sim_hour),
        time_scale, "sim_hour initialisiert"
    );

    // Telemetrie-Gauge fuer Dashboard/Prometheus (TOGAF: tick_duration_ms)
    let tick_duration_gauge =
        sentinel_telemetry::MetricsRegistry::global().gauge("sentinel_tick_duration_ms");
    let tick_rate_effective_gauge =
        sentinel_telemetry::MetricsRegistry::global().gauge("sentinel_tick_rate_effective_ms");
    let psi_cpu_gauge =
        sentinel_telemetry::MetricsRegistry::global().gauge("sentinel_psi_cpu_avg10");
    let psi_mem_gauge =
        sentinel_telemetry::MetricsRegistry::global().gauge("sentinel_psi_mem_avg10");
    let psi_io_gauge = sentinel_telemetry::MetricsRegistry::global().gauge("sentinel_psi_io_avg10");

    loop {
        let tick_start = Instant::now();

        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        // PSI-basierte adaptive Tick-Rate aktualisieren (alle N Ticks)
        adaptive_tick.update(tick_count);

        // SimulationTime aktualisieren (Zeitvirtualisierung via time_scale)
        if let Some(mut time) = world.get_resource_mut::<SimulationTime>() {
            time.tick = sentinel_common::Tick(tick_count);
            time.tick_count = tick_count;
            // delta_seconds = echte Tick-Dauer * time_scale (Zeitvirtualisierung)
            time.delta_seconds = tick_rate.as_secs_f32() * time_scale;
            // sim_hour inkrementell (persistiert in redb, ueberlebt Restart)
            sim_hour = (sim_hour + time.delta_seconds / 3600.0) % 24.0;
            time.sim_hour = sim_hour;
        }

        // PSI-Metriken in ECS World injizieren (fuer bio_system → apply_psi_stress)
        if let Some(mut psi) = world.get_resource_mut::<sentinel_ecs::PsiMetrics>() {
            psi.cpu_avg10 = adaptive_tick.cpu_avg10();
            psi.mem_avg10 = adaptive_tick.mem_avg10();
        }

        // RuntimeOrchestrator Tick synchronisieren
        runtime_orch.set_tick(tick_count);

        // ECS Schedule ausfuehren (alle 12 Systems in Reihenfolge)
        schedule.run(&mut world);

        // Controlplane-Zyklus (alle N Ticks)
        if controlplane.should_run(tick_count) {
            if let Err(e) = controlplane.cycle(&mut world, tick_count) {
                error!(error = %e, tick = tick_count, "Controlplane-Zyklus fehlgeschlagen");
            }
        }

        // Shift-Erkennung (alle 60 Ticks = ~1 Minute bei 1s Tick-Rate)
        if tick_count > 0 && tick_count.is_multiple_of(60) {
            let new_shift = if (time_scale - 1.0).abs() < f32::EPSILON {
                detect_current_shift() // Production: System-Uhrzeit
            } else {
                detect_shift_from_sim_hour(sim_hour) // Beschleunigt: sim_hour
            };
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
                    // AgentProcess droppen (reaps Zombie via Drop impl)
                    agent_processes.remove(agent_id);

                    if !despawn_agent_from_world(&mut world, *agent_id) {
                        warn!(agent_id = %agent_id, "ECS Entity fuer entfernten Agent nicht gefunden");
                    }
                }

                // Memory-Konsolidierung fuer entfernte Agents (nutzt den
                // bereits geoeffneten HippocampusService Handle, vermeidet
                // redb Lock-Konflikte mit Night-Run)
                let redb_store = world
                    .get_resource::<sentinel_ecs::RedbStateStore>()
                    .map(|r| r.store.clone());
                for agent_id in &removed {
                    let agent_name = all_agents
                        .iter()
                        .find(|a| AgentId(a.identity.id) == *agent_id)
                        .map(|a| a.identity.name.as_str());
                    if let Some(name) = agent_name {
                        match episode_producer.hippocampus().consolidate_agent(name) {
                            Ok(result) => {
                                if result.episodes_processed > 0 {
                                    info!(
                                        agent = name,
                                        episodes_processed = result.episodes_processed,
                                        episodes_consolidated = result.episodes_consolidated,
                                        "Schichtwechsel-Konsolidierung abgeschlossen"
                                    );

                                    // Evolution-Daten nach redb schreiben
                                    if let Some(ref store) = redb_store {
                                        let narrative: String = result
                                            .consolidated_summaries
                                            .iter()
                                            .map(|(s, _score)| s.as_str())
                                            .collect::<Vec<_>>()
                                            .join("; ");

                                        // LLM-basierte Voice-Style + Behavioral-Notes Generierung
                                        let agent_role = all_agents
                                            .iter()
                                            .find(|a| AgentId(a.identity.id) == *agent_id)
                                            .map(|a| a.identity.role.as_str())
                                            .unwrap_or("Mitarbeiter");
                                        let (voice_style, behavioral_notes) =
                                            generate_evolution_fields(name, agent_role, &narrative);

                                        match store.set_evolution_batch(
                                            *agent_id,
                                            voice_style.as_deref(),
                                            behavioral_notes.as_deref(),
                                            Some(narrative.as_bytes()),
                                            None, // agent_facts bridged separately
                                        ) {
                                            Ok(version) => {
                                                info!(
                                                    agent = name,
                                                    version,
                                                    voice_style = voice_style.is_some(),
                                                    behavioral_notes = behavioral_notes.is_some(),
                                                    "Evolution nach redb geschrieben, EVOLUTION_VERSION = {version}"
                                                );
                                            }
                                            Err(e) => {
                                                warn!(
                                                    agent = name,
                                                    error = %e,
                                                    "Evolution redb-Write fehlgeschlagen"
                                                );
                                            }
                                        }

                                        // NMDA scores nach redb schreiben
                                        let scores: Vec<f64> = result
                                            .consolidated_summaries
                                            .iter()
                                            .map(|(_s, score)| *score)
                                            .collect();
                                        if !scores.is_empty() {
                                            let avg_score: f64 =
                                                scores.iter().sum::<f64>() / scores.len() as f64;
                                            match store.set_nmda_scores(*agent_id, &scores) {
                                                Ok(()) => {
                                                    info!(
                                                        agent = name,
                                                        nmda_count = scores.len(),
                                                        nmda_avg = format!("{avg_score:.4}"),
                                                        "NMDA scores nach redb geschrieben"
                                                    );
                                                }
                                                Err(e) => {
                                                    warn!(
                                                        agent = name,
                                                        error = %e,
                                                        "NMDA scores redb-Write fehlgeschlagen"
                                                    );
                                                }
                                            }
                                        }

                                        // Facts aus Hippocampus FactRetriever nach state.redb bridgen
                                        let facts =
                                            episode_producer.hippocampus().retrieve_facts(name);
                                        if !facts.is_empty() {
                                            let facts_json =
                                                serde_json::to_vec(&facts).unwrap_or_default();
                                            match store.set_agent_facts(*agent_id, &facts_json) {
                                                Ok(()) => {
                                                    info!(
                                                        agent = name,
                                                        facts_count = facts.len(),
                                                        "AGENT_FACTS nach state.redb geschrieben"
                                                    );
                                                }
                                                Err(e) => {
                                                    warn!(
                                                        agent = name,
                                                        error = %e,
                                                        "AGENT_FACTS redb-Write fehlgeschlagen"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(agent = name, error = %e, "Schichtwechsel-Konsolidierung fehlgeschlagen");
                            }
                        }
                    }
                }

                // Memory-Pressure Check: Agent-Spawn blockieren wenn Mem PSI > Threshold
                if adaptive_tick.should_block_spawn() {
                    warn!(
                        mem_psi = format!("{:.1}", adaptive_tick.mem_avg10()),
                        "Memory PSI ueber Schwellwert — Agent-Spawn verzoegert bis Druck sinkt"
                    );
                    // Schichtwechsel registrieren aber Spawn auf naechsten Zyklus verschieben
                    current_shift = new_shift;
                    continue;
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
                        &mut ebpf_collector,
                        &mut agent_processes,
                        &agent_command,
                    ) {
                        // GOLF: Default-Goals fuer neuen Schicht-Agent erstellen
                        let existing = episode_producer
                            .hippocampus()
                            .get_goals(&agent_cfg.identity.name)
                            .unwrap_or_default();
                        if existing.is_empty() {
                            let goals = sentinel_hippocampus::default_goals_for_role(
                                &agent_cfg.identity.name,
                                &agent_cfg.identity.role,
                                tick_count,
                            );
                            if let Err(e) = episode_producer
                                .hippocampus()
                                .create_goals(&agent_cfg.identity.name, &goals)
                            {
                                warn!(
                                    agent = %agent_cfg.identity.name,
                                    error = %e,
                                    "GOLF: Default-Goals konnten nicht erstellt werden"
                                );
                            } else {
                                info!(
                                    agent = %agent_cfg.identity.name,
                                    goal_count = goals.len(),
                                    "GOLF: Default-Goals erstellt"
                                );
                            }
                        }
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

        // Periodischer Runtime-Snapshot (alle 600 Ticks = ~10 Minuten bei 1s Tick-Rate)
        if tick_count > 0 && tick_count.is_multiple_of(600) {
            if let Err(e) = runtime_orch.save_state() {
                warn!(error = %e, tick = tick_count, "Periodischer Snapshot fehlgeschlagen");
            } else {
                info!(
                    tick = tick_count,
                    "Periodischer Runtime-Snapshot gespeichert"
                );
            }
        }

        tick_count += 1;

        if tick_count.is_multiple_of(60) {
            // sim_hour periodisch persistieren
            if let Err(e) = state_store_for_sim.set_sim_hour(sim_hour) {
                warn!(error = %e, "sim_hour persist fehlgeschlagen");
            }
            info!(
                tick = tick_count,
                sim_hour = format!("{:.2}", sim_hour),
                "Tick Checkpoint"
            );
        }

        // Adaptive Tick-Rate: PSI-basiert (TOGAF Adaptive Scheduling)
        let effective_rate = adaptive_tick.compute_effective_rate(tick_rate);
        let tick_elapsed = tick_start.elapsed();
        if effective_rate > tick_elapsed {
            std::thread::sleep(effective_rate - tick_elapsed);
        }

        // Telemetrie: Tick-Dauer + PSI-Werte fuer Dashboard/Prometheus
        let total_tick_ms = tick_start.elapsed().as_millis() as i64;
        tick_duration_gauge.set(total_tick_ms);
        tick_rate_effective_gauge.set(effective_rate.as_millis() as i64);
        // PSI avg10 in Promille (×10) um eine Dezimalstelle Praezision zu erhalten.
        // Dashboard teilt durch 1000 fuer Fraktion [0,1].
        psi_cpu_gauge.set((adaptive_tick.cpu_avg10() * 10.0) as i64);
        psi_mem_gauge.set((adaptive_tick.mem_avg10() * 10.0) as i64);
        psi_io_gauge.set((adaptive_tick.io_avg10() * 10.0) as i64);
    }

    // -- Graceful Shutdown: Agent-Prozesse droppen (reaps Zombies via Drop impl) --
    agent_processes.clear();

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

    // sim_hour vor Shutdown persistieren
    if let Err(e) = state_store_for_sim.set_sim_hour(sim_hour) {
        warn!(error = %e, "sim_hour Shutdown-Persist fehlgeschlagen");
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

/// Generiert voice_style und behavioral_notes via LLM (Cortex Gateway).
///
/// Laeuft im ECS std::thread — nutzt `reqwest::blocking` (kein Tokio).
/// Fail-safe: Bei jedem Fehler wird `(None, None)` zurueckgegeben,
/// die Konsolidierung laeuft trotzdem durch.
#[cfg(feature = "llm")]
fn generate_evolution_fields(
    agent_name: &str,
    agent_role: &str,
    narrative: &str,
) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let gateway_url =
        std::env::var("CORTEX_GATEWAY_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());
    let url = format!("{gateway_url}/v1/chat/completions");

    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "Evolution LLM Client erstellen fehlgeschlagen");
            return (None, None);
        }
    };

    // Voice-Style Analyse
    let voice_style = llm_evolution_call(
        &client,
        &url,
        agent_name,
        "Du bist ein linguistischer Analyst fuer eine Firmen-Simulation. \
         Analysiere den Sprachstil des Agenten basierend auf seiner Schicht-Zusammenfassung. \
         Antworte AUSSCHLIESSLICH als valides JSON.",
        &format!(
            "Agent \"{agent_name}\" (Rolle: {agent_role}) hatte folgende Schicht-Erfahrungen:\n\n\
             {narrative}\n\n\
             Analysiere den Sprachstil. Antwort als JSON:\n\
             {{\"phrases\": [\"phrase1\"], \"sentence_style\": \"kurz|mittel|lang\", \"formality\": 0.X}}"
        ),
    );

    // Behavioral-Notes Analyse
    let behavioral_notes = llm_evolution_call(
        &client,
        &url,
        agent_name,
        "Du bist ein Verhaltensanalyst fuer eine Firmen-Simulation. \
         Analysiere Verhaltensmuster des Agenten basierend auf seiner Schicht-Zusammenfassung. \
         Antworte AUSSCHLIESSLICH als valides JSON.",
        &format!(
            "Agent \"{agent_name}\" (Rolle: {agent_role}) hatte folgende Schicht-Erfahrungen:\n\n\
             {narrative}\n\n\
             Identifiziere Verhaltensmuster. Antwort als JSON:\n\
             {{\"habits\": [\"habit1\"], \"interaction_style\": \"proaktiv|reaktiv|gemischt\", \
             \"decision_style\": \"schnell|zoegerlich|ausgewogen\", \"anomalies\": []}}"
        ),
    );

    (voice_style, behavioral_notes)
}

/// Einzelner LLM-Call fuer Evolution-Feld-Generierung.
#[cfg(feature = "llm")]
fn llm_evolution_call(
    client: &reqwest::blocking::Client,
    url: &str,
    agent_name: &str,
    system_prompt: &str,
    user_prompt: &str,
) -> Option<Vec<u8>> {
    let body = serde_json::json!({
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_prompt}
        ],
        "temperature": 0.3,
        "max_tokens": 500,
        "model": "default",
        "metadata": {
            "agent_id": agent_name,
            "request_type": "evolution_analysis"
        }
    });

    match client.post(url).json(&body).send() {
        Ok(resp) => {
            if !resp.status().is_success() {
                warn!(
                    agent = agent_name,
                    status = %resp.status(),
                    "Evolution LLM Call fehlgeschlagen (HTTP)"
                );
                return None;
            }
            match resp.json::<serde_json::Value>() {
                Ok(json) => {
                    let content = json
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    if content.is_empty() {
                        warn!(agent = agent_name, "Evolution LLM Response leer");
                        return None;
                    }
                    Some(content.as_bytes().to_vec())
                }
                Err(e) => {
                    warn!(agent = agent_name, error = %e, "Evolution LLM Response parse fehlgeschlagen");
                    None
                }
            }
        }
        Err(e) => {
            warn!(agent = agent_name, error = %e, "Evolution LLM Call fehlgeschlagen");
            None
        }
    }
}

/// Fallback wenn LLM-Feature deaktiviert ist.
#[cfg(not(feature = "llm"))]
fn generate_evolution_fields(
    _agent_name: &str,
    _agent_role: &str,
    _narrative: &str,
) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    (None, None)
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
            capabilities: Default::default(),
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
        let state_store = Arc::new(StateStore::open(state_path.to_str().unwrap()).unwrap());

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
            1.0, // time_scale
            shutdown,
            controlplane,
            runtime_orch,
            test_sandbox(),
            ebpf_collector,
            ebpf_tx,
            ep,
            vec!["true".to_string()],
            crate::adaptive_tick::AdaptiveConfig::default(),
            sentinel_ecs::RoomDistanceMap::default(),
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
        let state_store = Arc::new(StateStore::open(state_path.to_str().unwrap()).unwrap());

        let (_tx, rx) = mpsc::channel();
        let (ptx, prx) = mpsc::sync_channel(64);

        let controlplane = test_controlplane(&tmp);
        let runtime_orch = RuntimeOrchestrator::new(10).with_event_store(Arc::clone(&event_store));
        let all_agents = vec![test_agent_config(1, "Test Agent", "Tester", 1)];

        // Deterministisch: Warte auf erste Perception (= mindestens 1 Tick abgeschlossen)
        std::thread::spawn(move || {
            let _ = prx.recv_timeout(Duration::from_secs(30));
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
            1.0, // time_scale
            shutdown,
            controlplane,
            runtime_orch,
            test_sandbox(),
            ebpf_collector,
            ebpf_tx,
            ep,
            vec!["true".to_string()],
            crate::adaptive_tick::AdaptiveConfig::default(),
            sentinel_ecs::RoomDistanceMap::default(),
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
        let state_store = Arc::new(StateStore::open(state_path.to_str().unwrap()).unwrap());

        let (_tx, rx) = mpsc::channel();
        let (ptx, prx) = mpsc::sync_channel(64);

        let controlplane = test_controlplane(&tmp);
        let runtime_orch = RuntimeOrchestrator::new(10).with_event_store(Arc::clone(&event_store));
        let all_agents = vec![
            test_agent_config(1, "Thomas", "CEO", 1),
            test_agent_config(2, "Lisa", "Designer", 1),
        ];

        // Deterministisch: Warte auf erste Perception (= mindestens 1 Tick abgeschlossen)
        std::thread::spawn(move || {
            let _ = prx.recv_timeout(Duration::from_secs(30));
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
            1.0, // time_scale
            shutdown,
            controlplane,
            runtime_orch,
            test_sandbox(),
            ebpf_collector,
            ebpf_tx,
            ep,
            vec!["true".to_string()],
            crate::adaptive_tick::AdaptiveConfig::default(),
            sentinel_ecs::RoomDistanceMap::default(),
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
