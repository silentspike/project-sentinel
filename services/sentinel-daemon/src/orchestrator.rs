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

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use tracing::{debug, error, info, warn};

use sentinel_common::agent_config::{load_all_agents, AgentConfig};
use sentinel_common::components::{AgentIdentity, ShiftInfo};
use sentinel_common::events::{DomainEvent, DomainEventPayload};
use sentinel_common::{AgentId, OperatorCommand, Perception};
use sentinel_ebpf::collector::MetricsSnapshot;
use sentinel_ebpf::EbpfCollector;
use sentinel_ecs::{
    apply_personality, create_simulation_world, despawn_agent_from_world, spawn_agent,
    ActionReceiver, LimboEventStore, PerceptionSender, SimulationTime,
};
use sentinel_limbo::EventStore;
use sentinel_redb::StateStore;
use sentinel_runtime::RuntimeOrchestrator;
use sentinel_sandbox::{CgroupLimits, SandboxEnforcer, SandboxHandle, SandboxWarning};
use sentinel_zenoh::SentinelBus;

use crate::adaptive_tick::AdaptiveTickRate;
use crate::config::DaemonConfig;
use crate::controlplane::config::ControlplaneConfig;
use crate::controlplane::store::ControlplaneStore;
use crate::controlplane::ControlplaneKernel;
use crate::episode_producer::EpisodeProducer;
use crate::operator_api;
use crate::runtime_control::{
    RespawnBackoffTracker, RespawnRetryDecision, RuntimeControlCommand, RuntimeReconcileRequest,
    RuntimeReconcileResponse, RuntimeStallRestartTestResponse,
};
use crate::runtime_health;
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

fn record_security_runtime_snapshot(
    security_runtime_state: &operator_api::SharedSecurityRuntimeState,
    agent_id: AgentId,
    agent_name: &str,
    bwrap_pid: Option<u32>,
    fs_mount: Option<&str>,
) {
    let aggregate_id = format!("AGENT-{:02}", agent_id.0);
    if let Ok(mut state) = security_runtime_state.write() {
        state.insert(
            agent_id.0,
            operator_api::SecurityAgentRuntimeSnapshot {
                agent_id: agent_id.0,
                aggregate_id: aggregate_id.clone(),
                agent_name: agent_name.to_string(),
                bwrap_pid,
                home_host_path: match fs_mount {
                    Some(mount) => format!("{mount}/{aggregate_id}"),
                    None => format!("/ram/agents/{agent_name}"),
                },
                fs_mount: fs_mount.map(str::to_string),
            },
        );
    }
}

fn remove_security_runtime_snapshot(
    security_runtime_state: &operator_api::SharedSecurityRuntimeState,
    agent_id: AgentId,
) {
    if let Ok(mut state) = security_runtime_state.write() {
        state.remove(&agent_id.0);
    }
}

fn proc_state(pid: u32) -> Option<char> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        let value = line.strip_prefix("State:")?.trim();
        value.chars().next()
    })
}

fn signal_pid(pid: u32, signal: &str) -> Result<()> {
    let status = std::process::Command::new("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .status()
        .with_context(|| format!("kill -{signal} fuer PID {pid} fehlgeschlagen"))?;
    if status.success() || !std::path::Path::new(&format!("/proc/{pid}")).exists() {
        Ok(())
    } else {
        Err(anyhow!("kill -{signal} fuer PID {pid} lieferte {status}"))
    }
}

fn mountpoint_is_active(path: &std::path::Path) -> bool {
    let target = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned();
    std::fs::read_to_string("/proc/self/mountinfo")
        .ok()
        .map(|mountinfo| {
            mountinfo
                .lines()
                .filter_map(|line| {
                    let (left, right) = line.split_once(" - ")?;
                    let mountpoint = left.split_whitespace().nth(4)?;
                    let mut suffix = right.split_whitespace();
                    let fs_type = suffix.next()?;
                    let mount_source = suffix.next()?;
                    Some((mountpoint, fs_type, mount_source))
                })
                .any(|(mountpoint, fs_type, mount_source)| {
                    mountpoint == target && fs_type == "fuse" && mount_source == "sentinel-fs"
                })
        })
        .unwrap_or(false)
}

fn suspend_pids(pids: &[u32], tracked_pid: Option<u32>) -> Result<()> {
    let mut unique_pids = pids.to_vec();
    unique_pids.sort_unstable();
    unique_pids.dedup();
    if unique_pids.is_empty() {
        return Err(anyhow!("keine PIDs zum Suspendieren vorhanden"));
    }

    for pid in &unique_pids {
        signal_pid(*pid, "STOP")?;
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let mut stopped = Vec::new();
        let mut still_running = Vec::new();
        let mut tracked_state = None;
        for pid in &unique_pids {
            match proc_state(*pid) {
                Some('T') => stopped.push(*pid),
                Some(state) => {
                    if Some(*pid) == tracked_pid {
                        tracked_state = Some(state);
                    }
                    if state != 'Z' {
                        still_running.push((*pid, state));
                    }
                }
                None => {
                    if Some(*pid) == tracked_pid {
                        tracked_state = None;
                    }
                }
            }
        }
        if still_running.is_empty() && !stopped.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            if !still_running.is_empty() {
                let details = still_running
                    .into_iter()
                    .map(|(pid, state)| format!("{pid}:{state}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(anyhow!("PIDs nach SIGSTOP nicht angehalten: {details}"));
            }
            if let Some(pid) = tracked_pid {
                let suffix = tracked_state
                    .map(|state| format!(" (tracked state={state})"))
                    .unwrap_or_default();
                return Err(anyhow!(
                    "kein laufender PID erreichte nach SIGSTOP Zustand T; tracked PID {pid}{suffix}"
                ));
            }
            return Err(anyhow!(
                "kein laufender PID erreichte nach SIGSTOP Zustand T"
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn suspend_agent_cgroup_processes(agent_name: &str, tracked_pid: Option<u32>) -> Result<Vec<u32>> {
    let mut pids = sentinel_sandbox::cgroups::list_pids_in_cgroup(agent_name)
        .with_context(|| format!("cgroup-Mitglieder fuer {agent_name} nicht lesbar"))?;
    if let Some(pid) = tracked_pid {
        pids.push(pid);
    }
    pids.sort_unstable();
    pids.dedup();
    suspend_pids(&pids, tracked_pid)?;
    Ok(pids)
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
    security_runtime_state: &operator_api::SharedSecurityRuntimeState,
    fs_mount: Option<&str>,
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
    if let Err(e) = runtime_orch.spawn_agent(identity, shift, &agent_cfg.preferences.favorite_room)
    {
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
                    let aggregate_id = format!("AGENT-{:02}", agent_id.0);
                    match sandbox.start_agent_process(
                        &agent_cfg.identity.name,
                        Some(&aggregate_id),
                        agent_command,
                    ) {
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
                            record_security_runtime_snapshot(
                                security_runtime_state,
                                agent_id,
                                &agent_cfg.identity.name,
                                Some(pid),
                                fs_mount,
                            );

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
                            record_security_runtime_snapshot(
                                security_runtime_state,
                                agent_id,
                                &agent_cfg.identity.name,
                                None,
                                fs_mount,
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
            record_security_runtime_snapshot(
                security_runtime_state,
                agent_id,
                &agent_cfg.identity.name,
                None,
                fs_mount,
            );
        }
    }

    let entity = spawn_agent(
        world,
        agent_id,
        &agent_cfg.identity.name,
        &agent_cfg.identity.role,
        agent_cfg.identity.shift_set,
        &agent_cfg.preferences.favorite_room,
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
    // -- Runtime Feature Flags initialisieren (Issue #233) --
    let flags = sentinel_common::feature_flags::RuntimeFlags::init();
    info!(?flags, "Runtime Feature Flags geladen");

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

    let fs_layer = if config.fs_mount.is_some() {
        let cas = sentinel_fs::cas::CasStore::open(data_dir).context("sentinel-fs CAS oeffnen")?;
        let meta = sentinel_fs::metadata::MetadataStore::open(data_dir.join("metadata.redb"))
            .context("sentinel-fs Metadata oeffnen")?;
        let layer = Arc::new(sentinel_fs::layer::LayerManager::new(cas, meta));
        layer
            .init_base_root()
            .context("sentinel-fs Base-Root initialisieren")?;
        Some(layer)
    } else {
        None
    };

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
        let fs_layer_clone = fs_layer
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("sentinel-fs Layer nicht initialisiert"))?;
        if !mountpoint.exists() {
            std::fs::create_dir_all(&mountpoint)
                .with_context(|| format!("FUSE mountpoint erstellen: {}", mountpoint.display()))?;
        }
        info!(
            mountpoint = %mountpoint.display(),
            data_dir = %data_dir.display(),
            "sentinel-fs FUSE-Mount starten"
        );
        let mountpoint_check = mountpoint.clone();
        std::thread::spawn(move || {
            if let Err(e) = sentinel_fs::fuse::start_fuse_layer(fs_layer_clone, &mountpoint) {
                error!(error = %e, "sentinel-fs FUSE-Mount fehlgeschlagen");
            }
        });
        let mut mount_ready = false;
        for _ in 0..20 {
            if mountpoint_is_active(&mountpoint_check) {
                mount_ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if mount_ready {
            info!(mountpoint = %mountpoint_check.display(), "sentinel-fs FUSE-Mount aktiv");
        } else {
            warn!(mountpoint = %mountpoint_check.display(), "sentinel-fs FUSE-Mount moeglicherweise nicht bereit");
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
    let (ebpf_collector, ebpf_mode) =
        crate::ebpf::init_ebpf(config.platform_controlplane.stall_detection_threshold_secs);
    info!(
        mode = %ebpf_mode,
        stall_threshold_secs = config.platform_controlplane.stall_detection_threshold_secs,
        "eBPF Monitoring initialisiert"
    );

    // -- eBPF Bridge: mpsc + shared Prometheus Text --
    let (ebpf_tx, ebpf_rx) = tokio::sync::mpsc::channel::<MetricsSnapshot>(4);
    let prometheus_text = Arc::new(RwLock::new(String::new()));

    // -- Channels fuer ECS <-> Async Bridge --
    let (action_tx, action_rx) = mpsc::channel();
    let (operator_tx, operator_rx) = mpsc::channel::<OperatorCommand>();
    let (platform_tx, platform_rx) =
        mpsc::channel::<crate::platform_controlplane::PlatformControlCommand>();
    let (runtime_tx, runtime_rx) = mpsc::channel::<RuntimeControlCommand>();
    let (nightrun_tx, nightrun_rx) = mpsc::channel::<sentinel_common::OperatorNightrunCommand>();
    let (snapshot_tx, snapshot_rx) = mpsc::channel::<sentinel_common::OperatorSnapshotCommand>();
    let (restore_tx, restore_rx) = mpsc::channel::<sentinel_common::OperatorRestoreCommand>();
    let (prune_tx, prune_rx) = mpsc::channel::<i64>();
    // Bounded Channel: 128 Slots. Bridge drainet per try_recv() vor jedem LLM-Call.
    // Output_system nutzt try_send() (non-blocking, WARN bei Drop).
    let (perception_tx, perception_rx) = mpsc::sync_channel::<Perception>(128);
    let platform_state = Arc::new(RwLock::new(
        crate::platform_controlplane::PlatformStateSnapshot::default(),
    ));
    let runtime_health = Arc::new(RwLock::new(
        crate::runtime_health::RuntimeHealthSnapshot::default(),
    ));
    let security_runtime_state: operator_api::SharedSecurityRuntimeState =
        Arc::new(RwLock::new(HashMap::new()));
    let projection_db_path = data_dir.join("projection.db").to_string_lossy().to_string();
    let operator_auth_required = config.operator_api.shared_secret.is_some();

    // -- Zenoh SentinelBus (Core-Bus fuer Real-Time Event-Verteilung) --
    let bus_config = config.zenoh.to_bus_config();
    let bus = match SentinelBus::with_config(bus_config).await {
        Ok(b) => {
            info!(transport = ?b.transport_mode(), "SentinelBus ready");
            Some(b)
        }
        Err(e) => {
            warn!(error = %e, "SentinelBus nicht verfuegbar — Zenoh Fan-Out deaktiviert");
            None
        }
    };

    // -- Zenoh Fan-Out Bridge (Events nach Limbo-Write auf Zenoh publizieren) --
    let fanout_capacity = config.zenoh.fanout_channel_capacity;
    let fanout_sender = if let Some(ref b) = bus {
        let (fanout_tx, fanout_rx) = tokio::sync::mpsc::channel(fanout_capacity);
        tokio::spawn(crate::fanout::zenoh_fanout_task(b.clone(), fanout_rx));
        info!(capacity = fanout_capacity, "Zenoh Fan-Out Bridge gestartet");
        Some(sentinel_ecs::ZenohFanoutSender { sender: fanout_tx })
    } else {
        None
    };

    // -- Zenoh Scoped Query Responder (beantwortet Queries mit redb State) --
    if config.zenoh.query_responder_enabled {
        if let Some(ref b) = bus {
            let qr_bus = b.clone();
            let qr_store = Arc::clone(&state_store);
            tokio::spawn(crate::query_responder::query_responder_task(
                qr_bus, qr_store,
            ));
            info!("Zenoh Query Responder gestartet");
        }
    }

    // -- Shutdown Flag --
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_ecs = Arc::clone(&shutdown);

    // -- Room Distance Map + Room Info Map (BFS-vorberechnet fuer Transit-Dauer + Capacity + Floor) --
    let rooms_toml_path = config.config_dir.join("rooms.toml");
    let (room_distances, room_info) = if rooms_toml_path.exists() {
        match sentinel_common::room::BuildingConfig::load(&rooms_toml_path) {
            Ok(building_cfg) => {
                let map = sentinel_ecs::RoomDistanceMap::from_building_config(&building_cfg);
                let info = sentinel_ecs::RoomInfoMap::from_building_config(&building_cfg);
                info!(
                    rooms = building_cfg.rooms.len(),
                    "RoomDistanceMap + RoomInfoMap aus rooms.toml geladen"
                );
                (map, info)
            }
            Err(e) => {
                warn!(error = %e, "rooms.toml konnte nicht geladen werden — Fallback auf Defaults");
                (
                    sentinel_ecs::RoomDistanceMap::default(),
                    sentinel_ecs::RoomInfoMap::default(),
                )
            }
        }
    } else {
        warn!("rooms.toml nicht gefunden — Transit-Dauer nutzt Default-Distanzen");
        (
            sentinel_ecs::RoomDistanceMap::default(),
            sentinel_ecs::RoomInfoMap::default(),
        )
    };
    let operator_room_ids = room_distances.all_rooms().to_vec();

    // -- Platform LLM Analyzer starten (daemon-interner Background-Worker) --
    #[cfg(feature = "llm")]
    let platform_llm_analyzer = {
        let gateway_url =
            std::env::var("CORTEX_GATEWAY_URL").unwrap_or_else(|_| "http://localhost:8080".into());
        let analyzer_config =
            crate::platform_controlplane::llm_analyzer::LlmAnalyzerConfig::from_platform_config(
                &config.platform_controlplane,
                gateway_url,
            );
        let handle = crate::platform_controlplane::llm_analyzer::PlatformLlmAnalyzerHandle::spawn(
            analyzer_config,
            Arc::clone(&event_store),
            platform_tx.clone(),
        );
        info!(
            enabled = handle.is_enabled(),
            "Platform LLM Analyzer initialisiert"
        );
        handle
    };

    let operator_api_handle = if config.operator_api.enabled {
        Some(
            operator_api::start_server(
                config.operator_api.clone(),
                data_dir.to_path_buf(),
                config.fs_mount.clone(),
                fs_layer.clone(),
                operator_room_ids,
                operator_tx.clone(),
                platform_tx.clone(),
                runtime_tx.clone(),
                nightrun_tx.clone(),
                snapshot_tx.clone(),
                restore_tx.clone(),
                Arc::clone(&event_store),
                prune_tx.clone(),
                Arc::clone(&state_store),
                Arc::clone(&platform_state),
                Arc::clone(&runtime_health),
                Arc::clone(&security_runtime_state),
            )
            .await?,
        )
    } else {
        info!("Operator-API deaktiviert");
        None
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
    let evolution_db_path_clone = data_dir
        .join("evolution.db")
        .to_str()
        .unwrap_or("data/evolution.db")
        .to_string();
    let ecs_state_store = Arc::clone(&state_store);
    let ecs_event_store = Arc::clone(&event_store);
    let retention_config = config.retention.clone();
    let resource_manager_config = config.resource_manager.clone();
    let platform_cp_config = config.platform_controlplane.clone();
    let events_db_path = events_path.to_string_lossy().to_string();
    let ecs_platform_state = Arc::clone(&platform_state);
    let ecs_runtime_health = Arc::clone(&runtime_health);
    let ecs_security_runtime_state = Arc::clone(&security_runtime_state);
    let ecs_fs_mount = config.fs_mount.clone();
    let ecs_projection_db_path = projection_db_path.clone();
    let ecs_handle = std::thread::Builder::new()
        .name("ecs-tick-loop".into())
        .spawn(move || {
            ecs_tick_loop(
                ecs_state_store,
                ecs_event_store,
                action_rx,
                operator_rx,
                platform_rx,
                runtime_rx,
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
                nightrun_rx,
                snapshot_rx,
                restore_rx,
                prune_rx,
                retention_config,
                evolution_db_path_clone,
                agent_command_cfg,
                adaptive_config,
                room_distances,
                room_info,
                fanout_sender,
                resource_manager_config,
                platform_cp_config,
                events_db_path,
                ecs_platform_state,
                ecs_runtime_health,
                ecs_security_runtime_state,
                ecs_projection_db_path,
                operator_auth_required,
                ecs_fs_mount,
                fs_layer.clone(),
                #[cfg(feature = "llm")]
                platform_llm_analyzer.clone(),
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
        bus.clone(),
    ));

    // -- LLM Bridge starten (Perception → Cortex Gateway → Action) --
    #[cfg(feature = "llm")]
    let _llm_bridge_handle = {
        let gateway_request_timeout_ms = std::env::var("SENTINEL_LLM_BRIDGE_REQUEST_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(config.traffic_control.gateway_request_timeout_ms);
        let bridge_config = crate::llm_bridge::bridge::LlmBridgeConfig {
            gateway_url: std::env::var("CORTEX_GATEWAY_URL")
                .unwrap_or_else(|_| "http://localhost:8080".to_string()),
            max_concurrent: config.traffic_control.max_forward_concurrency.max(1),
            request_timeout: std::time::Duration::from_millis(gateway_request_timeout_ms),
            ..Default::default()
        };
        let bridge_telemetry =
            std::sync::Arc::new(crate::llm_bridge::bridge::BridgeTelemetry::default());
        let bridge_action_tx = action_tx.clone();
        let bridge_telem = std::sync::Arc::clone(&bridge_telemetry);
        info!(
            gateway_url = %bridge_config.gateway_url,
            max_concurrent = bridge_config.max_concurrent,
            request_timeout_ms = gateway_request_timeout_ms,
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
    if let Some(handle) = operator_api_handle {
        handle.abort();
    }

    // Action-Channel schliessen damit ECS-Thread aufwacht falls er blockt
    drop(action_tx);
    drop(operator_tx);

    // ECS-Thread mit 8s Timeout joinen (AC-3 #255)
    let join_deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if ecs_handle.is_finished() {
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
            break;
        }
        if Instant::now() > join_deadline {
            warn!("ECS-Thread Shutdown-Timeout (8s) — erzwinge Beendigung");
            break; // Daemon exits, --die-with-parent kills agents
        }
        std::thread::sleep(Duration::from_millis(100));
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

fn collect_platform_metrics_snapshot(
    runtime_orch: &RuntimeOrchestrator,
    pcp_metrics_collector: &mut crate::platform_controlplane::metrics::PlatformMetricsCollector,
    last_ebpf_snapshot: &Option<sentinel_ebpf::collector::MetricsSnapshot>,
    event_store: &EventStore,
    events_db_path_str: &str,
    tick_count: u64,
    service_health_checker: &crate::service_health::ServiceHealthChecker,
) -> (
    crate::platform_controlplane::metrics::PlatformMetrics,
    std::collections::HashMap<String, sentinel_common::AgentId>,
) {
    let agent_names: Vec<String> = runtime_orch
        .agents()
        .values()
        .map(|h| h.identity.name.clone())
        .collect();
    let agent_name_to_id: std::collections::HashMap<String, sentinel_common::AgentId> =
        runtime_orch
            .agents()
            .iter()
            .map(|(id, h)| (h.identity.name.clone(), *id))
            .collect();
    let failed_services = service_health_checker.poll_failed();
    let mut pcp_metrics = crate::platform_controlplane::metrics::collect(
        pcp_metrics_collector,
        last_ebpf_snapshot,
        event_store,
        events_db_path_str,
        &agent_names,
        tick_count,
        failed_services,
    );
    for handle in runtime_orch.agents().values() {
        pcp_metrics
            .last_action_ticks
            .insert(handle.identity.name.clone(), handle.last_activity_tick.0);
    }
    (pcp_metrics, agent_name_to_id)
}

fn publish_platform_state_snapshot(
    platform_state: &Arc<RwLock<crate::platform_controlplane::PlatformStateSnapshot>>,
    tick_count: u64,
    platform_cp: &crate::platform_controlplane::PlatformControlplane,
    runtime_orch: &RuntimeOrchestrator,
    resource_manager: &crate::resource_manager::ResourceManager,
) {
    let agents = runtime_orch
        .agents()
        .iter()
        .map(|(agent_id, handle)| {
            let cgroup_path = sentinel_sandbox::cgroup_path(&handle.identity.name);
            let cgroup_path = if std::path::Path::new(&cgroup_path).exists() {
                cgroup_path
            } else {
                String::new()
            };
            crate::platform_controlplane::PlatformAgentSnapshot {
                agent_id: agent_id.0,
                aggregate_id: agent_id.to_string(),
                name: handle.identity.name.clone(),
                last_activity_tick: handle.last_activity_tick.0,
                cgroup_path,
                current_profile: resource_manager.get_profile(agent_id).to_string(),
            }
        })
        .collect::<Vec<_>>();
    let resource_profiles = agents
        .iter()
        .map(|agent| (agent.aggregate_id.clone(), agent.current_profile.clone()))
        .collect();

    if let Ok(mut snapshot) = platform_state.write() {
        *snapshot = crate::platform_controlplane::PlatformStateSnapshot {
            current_tick: tick_count,
            cycle_interval_ticks: platform_cp.config().cycle_interval_ticks,
            ebpf_collect_interval_ticks: platform_cp.config().ebpf_collect_interval_ticks,
            stall_detection_threshold_secs: platform_cp.config().stall_detection_threshold_secs,
            stall_recent_activity_grace_ticks: platform_cp
                .config()
                .stall_recent_activity_grace_ticks,
            llm_enabled: platform_cp.config().llm_enabled,
            llm_analysis_interval_secs: platform_cp.config().llm_analysis_interval_secs,
            llm_retry_delay_secs: platform_cp.config().llm_retry_delay_secs,
            last_analysis_tick: platform_cp.last_analysis_tick(),
            last_analysis_trigger: platform_cp.last_analysis_trigger().map(str::to_string),
            last_scheduled_analysis_tick: platform_cp.last_scheduled_analysis_tick(),
            unresolved_counts: platform_cp
                .unresolved_counts()
                .iter()
                .map(|(key, value)| (key.clone(), *value))
                .collect(),
            threshold_overrides: platform_cp.threshold_overrides().clone(),
            resource_profiles,
            agents,
        };
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FastRestartResult {
    agent_name: String,
    pid_before: Option<u32>,
    pid_after: Option<u32>,
    runtime_present_after: bool,
    security_runtime_present_after: bool,
}

fn tracked_pid_for_agent(
    agent_id: AgentId,
    sandbox_handles: &HashMap<AgentId, SandboxHandle>,
    agent_processes: &HashMap<AgentId, sentinel_sandbox::AgentProcess>,
    security_runtime_state: &operator_api::SharedSecurityRuntimeState,
) -> Option<u32> {
    sandbox_handles
        .get(&agent_id)
        .and_then(|handle| handle.bwrap_pid)
        .or_else(|| agent_processes.get(&agent_id).map(|proc| proc.pid))
        .or_else(|| {
            security_runtime_state.read().ok().and_then(|state| {
                state
                    .get(&agent_id.0)
                    .and_then(|snapshot| snapshot.bwrap_pid)
            })
        })
}

#[allow(clippy::too_many_arguments)]
fn restart_agent_fast_path(
    world: &mut bevy_ecs::prelude::World,
    runtime_orch: &mut RuntimeOrchestrator,
    agent_cfg: &AgentConfig,
    sandbox: &SandboxEnforcer,
    sandbox_handles: &mut HashMap<AgentId, SandboxHandle>,
    ebpf_collector: &mut EbpfCollector,
    agent_processes: &mut HashMap<AgentId, sentinel_sandbox::AgentProcess>,
    agent_command: &[String],
    security_runtime_state: &operator_api::SharedSecurityRuntimeState,
    fs_mount: Option<&str>,
) -> Result<FastRestartResult> {
    let agent_id = AgentId(agent_cfg.identity.id);
    let pid_before = tracked_pid_for_agent(
        agent_id,
        sandbox_handles,
        agent_processes,
        security_runtime_state,
    );

    if let Some(handle) = sandbox_handles.remove(&agent_id) {
        if handle.cgroup_created {
            if let Some(cid) = sentinel_sandbox::cgroup_id(&handle.agent_name) {
                ebpf_collector.unregister_agent(cid);
            }
        }
        if let Err(error) = sandbox.teardown_agent(&handle) {
            warn!(
                agent_id = %agent_id,
                error = %error,
                "Sandbox teardown bei Fast-Restart fehlgeschlagen"
            );
        }
    }

    if let Some(proc_handle) = agent_processes.remove(&agent_id) {
        let _ = signal_pid(proc_handle.pid, "TERM");
        drop(proc_handle);
    }

    remove_security_runtime_snapshot(security_runtime_state, agent_id);
    let _ = despawn_agent_from_world(world, agent_id);
    let _ = runtime_orch.despawn_agent(agent_id);

    if !spawn_agent_full(
        runtime_orch,
        world,
        agent_cfg,
        sandbox,
        sandbox_handles,
        ebpf_collector,
        agent_processes,
        agent_command,
        security_runtime_state,
        fs_mount,
    ) {
        return Err(anyhow!(
            "Fast-Respawn fuer {} fehlgeschlagen",
            agent_cfg.identity.name
        ));
    }

    let pid_after = tracked_pid_for_agent(
        agent_id,
        sandbox_handles,
        agent_processes,
        security_runtime_state,
    );
    let security_runtime_present_after = security_runtime_state
        .read()
        .map(|state| state.contains_key(&agent_id.0))
        .unwrap_or(false);

    Ok(FastRestartResult {
        agent_name: agent_cfg.identity.name.clone(),
        pid_before,
        pid_after,
        runtime_present_after: runtime_orch.agents().contains_key(&agent_id),
        security_runtime_present_after,
    })
}

#[derive(Debug, Default)]
struct RuntimeCleanupStats {
    repairs: usize,
    security_snapshots_removed: usize,
    orphan_cgroups_removed: usize,
}

struct RuntimeReconcileContext<'a> {
    tick_count: u64,
    current_shift: u8,
    all_agents: &'a [AgentConfig],
    world: &'a mut bevy_ecs::prelude::World,
    runtime_orch: &'a mut RuntimeOrchestrator,
    sandbox: &'a SandboxEnforcer,
    sandbox_handles: &'a mut HashMap<AgentId, SandboxHandle>,
    ebpf_collector: &'a mut EbpfCollector,
    agent_processes: &'a mut HashMap<AgentId, sentinel_sandbox::AgentProcess>,
    agent_command: &'a [String],
    security_runtime_state: &'a operator_api::SharedSecurityRuntimeState,
    event_store: &'a Arc<EventStore>,
    runtime_health: &'a crate::runtime_health::SharedRuntimeHealthState,
    projection_db_path: &'a std::path::Path,
    operator_auth_required: bool,
    service_health_state: crate::service_health::ServiceHealthWorkerSnapshot,
    fs_mount: Option<&'a str>,
    data_dir: &'a std::path::Path,
}

fn runtime_agent_is_healthy(agent: &runtime_health::RuntimeHealthAgentSnapshot) -> bool {
    agent.runtime_present
        && agent.projection_present
        && agent.security_runtime_present
        && agent.tracked_pid_alive
        && agent.cgroup_live_pid_count > 0
}

fn emit_runtime_repair_blocked_event(
    event_store: &EventStore,
    agent_id: AgentId,
    description: &str,
    tick_count: u64,
) {
    let aggregate_id = format!("AGENT-{:02}", agent_id.0);
    let payload = DomainEventPayload::PlatformIntervention {
        rule_name: "runtime_reconcile".to_string(),
        target: aggregate_id.clone(),
        action: "repair_blocked".to_string(),
        description: description.to_string(),
    };
    let event = DomainEvent::new(
        payload.event_type_str(),
        &aggregate_id,
        &payload.to_json(),
        &format!("runtime-reconcile-{}", agent_id.0),
        tick_count,
    );
    if let Err(error) = event_store.append_event(&event) {
        warn!(
            agent_id = %agent_id,
            error = %error,
            "Repair-Blocked-Event konnte nicht persistiert werden"
        );
    }
}

fn remove_agent_runtime_fragments(
    ctx: &mut RuntimeReconcileContext<'_>,
    agent: &runtime_health::RuntimeHealthAgentSnapshot,
) -> RuntimeCleanupStats {
    let agent_id = AgentId(agent.agent_id);
    let mut stats = RuntimeCleanupStats::default();

    if let Some(handle) = ctx.sandbox_handles.remove(&agent_id) {
        if handle.cgroup_created {
            if let Some(cid) = sentinel_sandbox::cgroup_id(&handle.agent_name) {
                ctx.ebpf_collector.unregister_agent(cid);
            }
        }
        if let Err(error) = ctx.sandbox.teardown_agent(&handle) {
            warn!(agent_id = %agent_id, error = %error, "Sandbox-Teardown bei Runtime-Reconcile fehlgeschlagen");
        } else {
            stats.repairs += 1;
        }
    }

    if let Some(proc_handle) = ctx.agent_processes.remove(&agent_id) {
        let _ = signal_pid(proc_handle.pid, "TERM");
        drop(proc_handle);
        stats.repairs += 1;
    }

    if agent.cgroup_live_pid_count > 0 {
        match sentinel_sandbox::cgroups::list_pids_in_cgroup(&agent.name) {
            Ok(pids) => {
                for pid in &pids {
                    let _ = signal_pid(*pid, "TERM");
                }
                if !pids.is_empty() {
                    stats.repairs += 1;
                }
            }
            Err(error) => warn!(
                agent = %agent.name,
                error = %error,
                "Live-Cgroup-PIDs konnten nicht beendet werden"
            ),
        }
    }

    let security_removed = ctx
        .security_runtime_state
        .write()
        .map(|mut state| state.remove(&agent.agent_id).is_some())
        .unwrap_or(false);
    if security_removed {
        stats.security_snapshots_removed += 1;
        stats.repairs += 1;
    }

    if ctx.runtime_orch.agents().contains_key(&agent_id) {
        if let Err(error) = ctx.runtime_orch.despawn_agent(agent_id) {
            warn!(agent_id = %agent_id, error = %error, "Runtime-Despawn bei Reconcile fehlgeschlagen");
        } else {
            stats.repairs += 1;
        }
    }

    if despawn_agent_from_world(ctx.world, agent_id) {
        stats.repairs += 1;
    }

    let cgroup_path = sentinel_sandbox::cgroups::cgroup_path(&agent.name);
    if std::path::Path::new(&cgroup_path).exists() && agent.cgroup_live_pid_count == 0 {
        match sentinel_sandbox::cgroups::remove_cgroup(&agent.name) {
            Ok(()) => {
                stats.orphan_cgroups_removed += 1;
                stats.repairs += 1;
            }
            Err(error) => warn!(
                agent = %agent.name,
                error = %error,
                "Orphan-Cgroup konnte nicht entfernt werden"
            ),
        }
    }

    stats
}

fn run_runtime_reconcile(
    ctx: &mut RuntimeReconcileContext<'_>,
    request: RuntimeReconcileRequest,
    respawn_backoff: &mut RespawnBackoffTracker,
) -> RuntimeReconcileResponse {
    let previous = ctx
        .runtime_health
        .read()
        .ok()
        .map(|snapshot| snapshot.clone());
    let before = runtime_health::build_runtime_health_snapshot(
        ctx.all_agents,
        ctx.current_shift,
        ctx.runtime_orch,
        ctx.sandbox_handles,
        ctx.agent_processes,
        ctx.security_runtime_state,
        ctx.projection_db_path,
        ctx.operator_auth_required,
        ctx.service_health_state.clone(),
        previous.as_ref(),
    );
    let expected_agents = agents_for_shift(ctx.all_agents, ctx.current_shift);
    let expected_ids = expected_agents
        .iter()
        .map(|cfg| cfg.identity.id)
        .collect::<HashSet<_>>();
    let before_by_id = before
        .agents
        .iter()
        .cloned()
        .map(|agent| (agent.agent_id, agent))
        .collect::<HashMap<_, _>>();

    let mut security_snapshots_removed = 0usize;
    let mut unexpected_runtime_removed = 0usize;
    let mut orphan_cgroups_removed = 0usize;
    let mut respawned_agents = 0usize;
    let mut respawn_skipped_backoff = 0usize;
    let mut respawn_blocked_agents = 0usize;
    let mut repair_ops_total = 0usize;
    let mut respawn_failures_added = 0u64;
    let mut repaired_agents = Vec::new();
    let mut blocked_agents = Vec::new();
    let mut errors = Vec::new();
    let mut agent_status_updates = HashMap::<u16, String>::new();

    if !request.dry_run {
        for agent in before.agents.iter().filter(|agent| {
            let expected_active = expected_ids.contains(&agent.agent_id);
            !expected_active
                && (agent.runtime_present
                    || agent.security_runtime_present
                    || agent.tracked_pid_alive
                    || agent.cgroup_live_pid_count > 0)
        }) {
            let stats = remove_agent_runtime_fragments(ctx, agent);
            if stats.repairs > 0 {
                unexpected_runtime_removed += 1;
                security_snapshots_removed += stats.security_snapshots_removed;
                orphan_cgroups_removed += stats.orphan_cgroups_removed;
                repair_ops_total += stats.repairs;
                repaired_agents.push(agent.name.clone());
                agent_status_updates
                    .insert(agent.agent_id, "unexpected_runtime_cleaned".to_string());
            }
        }

        let runtime_agent_names = ctx
            .runtime_orch
            .agents()
            .values()
            .map(|handle| handle.identity.name.clone())
            .collect::<HashSet<_>>();
        if let Ok(entries) = std::fs::read_dir("/sys/fs/cgroup/sentinel") {
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if !file_type.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if runtime_agent_names.contains(&name) {
                    continue;
                }
                match sentinel_sandbox::cgroups::list_pids_in_cgroup(&name) {
                    Ok(pids) if pids.is_empty() => {
                        if sentinel_sandbox::cgroups::remove_cgroup(&name).is_ok() {
                            orphan_cgroups_removed += 1;
                            repair_ops_total += 1;
                        }
                    }
                    Ok(_) => {}
                    Err(error) => warn!(
                        cgroup = %name,
                        error = %error,
                        "Orphan-Cgroup konnte nicht inspiziert werden"
                    ),
                }
            }
        }
    }

    for agent_cfg in &expected_agents {
        let agent_id = agent_cfg.identity.id;
        let snapshot = before_by_id.get(&agent_id);

        if let Some(snapshot) = snapshot {
            let runtime_core_healthy = snapshot.runtime_present
                && snapshot.tracked_pid_alive
                && snapshot.cgroup_live_pid_count > 0;
            if runtime_agent_is_healthy(snapshot) {
                respawn_backoff.record_success(agent_id);
                continue;
            }
            if runtime_core_healthy && !snapshot.security_runtime_present {
                if request.dry_run {
                    agent_status_updates
                        .insert(agent_id, "security_runtime_restore_planned".to_string());
                } else {
                    let tracked_pid = ctx
                        .sandbox_handles
                        .get(&AgentId(agent_id))
                        .and_then(|handle| handle.bwrap_pid)
                        .or(snapshot.tracked_pid);
                    record_security_runtime_snapshot(
                        ctx.security_runtime_state,
                        AgentId(agent_id),
                        &agent_cfg.identity.name,
                        tracked_pid,
                        ctx.fs_mount,
                    );
                    repair_ops_total += 1;
                    repaired_agents.push(agent_cfg.identity.name.clone());
                    agent_status_updates.insert(agent_id, "security_runtime_restored".to_string());
                }
                respawn_backoff.record_success(agent_id);
                continue;
            }
            if runtime_core_healthy
                && snapshot.security_runtime_present
                && !snapshot.projection_present
            {
                agent_status_updates.insert(agent_id, "projection_reconcile_pending".to_string());
                respawn_backoff.record_success(agent_id);
                continue;
            }
        }

        if !request.respawn_missing {
            continue;
        }

        match respawn_backoff.decision(agent_id, ctx.tick_count) {
            RespawnRetryDecision::Blocked => {
                respawn_blocked_agents += 1;
                blocked_agents.push(agent_cfg.identity.name.clone());
                agent_status_updates.insert(agent_id, "repair_blocked".to_string());
                continue;
            }
            RespawnRetryDecision::BackoffActive { .. } => {
                respawn_skipped_backoff += 1;
                agent_status_updates.insert(agent_id, "respawn_backoff".to_string());
                continue;
            }
            RespawnRetryDecision::Ready => {}
        }

        if request.dry_run {
            agent_status_updates.insert(agent_id, "respawn_planned".to_string());
            continue;
        }

        if let Some(snapshot) = snapshot {
            let stats = remove_agent_runtime_fragments(ctx, snapshot);
            security_snapshots_removed += stats.security_snapshots_removed;
            orphan_cgroups_removed += stats.orphan_cgroups_removed;
            repair_ops_total += stats.repairs;
        }

        if spawn_agent_full(
            ctx.runtime_orch,
            ctx.world,
            agent_cfg,
            ctx.sandbox,
            ctx.sandbox_handles,
            ctx.ebpf_collector,
            ctx.agent_processes,
            ctx.agent_command,
            ctx.security_runtime_state,
            ctx.fs_mount,
        ) {
            respawned_agents += 1;
            repair_ops_total += 1;
            repaired_agents.push(agent_cfg.identity.name.clone());
            agent_status_updates.insert(agent_id, "respawned".to_string());
            respawn_backoff.record_success(agent_id);
        } else {
            respawn_failures_added += 1;
            let message = format!("Respawn fehlgeschlagen fuer {}", agent_cfg.identity.name);
            errors.push(message.clone());
            match respawn_backoff.record_failure(agent_id, ctx.tick_count) {
                RespawnRetryDecision::Blocked => {
                    respawn_blocked_agents += 1;
                    blocked_agents.push(agent_cfg.identity.name.clone());
                    agent_status_updates.insert(agent_id, "repair_blocked".to_string());
                    emit_runtime_repair_blocked_event(
                        ctx.event_store,
                        AgentId(agent_id),
                        &message,
                        ctx.tick_count,
                    );
                }
                RespawnRetryDecision::BackoffActive { .. } => {
                    agent_status_updates.insert(agent_id, "respawn_backoff".to_string());
                }
                RespawnRetryDecision::Ready => {}
            }
        }
    }

    let projection_rebuild_requested = if request.projection_rebuild && !request.dry_run {
        match crate::runtime_control::write_projection_rebuild_request(ctx.data_dir, ctx.tick_count)
        {
            Ok(()) => {
                repair_ops_total += 1;
                true
            }
            Err(error) => {
                errors.push(error.to_string());
                false
            }
        }
    } else {
        false
    };

    let mut after = runtime_health::build_runtime_health_snapshot(
        ctx.all_agents,
        ctx.current_shift,
        ctx.runtime_orch,
        ctx.sandbox_handles,
        ctx.agent_processes,
        ctx.security_runtime_state,
        ctx.projection_db_path,
        ctx.operator_auth_required,
        ctx.service_health_state.clone(),
        Some(&before),
    );
    after.reconcile_runs_total = before.reconcile_runs_total.saturating_add(1);
    after.reconcile_repairs_total = before
        .reconcile_repairs_total
        .saturating_add(repair_ops_total as u64);
    after.respawn_failures = before
        .respawn_failures
        .saturating_add(respawn_failures_added);
    after.last_repair_error = errors.last().cloned().or(before.last_repair_error.clone());
    after.repair_last_status = Some(if request.dry_run {
        "dry_run".to_string()
    } else if respawn_blocked_agents > 0 {
        "repair_blocked".to_string()
    } else if !errors.is_empty() {
        "repair_error".to_string()
    } else if repair_ops_total > 0 || projection_rebuild_requested {
        "repaired".to_string()
    } else if after.stale_runtime_entries == 0 && after.orphan_cgroups == 0 {
        "healthy".to_string()
    } else {
        "drift_detected".to_string()
    });
    for agent in &mut after.agents {
        if let Some(status) = agent_status_updates.get(&agent.agent_id) {
            agent.last_repair_status = Some(status.clone());
        }
    }
    if let Ok(mut runtime_health) = ctx.runtime_health.write() {
        *runtime_health = after.clone();
    }

    RuntimeReconcileResponse {
        accepted: true,
        dry_run: request.dry_run,
        current_shift: ctx.current_shift,
        stale_agents_before: before.stale_runtime_entries,
        stale_agents_after: after.stale_runtime_entries,
        orphan_cgroups_before: before.orphan_cgroups,
        orphan_cgroups_after: after.orphan_cgroups,
        security_snapshots_removed,
        unexpected_runtime_removed,
        orphan_cgroups_removed,
        respawned_agents,
        respawn_skipped_backoff,
        respawn_blocked_agents,
        projection_rebuild_requested,
        respawn_failures_total: after.respawn_failures,
        repair_last_status: after
            .repair_last_status
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        repaired_agents,
        blocked_agents,
        errors,
    }
}

fn resolve_platform_analysis_target(
    runtime_orch: &RuntimeOrchestrator,
    target: &str,
) -> Option<(AgentId, String)> {
    let target = target.trim();
    runtime_orch.agents().iter().find_map(|(agent_id, handle)| {
        if handle.identity.name.eq_ignore_ascii_case(target)
            || agent_id.to_string().eq_ignore_ascii_case(target)
        {
            Some((*agent_id, handle.identity.name.clone()))
        } else {
            None
        }
    })
}

fn parse_analysis_profile(
    parameters: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Option<sentinel_sandbox::ResourceProfile> {
    let profile = parameters
        .get("profile")?
        .as_str()?
        .trim()
        .to_ascii_lowercase();
    match profile.as_str() {
        "idle" => Some(sentinel_sandbox::ResourceProfile::Idle),
        "normal" => Some(sentinel_sandbox::ResourceProfile::Normal),
        "heavy" => Some(sentinel_sandbox::ResourceProfile::Heavy),
        "suspended" => Some(sentinel_sandbox::ResourceProfile::Suspended),
        _ => None,
    }
}

fn apply_platform_analysis_command(
    analysis: crate::platform_controlplane::PlatformAnalysisCommand,
    tick: u64,
    runtime_orch: &RuntimeOrchestrator,
    platform_cp: &mut crate::platform_controlplane::PlatformControlplane,
    resource_manager: &mut crate::resource_manager::ResourceManager,
    event_store: &EventStore,
) -> Result<()> {
    crate::platform_controlplane::persist_platform_analysis_event(event_store, tick, &analysis)?;

    let target = analysis.normalized_target();
    let Some(action) = analysis
        .suggested_action
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        debug!(
            trigger = %analysis.trigger,
            severity = %analysis.severity,
            target = %target,
            "Platform-Analyse ohne Suggested Action persistiert"
        );
        return Ok(());
    };

    match action {
        "force_profile" => {
            let (agent_id, agent_name) = resolve_platform_analysis_target(runtime_orch, &target)
                .with_context(|| format!("PlatformAnalysis target nicht aufloesbar: {target}"))?;
            let profile = parse_analysis_profile(&analysis.parameters)
                .context("force_profile braucht gueltiges parameters.profile")?;
            resource_manager.force_profile_and_apply(
                agent_id,
                &agent_name,
                profile,
                event_store,
                tick,
            )?;
            info!(
                trigger = %analysis.trigger,
                target = %target,
                profile = %profile,
                "Platform-Analyse force_profile ausgefuehrt"
            );
        }
        "adjust_threshold" => {
            let key = analysis
                .parameters
                .get("key")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .context("adjust_threshold braucht parameters.key")?;
            let value = analysis
                .parameters
                .get("value")
                .cloned()
                .context("adjust_threshold braucht parameters.value")?;
            platform_cp.apply_threshold_override(key, value)?;
            info!(
                trigger = %analysis.trigger,
                target = %target,
                key,
                "Platform-Analyse adjust_threshold ausgefuehrt"
            );
        }
        "escalate_to_operator" => {
            warn!(
                trigger = %analysis.trigger,
                severity = %analysis.severity,
                target = %target,
                summary = %analysis.summary,
                recommendation = %analysis.recommendation,
                "Platform-Analyse an Operator eskaliert"
            );
        }
        other => {
            warn!(
                trigger = %analysis.trigger,
                suggested_action = other,
                target = %target,
                "Platform-Analyse Suggested Action ignoriert"
            );
        }
    }

    Ok(())
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
    operator_rx: mpsc::Receiver<sentinel_common::OperatorCommand>,
    platform_rx: mpsc::Receiver<crate::platform_controlplane::PlatformControlCommand>,
    runtime_rx: mpsc::Receiver<RuntimeControlCommand>,
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
    nightrun_rx: mpsc::Receiver<sentinel_common::OperatorNightrunCommand>,
    snapshot_rx: mpsc::Receiver<sentinel_common::OperatorSnapshotCommand>,
    restore_rx: mpsc::Receiver<sentinel_common::OperatorRestoreCommand>,
    prune_rx: mpsc::Receiver<i64>,
    retention_config: crate::config::RetentionConfig,
    evolution_db_path: String,
    agent_command_cfg: Vec<String>,
    adaptive_config: crate::adaptive_tick::AdaptiveConfig,
    room_distances: sentinel_ecs::RoomDistanceMap,
    room_info: sentinel_ecs::RoomInfoMap,
    fanout_sender: Option<sentinel_ecs::ZenohFanoutSender>,
    resource_manager_config: crate::config::ResourceManagerConfig,
    platform_cp_config: crate::config::PlatformControlplaneConfig,
    events_db_path_str: String,
    platform_state: Arc<RwLock<crate::platform_controlplane::PlatformStateSnapshot>>,
    runtime_health: crate::runtime_health::SharedRuntimeHealthState,
    security_runtime_state: operator_api::SharedSecurityRuntimeState,
    projection_db_path: String,
    operator_auth_required: bool,
    fs_mount: Option<String>,
    fs_layer: Option<Arc<sentinel_fs::layer::LayerManager>>,
    #[cfg(feature = "llm")]
    platform_llm_analyzer: crate::platform_controlplane::llm_analyzer::PlatformLlmAnalyzerHandle,
) -> Result<u64> {
    // Adaptive Tick-Rate Controller (PSI-basiert, TOGAF Adaptive Scheduling)
    let mut adaptive_tick = AdaptiveTickRate::new(adaptive_config);

    // Time Machine: SnapshotManager (Config aus daemon.toml)
    let mut snapshot_manager = crate::snapshot::SnapshotManager::new(retention_config);

    // Smart Resource Management: Dynamische cgroup-Limits
    let mut resource_manager =
        crate::resource_manager::ResourceManager::new(resource_manager_config);

    // Platform-Controlplane: Self-Healing
    let platform_cp_config_clone = platform_cp_config.clone();
    let mut platform_cp =
        crate::platform_controlplane::PlatformControlplane::new(platform_cp_config);
    let mut last_ebpf_snapshot: Option<sentinel_ebpf::collector::MetricsSnapshot> = None;
    let mut pcp_metrics_collector =
        crate::platform_controlplane::metrics::PlatformMetricsCollector::default();

    // Service-Health-Checker: Separater Thread fuer systemctl Calls (non-blocking)
    let service_health_checker = crate::service_health::ServiceHealthChecker::spawn(
        platform_cp_config_clone.monitored_services.clone(),
        std::time::Duration::from_secs(platform_cp_config_clone.service_check_interval_secs),
    );
    let mut respawn_backoff = RespawnBackoffTracker::new(3);

    // ECS World + Schedule erstellen
    let (mut world, mut schedule) = create_simulation_world();

    // Diegetisches HW-Mapping: PSI-Metriken als ECS Resource (bio_system liest diese)
    world.insert_resource(sentinel_ecs::PsiMetrics::default());
    // Room-Distanzen fuer Transit-Dauer und Smell-Propagation
    world.insert_resource(room_distances);
    // Room-Info fuer Capacity-Checks und Floor-Lookup
    world.insert_resource(room_info);

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
    let event_store_for_prune = Arc::clone(&event_store);
    world.insert_resource(LimboEventStore(event_store));
    world.insert_resource(ActionReceiver(std::sync::Mutex::new(action_rx)));
    world.insert_resource(sentinel_ecs::OperatorCommandReceiver(
        std::sync::Mutex::new(operator_rx),
    ));
    world.insert_resource(PerceptionSender(perception_tx));

    // Zenoh Fan-Out Sender als ECS Resource (persist_system nutzt try_send)
    if let Some(sender) = fanout_sender {
        world.insert_resource(sender);
    }

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
                                    match tool_runtime.plugin_host().query_meta(&path, agent_home) {
                                        Ok(meta) => {
                                            let tool_def = sentinel_wasm::ToolDefinition {
                                                name: meta.tool_name.clone(),
                                                description: meta.tool_description,
                                                wasm_path: Some(path.to_string_lossy().to_string()),
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
            if let Err(e) =
                runtime_orch.spawn_agent(identity, shift, &agent_cfg.preferences.favorite_room)
            {
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
                        let aggregate_id = format!("AGENT-{:02}", agent_id.0);
                        match sandbox.start_agent_process(
                            &agent_cfg.identity.name,
                            Some(&aggregate_id),
                            &agent_command,
                        ) {
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
                                record_security_runtime_snapshot(
                                    &security_runtime_state,
                                    agent_id,
                                    &agent_cfg.identity.name,
                                    Some(pid),
                                    fs_mount.as_deref(),
                                );

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
                                record_security_runtime_snapshot(
                                    &security_runtime_state,
                                    agent_id,
                                    &agent_cfg.identity.name,
                                    None,
                                    fs_mount.as_deref(),
                                );
                            }
                        }
                    } else {
                        record_security_runtime_snapshot(
                            &security_runtime_state,
                            agent_id,
                            &agent_cfg.identity.name,
                            None,
                            fs_mount.as_deref(),
                        );
                    }
                }
            }
            Err(e) => {
                warn!(
                    agent = %agent_cfg.identity.name,
                    error = %e,
                    "Sandbox setup fehlgeschlagen (Agent laeuft ohne Isolation)"
                );
                record_security_runtime_snapshot(
                    &security_runtime_state,
                    agent_id,
                    &agent_cfg.identity.name,
                    None,
                    fs_mount.as_deref(),
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
            &agent_cfg.preferences.favorite_room,
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

    // AgentSpawned Events BEHALTEN — Projection braucht sie nach Restore.
    // spawn_agent() emittiert AgentSpawned, upsert_agent() in Projection ist idempotent.
    // Vorher wurden ALLE Events hier geloescht, was nach Restore dazu fuehrte dass
    // die Projection 0 aktive Agents zeigte (Dashboard-API leer).

    info!(
        agent_count = shift_agents.len(),
        orchestrator_count = runtime_orch.agent_count(),
        restored = is_restored,
        shift_set = initial_shift,
        "ECS World initialisiert"
    );

    let mut tick_count: u64 = runtime_orch.current_tick();
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

        // Activity-Tracking: Agents die eine Action ausgefuehrt haben als aktiv markieren
        if let Some(active) = world.get_resource::<sentinel_ecs::ActiveAgentsThisTick>() {
            for agent_id in &active.0 {
                runtime_orch.record_activity(*agent_id, tick_count);
            }
        }
        if let Some(mut active) = world.get_resource_mut::<sentinel_ecs::ActiveAgentsThisTick>() {
            active.0.clear();
        }

        // Smart Resource Management: Profil-Erkennung + cgroup Hot-Resize
        resource_manager.cycle(
            tick_count,
            &runtime_orch,
            &event_store_for_prune,
            adaptive_tick.should_block_spawn(),
        );

        while let Ok(command) = platform_rx.try_recv() {
            info!(command = ?command, "Platform-Controlplane Trigger empfangen");
            match command {
                crate::platform_controlplane::PlatformControlCommand::AnalyzeNow => platform_cp
                    .enqueue_control_command(
                        crate::platform_controlplane::PlatformControlCommand::AnalyzeNow,
                    ),
                crate::platform_controlplane::PlatformControlCommand::TriggerTest(trigger) => {
                    platform_cp.enqueue_control_command(
                        crate::platform_controlplane::PlatformControlCommand::TriggerTest(trigger),
                    );
                }
                crate::platform_controlplane::PlatformControlCommand::ApplyAnalysis(analysis) => {
                    if let Err(error) = apply_platform_analysis_command(
                        analysis,
                        tick_count,
                        &runtime_orch,
                        &mut platform_cp,
                        &mut resource_manager,
                        &event_store_for_prune,
                    ) {
                        warn!(error = %error, "Platform-Analyse konnte nicht ausgefuehrt werden");
                    }
                }
            }
        }

        while let Ok(command) = runtime_rx.try_recv() {
            match command {
                RuntimeControlCommand::Reconcile {
                    request,
                    response_tx,
                } => {
                    let projection_path = std::path::Path::new(&projection_db_path);
                    let data_dir = projection_path
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."));
                    let mut reconcile_ctx = RuntimeReconcileContext {
                        tick_count,
                        current_shift,
                        all_agents: &all_agents,
                        world: &mut world,
                        runtime_orch: &mut runtime_orch,
                        sandbox: &sandbox,
                        sandbox_handles: &mut sandbox_handles,
                        ebpf_collector: &mut ebpf_collector,
                        agent_processes: &mut agent_processes,
                        agent_command: &agent_command,
                        security_runtime_state: &security_runtime_state,
                        event_store: &event_store_for_prune,
                        runtime_health: &runtime_health,
                        projection_db_path: projection_path,
                        operator_auth_required,
                        service_health_state: service_health_checker.worker_state(),
                        fs_mount: fs_mount.as_deref(),
                        data_dir,
                    };
                    let response =
                        run_runtime_reconcile(&mut reconcile_ctx, request, &mut respawn_backoff);
                    let _ = response_tx.send(response);
                }
                RuntimeControlCommand::StallRestartTest {
                    request,
                    response_tx,
                } => {
                    let agent_id = AgentId(request.agent_id);
                    let response = match all_agents
                        .iter()
                        .find(|cfg| cfg.identity.id == request.agent_id)
                    {
                        Some(agent_cfg) => {
                            let pid_before = tracked_pid_for_agent(
                                agent_id,
                                &sandbox_handles,
                                &agent_processes,
                                &security_runtime_state,
                            );
                            let pre_suspend_result = match request.mode.as_str() {
                                "sigstop" => suspend_agent_cgroup_processes(
                                    &agent_cfg.identity.name,
                                    pid_before,
                                )
                                .map(|_| ()),
                                "direct" => Ok(()),
                                _ => Err(anyhow!("unbekannter stall-restart-test mode")),
                            };

                            match pre_suspend_result.and_then(|_| {
                                restart_agent_fast_path(
                                    &mut world,
                                    &mut runtime_orch,
                                    agent_cfg,
                                    &sandbox,
                                    &mut sandbox_handles,
                                    &mut ebpf_collector,
                                    &mut agent_processes,
                                    &agent_command,
                                    &security_runtime_state,
                                    fs_mount.as_deref(),
                                )
                            }) {
                                Ok(result) => {
                                    info!(
                                        agent_id = %agent_id,
                                        mode = %request.mode,
                                        stall_secs = request.stall_secs,
                                        pid_before = ?result.pid_before,
                                        pid_after = ?result.pid_after,
                                        "Deterministischer Stall-Restart-Test ausgefuehrt"
                                    );
                                    RuntimeStallRestartTestResponse {
                                        accepted: true,
                                        agent_id: request.agent_id,
                                        aggregate_id: format!("AGENT-{:02}", request.agent_id),
                                        agent_name: result.agent_name,
                                        mode: request.mode,
                                        stall_secs: request.stall_secs,
                                        pid_before: result.pid_before,
                                        pid_after: result.pid_after,
                                        runtime_present_after: result.runtime_present_after,
                                        security_runtime_present_after: result
                                            .security_runtime_present_after,
                                        note: "fast_path_restart_executed_without_shift_wait"
                                            .to_string(),
                                    }
                                }
                                Err(error) => {
                                    warn!(
                                        agent_id = %agent_id,
                                        mode = %request.mode,
                                        error = %error,
                                        "Stall-Restart-Test fehlgeschlagen"
                                    );
                                    RuntimeStallRestartTestResponse {
                                        accepted: false,
                                        agent_id: request.agent_id,
                                        aggregate_id: format!("AGENT-{:02}", request.agent_id),
                                        agent_name: agent_cfg.identity.name.clone(),
                                        mode: request.mode,
                                        stall_secs: request.stall_secs,
                                        pid_before,
                                        pid_after: None,
                                        runtime_present_after: runtime_orch
                                            .agents()
                                            .contains_key(&agent_id),
                                        security_runtime_present_after: security_runtime_state
                                            .read()
                                            .map(|state| state.contains_key(&request.agent_id))
                                            .unwrap_or(false),
                                        note: error.to_string(),
                                    }
                                }
                            }
                        }
                        None => RuntimeStallRestartTestResponse {
                            accepted: false,
                            agent_id: request.agent_id,
                            aggregate_id: format!("AGENT-{:02}", request.agent_id),
                            agent_name: String::new(),
                            mode: request.mode,
                            stall_secs: request.stall_secs,
                            pid_before: None,
                            pid_after: None,
                            runtime_present_after: false,
                            security_runtime_present_after: false,
                            note: "agent_id unbekannt".to_string(),
                        },
                    };
                    let _ = response_tx.send(response);
                }
            }
        }

        // Platform-Controlplane: Self-Healing (alle N Ticks)
        if sentinel_common::feature_flags::RuntimeFlags::global().platform_controlplane_enabled
            && platform_cp.should_run(tick_count)
        {
            let (pcp_metrics, agent_name_to_id) = collect_platform_metrics_snapshot(
                &runtime_orch,
                &mut pcp_metrics_collector,
                &last_ebpf_snapshot,
                &event_store_for_prune,
                &events_db_path_str,
                tick_count,
                &service_health_checker,
            );
            let output = platform_cp.cycle(
                &pcp_metrics,
                &event_store_for_prune,
                tick_count,
                &agent_name_to_id,
            );
            #[cfg(feature = "llm")]
            for request in output.analysis_requests {
                if let Err(error) = platform_llm_analyzer.enqueue(request) {
                    warn!(error = %error, "Platform LLM Analyzer enqueue fehlgeschlagen");
                }
            }
            for effect in output.side_effects {
                match effect {
                    crate::platform_controlplane::rules::PlatformSideEffect::TriggerPrune(
                        _cutoff,
                    ) => {
                        // Auto-detect cutoff: nutze bestehenden SnapshotManager
                        if let Ok(snapshots) = event_store_for_prune.list_world_snapshots() {
                            if snapshots.len() >= 2 {
                                let prune_point = snapshots[snapshots.len() - 2].last_event_id;
                                snapshot_manager.start_prune(prune_point);
                            }
                        }
                    }
                    crate::platform_controlplane::rules::PlatformSideEffect::ForceIdleProfile(
                        agent_id,
                    ) => {
                        if let Some(handle) = runtime_orch.agents().get(&agent_id) {
                            if let Err(error) = resource_manager.force_profile_and_apply(
                                agent_id,
                                &handle.identity.name,
                                sentinel_sandbox::ResourceProfile::Idle,
                                &event_store_for_prune,
                                tick_count,
                            ) {
                                warn!(
                                    agent_id = %agent_id,
                                    error = %error,
                                    "Idle-Profil konnte nicht erzwungen werden"
                                );
                            }
                        }
                    }
                    crate::platform_controlplane::rules::PlatformSideEffect::SuspendAgent(
                        agent_id,
                    ) => {
                        if let Some(handle) = sandbox_handles.get(&agent_id) {
                            match suspend_agent_cgroup_processes(
                                &handle.agent_name,
                                handle.bwrap_pid,
                            ) {
                                Ok(pids) => {
                                    info!(
                                        agent_id = %agent_id,
                                        agent = %handle.agent_name,
                                        tracked_pid = ?handle.bwrap_pid,
                                        stopped_pids = pids.len(),
                                        "Agent nach Write-Anomaly via SIGSTOP suspendiert"
                                    );
                                }
                                Err(error) => {
                                    warn!(
                                        agent_id = %agent_id,
                                        agent = %handle.agent_name,
                                        tracked_pid = ?handle.bwrap_pid,
                                        error = %error,
                                        "SIGSTOP fuer Write-Anomaly fehlgeschlagen"
                                    );
                                }
                            }
                        } else {
                            warn!(
                                agent_id = %agent_id,
                                "SandboxHandle fuer Write-Anomaly-Suspend fehlt"
                            );
                        }
                    }
                    crate::platform_controlplane::rules::PlatformSideEffect::RestartAgent(
                        agent_id,
                    ) => match all_agents.iter().find(|cfg| cfg.identity.id == agent_id.0) {
                        Some(agent_cfg) => match restart_agent_fast_path(
                            &mut world,
                            &mut runtime_orch,
                            agent_cfg,
                            &sandbox,
                            &mut sandbox_handles,
                            &mut ebpf_collector,
                            &mut agent_processes,
                            &agent_command,
                            &security_runtime_state,
                            fs_mount.as_deref(),
                        ) {
                            Ok(result) => info!(
                                agent_id = %agent_id,
                                agent = %result.agent_name,
                                pid_before = ?result.pid_before,
                                pid_after = ?result.pid_after,
                                "Agent nach Stall sofort via Fast-Respawn wiederhergestellt"
                            ),
                            Err(error) => warn!(
                                agent_id = %agent_id,
                                error = %error,
                                "Fast-Respawn nach Stall fehlgeschlagen"
                            ),
                        },
                        None => warn!(
                            agent_id = %agent_id,
                            "Agent-Konfiguration fuer Stall-Restart nicht gefunden"
                        ),
                    },
                    crate::platform_controlplane::rules::PlatformSideEffect::RestartService(
                        service_name,
                    ) => {
                        if crate::service_health::restart_service_now(&service_name) {
                            let active =
                                crate::service_health::is_service_active_now(&service_name);
                            info!(
                                service = %service_name,
                                active,
                                "Service nach Platform-Intervention restartet"
                            );
                        } else {
                            warn!(
                                service = %service_name,
                                "Service-Restart via Platform-Intervention fehlgeschlagen"
                            );
                        }
                    }
                }
            }
        }
        publish_platform_state_snapshot(
            &platform_state,
            tick_count,
            &platform_cp,
            &runtime_orch,
            &resource_manager,
        );
        runtime_health::publish_runtime_health_snapshot(
            &runtime_health,
            &all_agents,
            current_shift,
            &runtime_orch,
            &sandbox_handles,
            &agent_processes,
            &security_runtime_state,
            std::path::Path::new(&projection_db_path),
            operator_auth_required,
            service_health_checker.worker_state(),
        );

        // Prune: Empfange Cutoff von Operator-API, arbeite 1 Batch/Tick ab
        while let Ok(cutoff) = prune_rx.try_recv() {
            snapshot_manager.start_prune(cutoff);
        }
        snapshot_manager.prune_tick(&event_store_for_prune, tick_count);

        // Controlplane-Zyklus (alle N Ticks) — SENTINEL_CONTROLPLANE_ENABLED gate (AC-6)
        if sentinel_common::feature_flags::RuntimeFlags::global().controlplane_enabled
            && controlplane.should_run(tick_count)
        {
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
                    remove_security_runtime_snapshot(&security_runtime_state, *agent_id);

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

                // GOLF: Goal-Progress fuer konsolidierte Agents aktualisieren
                // Pro ueberlebte Schicht erhoehen wir den Progress aktiver Goals
                // um einen kleinen Betrag (0.05 = ~20 Schichten bis Completion).
                for agent_id in &removed {
                    let agent_name = all_agents
                        .iter()
                        .find(|a| AgentId(a.identity.id) == *agent_id)
                        .map(|a| a.identity.name.as_str());
                    if let Some(name) = agent_name {
                        let goals = episode_producer
                            .hippocampus()
                            .get_goals(name)
                            .unwrap_or_default();
                        let active_goals: Vec<_> = goals.iter().filter(|g| g.is_active()).collect();
                        for goal in &active_goals {
                            let new_progress = (goal.progress + 0.05).min(1.0);
                            match episode_producer.hippocampus().update_goal_progress(
                                name,
                                goal.id,
                                new_progress,
                                tick_count,
                            ) {
                                Ok(true) => {
                                    info!(
                                        agent = name,
                                        goal_id = goal.id,
                                        goal_type = %goal.goal_type,
                                        progress = format!("{:.2}", new_progress),
                                        "GOLF: Goal-Progress aktualisiert"
                                    );
                                }
                                Ok(false) => {} // Goal not found (unlikely)
                                Err(e) => {
                                    warn!(
                                        agent = name,
                                        goal_id = goal.id,
                                        error = %e,
                                        "GOLF: Goal-Progress Update fehlgeschlagen"
                                    );
                                }
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
                        &security_runtime_state,
                        fs_mount.as_deref(),
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

                // AgentSpawned Events BEHALTEN — Projection braucht sie bei Schichtwechsel.
                // upsert_agent() in Projection ist idempotent.

                info!(
                    removed = removed.len(),
                    spawned = spawned_count,
                    active = runtime_orch.agent_count(),
                    "Schichtwechsel abgeschlossen"
                );

                current_shift = new_shift;
            }
        }

        // Nightrun-Trigger verarbeiten (via Operator-API)
        while let Ok(nightrun_cmd) = nightrun_rx.try_recv() {
            info!(
                shift_set = ?nightrun_cmd.shift_set,
                dry_run = nightrun_cmd.dry_run,
                "Nightrun-Trigger empfangen, starte Konsolidierung"
            );
            let target_agents: Vec<_> = all_agents
                .iter()
                .filter(|a| {
                    let set = a.identity.shift_set;
                    if set == 0 {
                        return false; // Sonder-Set nie konsolidieren
                    }
                    nightrun_cmd.shift_set.is_none_or(|s| set == s)
                })
                .collect();
            let mut consolidated_total = 0u32;
            let mut evolution_entries: Vec<(String, u32)> = Vec::new();
            for agent_cfg in &target_agents {
                let name = &agent_cfg.identity.name;
                if nightrun_cmd.dry_run {
                    info!(agent = %name, "Nightrun dry-run: wuerde konsolidieren");
                    continue;
                }
                match episode_producer.hippocampus().consolidate_agent(name) {
                    Ok(result) if result.episodes_processed > 0 => {
                        consolidated_total += result.episodes_processed as u32;
                        evolution_entries
                            .push((name.to_string(), result.episodes_processed as u32));
                        info!(
                            agent = %name,
                            episodes = result.episodes_processed,
                            "Nightrun: Agent konsolidiert"
                        );
                    }
                    Ok(_) => {} // Keine Episodes = nichts zu tun
                    Err(e) => {
                        warn!(agent = %name, error = %e, "Nightrun-Konsolidierung fehlgeschlagen");
                    }
                }
            }

            // personality_evolution Eintraege fuer konsolidierte Agents schreiben
            if !evolution_entries.is_empty() && !nightrun_cmd.dry_run {
                match sentinel_limbo::rusqlite::Connection::open(&evolution_db_path) {
                    Ok(evo_db) => {
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as i64;
                        let mut written = 0u32;
                        for (agent_name, episodes) in &evolution_entries {
                            let agent_id = format!(
                                "AGENT-{:02}",
                                all_agents
                                    .iter()
                                    .find(|a| &a.identity.name == agent_name)
                                    .map(|a| a.identity.id)
                                    .unwrap_or(0)
                            );
                            if evo_db.execute(
                                "INSERT INTO personality_evolution \
                                 (agent_id, tick, field, change_type, old_value, new_value, reason, nmda_score, source, created_at_ms) \
                                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                                sentinel_limbo::rusqlite::params![
                                    agent_id,
                                    tick_count as i64,
                                    "memory_consolidation",
                                    "night_run",
                                    "",
                                    format!("{episodes} episodes consolidated"),
                                    format!("Nightrun-Konsolidierung: {episodes} Episoden verarbeitet"),
                                    0.0_f64,
                                    "night_run",
                                    now_ms,
                                ],
                            ).is_ok() {
                                written += 1;
                            }
                        }
                        info!(
                            written,
                            total = evolution_entries.len(),
                            "personality_evolution: night_run Eintraege geschrieben"
                        );
                    }
                    Err(e) => {
                        warn!(error = %e, "evolution.db nicht oeffenbar — Eintraege uebersprungen");
                    }
                }
            }

            info!(
                agents = target_agents.len(),
                consolidated = consolidated_total,
                dry_run = nightrun_cmd.dry_run,
                "Nightrun abgeschlossen"
            );
        }

        // Time Machine: Periodische World Snapshots
        if snapshot_manager.should_create_snapshot(tick_count) {
            let event_store_for_snapshot = world
                .get_resource::<sentinel_ecs::LimboEventStore>()
                .map(|es| Arc::clone(&es.0));
            if let Some(es) = event_store_for_snapshot {
                let state_store_for_snapshot = world
                    .get_resource::<sentinel_ecs::RedbStateStore>()
                    .map(|rs| rs.store.clone());
                if let Some(ss) = state_store_for_snapshot {
                    let data_dir = std::path::Path::new(&events_db_path_str)
                        .parent()
                        .unwrap_or(std::path::Path::new("/opt/sentinel/data"));
                    match snapshot_manager.create_and_store(
                        &mut world,
                        &ss,
                        &es,
                        data_dir,
                        fs_layer.as_deref(),
                        fs_mount.as_deref(),
                        tick_count,
                        sim_hour,
                    ) {
                        Ok(id) => {
                            debug!(snapshot_id = %id, "World Snapshot erstellt");
                            // Maintenance: Promotion + Cleanup
                            if let Err(e) = snapshot_manager.maintain(&es) {
                                warn!(error = %e, "Snapshot Maintenance fehlgeschlagen");
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "World Snapshot Erstellung fehlgeschlagen");
                        }
                    }
                }
            }
        }

        // Time Machine: Manuelle Snapshot-Trigger via Operator-API
        while let Ok(_snap_cmd) = snapshot_rx.try_recv() {
            let event_store_for_snap = world
                .get_resource::<sentinel_ecs::LimboEventStore>()
                .map(|es| Arc::clone(&es.0));
            let state_store_for_snap = world
                .get_resource::<sentinel_ecs::RedbStateStore>()
                .map(|rs| rs.store.clone());
            if let (Some(es), Some(ss)) = (event_store_for_snap, state_store_for_snap) {
                let data_dir = std::path::Path::new(&events_db_path_str)
                    .parent()
                    .unwrap_or(std::path::Path::new("/opt/sentinel/data"));
                match snapshot_manager.create_and_store(
                    &mut world,
                    &ss,
                    &es,
                    data_dir,
                    fs_layer.as_deref(),
                    fs_mount.as_deref(),
                    tick_count,
                    sim_hour,
                ) {
                    Ok(id) => info!(snapshot_id = %id, "Manueller World Snapshot erstellt"),
                    Err(e) => warn!(error = %e, "Manueller Snapshot fehlgeschlagen"),
                }
            }
        }

        // Time Machine: Hot-Swap Restore via Operator-API
        while let Ok(restore_cmd) = restore_rx.try_recv() {
            info!(snapshot_id = %restore_cmd.snapshot_id, "Hot-Swap Restore gestartet");
            let event_store_for_restore = world
                .get_resource::<sentinel_ecs::LimboEventStore>()
                .map(|es| Arc::clone(&es.0));
            let state_store_for_restore = world
                .get_resource::<sentinel_ecs::RedbStateStore>()
                .map(|rs| rs.store.clone());

            if let (Some(es), Some(ss)) = (event_store_for_restore, state_store_for_restore) {
                // 0. Pre-Restore Snapshot (Rollback-Punkt)
                let data_dir = std::path::Path::new(&events_db_path_str)
                    .parent()
                    .unwrap_or(std::path::Path::new("/opt/sentinel/data"));
                match snapshot_manager.create_and_store(
                    &mut world,
                    &ss,
                    &es,
                    data_dir,
                    fs_layer.as_deref(),
                    fs_mount.as_deref(),
                    tick_count,
                    sim_hour,
                ) {
                    Ok(id) => {
                        info!(snapshot_id = %id, "Pre-Restore Snapshot erstellt (Rollback-Punkt)")
                    }
                    Err(e) => {
                        warn!(error = %e, "Pre-Restore Snapshot fehlgeschlagen (Restore wird fortgesetzt)")
                    }
                }

                // 1. Snapshot laden
                match es.load_world_snapshot(&restore_cmd.snapshot_id) {
                    Ok(Some(bytes)) => {
                        match sentinel_common::decode_world_snapshot(&bytes) {
                            Ok(snapshot) => {
                                // 2. redb restore (atomare Transaktion)
                                if let Err(e) = ss.restore_all_tables(&snapshot.redb) {
                                    error!(error = %e, "redb Restore fehlgeschlagen");
                                    continue;
                                }
                                if let Some(fs_metadata) = &snapshot.fs_metadata {
                                    let Some(layer) = fs_layer.as_deref() else {
                                        error!(
                                            "sentinel-fs Restore angefordert, aber Layer nicht initialisiert"
                                        );
                                        continue;
                                    };
                                    if let Err(e) = layer.meta().restore_all_tables(fs_metadata) {
                                        error!(error = %e, "sentinel-fs Restore fehlgeschlagen");
                                        continue;
                                    }
                                }

                                // 3. Agent-Prozesse terminieren (Sandbox Teardown)
                                let agent_ids: Vec<sentinel_common::AgentId> =
                                    agent_processes.keys().copied().collect();
                                for agent_id in &agent_ids {
                                    if let Some(handle) = sandbox_handles.remove(agent_id) {
                                        if handle.cgroup_created {
                                            if let Some(cid) =
                                                sentinel_sandbox::cgroup_id(&handle.agent_name)
                                            {
                                                ebpf_collector.unregister_agent(cid);
                                            }
                                        }
                                        if let Err(e) = sandbox.teardown_agent(&handle) {
                                            warn!(
                                                agent_id = %agent_id,
                                                error = %e,
                                                "Sandbox teardown bei Restore fehlgeschlagen"
                                            );
                                        }
                                    }
                                    agent_processes.remove(agent_id);
                                    remove_security_runtime_snapshot(
                                        &security_runtime_state,
                                        *agent_id,
                                    );
                                }
                                info!(
                                    terminated = agent_ids.len(),
                                    "Agent-Prozesse fuer Restore terminiert"
                                );

                                // 4. ECS Restore
                                sentinel_ecs::restore_ecs_state(&mut world, &snapshot.ecs);

                                // 6. Tick/SimHour zuruecksetzen + sofortiger Agent-Respawn
                                // Restore wird NACH dem Shift-Check des aktuellen Zyklus
                                // verarbeitet. Darum muessen wir auf "next multiple - 1"
                                // setzen, damit der naechste Loop-Durchlauf direkt wieder
                                // ein Vielfaches von 60 sieht und den Respawn sofort
                                // anstoesst, statt fast 60 Sekunden spaeter.
                                let aligned_tick = ((snapshot.tick / 60) + 1) * 60;
                                let respawn_tick = aligned_tick.saturating_sub(1);
                                tick_count = respawn_tick;
                                sim_hour = snapshot.sim_hour;
                                current_shift = 0;
                                if let Some(mut sim_time) =
                                    world.get_resource_mut::<sentinel_ecs::SimulationTime>()
                                {
                                    sim_time.tick = sentinel_common::Tick(respawn_tick);
                                    sim_time.tick_count = respawn_tick;
                                    sim_time.sim_hour = snapshot.sim_hour;
                                }

                                // 7. CQRS Snapshot-Seeding: Projection direkt aus Snapshot befuellen.
                                //    NICHT Events replayen — das wuerde den Jetzt-State reproduzieren.
                                {
                                    let proj_path =
                                        evolution_db_path.replace("evolution.db", "projection.db");
                                    match sentinel_limbo::rusqlite::Connection::open(&proj_path) {
                                        Ok(db) => {
                                            // 7a. Tables clearen
                                            let _ = db.execute_batch(
                                                "DELETE FROM agent_live_view; \
                                                 DELETE FROM room_live_view; \
                                                 DELETE FROM kpi;",
                                            );

                                            let now_ms = std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .unwrap_or_default()
                                                .as_millis()
                                                as i64;

                                            // 7b. Agent-Daten aus Snapshot in Projection seeden
                                            let mut agents_seeded = 0u32;
                                            for (id, identity) in &snapshot.ecs.identities {
                                                let bio = snapshot
                                                    .ecs
                                                    .bio_states
                                                    .iter()
                                                    .find(|(aid, _)| aid == id)
                                                    .map(|(_, b)| b);
                                                let pos = snapshot
                                                    .ecs
                                                    .positions
                                                    .iter()
                                                    .find(|(aid, _)| aid == id)
                                                    .map(|(_, p)| p);
                                                let mood = snapshot
                                                    .ecs
                                                    .moods
                                                    .iter()
                                                    .find(|(aid, _)| aid == id)
                                                    .map(|(_, m)| m);
                                                let shift = snapshot
                                                    .ecs
                                                    .shift_infos
                                                    .iter()
                                                    .find(|(aid, _)| aid == id)
                                                    .map(|(_, s)| s);

                                                let agent_id_num = *id as i64;
                                                let name = &identity.name;
                                                let role = &identity.role;
                                                let shift_set =
                                                    shift.map(|s| s.shift_set as i64).unwrap_or(1);
                                                let room = pos
                                                    .map(|p| p.room_id.as_str())
                                                    .unwrap_or("empfang");
                                                let in_transit =
                                                    pos.map(|p| p.in_transit as i64).unwrap_or(0);
                                                let hunger =
                                                    bio.map(|b| b.hunger as f64).unwrap_or(20.0);
                                                let energy =
                                                    bio.map(|b| b.energy as f64).unwrap_or(80.0);
                                                let stress =
                                                    bio.map(|b| b.stress as f64).unwrap_or(15.0);
                                                let bladder =
                                                    bio.map(|b| b.bladder as f64).unwrap_or(10.0);
                                                let social = bio
                                                    .map(|b| b.social_need as f64)
                                                    .unwrap_or(50.0);
                                                let caffeine = bio
                                                    .map(|b| b.caffeine_mg as f64)
                                                    .unwrap_or(0.0);
                                                let mood_str = mood
                                                    .map(|m| format!("{:?}", m.dominant_emotion))
                                                    .unwrap_or_else(|| "Neutral".to_string());

                                                if db.execute(
                                                    "INSERT OR REPLACE INTO agent_live_view \
                                                     (agent_id, name, role, shift_set, status, current_room, in_transit, \
                                                      hunger, energy, stress, bladder, social_need, caffeine_mg, mood, \
                                                      last_event_id, updated_at) \
                                                     VALUES (?1,?2,?3,?4,'active',?5,?6,?7,?8,?9,?10,?11,?12,?13,0,?14)",
                                                    sentinel_limbo::rusqlite::params![
                                                        agent_id_num, name, role, shift_set, room, in_transit,
                                                        hunger, energy, stress, bladder, social, caffeine,
                                                        mood_str, now_ms,
                                                    ],
                                                ).is_ok() {
                                                    agents_seeded += 1;
                                                }
                                            }

                                            // 7c. Room-Daten aus ECS RoomPhysicsState seeden
                                            let mut rooms_seeded = 0u32;
                                            if let Ok(physics) = serde_json::from_slice::<
                                                std::collections::HashMap<
                                                    String,
                                                    sentinel_ecs::RoomPhysicsSnapshot,
                                                >,
                                            >(
                                                // RoomPhysicsState nicht im Snapshot — nutze Position-Daten fuer Occupancy
                                                b"{}",
                                            ) {
                                                // Leeres HashMap — Rooms werden aus Position-Daten berechnet
                                                let _ = physics;
                                            }
                                            // Room occupancy aus Agent-Positionen berechnen
                                            let mut room_occupancy: std::collections::HashMap<
                                                String,
                                                u32,
                                            > = std::collections::HashMap::new();
                                            for (_, pos) in &snapshot.ecs.positions {
                                                if !pos.in_transit {
                                                    *room_occupancy
                                                        .entry(pos.room_id.clone())
                                                        .or_default() += 1;
                                                }
                                            }
                                            for (room_id, count) in &room_occupancy {
                                                if db.execute(
                                                    "INSERT OR REPLACE INTO room_live_view \
                                                     (room_id, occupant_count, transit_count, updated_at) \
                                                     VALUES (?1, ?2, 0, ?3)",
                                                    sentinel_limbo::rusqlite::params![room_id, *count as i64, now_ms],
                                                ).is_ok() {
                                                    rooms_seeded += 1;
                                                }
                                            }

                                            info!(
                                                agents_seeded,
                                                rooms_seeded,
                                                "Projection Snapshot-Seeding abgeschlossen"
                                            );
                                        }
                                        Err(e) => {
                                            warn!(error = %e, "projection.db nicht oeffenbar — Seeding uebersprungen");
                                        }
                                    }
                                }

                                // 7d. Offset auf max(event_id) setzen — NICHT snapshot.last_event_id!
                                //     snapshot.last_event_id wuerde alle Events danach replayen
                                //     und den Jetzt-State reproduzieren. max(id) ueberspringt alles.
                                let max_event_id = es.get_latest_event_id().unwrap_or(0);
                                for (name, _) in &snapshot.projection_offsets {
                                    let _ = es.force_reset_offset(name, max_event_id);
                                }

                                // 8. SnapshotRestored Event emittieren
                                //    (Bridge leitet an NATS weiter → Judge/Consumer reagieren)
                                let restore_event = sentinel_common::DomainEvent {
                                    event_id: uuid::Uuid::new_v4().to_string(),
                                    event_type: "snapshot_restored".to_string(),
                                    aggregate_id: "system".to_string(),
                                    payload: serde_json::json!({
                                        "snapshot_id": restore_cmd.snapshot_id,
                                        "restored_tick": snapshot.tick,
                                        "restored_sim_hour": snapshot.sim_hour,
                                        "agents_count": snapshot.ecs.identities.len(),
                                    })
                                    .to_string(),
                                    correlation_id: restore_cmd.snapshot_id.clone(),
                                    causation_id: None,
                                    operation_id: uuid::Uuid::new_v4().to_string(),
                                    tick: snapshot.tick,
                                    timestamp_ms: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_millis()
                                        as u64,
                                    schema_version: 1,
                                    compensation_type: "none".to_string(),
                                };
                                // append_with_outbox: Bridge pollt Outbox → NATS → Judge
                                if let Err(e) =
                                    es.append_with_outbox(&restore_event, "sentinel.events")
                                {
                                    warn!(error = %e, "SnapshotRestored Event schreiben fehlgeschlagen");
                                }

                                // 9. Action-Channel leeren (Zukunfts-Events verwerfen)
                                if let Some(receiver) =
                                    world.get_resource::<sentinel_ecs::ActionReceiver>()
                                {
                                    if let Ok(rx) = receiver.0.lock() {
                                        let mut drained = 0u32;
                                        while rx.try_recv().is_ok() {
                                            drained += 1;
                                        }
                                        if drained > 0 {
                                            info!(drained, "Action-Channel geleert (Zukunfts-Events verworfen)");
                                        }
                                    }
                                }

                                info!(
                                    snapshot_id = %restore_cmd.snapshot_id,
                                    tick = snapshot.tick,
                                    sim_hour = snapshot.sim_hour,
                                    agents = snapshot.ecs.identities.len(),
                                    "Hot-Swap Restore abgeschlossen"
                                );
                            }
                            Err(e) => {
                                error!(error = %e, "Snapshot-Deserialisierung fehlgeschlagen");
                            }
                        }
                    }
                    Ok(None) => {
                        warn!(
                            snapshot_id = %restore_cmd.snapshot_id,
                            "Snapshot nicht gefunden"
                        );
                    }
                    Err(e) => {
                        error!(error = %e, "Snapshot laden fehlgeschlagen");
                    }
                }
            }
        }

        // eBPF Metrics Collection (Intervall konfigurierbar fuer deterministische PCP-Tests)
        let ebpf_collect_interval = platform_cp.config().ebpf_collect_interval_ticks.max(1);
        if tick_count > 0 && tick_count.is_multiple_of(ebpf_collect_interval) {
            match ebpf_collector.collect() {
                Ok(snapshot) => {
                    last_ebpf_snapshot = Some(snapshot.clone());
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

    // ── Graceful Shutdown mit Timing-Instrumentierung (AC-4 #255) ──
    let shutdown_start = Instant::now();

    // 1. SIGTERM an alle Agent-Prozesse senden BEVOR Drop (AC-2 #255)
    let t = Instant::now();
    let agent_count = agent_processes.len();
    for proc in agent_processes.values() {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &proc.pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    if agent_count > 0 {
        // Kurz warten damit Agents auf SIGTERM reagieren koennen
        std::thread::sleep(Duration::from_millis(200));
    }
    // Drop reaps exitierte Prozesse via try_wait()
    agent_processes.clear();
    if let Ok(mut state) = security_runtime_state.write() {
        state.clear();
    }
    info!(
        agents = agent_count,
        duration_ms = t.elapsed().as_millis() as u64,
        "Shutdown: Agent-Teardown"
    );

    // 3. Sandbox teardown (cgroups, netns)
    let t = Instant::now();
    let teardown_count = sandbox_handles.len();
    for (agent_id, handle) in sandbox_handles.drain() {
        if let Err(e) = sandbox.teardown_agent(&handle) {
            warn!(agent_id = %agent_id, error = %e, "Sandbox teardown fehlgeschlagen");
        }
    }
    info!(
        count = teardown_count,
        duration_ms = t.elapsed().as_millis() as u64,
        "Shutdown: Sandbox teardown"
    );

    // 4. sim_hour persistieren
    let t = Instant::now();
    if let Err(e) = state_store_for_sim.set_sim_hour(sim_hour) {
        warn!(error = %e, "sim_hour Shutdown-Persist fehlgeschlagen");
    }
    info!(
        duration_ms = t.elapsed().as_millis() as u64,
        "Shutdown: sim_hour persist"
    );

    // 5. Runtime-Snapshot speichern (VOR Despawn! Snapshot muss aktuelle Agents enthalten,
    //    nicht 0. Beim Restart erkennt shift_transition() ob Schichtwechsel stattfand
    //    und entfernt/spawnt Agents entsprechend.)
    let t = Instant::now();
    if let Err(e) = runtime_orch.save_state() {
        error!(error = %e, "Runtime State Snapshot fehlgeschlagen");
    } else {
        info!(
            agent_count = runtime_orch.agent_count(),
            "Runtime State Snapshot gespeichert"
        );
    }
    info!(
        duration_ms = t.elapsed().as_millis() as u64,
        "Shutdown: Runtime-Snapshot"
    );

    // 6. Despawn-Events emittieren (Projection occupant_count Drift vermeiden)
    let t = Instant::now();
    let despawned = runtime_orch.despawn_all_for_shutdown();
    info!(
        agents = despawned,
        duration_ms = t.elapsed().as_millis() as u64,
        "Shutdown: Despawn-Events"
    );

    info!(
        total_ms = shutdown_start.elapsed().as_millis() as u64,
        "Shutdown abgeschlossen"
    );

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
    use sentinel_common::{DomainEventPayload, EventType, OperatorChaosCommand, OperatorCommand};
    use sentinel_ebpf::loader::MonitoringMode;
    use std::collections::HashMap;
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
                kpis: Vec::new(),
                reports_to: None,
                direct_reports: Vec::new(),
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
    fn test_suspend_pids_stops_tracked_process() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep child");
        let pid = child.id();

        suspend_pids(&[pid], Some(pid)).expect("pid suspended");
        assert_eq!(proc_state(pid), Some('T'));

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn test_restart_agent_fast_path_recreates_runtime_and_security_state() {
        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");
        let event_store = Arc::new(EventStore::open(events_path.to_str().unwrap()).unwrap());

        let mut runtime_orch = RuntimeOrchestrator::new(10).with_event_store(event_store);
        let (mut world, _schedule) = create_simulation_world();
        let sandbox = test_sandbox();
        let (mut ebpf_collector, _ebpf_tx) = test_ebpf();
        let mut sandbox_handles = HashMap::new();
        let mut agent_processes = HashMap::new();
        let security_runtime_state = Arc::new(RwLock::new(HashMap::new()));
        let agent_cfg = test_agent_config(1, "Test Agent", "Tester", 1);
        let agent_command = vec!["true".to_string()];

        assert!(spawn_agent_full(
            &mut runtime_orch,
            &mut world,
            &agent_cfg,
            &sandbox,
            &mut sandbox_handles,
            &mut ebpf_collector,
            &mut agent_processes,
            &agent_command,
            &security_runtime_state,
            None,
        ));

        let before_pid = tracked_pid_for_agent(
            AgentId(1),
            &sandbox_handles,
            &agent_processes,
            &security_runtime_state,
        );
        let result = restart_agent_fast_path(
            &mut world,
            &mut runtime_orch,
            &agent_cfg,
            &sandbox,
            &mut sandbox_handles,
            &mut ebpf_collector,
            &mut agent_processes,
            &agent_command,
            &security_runtime_state,
            None,
        )
        .expect("fast restart");

        assert_eq!(result.agent_name, "Test Agent");
        assert_eq!(result.pid_before, before_pid);
        assert!(result.runtime_present_after);
        assert!(result.security_runtime_present_after);
        assert!(runtime_orch.agents().contains_key(&AgentId(1)));
        assert!(security_runtime_state.read().unwrap().contains_key(&1));
        assert_eq!(runtime_orch.agent_count(), 1);
        if let (Some(pid_before), Some(pid_after)) = (result.pid_before, result.pid_after) {
            assert_ne!(
                pid_before, pid_after,
                "Fast-Restart sollte einen neuen PID liefern"
            );
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
        let (_operator_tx, operator_rx) = mpsc::channel();
        let (ptx, _prx) = mpsc::sync_channel(128);

        let controlplane = test_controlplane(&tmp);
        let runtime_orch = RuntimeOrchestrator::new(10).with_event_store(Arc::clone(&event_store));

        let ep = test_episode_producer(&tmp, &event_store);
        let (ebpf_collector, ebpf_tx) = test_ebpf();
        let result = ecs_tick_loop(
            state_store,
            event_store,
            rx,
            operator_rx,
            mpsc::channel::<crate::platform_controlplane::PlatformControlCommand>().1,
            mpsc::channel::<RuntimeControlCommand>().1,
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
            mpsc::channel::<sentinel_common::OperatorNightrunCommand>().1,
            mpsc::channel::<sentinel_common::OperatorSnapshotCommand>().1,
            mpsc::channel::<sentinel_common::OperatorRestoreCommand>().1,
            mpsc::channel::<i64>().1,
            crate::config::RetentionConfig::default(),
            String::new(),
            vec!["true".to_string()],
            crate::adaptive_tick::AdaptiveConfig::default(),
            sentinel_ecs::RoomDistanceMap::default(),
            sentinel_ecs::RoomInfoMap::default(),
            None, // Kein Zenoh Fan-Out in Tests
            crate::config::ResourceManagerConfig::default(),
            crate::config::PlatformControlplaneConfig::default(),
            String::new(),
            Arc::new(RwLock::new(
                crate::platform_controlplane::PlatformStateSnapshot::default(),
            )),
            Arc::new(RwLock::new(
                crate::runtime_health::RuntimeHealthSnapshot::default(),
            )),
            Arc::new(RwLock::new(HashMap::new())),
            String::new(),
            false,
            None,
            None,
            #[cfg(feature = "llm")]
            crate::platform_controlplane::llm_analyzer::PlatformLlmAnalyzerHandle::disabled(),
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_ecs_tick_loop_runs_ticks() {
        // RuntimeFlags must be initialized before ECS systems run (#233)
        sentinel_common::feature_flags::RuntimeFlags::init();

        // Deterministisch: ecs_tick_loop laeuft im Background-Thread, Main-Thread wartet
        // auf erste Perception (beweist mindestens 1 Tick). Kein Race moeglich, da Shutdown
        // erst NACH Perception-Empfang gesetzt wird.
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);

        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");
        let state_path = tmp.path().join("state.redb");

        let event_store = Arc::new(EventStore::open(events_path.to_str().unwrap()).unwrap());
        let state_store = Arc::new(StateStore::open(state_path.to_str().unwrap()).unwrap());

        let (_tx, rx) = mpsc::channel();
        let (_operator_tx, operator_rx) = mpsc::channel();
        let (ptx, prx) = mpsc::sync_channel(128);

        let controlplane = test_controlplane(&tmp);
        let runtime_orch = RuntimeOrchestrator::new(10).with_event_store(Arc::clone(&event_store));
        let all_agents = vec![test_agent_config(1, "Test Agent", "Tester", 1)];

        let (ebpf_collector, ebpf_tx) = test_ebpf();
        let ep = test_episode_producer(&tmp, &event_store);

        // ecs_tick_loop in Background-Thread (Setup-Dauer irrelevant)
        let handle = std::thread::spawn(move || {
            ecs_tick_loop(
                state_store,
                event_store,
                rx,
                operator_rx,
                mpsc::channel::<crate::platform_controlplane::PlatformControlCommand>().1,
                mpsc::channel::<RuntimeControlCommand>().1,
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
                mpsc::channel::<sentinel_common::OperatorNightrunCommand>().1,
                mpsc::channel::<sentinel_common::OperatorSnapshotCommand>().1,
                mpsc::channel::<sentinel_common::OperatorRestoreCommand>().1,
                mpsc::channel::<i64>().1,
                crate::config::RetentionConfig::default(),
                String::new(),
                vec!["true".to_string()],
                crate::adaptive_tick::AdaptiveConfig::default(),
                sentinel_ecs::RoomDistanceMap::default(),
                sentinel_ecs::RoomInfoMap::default(),
                None, // Kein Zenoh Fan-Out in Tests
                crate::config::ResourceManagerConfig::default(),
                crate::config::PlatformControlplaneConfig::default(),
                String::new(),
                Arc::new(RwLock::new(
                    crate::platform_controlplane::PlatformStateSnapshot::default(),
                )),
                Arc::new(RwLock::new(
                    crate::runtime_health::RuntimeHealthSnapshot::default(),
                )),
                Arc::new(RwLock::new(HashMap::new())),
                String::new(),
                false,
                None,
                None,
                #[cfg(feature = "llm")]
                crate::platform_controlplane::llm_analyzer::PlatformLlmAnalyzerHandle::disabled(),
            )
        });

        // Warte auf erste Perception (event-driven, nicht zeit-basiert)
        let perception = prx.recv_timeout(Duration::from_secs(30));
        assert!(
            perception.is_ok(),
            "Erste Perception muss innerhalb 30s ankommen"
        );

        // Shutdown erst NACH Perception-Empfang → garantiert tick_count >= 1
        shutdown_clone.store(true, Ordering::SeqCst);

        let result = handle.join().expect("ecs_tick_loop thread panicked");
        assert!(result.is_ok());
        let ticks = result.unwrap();
        assert!(ticks >= 1, "Mindestens 1 Tick erwartet, bekam {ticks}");
    }

    #[test]
    fn test_save_state_on_shutdown() {
        // RuntimeFlags must be initialized before ECS systems run (#233)
        sentinel_common::feature_flags::RuntimeFlags::init();

        // Verifiziert dass Runtime-Snapshot nach Loop-Exit existiert.
        // Gleiche deterministische Struktur wie test_ecs_tick_loop_runs_ticks:
        // ecs_tick_loop im Background-Thread, Perception-Warten im Main-Thread.
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);

        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");
        let state_path = tmp.path().join("state.redb");

        let event_store = Arc::new(EventStore::open(events_path.to_str().unwrap()).unwrap());
        let state_store = Arc::new(StateStore::open(state_path.to_str().unwrap()).unwrap());

        let (_tx, rx) = mpsc::channel();
        let (_operator_tx, operator_rx) = mpsc::channel();
        let (ptx, prx) = mpsc::sync_channel(128);

        let controlplane = test_controlplane(&tmp);
        let runtime_orch = RuntimeOrchestrator::new(10).with_event_store(Arc::clone(&event_store));
        let all_agents = vec![
            test_agent_config(1, "Thomas", "CEO", 1),
            test_agent_config(2, "Lisa", "Designer", 1),
        ];

        let es_clone = Arc::clone(&event_store);
        let ep = test_episode_producer(&tmp, &event_store);
        let (ebpf_collector, ebpf_tx) = test_ebpf();

        // ecs_tick_loop in Background-Thread (Setup-Dauer irrelevant)
        let handle = std::thread::spawn(move || {
            ecs_tick_loop(
                state_store,
                event_store,
                rx,
                operator_rx,
                mpsc::channel::<crate::platform_controlplane::PlatformControlCommand>().1,
                mpsc::channel::<RuntimeControlCommand>().1,
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
                mpsc::channel::<sentinel_common::OperatorNightrunCommand>().1,
                mpsc::channel::<sentinel_common::OperatorSnapshotCommand>().1,
                mpsc::channel::<sentinel_common::OperatorRestoreCommand>().1,
                mpsc::channel::<i64>().1,
                crate::config::RetentionConfig::default(),
                String::new(),
                vec!["true".to_string()],
                crate::adaptive_tick::AdaptiveConfig::default(),
                sentinel_ecs::RoomDistanceMap::default(),
                sentinel_ecs::RoomInfoMap::default(),
                None, // Kein Zenoh Fan-Out in Tests
                crate::config::ResourceManagerConfig::default(),
                crate::config::PlatformControlplaneConfig::default(),
                String::new(),
                Arc::new(RwLock::new(
                    crate::platform_controlplane::PlatformStateSnapshot::default(),
                )),
                Arc::new(RwLock::new(
                    crate::runtime_health::RuntimeHealthSnapshot::default(),
                )),
                Arc::new(RwLock::new(HashMap::new())),
                String::new(),
                false,
                None,
                None,
                #[cfg(feature = "llm")]
                crate::platform_controlplane::llm_analyzer::PlatformLlmAnalyzerHandle::disabled(),
            )
        });

        // Warte auf erste Perception (event-driven, nicht zeit-basiert)
        let perception = prx.recv_timeout(Duration::from_secs(30));
        assert!(
            perception.is_ok(),
            "Erste Perception muss innerhalb 30s ankommen"
        );

        // Shutdown erst NACH Perception-Empfang
        shutdown_clone.store(true, Ordering::SeqCst);

        let result = handle.join().expect("ecs_tick_loop thread panicked");
        assert!(result.is_ok());

        // Snapshot muss existieren
        let snapshot = es_clone.get_latest_snapshot("runtime");
        assert!(
            snapshot.is_ok() && snapshot.unwrap().is_some(),
            "Runtime-Snapshot muss nach Shutdown existieren"
        );
    }

    #[test]
    fn test_operator_command_is_forwarded_to_ecs_and_persisted() {
        sentinel_common::feature_flags::RuntimeFlags::init();

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);

        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");
        let state_path = tmp.path().join("state.redb");

        let event_store = Arc::new(EventStore::open(events_path.to_str().unwrap()).unwrap());
        let state_store = Arc::new(StateStore::open(state_path.to_str().unwrap()).unwrap());
        let es_clone = Arc::clone(&event_store);

        let (_tx, rx) = mpsc::channel();
        let (operator_tx, operator_rx) = mpsc::channel();
        let (ptx, prx) = mpsc::sync_channel(128);

        operator_tx
            .send(OperatorCommand::Chaos(OperatorChaosCommand {
                event_id: "evt-operator-test".to_string(),
                correlation_id: "corr-operator-test".to_string(),
                operation_id: "op-operator-test".to_string(),
                room_id: "empfang".to_string(),
                chaos_type: EventType::AirConBroken,
                description: "Manueller Operator-Test".to_string(),
                duration_ticks: Some(45),
            }))
            .unwrap();

        let controlplane = test_controlplane(&tmp);
        let runtime_orch = RuntimeOrchestrator::new(10).with_event_store(Arc::clone(&event_store));
        let all_agents = vec![test_agent_config(1, "Test Agent", "Tester", 1)];
        let ep = test_episode_producer(&tmp, &event_store);
        let (ebpf_collector, ebpf_tx) = test_ebpf();

        let handle = std::thread::spawn(move || {
            ecs_tick_loop(
                state_store,
                event_store,
                rx,
                operator_rx,
                mpsc::channel::<crate::platform_controlplane::PlatformControlCommand>().1,
                mpsc::channel::<RuntimeControlCommand>().1,
                ptx,
                all_agents,
                1,
                Duration::from_millis(50),
                1.0,
                shutdown,
                controlplane,
                runtime_orch,
                test_sandbox(),
                ebpf_collector,
                ebpf_tx,
                ep,
                mpsc::channel::<sentinel_common::OperatorNightrunCommand>().1,
                mpsc::channel::<sentinel_common::OperatorSnapshotCommand>().1,
                mpsc::channel::<sentinel_common::OperatorRestoreCommand>().1,
                mpsc::channel::<i64>().1,
                crate::config::RetentionConfig::default(),
                String::new(),
                vec!["true".to_string()],
                crate::adaptive_tick::AdaptiveConfig::default(),
                sentinel_ecs::RoomDistanceMap::default(),
                sentinel_ecs::RoomInfoMap::default(),
                None, // Kein Zenoh Fan-Out in Tests
                crate::config::ResourceManagerConfig::default(),
                crate::config::PlatformControlplaneConfig::default(),
                String::new(),
                Arc::new(RwLock::new(
                    crate::platform_controlplane::PlatformStateSnapshot::default(),
                )),
                Arc::new(RwLock::new(
                    crate::runtime_health::RuntimeHealthSnapshot::default(),
                )),
                Arc::new(RwLock::new(HashMap::new())),
                String::new(),
                false,
                None,
                None,
                #[cfg(feature = "llm")]
                crate::platform_controlplane::llm_analyzer::PlatformLlmAnalyzerHandle::disabled(),
            )
        });

        let perception = prx.recv_timeout(Duration::from_secs(30));
        assert!(
            perception.is_ok(),
            "Erste Perception muss innerhalb 30s ankommen"
        );

        shutdown_clone.store(true, Ordering::SeqCst);

        let result = handle.join().expect("ecs_tick_loop thread panicked");
        assert!(result.is_ok());

        let operator_event = es_clone
            .get_all_events()
            .unwrap()
            .into_iter()
            .find(|event| event.event_id == "evt-operator-test")
            .expect("operator command should create a persisted chaos event");

        assert_eq!(operator_event.event_type, "chaos_triggered");
        assert_eq!(operator_event.aggregate_id, "empfang");
        assert_eq!(operator_event.correlation_id, "corr-operator-test");
        assert_eq!(operator_event.operation_id, "op-operator-test");

        let payload: DomainEventPayload = serde_json::from_str(&operator_event.payload).unwrap();
        match payload {
            DomainEventPayload::ChaosTriggered {
                event_type,
                target_room,
                description,
            } => {
                assert_eq!(event_type, EventType::AirConBroken);
                assert_eq!(target_room.as_deref(), Some("empfang"));
                assert_eq!(description, "Manueller Operator-Test");
            }
            other => panic!("unexpected payload: {other:?}"),
        }
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
