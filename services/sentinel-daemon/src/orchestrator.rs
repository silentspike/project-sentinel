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
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use tracing::{debug, error, info, warn};

use sentinel_common::agent_config::{load_all_agents_with_validation, AgentConfig};
use sentinel_common::components::{AgentIdentity, ShiftInfo};
use sentinel_common::events::{DomainEvent, DomainEventPayload};
use sentinel_common::{AgentId, AgentIdBounds, OperatorCommand, Perception};
use sentinel_ebpf::collector::MetricsSnapshot;
use sentinel_ebpf::EbpfCollector;
use sentinel_ecs::{
    apply_personality, create_simulation_world, despawn_agent_from_world, spawn_agent,
    ActionReceiver, LimboEventStore, PerceptionSender, SimulationTime,
};
use sentinel_hippocampus::{NMDA_CONSOLIDATION_THRESHOLD, NMDA_MAX_CONSOLIDATION_EPISODES};
use sentinel_limbo::EventStore;
use sentinel_redb::StateStore;
use sentinel_runtime::RuntimeOrchestrator;
use sentinel_sandbox::{
    CgroupLimits, IsolationStatus, SandboxEnforcer, SandboxHandle, SandboxWarning,
};
use sentinel_zenoh::SentinelBus;
use sha2::{Digest, Sha256};

use crate::adaptive_tick::AdaptiveTickRate;
use crate::config::DaemonConfig;
use crate::controlplane::config::ControlplaneConfig;
use crate::controlplane::store::ControlplaneStore;
use crate::controlplane::ControlplaneKernel;
use crate::episode_producer::EpisodeProducer;
use crate::evolution_task::{EvolutionJob, EvolutionResult, EvolutionSource};
use crate::operator_api;
use crate::runtime_control::{
    AgentLifecycleResponse, RespawnBackoffTracker, RespawnRetryDecision,
    RuntimeAnalysisFloodTestResponse, RuntimeControlCommand, RuntimePanicTestResponse,
    RuntimeReconcileRequest, RuntimeReconcileResponse, RuntimeStallRestartTestResponse,
};
use crate::runtime_health;
use crate::shift::{agents_for_shift, detect_current_shift, detect_shift_from_sim_hour};
use crate::signal::wait_for_shutdown;

const PERSONALITY_EVOLUTION_PER_AGENT_FIELD_KEEP: i64 = 2000;
const PERSONALITY_EVOLUTION_GLOBAL_HIGH_WATER: i64 = 499_000;
const PERSONALITY_EVOLUTION_GLOBAL_RETAIN: i64 = 490_000;

fn retain_personality_evolution_agent_field(
    evo_db: &sentinel_limbo::rusqlite::Connection,
    agent_id: &str,
    field: &str,
) -> Result<()> {
    evo_db
        .execute(
            "DELETE FROM personality_evolution
             WHERE agent_id = ?1
               AND field = ?2
               AND id <= (
                 SELECT id
                 FROM personality_evolution
                 WHERE agent_id = ?1 AND field = ?2
                 ORDER BY id DESC
                 LIMIT 1 OFFSET ?3
               )",
            sentinel_limbo::rusqlite::params![
                agent_id,
                field,
                PERSONALITY_EVOLUTION_PER_AGENT_FIELD_KEEP
            ],
        )
        .context("personality_evolution agent-field retention failed")?;
    Ok(())
}

fn retain_personality_evolution_global(
    evo_db: &sentinel_limbo::rusqlite::Connection,
) -> Result<()> {
    let count: i64 = evo_db
        .query_row("SELECT COUNT(*) FROM personality_evolution", [], |row| {
            row.get(0)
        })
        .context("personality_evolution global retention count failed")?;
    if count <= PERSONALITY_EVOLUTION_GLOBAL_HIGH_WATER {
        return Ok(());
    }
    evo_db
        .execute(
            "DELETE FROM personality_evolution
             WHERE id <= (
               SELECT id
               FROM personality_evolution
               ORDER BY id DESC
               LIMIT 1 OFFSET ?1
             )",
            sentinel_limbo::rusqlite::params![PERSONALITY_EVOLUTION_GLOBAL_RETAIN],
        )
        .context("personality_evolution global retention failed")?;
    Ok(())
}

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

fn parse_judge_alert_agent_id(agent_id: &str, bounds: AgentIdBounds) -> Result<AgentId> {
    let agent_num = agent_id
        .strip_prefix("AGENT-")
        .ok_or_else(|| anyhow!("Judge alert agent id {agent_id} lacks AGENT- prefix"))?
        .parse::<u16>()
        .with_context(|| format!("Judge alert agent id {agent_id} has invalid number"))?;
    AgentId::new_with_bounds(agent_num, bounds)
        .with_context(|| format!("Judge alert agent id {agent_id} is outside configured bounds"))
}

#[derive(Clone, Copy, Debug)]
struct NmdaScoreStats {
    min: Option<f64>,
    avg: Option<f64>,
    max: Option<f64>,
}

#[derive(Debug)]
struct NightrunHashChain {
    current: [u8; 32],
}

impl NightrunHashChain {
    fn new(seed: &str, run_id: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(run_id.as_bytes());
        hasher.update(b":");
        hasher.update(seed.as_bytes());
        Self {
            current: hasher.finalize().into(),
        }
    }

    fn extend(&mut self, event: &DomainEvent) {
        let mut hasher = Sha256::new();
        hasher.update(self.current);
        hasher.update(event.event_id.as_bytes());
        hasher.update(event.payload.as_bytes());
        hasher.update(event.tick.to_le_bytes());
        self.current = hasher.finalize().into();
    }

    fn current_hash(&self) -> String {
        hex_encode(&self.current)
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn nmda_selection_rate(episodes_processed: u32, episodes_consolidated: u32) -> f64 {
    if episodes_processed == 0 {
        0.0
    } else {
        episodes_consolidated as f64 / episodes_processed as f64
    }
}

fn nmda_consolidated_scores(result: &sentinel_hippocampus::ConsolidationResult) -> Vec<f64> {
    result
        .consolidated_summaries
        .iter()
        .map(|(_summary, score)| *score)
        .collect()
}

fn nmda_score_stats(scores: &[f64]) -> NmdaScoreStats {
    let min = scores.iter().copied().reduce(f64::min);
    let max = scores.iter().copied().reduce(f64::max);
    let avg = if scores.is_empty() {
        None
    } else {
        Some(scores.iter().sum::<f64>() / scores.len() as f64)
    };

    NmdaScoreStats { min, avg, max }
}

fn nightrun_run_id(prefix: &str, tick_count: u64, from_shift: u8, to_shift: u8) -> String {
    format!("{prefix}-tick-{tick_count}-shift-{from_shift}-to-{to_shift}")
}

fn append_nightrun_event(
    event_store: &EventStore,
    payload: DomainEventPayload,
    aggregate_id: &str,
    run_id: &str,
    tick_count: u64,
    hash_chain: Option<&mut NightrunHashChain>,
) -> Result<DomainEvent> {
    let event = DomainEvent::new(
        payload.event_type_str(),
        aggregate_id,
        &payload.to_json(),
        run_id,
        tick_count,
    );
    event_store.append_with_outbox(&event, "nightrun")?;
    if let Some(chain) = hash_chain {
        chain.extend(&event);
    }
    Ok(event)
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

fn terminate_agent_process(mut proc_handle: sentinel_sandbox::AgentProcess) {
    let _ = signal_pid(proc_handle.pid, "TERM");
    for _ in 0..10 {
        if !proc_handle.is_running() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    proc_handle.terminate();
}

/// #75: verifies the just-spawned agent runs in its own network namespace
/// (full cage) and enforces it.
///
/// `child_pid` is the sandboxed `agent-runtime` PID from bwrap `--info-fd`
/// (NOT the supervisor PID). Behavior per [`IsolationStatus`]:
/// - `Isolated`: mark the handle, continue.
/// - `ProbeError`: transient `/proc` read glitch — warn only, do NOT terminate
///   (the bwrap exit code is the primary fail-closed signal).
/// - `NotIsolated`: the agent is NOT caged (e.g. forced `share_net`). Make it
///   visible (warn + health snapshot + string-typed `AgentIsolationFailed`
///   event where an event store is available) and terminate the un-caged
///   process — the agent drops to the existing ECS-only fallback rather than
///   running with host network access.
///
/// `event_store` is `Some` on paths that own one (ecs tick loop); the initial
/// spawn path relies on the warn + health snapshot (the durable signal there).
#[allow(clippy::too_many_arguments)]
fn enforce_agent_netns_isolation(
    agent_id: AgentId,
    agent_name: &str,
    child_pid: Option<u32>,
    sandbox: &SandboxEnforcer,
    sandbox_handles: &mut HashMap<AgentId, SandboxHandle>,
    ebpf_collector: &mut EbpfCollector,
    agent_processes: &mut HashMap<AgentId, sentinel_sandbox::AgentProcess>,
    security_runtime_state: &operator_api::SharedSecurityRuntimeState,
    event_store: Option<&EventStore>,
) {
    let Some(cpid) = child_pid else {
        warn!(
            agent = %agent_name,
            "bwrap meldete keinen sandboxed child-pid (--info-fd); netns-Verifikation uebersprungen (bwrap-Exit bleibt fail-closed-Signal)"
        );
        return;
    };

    match sandbox.verify_agent_netns_isolation(cpid) {
        IsolationStatus::Isolated => {
            if let Some(handle) = sandbox_handles.get_mut(&agent_id) {
                handle.network_isolated = true;
            }
            debug!(agent = %agent_name, child_pid = cpid, "Agent net-cage verifiziert (#75)");
        }
        IsolationStatus::ProbeError => {
            warn!(
                agent = %agent_name,
                child_pid = cpid,
                "netns-Verifikation nicht lesbar (ProbeError) — Agent laeuft weiter; bwrap-Exit ist primaeres fail-closed-Signal (#75)"
            );
        }
        IsolationStatus::NotIsolated => {
            warn!(
                agent = %agent_name,
                child_pid = cpid,
                "Agent ist NICHT netz-isoliert (share_net?) — terminiere Prozess, ECS-only Fallback (#75)"
            );
            // Durable health/monitoring status: no valid sandboxed process.
            record_security_runtime_snapshot(
                security_runtime_state,
                agent_id,
                agent_name,
                None,
                None,
            );
            // String-typed event — no DomainEventPayload enum variant / no
            // sentinel-common schema change (keeps clear of #493).
            if let Some(store) = event_store {
                let aggregate = format!("AGENT-{:02}", agent_id.0);
                let payload = serde_json::json!({
                    "agent_id": agent_id.0,
                    "agent_name": agent_name,
                    "child_pid": cpid,
                    "reason": "not_isolated",
                })
                .to_string();
                let event =
                    DomainEvent::new("AgentIsolationFailed", &aggregate, &payload, &aggregate, 0);
                if let Err(e) = store.append_event(&event) {
                    warn!(agent = %agent_name, error = %e, "AgentIsolationFailed-Event speichern fehlgeschlagen");
                }
            }
            // Terminate the un-caged process; fall back to ECS-only.
            if let Some(proc) = agent_processes.remove(&agent_id) {
                terminate_agent_process(proc);
            }
            if let Some(handle) = sandbox_handles.remove(&agent_id) {
                if handle.cgroup_created {
                    if let Some(cid) = sentinel_sandbox::cgroup_id(&handle.agent_name) {
                        ebpf_collector.unregister_agent(cid);
                    }
                }
                if let Err(e) = sandbox.teardown_agent(&handle) {
                    warn!(agent = %agent_name, error = %e, "Sandbox-Teardown nach Isolations-Fehler fehlgeschlagen");
                }
            }
        }
    }
}

fn mountinfo_contains_mountpoint(mountinfo: &str, path: &std::path::Path) -> bool {
    let target = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned();

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
}

fn mountpoint_is_active(path: &std::path::Path) -> bool {
    std::fs::read_to_string("/proc/self/mountinfo")
        .ok()
        .map(|mountinfo| mountinfo_contains_mountpoint(&mountinfo, path))
        .unwrap_or(false)
}

fn wait_for_fuse_mount(path: &std::path::Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if mountpoint_is_active(path) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    mountpoint_is_active(path)
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

/// #428: Gegenstueck zu `suspend_agent_cgroup_processes` — schickt SIGCONT an alle Prozesse im
/// Agent-Cgroup (+ tracked PID), um eine SIGSTOP-Pause aufzuheben. Gibt die fortgesetzten PIDs zurueck.
fn resume_agent_cgroup_processes(agent_name: &str, tracked_pid: Option<u32>) -> Result<Vec<u32>> {
    let mut pids = sentinel_sandbox::cgroups::list_pids_in_cgroup(agent_name)
        .with_context(|| format!("cgroup-Mitglieder fuer {agent_name} nicht lesbar"))?;
    if let Some(pid) = tracked_pid {
        pids.push(pid);
    }
    pids.sort_unstable();
    pids.dedup();
    for pid in &pids {
        signal_pid(*pid, "CONT")?;
    }
    Ok(pids)
}

/// Spawnt einen Agenten sowohl im RuntimeOrchestrator als auch in der ECS World.
/// Richtet die Sandbox (cgroup + home dir + bwrap-Prozess) ein wenn verfuegbar.
/// Gibt `true` zurueck wenn erfolgreich.
#[allow(clippy::too_many_arguments)]
fn spawn_agent_runtime_stack(
    runtime_orch: &mut RuntimeOrchestrator,
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

            // #75: captured inside the handle-borrow block; netns isolation is
            // verified after the borrow is released (helper needs the maps).
            let mut agent_process_started = false;
            let mut started_child_pid: Option<u32> = None;

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
                            // #75: sandboxed child PID (from bwrap --info-fd) for
                            // netns verification — NOT the supervisor `pid`.
                            let proc_child_pid = proc.child_pid;
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

                            // #75: defer netns isolation check until the handle
                            // borrow is released (the verifier needs the maps).
                            agent_process_started = true;
                            started_child_pid = proc_child_pid;
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
                } else {
                    record_security_runtime_snapshot(
                        security_runtime_state,
                        agent_id,
                        &agent_cfg.identity.name,
                        None,
                        fs_mount,
                    );
                }
            }

            // #75: verify full-cage isolation after the handle borrow is
            // released (the verifier needs the maps). Initial-spawn path has no
            // event store; warn + health snapshot are the durable signals here.
            if agent_process_started {
                enforce_agent_netns_isolation(
                    agent_id,
                    &agent_cfg.identity.name,
                    started_child_pid,
                    sandbox,
                    sandbox_handles,
                    ebpf_collector,
                    agent_processes,
                    security_runtime_state,
                    None,
                );
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

    true
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
    if !spawn_agent_runtime_stack(
        runtime_orch,
        agent_cfg,
        sandbox,
        sandbox_handles,
        ebpf_collector,
        agent_processes,
        agent_command,
        security_runtime_state,
        fs_mount,
    ) {
        return false;
    }

    let agent_id = AgentId(agent_cfg.identity.id);
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

    // -- Cluster 12 (#496): pin the owner registry to this node's identity FIRST, before
    // any store opens or writes. An early fenced write (snapshot restore, projection)
    // would otherwise lock the process-global `OwnerRegistry` to its nil single-node
    // default (`OnceLock`) and make a later `init_single_node` a silent no-op — leaving
    // the seed's registry identity as nil. The durable term/retirement reconcile still
    // runs below, once the cluster-meta store is open. --
    if let Some(cluster) = config.cluster.as_ref() {
        sentinel_common::OwnerRegistry::init_single_node(cluster.node_id);
    }

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
        let metadata_durability = match config.fs_metadata_durability {
            crate::config::FsMetadataDurability::Immediate => {
                sentinel_fs::metadata::MetadataDurability::Immediate
            }
            crate::config::FsMetadataDurability::Eventual => {
                sentinel_fs::metadata::MetadataDurability::Eventual
            }
        };
        let meta = sentinel_fs::metadata::MetadataStore::open_with_durability(
            data_dir.join("metadata.redb"),
            metadata_durability,
        )
        .context("sentinel-fs Metadata oeffnen")?;
        info!(
            ?metadata_durability,
            "sentinel-fs Metadata-Durability konfiguriert"
        );
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
    let agent_validation = config.agent_config_validation()?;
    let all_agents = load_all_agents_with_validation(&agents_dir, agent_validation)
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
    let active_fs_mount: Option<String> = {
        #[cfg(feature = "fuse")]
        {
            let mut active_mount = None;
            if let Some(ref fs_mount) = config.fs_mount {
                let mountpoint = std::path::PathBuf::from(fs_mount);
                let fs_layer_clone = fs_layer
                    .as_ref()
                    .cloned()
                    .ok_or_else(|| anyhow!("sentinel-fs Layer nicht initialisiert"))?;
                if !mountpoint.exists() {
                    std::fs::create_dir_all(&mountpoint).with_context(|| {
                        format!("FUSE mountpoint erstellen: {}", mountpoint.display())
                    })?;
                }
                info!(
                    mountpoint = %mountpoint.display(),
                    data_dir = %data_dir.display(),
                    "sentinel-fs FUSE-Mount starten"
                );
                let mountpoint_check = mountpoint.clone();
                std::thread::spawn(move || {
                    if let Err(e) = sentinel_fs::fuse::start_fuse_layer(fs_layer_clone, &mountpoint)
                    {
                        error!(error = %e, "sentinel-fs FUSE-Mount fehlgeschlagen");
                    }
                });
                if wait_for_fuse_mount(&mountpoint_check, Duration::from_secs(2)) {
                    info!(mountpoint = %mountpoint_check.display(), "sentinel-fs FUSE-Mount aktiv");
                    active_mount = Some(fs_mount.clone());
                } else {
                    warn!(
                        mountpoint = %mountpoint_check.display(),
                        "sentinel-fs FUSE-Mount nicht aktiv, fallback auf /ram/agents"
                    );
                }
            }
            active_mount
        }
        #[cfg(not(feature = "fuse"))]
        {
            None
        }
    };

    // -- Sandbox Enforcer (Landlock + cgroups v2 + bwrap) --
    let (mut sandbox, sandbox_warnings) = SandboxEnforcer::detect();

    // Wenn sentinel-fs FUSE aktiv ist: bwrap nutzt FUSE-Mount statt /ram/agents/.
    if let Some(ref fs_mount) = active_fs_mount {
        sandbox.set_fs_mount(fs_mount.clone());
        info!(fs_mount = %fs_mount, "Sandbox nutzt sentinel-fs FUSE-Mount fuer Agent-Homes");
    } else if config.fs_mount.is_some() {
        warn!("Konfiguriertes fs_mount deaktiviert, weil kein tragfaehiger FUSE-Mount aktiv ist");
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
        }
    }
    info!(
        landlock = sandbox.has_landlock(),
        cgroups = sandbox.has_cgroups(),
        bwrap = sandbox.has_bwrap(),
        // #75: agents are full-caged by bwrap --unshare-all; the daemon verifies
        // isolation post-spawn on the sandboxed child PID.
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
    let (config_apply_tx, config_apply_rx) =
        mpsc::channel::<sentinel_common::OperatorConfigApplyCommand>();
    let (migrate_tx, migrate_rx) = mpsc::channel::<sentinel_common::OperatorMigrateCommand>();
    let (provision_tx, provision_rx) = mpsc::channel::<sentinel_common::OperatorProvisionCommand>();
    let (prune_tx, prune_rx) = mpsc::channel::<i64>();
    let (evolution_result_tx, evolution_result_rx) = mpsc::channel::<EvolutionResult>();
    let evolution_job_tx = crate::evolution_task::spawn_evolution_background_task(
        crate::evolution_task::EvolutionTaskConfig::from_env(),
        evolution_result_tx,
    );
    info!("Evolution Background-Task initialisiert");
    // Bounded Channel: 128 Slots. Bridge drainet per try_recv() vor jedem LLM-Call.
    // Output_system nutzt try_send() (non-blocking, WARN bei Drop).
    let (perception_tx, perception_rx) = mpsc::sync_channel::<Perception>(128);
    let platform_state = Arc::new(RwLock::new(
        crate::platform_controlplane::PlatformStateSnapshot::default(),
    ));
    let runtime_health = Arc::new(RwLock::new(
        crate::runtime_health::RuntimeHealthSnapshot::default(),
    ));
    let llm_circuit_open = Arc::new(AtomicBool::new(false));
    let llm_activity_ticks = Arc::new(Mutex::new(HashMap::<AgentId, u64>::new()));
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

    // -- Cluster 12 (#496): pin the owner registry to this node's identity so every
    // fenced store write validates against committed ownership (V19), and open the
    // durable cluster-meta store (ADR-3). Single-node the seed owns every scope, so live
    // behavior is unchanged; without [daemon.cluster] the registry keeps its default
    // single-node owner and no meta store is opened. The `Arc` is shared with the #569
    // control-stream handler so an `OwnerCommit` RPC persists ownership here (PR2b-2). --
    let cluster_meta: Option<Arc<sentinel_redb::ClusterMetaStore>> = match config.cluster.as_ref() {
        Some(cluster) => {
            // `init_single_node` already ran at the top of `run` (before any store write);
            // here we only open the durable cluster-meta store and reconcile from it.
            let cluster_meta_path = data_dir.join("cluster_meta.redb");
            match sentinel_redb::ClusterMetaStore::open(&cluster_meta_path.to_string_lossy()) {
                Ok(meta) => {
                    // Seed the `World` term on first start; on restart it is read back
                    // (PR2b-1c) so ownership survives a reboot.
                    match meta.get_owner_term(&sentinel_common::StateTransferScope::World) {
                        Ok(Some(term)) => {
                            info!(
                                epoch = term.epoch,
                                "Cluster 12: Owner-Term aus Meta-Store geladen (World)"
                            )
                        }
                        Ok(None) => {
                            let term = sentinel_common::OwnerTerm {
                                scope: sentinel_common::StateTransferScope::World,
                                owner_node: cluster.node_id,
                                epoch: 1,
                            };
                            match meta.put_owner_term(&term) {
                                Ok(()) => info!("Cluster 12: Seed-Owner-Term im Meta-Store angelegt (World @ epoch 1)"),
                                Err(e) => warn!(error = %e, "Cluster 12: Seed-Owner-Term konnte nicht persistiert werden"),
                            }
                        }
                        Err(e) => warn!(error = %e, "Cluster 12: Owner-Term-Lesen fehlgeschlagen"),
                    }
                    // Restart-reconcile (PR2b-2): re-establish the in-memory registry from
                    // every persisted term. A committed cross-node term (owner is not this
                    // seed, or epoch != 1) re-enters cluster mode so a handoff's
                    // OwnerCommit(E+1) survives a reboot; the seed's own World@1 stays on
                    // the lock-free fast path (no commit_owner -> mode unchanged).
                    match meta.list_owner_terms() {
                        Ok(terms) => {
                            for term in terms {
                                if term.owner_node != cluster.node_id || term.epoch != 1 {
                                    info!(scope = ?term.scope, epoch = term.epoch, owner = %term.owner_node,
                                        "Cluster 12: Owner-Term aus Meta-Store re-etabliert (Cluster-Mode)");
                                    sentinel_common::OwnerRegistry::global().commit_owner(term);
                                }
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "Cluster 12: Owner-Terms-Reconcile fehlgeschlagen")
                        }
                    }
                    // Re-establish durable local retirements (V4, PR2b-2ii): a scope this
                    // node retired in a prior cooperative handoff stays fenced across the
                    // restart, even before any cross-node term update is visible.
                    match meta.list_local_states() {
                        Ok(states) if !states.is_empty() => {
                            let n = states.len();
                            sentinel_common::OwnerRegistry::global()
                                .restore_local_retirements(states);
                            info!(
                                count = n,
                                "Cluster 12: lokale Retirements aus Meta-Store re-etabliert (V4)"
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!(error = %e, "Cluster 12: lokale Retirements-Reconcile fehlgeschlagen")
                        }
                    }
                    info!(node_id = %cluster.node_id, "Cluster 12: OwnerRegistry als Single-Node-Seed initialisiert");
                    Some(Arc::new(meta))
                }
                Err(e) => {
                    warn!(error = %e, "Cluster 12: ClusterMetaStore konnte nicht geoeffnet werden");
                    None
                }
            }
        }
        None => None,
    };

    // -- Cluster 12 Membership (#495): Heartbeat + Liveness-View, nur mit [daemon.cluster] --
    if let (Some(b), Some(cluster)) = (bus.as_ref(), config.cluster.as_ref()) {
        let identity = sentinel_common::NodeIdentity::from_config(cluster);
        let view = std::sync::Arc::new(std::sync::Mutex::new(
            sentinel_common::MembershipView::new(sentinel_common::MembershipConfig::default()),
        ));
        tokio::spawn(crate::cluster_membership::run_cluster_membership(
            b.clone(),
            identity,
            view,
            std::time::Duration::from_secs(1),
        ));
        info!(node_id = %cluster.node_id, "Cluster 12: Membership-Service gespawnt");
    }

    // -- Cluster 12 ProvisionNode worker (#495, G3): only on the seed --
    // The seed absorbs allowlisted bare targets into cluster nodes. On a member or a
    // single-node daemon the receiver is dropped, so a `ProvisionNode` request fails
    // fast at the operator endpoint (503) instead of buffering forever.
    match config.cluster.as_ref() {
        Some(cluster) if cluster.role() == sentinel_common::cluster::ClusterRole::Seed => {
            let binary_path = cluster
                .provision_binary_path
                .clone()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("/opt/sentinel/bin/sentinel-daemon"));
            let pending_targets = cluster.pending_targets.clone();
            let cluster_id = cluster.cluster_id;
            let provision_event_store = Arc::clone(&event_store);
            if let Err(e) = std::thread::Builder::new()
                .name("provision-worker".into())
                .spawn(move || {
                    run_provision_worker(
                        provision_rx,
                        cluster_id,
                        pending_targets,
                        binary_path,
                        "ubuntu".to_string(),
                        provision_event_store,
                    );
                })
            {
                warn!(error = %e, "Cluster 12: ProvisionNode-Worker konnte nicht gestartet werden");
            } else {
                info!(
                    targets = cluster.pending_targets.len(),
                    "Cluster 12: ProvisionNode-Worker gespawnt (seed)"
                );
            }
        }
        _ => drop(provision_rx),
    }

    // -- Cluster 12 control stream (#569, ADR-2): one cert-pinned QUIC RPC server,
    // started only when [daemon.cluster].control_bind is set. The handle (server kept
    // alive + outbound client) is shared with the operator API for the live RPC AC. --
    let cluster_control: Option<Arc<crate::cluster_control::ClusterControl>> =
        match config.cluster.as_ref() {
            Some(cluster) => match cluster.control_bind.as_deref() {
                Some(bind) => {
                    let alias = cluster
                        .alias
                        .clone()
                        .unwrap_or_else(|| cluster.node_id.to_string());
                    match crate::cluster_control::ClusterControl::start(
                        bind,
                        data_dir,
                        &alias,
                        &cluster.control_peers,
                        cluster_meta.clone(),
                    ) {
                        Ok(cc) => Some(Arc::new(cc)),
                        Err(e) => {
                            warn!(error = %e, "Cluster 12: control stream failed to start");
                            None
                        }
                    }
                }
                None => None,
            },
            None => None,
        };

    // -- #498 CAS block-map gossip republish (Cluster 12): only when the control stream
    // is active (cluster mode). Single-node prod has no [daemon.cluster].control_bind, so
    // this never spawns — the production tick path is unchanged (Strangler S4). --
    if let Some(ref cc) = cluster_control {
        if let Some(cluster) = config.cluster.as_ref() {
            tokio::spawn(crate::cluster_control::run_cas_gossip_republish(
                Arc::clone(cc),
                data_dir.to_path_buf(),
                cluster.node_id,
                uuid::Uuid::new_v4(),
                std::time::Duration::from_secs(15),
                256,
            ));
            info!("Cluster 12: #498 CAS block-map gossip republish gestartet");
        }
    }

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

    let nightrun_agent_counts = operator_api::NightrunAgentCounts::from_shift_sets(
        all_agents.iter().map(|agent| agent.identity.shift_set),
    );

    let operator_api_handle = if config.operator_api.enabled {
        Some(
            operator_api::start_server(
                config.operator_api.clone(),
                data_dir.to_path_buf(),
                active_fs_mount.clone(),
                fs_layer.clone(),
                operator_room_ids,
                operator_tx.clone(),
                platform_tx.clone(),
                runtime_tx.clone(),
                nightrun_tx.clone(),
                nightrun_agent_counts,
                snapshot_tx.clone(),
                restore_tx.clone(),
                config_apply_tx.clone(),
                migrate_tx.clone(),
                provision_tx.clone(),
                config.max_agents,
                agent_validation,
                Arc::clone(&event_store),
                prune_tx.clone(),
                Arc::clone(&state_store),
                Arc::clone(&platform_state),
                Arc::clone(&runtime_health),
                Arc::clone(&security_runtime_state),
                cluster_control.clone(),
                cluster_meta.clone(),
                config
                    .cluster
                    .as_ref()
                    .map(|c| c.alias.clone().unwrap_or_else(|| c.node_id.to_string())),
                config.cluster.as_ref().map(|c| c.seed).unwrap_or(false),
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
    let ecs_llm_circuit_open = Arc::clone(&llm_circuit_open);
    let ecs_llm_activity_ticks = Arc::clone(&llm_activity_ticks);
    let ecs_security_runtime_state = Arc::clone(&security_runtime_state);
    let ecs_fs_mount = active_fs_mount.clone();
    let ecs_projection_db_path = projection_db_path.clone();
    let ecs_config_dir = config.config_dir.clone();
    let ecs_max_agents = config.max_agents;
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
                config.phase_timing_enabled,
                shutdown_ecs,
                controlplane,
                runtime_orch,
                sandbox,
                ebpf_collector,
                ebpf_tx,
                episode_producer,
                nightrun_rx,
                Some(evolution_job_tx),
                Some(evolution_result_rx),
                snapshot_rx,
                restore_rx,
                config_apply_rx,
                migrate_rx,
                ecs_config_dir,
                ecs_max_agents,
                agent_validation,
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
                ecs_llm_circuit_open,
                ecs_llm_activity_ticks,
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

    // -- Prometheus eBPF Metrics Server (loopback, #525: [daemon.metrics] bind_addr) --
    let prom_text = Arc::clone(&prometheus_text);
    tokio::spawn(crate::ebpf::prometheus_server(
        prom_text,
        config.metrics.bind_addr.clone(),
    ));

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
            Arc::clone(&event_store), // #427: emit AgentLlmUsage per LLM call
            Arc::clone(&llm_circuit_open),
            Arc::clone(&llm_activity_ticks),
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
        let alert_agent_id_bounds = agent_validation.agent_id_bounds;
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
                // Parse "AGENT-XX" → u16 → AgentId with the same bounds as Agent-TOMLs.
                let agent_id =
                    parse_judge_alert_agent_id(&alert.agent_id, alert_agent_id_bounds).unwrap_or_else(
                        |error| {
                            warn!(
                                error = %error,
                                agent = %alert.agent_id,
                                "Judge Alert AgentId konnte nicht validiert werden; fallback auf AGENT-01"
                            );
                            AgentId(1)
                        },
                    );
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
    llm_circuit_open: &AtomicBool,
    llm_activity_ticks: &Mutex<HashMap<AgentId, u64>>,
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
        llm_circuit_open,
    );
    for handle in runtime_orch.agents().values() {
        pcp_metrics
            .last_action_ticks
            .insert(handle.identity.name.clone(), handle.last_activity_tick.0);
    }
    if let Ok(llm_ticks) = llm_activity_ticks.lock() {
        for (agent_id, last_tick) in llm_ticks.iter() {
            if let Some(handle) = runtime_orch.agents().get(agent_id) {
                pcp_metrics
                    .last_llm_call_ticks
                    .insert(handle.identity.name.clone(), *last_tick);
            }
        }
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

    if let Some(proc_handle) = agent_processes.remove(&agent_id) {
        terminate_agent_process(proc_handle);
    }

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
    restart_service_fn: fn(&str) -> bool,
    is_service_active_fn: fn(&str) -> bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeReconcileSource {
    Operator,
    Periodic,
}

impl RuntimeReconcileSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Periodic => "periodic",
        }
    }

    fn is_periodic(self) -> bool {
        matches!(self, Self::Periodic)
    }
}

fn should_run_periodic_runtime_reconcile(
    config: &crate::config::PlatformControlplaneConfig,
    tick_count: u64,
) -> bool {
    config.runtime_reconcile_enabled
        && tick_count > 0
        && tick_count.is_multiple_of(config.runtime_reconcile_interval_ticks.max(1))
}

fn should_run_periodic_runtime_reconcile_unfenced(
    config: &crate::config::PlatformControlplaneConfig,
    tick_count: u64,
    restore_fence: &RestoreFence,
) -> bool {
    !restore_fence.is_active() && should_run_periodic_runtime_reconcile(config, tick_count)
}

fn periodic_runtime_reconcile_request(
    config: &crate::config::PlatformControlplaneConfig,
) -> RuntimeReconcileRequest {
    RuntimeReconcileRequest {
        dry_run: false,
        projection_rebuild: config.runtime_reconcile_projection_rebuild,
        respawn_missing: config.runtime_reconcile_respawn_missing,
    }
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

fn emit_runtime_projection_despawn_event(
    event_store: &EventStore,
    agent_id: AgentId,
    tick_count: u64,
) -> Result<i64> {
    let aggregate_id = format!("AGENT-{:02}", agent_id.0);
    let payload = DomainEventPayload::AgentDespawned {
        agent_id,
        reason: "runtime_reconcile_projection_only".to_string(),
    };
    let op_id = format!(
        "runtime-reconcile-projection-despawn-{}-{}-{}",
        agent_id.0,
        tick_count,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let event = DomainEvent::new(
        payload.event_type_str(),
        &aggregate_id,
        &payload.to_json(),
        &op_id,
        tick_count,
    )
    .with_operation_id(&op_id);
    event_store
        .append_event(&event)
        .with_context(|| format!("Projection-only Despawn-Event persistieren fuer {aggregate_id}"))
}

fn mark_agent_projection_despawned(
    projection_db_path: &std::path::Path,
    agent_id: AgentId,
    last_event_id: i64,
) -> Result<()> {
    let db = sentinel_limbo::rusqlite::Connection::open(projection_db_path)
        .with_context(|| format!("Projection DB oeffnen: {}", projection_db_path.display()))?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    db.execute(
        "UPDATE agent_live_view
         SET status = 'despawned',
             in_transit = 0,
             transit_target = NULL,
             last_event_id = CASE
               WHEN agent_live_view.last_event_id > ?2
               THEN agent_live_view.last_event_id
               ELSE ?2
             END,
             updated_at = ?3
         WHERE agent_id = ?1",
        sentinel_limbo::rusqlite::params![agent_id.0 as i64, last_event_id, now_ms],
    )?;
    Ok(())
}

fn emit_runtime_projection_spawn_event(
    event_store: &EventStore,
    agent_cfg: &AgentConfig,
    tick_count: u64,
) -> Result<i64> {
    let agent_id = AgentId(agent_cfg.identity.id);
    let aggregate_id = format!("AGENT-{:02}", agent_id.0);
    let payload = DomainEventPayload::AgentSpawned {
        agent_id,
        name: agent_cfg.identity.name.clone(),
        role: agent_cfg.identity.role.clone(),
        shift_set: agent_cfg.identity.shift_set,
        room_id: agent_cfg.preferences.favorite_room.clone(),
    };
    let op_id = format!(
        "runtime-reconcile-projection-spawn-{}-{}-{}",
        agent_id.0,
        tick_count,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let event = DomainEvent::new(
        payload.event_type_str(),
        &aggregate_id,
        &payload.to_json(),
        &op_id,
        tick_count,
    )
    .with_operation_id(&op_id);
    event_store.append_event(&event)
}

fn upsert_agent_projection_seed(
    projection_db_path: &std::path::Path,
    agent_cfg: &AgentConfig,
    last_event_id: i64,
) -> Result<()> {
    let db = sentinel_limbo::rusqlite::Connection::open(projection_db_path)
        .with_context(|| format!("Projection DB oeffnen: {}", projection_db_path.display()))?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    db.execute(
        "INSERT INTO agent_live_view
           (agent_id, name, role, shift_set, status, current_room, in_transit, last_event_id, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'active', ?5, 0, ?6, ?7)
         ON CONFLICT(agent_id) DO UPDATE SET
           name = excluded.name,
           role = excluded.role,
           shift_set = excluded.shift_set,
           status = 'active',
           current_room = excluded.current_room,
           in_transit = 0,
           transit_target = NULL,
           last_event_id = CASE
             WHEN agent_live_view.last_event_id > excluded.last_event_id
             THEN agent_live_view.last_event_id
             ELSE excluded.last_event_id
           END,
           updated_at = excluded.updated_at",
        sentinel_limbo::rusqlite::params![
            agent_cfg.identity.id as i64,
            &agent_cfg.identity.name,
            &agent_cfg.identity.role,
            agent_cfg.identity.shift_set as i64,
            &agent_cfg.preferences.favorite_room,
            last_event_id,
            now_ms,
        ],
    )?;
    Ok(())
}

fn remove_agent_runtime_fragments(
    ctx: &mut RuntimeReconcileContext<'_>,
    agent: &runtime_health::RuntimeHealthAgentSnapshot,
) -> RuntimeCleanupStats {
    let agent_id = AgentId(agent.agent_id);
    let mut stats = RuntimeCleanupStats::default();

    if let Some(proc_handle) = ctx.agent_processes.remove(&agent_id) {
        terminate_agent_process(proc_handle);
        stats.repairs += 1;
    }

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

    if agent.cgroup_live_pid_count > 0 {
        match sentinel_sandbox::cgroups::kill_cgroup_processes(&agent.name) {
            Ok(killed) if killed > 0 => {
                stats.repairs += 1;
            }
            Ok(_) => {}
            Err(error) => warn!(
                agent = %agent.name,
                error = %error,
                "Live-Cgroup-PIDs konnten nicht hart beendet werden"
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
    let cgroup_empty = sentinel_sandbox::cgroups::list_pids_in_cgroup(&agent.name)
        .map(|pids| pids.is_empty())
        .unwrap_or(false);
    if std::path::Path::new(&cgroup_path).exists() && cgroup_empty {
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

#[allow(clippy::too_many_arguments)]
fn execute_runtime_reconcile(
    tick_count: u64,
    current_shift: u8,
    all_agents: &[AgentConfig],
    world: &mut bevy_ecs::prelude::World,
    runtime_orch: &mut RuntimeOrchestrator,
    sandbox: &SandboxEnforcer,
    sandbox_handles: &mut HashMap<AgentId, SandboxHandle>,
    ebpf_collector: &mut EbpfCollector,
    agent_processes: &mut HashMap<AgentId, sentinel_sandbox::AgentProcess>,
    agent_command: &[String],
    security_runtime_state: &operator_api::SharedSecurityRuntimeState,
    event_store: &Arc<EventStore>,
    runtime_health: &crate::runtime_health::SharedRuntimeHealthState,
    projection_db_path: &str,
    operator_auth_required: bool,
    service_health_state: crate::service_health::ServiceHealthWorkerSnapshot,
    fs_mount: Option<&str>,
    request: RuntimeReconcileRequest,
    respawn_backoff: &mut RespawnBackoffTracker,
    source: RuntimeReconcileSource,
) -> RuntimeReconcileResponse {
    let projection_path = std::path::Path::new(projection_db_path);
    let data_dir = projection_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut reconcile_ctx = RuntimeReconcileContext {
        tick_count,
        current_shift,
        all_agents,
        world,
        runtime_orch,
        sandbox,
        sandbox_handles,
        ebpf_collector,
        agent_processes,
        agent_command,
        security_runtime_state,
        event_store,
        runtime_health,
        projection_db_path: projection_path,
        operator_auth_required,
        service_health_state,
        fs_mount,
        data_dir,
        restart_service_fn: crate::service_health::restart_service_now,
        is_service_active_fn: crate::service_health::is_service_active_now,
    };
    run_runtime_reconcile(&mut reconcile_ctx, request, respawn_backoff, source)
}

fn run_runtime_reconcile(
    ctx: &mut RuntimeReconcileContext<'_>,
    request: RuntimeReconcileRequest,
    respawn_backoff: &mut RespawnBackoffTracker,
    source: RuntimeReconcileSource,
) -> RuntimeReconcileResponse {
    let elapsed_started = std::time::Instant::now();
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
    let projection_drift_before = before.projection_drift_detected;

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
    let mut projection_restart_attempted = false;
    let mut projection_restart_succeeded = false;

    if projection_drift_before && !request.dry_run {
        let projection_service_active = (ctx.is_service_active_fn)("sentinel-projection");
        if request.projection_rebuild && projection_service_active {
            info!(
                "Projection-Drift erkannt, aber laufender Projection-Service bleibt fuer Rebuild in Ruhe"
            );
        } else {
            projection_restart_attempted = true;
            if (ctx.restart_service_fn)("sentinel-projection") {
                projection_restart_succeeded = true;
                repair_ops_total += 1;
            } else {
                errors.push("Projection-Restart fehlgeschlagen".to_string());
            }
        }
    }

    if !request.dry_run {
        for agent in before.agents.iter().filter(|agent| {
            let expected_active = expected_ids.contains(&agent.agent_id);
            !expected_active
                && (agent.runtime_present
                    || agent.projection_present
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
            if agent.projection_present {
                match emit_runtime_projection_despawn_event(
                    ctx.event_store,
                    AgentId(agent.agent_id),
                    ctx.tick_count,
                )
                .and_then(|row_id| {
                    mark_agent_projection_despawned(
                        ctx.projection_db_path,
                        AgentId(agent.agent_id),
                        row_id,
                    )
                }) {
                    Ok(()) => {
                        repair_ops_total += 1;
                        repaired_agents.push(agent.name.clone());
                        agent_status_updates
                            .insert(agent.agent_id, "projection_despawned".to_string());
                    }
                    Err(error) => {
                        errors.push(format!(
                            "Projection-only Despawn fehlgeschlagen fuer {}: {error}",
                            agent.name
                        ));
                    }
                }
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
                    Ok(_) => match sentinel_sandbox::cgroups::kill_cgroup_processes(&name) {
                        Ok(killed) if killed > 0 => {
                            repair_ops_total += 1;
                            if sentinel_sandbox::cgroups::remove_cgroup(&name).is_ok() {
                                orphan_cgroups_removed += 1;
                                repair_ops_total += 1;
                            }
                        }
                        Ok(_) => {}
                        Err(error) => warn!(
                            cgroup = %name,
                            error = %error,
                            "Orphan-Cgroup-PIDs konnten nicht hart beendet werden"
                        ),
                    },
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
                if request.projection_rebuild && !request.dry_run {
                    match emit_runtime_projection_spawn_event(
                        ctx.event_store,
                        agent_cfg,
                        ctx.tick_count,
                    )
                    .and_then(|row_id| {
                        upsert_agent_projection_seed(ctx.projection_db_path, agent_cfg, row_id)
                    }) {
                        Ok(()) => {
                            repair_ops_total += 1;
                            repaired_agents.push(agent_cfg.identity.name.clone());
                            agent_status_updates.insert(agent_id, "projection_seeded".to_string());
                        }
                        Err(error) => {
                            errors.push(format!(
                                "Projection-Seed fehlgeschlagen fuer {}: {error}",
                                agent_cfg.identity.name
                            ));
                            agent_status_updates
                                .insert(agent_id, "projection_reconcile_pending".to_string());
                        }
                    }
                } else {
                    agent_status_updates
                        .insert(agent_id, "projection_reconcile_pending".to_string());
                }
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
        let reason = if projection_drift_before {
            "projection_drift"
        } else {
            "manual_request"
        };
        match crate::runtime_control::write_projection_rebuild_request(
            ctx.data_dir,
            ctx.tick_count,
            reason,
        ) {
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
    if source.is_periodic() {
        after.auto_reconcile_runs_total = before.auto_reconcile_runs_total.saturating_add(1);
    }
    after.last_reconcile_tick = ctx.tick_count;
    after.last_reconcile_source = source.as_str().to_string();
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
    } else if projection_rebuild_requested {
        "projection_rebuild_requested".to_string()
    } else if projection_restart_attempted && after.projection_drift_detected {
        "projection_restart_requested".to_string()
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
        projection_drift_before,
        projection_drift_after: after.projection_drift_detected,
        projection_restart_attempted,
        projection_restart_succeeded,
        projection_rebuild_requested,
        respawn_failures_total: after.respawn_failures,
        repair_last_status: after
            .repair_last_status
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        repaired_agents,
        blocked_agents,
        errors,
        elapsed_us: elapsed_started.elapsed().as_micros() as u64,
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

fn queue_evolution_job(
    tx: &Option<tokio::sync::mpsc::Sender<EvolutionJob>>,
    job: EvolutionJob,
) -> bool {
    let Some(tx) = tx else {
        warn!(
            agent = %job.agent_name,
            source = job.source.as_str(),
            "Evolution Background-Task nicht verfuegbar, Job wird nicht eingereiht"
        );
        return false;
    };

    let source = job.source.as_str();
    let agent_name = job.agent_name.clone();
    match tx.try_send(job) {
        Ok(()) => {
            info!(
                agent = %agent_name,
                source = source,
                "Evolution Background-Job eingereiht"
            );
            true
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(job)) => {
            warn!(
                agent = %job.agent_name,
                source = job.source.as_str(),
                "Evolution Background-Channel voll, Job wird nicht blockierend verworfen"
            );
            false
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(job)) => {
            warn!(
                agent = %job.agent_name,
                source = job.source.as_str(),
                "Evolution Background-Channel geschlossen, Job wird nicht eingereiht"
            );
            false
        }
    }
}

fn write_evolution_narrative_only(
    store: &StateStore,
    agent_id: AgentId,
    agent_name: &str,
    source: EvolutionSource,
    narrative: &str,
) {
    match store.set_evolution_batch(agent_id, None, None, Some(narrative.as_bytes()), None) {
        Ok(version) => {
            info!(
                agent = agent_name,
                source = source.as_str(),
                version,
                "Evolution Narrative fallback nach redb geschrieben, EVOLUTION_VERSION = {version}"
            );
        }
        Err(error) => {
            warn!(
                agent = agent_name,
                source = source.as_str(),
                error = %error,
                "Evolution Narrative fallback redb-Write fehlgeschlagen"
            );
        }
    }
}

fn drain_evolution_results(store: &StateStore, rx: &mpsc::Receiver<EvolutionResult>) {
    while let Ok(result) = rx.try_recv() {
        match crate::evolution_task::apply_evolution_result(store, &result) {
            Ok(version) => {
                info!(
                    agent = %result.agent_name,
                    source = result.source.as_str(),
                    version,
                    voice_style = result.voice_style.is_some(),
                    behavioral_notes = result.behavioral_notes.is_some(),
                    "Evolution nach redb geschrieben, EVOLUTION_VERSION = {version}"
                );
            }
            Err(error) => {
                warn!(
                    agent = %result.agent_name,
                    source = result.source.as_str(),
                    error = %error,
                    "Evolution redb-Write fehlgeschlagen"
                );
            }
        }
    }
}

/// Vollstaendiger Teardown eines Agents (#425 Live-Despawn / Fresh-Load): Sandbox-Handle, bwrap-
/// Prozess, eBPF-Registrierung, Security-Snapshot, RuntimeOrchestrator-Eintrag und ECS-Entity.
/// Spiegelt den Reconcile-Cleanup-Pfad (`remove_agent_runtime_fragments`).
#[allow(clippy::too_many_arguments)]
fn teardown_agent_full(
    agent_id: AgentId,
    world: &mut bevy_ecs::prelude::World,
    runtime_orch: &mut RuntimeOrchestrator,
    sandbox: &SandboxEnforcer,
    sandbox_handles: &mut HashMap<AgentId, SandboxHandle>,
    ebpf_collector: &mut EbpfCollector,
    agent_processes: &mut HashMap<AgentId, sentinel_sandbox::AgentProcess>,
    security_runtime_state: &operator_api::SharedSecurityRuntimeState,
) {
    if let Some(proc_handle) = agent_processes.remove(&agent_id) {
        terminate_agent_process(proc_handle);
    }

    if let Some(handle) = sandbox_handles.remove(&agent_id) {
        if handle.cgroup_created {
            if let Some(cid) = sentinel_sandbox::cgroup_id(&handle.agent_name) {
                ebpf_collector.unregister_agent(cid);
            }
        }
        if let Err(e) = sandbox.teardown_agent(&handle) {
            warn!(agent_id = %agent_id, error = %e, "Sandbox-Teardown bei Config-Apply fehlgeschlagen");
        }
    }
    remove_security_runtime_snapshot(security_runtime_state, agent_id);
    if runtime_orch.agents().contains_key(&agent_id) {
        if let Err(e) = runtime_orch.despawn_agent(agent_id) {
            warn!(agent_id = %agent_id, error = %e, "Runtime-Despawn bei Config-Apply fehlgeschlagen");
        }
    }
    let _ = despawn_agent_from_world(world, agent_id);
}

/// Alle Agent-IDs der aktuellen ECS-Welt (fuer Fresh-Load Reset).
fn world_agent_ids(world: &mut bevy_ecs::prelude::World) -> Vec<AgentId> {
    let mut query = world.query::<&AgentIdentity>();
    query
        .iter(world)
        .map(|identity| identity.agent_id)
        .collect()
}

#[derive(Debug, Default)]
struct RestoreFence {
    active: bool,
    owner_epoch: u64,
}

impl RestoreFence {
    fn begin(&mut self) -> u64 {
        self.owner_epoch = self.owner_epoch.saturating_add(1);
        self.active = true;
        self.owner_epoch
    }

    fn end(&mut self) {
        self.active = false;
    }

    fn is_active(&self) -> bool {
        self.active
    }
}

/// #491 (TM-3): leitet das PSI-Band `(cpu_above, mem_above)` aus den avg10-Metriken ab.
/// Schwellen aus sentinel-bio (via sentinel-ecs re-exportiert), strikt `>` — ein Wert exakt auf der
/// Schwelle ergibt `false`, konsistent mit `apply_psi_stress`. Reiner Helper -> deterministisch und
/// fuer den Replay-Pfad (#491 PR-B) wiederverwendbar.
pub(crate) fn psi_band_from_metrics(cpu_avg10: f64, mem_avg10: f64) -> (bool, bool) {
    (
        cpu_avg10 > sentinel_ecs::PSI_CPU_STRESS_THRESHOLD,
        mem_avg10 > sentinel_ecs::PSI_MEM_STRESS_THRESHOLD,
    )
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ProjectionRestoreSeedReport {
    agents_seeded: u32,
    rooms_seeded: u32,
    tasks_seeded: u32,
    kpi_rows_seeded: u32,
    watermarks_seeded: u32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct RestoreCommitReport {
    projection_report: ProjectionRestoreSeedReport,
    /// #491: finaler Tick nach (optionalem) Replay — = Ziel-Tick, sonst Anchor-Tick.
    final_tick: u64,
    /// #491: finale sim_hour nach Replay (aus dem Post-Replay-Zustand).
    final_sim_hour: f32,
    /// #491: Anzahl waehrend des Replays eingespeister Eingaben (0 = reiner Snapshot-Restore).
    replayed_inputs: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum RestoreCommitFailurePoint {
    #[default]
    None,
    AfterRedb,
    AfterFs,
    AfterEcs,
    AfterProjection,
}

impl RestoreCommitFailurePoint {
    fn fail_if(self, point: Self) -> Result<()> {
        if self == point {
            Err(anyhow!("injected restore commit failure at {point:?}"))
        } else {
            Ok(())
        }
    }
}

fn now_ms_i64() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn validate_projection_restore_schema(projection_db_path: &str) -> Result<()> {
    let db = sentinel_limbo::rusqlite::Connection::open(projection_db_path)
        .with_context(|| format!("Projection DB oeffnen: {projection_db_path}"))?;
    for table in [
        "agent_live_view",
        "room_live_view",
        "kpi_1m",
        "task_kanban",
        "projection_watermarks",
    ] {
        let exists: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                sentinel_limbo::rusqlite::params![table],
                |row| row.get(0),
            )
            .with_context(|| format!("Projection-Schema fuer {table} pruefen"))?;
        if exists == 0 {
            return Err(anyhow!("Projection-Tabelle fehlt: {table}"));
        }
    }
    Ok(())
}

fn validate_fs_metadata_blobs(
    data_dir: &std::path::Path,
    fs_metadata: &sentinel_common::FsMetadataDump,
) -> Result<()> {
    let cas = sentinel_fs::cas::CasStore::open(data_dir)
        .with_context(|| format!("sentinel-fs CAS oeffnen: {}", data_dir.display()))?;
    let missing = fs_metadata
        .refcounts
        .iter()
        .filter(|(_, count)| *count > 0)
        .filter_map(|(hash, _)| {
            if cas.contains(hash) {
                None
            } else {
                Some(sentinel_fs::cas::hex_encode(hash))
            }
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "Restore-Snapshot referenziert fehlende CAS-Blobs: {}",
            missing.join(",")
        ))
    }
}

fn seed_projection_from_world_snapshot(
    projection_db_path: &str,
    snapshot: &sentinel_common::WorldSnapshot,
    max_event_id: i64,
    now_ms: i64,
) -> Result<ProjectionRestoreSeedReport> {
    let mut db = sentinel_limbo::rusqlite::Connection::open(projection_db_path)
        .with_context(|| format!("Projection DB oeffnen: {projection_db_path}"))?;
    db.busy_timeout(Duration::from_secs(5))?;
    let tx = db.transaction().context("Projection-Restore Txn starten")?;
    tx.execute_batch(
        "DELETE FROM agent_live_view;
         DELETE FROM room_live_view;
         DELETE FROM kpi_1m;
         DELETE FROM task_kanban;
         DELETE FROM projection_watermarks;",
    )
    .context("Projection-Tabellen fuer Restore leeren")?;

    let mut report = ProjectionRestoreSeedReport::default();
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

        let mood_str = mood
            .map(|m| format!("{:?}", m.dominant_emotion))
            .unwrap_or_else(|| "Neutral".to_string());
        tx.execute(
            "INSERT OR REPLACE INTO agent_live_view
             (agent_id, name, role, shift_set, status, current_room, in_transit,
              hunger, energy, stress, bladder, social_need, caffeine_mg, mood,
              last_event_id, updated_at)
             VALUES (?1,?2,?3,?4,'active',?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            sentinel_limbo::rusqlite::params![
                *id as i64,
                &identity.name,
                &identity.role,
                shift.map(|s| s.shift_set as i64).unwrap_or(1),
                pos.map(|p| p.room_id.as_str()).unwrap_or("empfang"),
                pos.map(|p| p.in_transit as i64).unwrap_or(0),
                bio.map(|b| b.hunger as f64).unwrap_or(20.0),
                bio.map(|b| b.energy as f64).unwrap_or(80.0),
                bio.map(|b| b.stress as f64).unwrap_or(15.0),
                bio.map(|b| b.bladder as f64).unwrap_or(10.0),
                bio.map(|b| b.social_need as f64).unwrap_or(50.0),
                bio.map(|b| b.caffeine_mg as f64).unwrap_or(0.0),
                mood_str,
                max_event_id,
                now_ms,
            ],
        )
        .with_context(|| format!("agent_live_view seed fuer AGENT-{id:02}"))?;
        report.agents_seeded += 1;
    }

    // RoomPhysicsState ist nicht Teil des WorldSnapshot. Restore rekonstruiert
    // room_live_view deshalb bewusst nur aus Occupancy der Agent-Positionen.
    let mut room_occupancy: HashMap<String, u32> = HashMap::new();
    for (_, pos) in &snapshot.ecs.positions {
        if !pos.in_transit {
            *room_occupancy.entry(pos.room_id.clone()).or_default() += 1;
        }
    }
    for (room_id, count) in &room_occupancy {
        tx.execute(
            "INSERT OR REPLACE INTO room_live_view
             (room_id, occupant_count, transit_count, last_event_id, updated_at)
             VALUES (?1, ?2, 0, ?3, ?4)",
            sentinel_limbo::rusqlite::params![room_id, *count as i64, max_event_id, now_ms],
        )
        .with_context(|| format!("room_live_view seed fuer {room_id}"))?;
        report.rooms_seeded += 1;
    }

    for task in &snapshot.ecs.task_states {
        tx.execute(
            "INSERT OR REPLACE INTO task_kanban
             (task_id, title, assigned_to, assigned_by, parent_task, status, result, last_event_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            sentinel_limbo::rusqlite::params![
                task.task_id.0 as i64,
                &task.title,
                task.assigned_to.0 as i64,
                task.assigned_by.map(|id| id.0 as i64),
                task.parent_task.map(|id| id.0 as i64),
                task.status.as_str(),
                task.result.as_deref(),
                max_event_id,
                now_ms,
            ],
        )
        .with_context(|| format!("task_kanban seed fuer TASK-{}", task.task_id.0))?;
        report.tasks_seeded += 1;
    }

    let bucket_start = (snapshot.tick / 60) as i64 * 60;
    tx.execute(
        "INSERT OR REPLACE INTO kpi_1m
         (bucket_start, active_agents, total_actions, total_transits, chaos_events,
          tick_count, shift_changes, nightrun_events, last_event_id, updated_at)
         VALUES (?1, ?2, 0, 0, 0, ?3, 0, 0, ?4, ?5)",
        sentinel_limbo::rusqlite::params![
            bucket_start,
            snapshot.ecs.identities.len() as i64,
            snapshot.tick as i64,
            max_event_id,
            now_ms,
        ],
    )
    .context("kpi_1m Restore-Bucket seeden")?;
    report.kpi_rows_seeded = 1;

    let mut watermark_names = snapshot
        .projection_offsets
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    if watermark_names.is_empty() {
        watermark_names.push("sentinel-projection");
    }
    for name in watermark_names {
        tx.execute(
            "INSERT OR REPLACE INTO projection_watermarks
             (projection_name, last_event_id, updated_at)
             VALUES (?1, ?2, ?3)",
            sentinel_limbo::rusqlite::params![name, max_event_id, now_ms],
        )
        .with_context(|| format!("projection_watermarks seed fuer {name}"))?;
        report.watermarks_seeded += 1;
    }

    tx.commit().context("Projection-Restore Txn committen")?;
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn teardown_runtime_for_world_restore(
    runtime_orch: &mut RuntimeOrchestrator,
    sandbox: &SandboxEnforcer,
    sandbox_handles: &mut HashMap<AgentId, SandboxHandle>,
    ebpf_collector: &mut EbpfCollector,
    agent_processes: &mut HashMap<AgentId, sentinel_sandbox::AgentProcess>,
    security_runtime_state: &operator_api::SharedSecurityRuntimeState,
) -> usize {
    let mut ids = agent_processes.keys().copied().collect::<HashSet<_>>();
    ids.extend(sandbox_handles.keys().copied());
    ids.extend(runtime_orch.agents().keys().copied());
    if let Ok(state) = security_runtime_state.read() {
        ids.extend(state.keys().copied().map(AgentId));
    }

    for agent_id in &ids {
        if let Some(proc_handle) = agent_processes.remove(agent_id) {
            terminate_agent_process(proc_handle);
        }

        if let Some(handle) = sandbox_handles.remove(agent_id) {
            if handle.cgroup_created {
                if let Some(cid) = sentinel_sandbox::cgroup_id(&handle.agent_name) {
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
        remove_security_runtime_snapshot(security_runtime_state, *agent_id);
        if runtime_orch.agents().contains_key(agent_id) {
            if let Err(e) = runtime_orch.despawn_agent(*agent_id) {
                warn!(agent_id = %agent_id, error = %e, "Runtime-Despawn bei Restore fehlgeschlagen");
            }
        }
    }

    ids.len()
}

/// #491 (TM-3): Sicherungs-Obergrenze fuer die Replay-Spanne in Ticks. Das Feature zielt auf die
/// erste Stunde (~3600 Ticks @1 Hz); ein Vielfaches davon faengt versehentlich riesige Spannen ab
/// (Fallback nearest, exact:false) statt den Tick-Loop minutenlang zu blockieren.
const REPLAY_TICK_CAP: u64 = 14_400;

/// #491: Replay-Anteil eines Restore (`None` im commit = reiner Snapshot-Punkt-Restore).
#[derive(Debug, Clone)]
struct RestoreReplayPlan {
    events: Vec<sentinel_common::DomainEvent>,
    anchor_tick: u64,
    target_tick: u64,
    target_event_id: i64,
}

/// #491: waehlt den Anchor-Snapshot, der STRIKT VOR dem Ziel liegt (Liste ist `tick DESC`, liefert
/// also den juengsten gueltigen). Beide Bedingungen noetig: `tick < target_tick` stellt sicher, dass
/// das Replay `(anchor_tick, target_tick]` mindestens den Ziel-Tick voll ausfuehrt; `last_event_id
/// <= target_event_id` haelt die Range gueltig. NUR `last_event_id` zu pruefen reicht NICHT: in
/// Ruhephasen (keine Events) teilen sich mehrere Snapshots dieselbe `last_event_id`, und ein
/// Snapshot mit Tick NACH dem Ziel wuerde sonst als Anchor gewaehlt -> leeres Replay -> Anchor-
/// Zustand statt Ziel-Zustand (VM-Befund #491).
fn select_anchor_snapshot(
    snapshots: &[sentinel_common::SnapshotMeta],
    target_tick: u64,
    target_event_id: i64,
) -> Option<&sentinel_common::SnapshotMeta> {
    // tick <= target_tick: ein Snapshot GENAU am Ziel-Tick ergibt ein leeres Replay-Fenster
    // `(target, target]` = exakter Ziel-Zustand (#529: der erzwungene Post-Shift-Anker am Shift-Tick
    // bedient damit auch ein Ziel == Shift-Tick, ohne ueber die Schichtgrenze zu replayen). Ein
    // Snapshot mit tick > target bleibt ausgeschlossen (#528-Bug: leeres Replay -> Anchor-Zustand
    // statt Ziel, in Ruhephasen mit geteilter last_event_id).
    snapshots
        .iter()
        .find(|s| s.tick <= target_tick && s.last_event_id <= target_event_id)
}

/// #491: aufgeloestes Restore-Ziel (Anchor + optionaler Replay-Cursor).
#[derive(Debug, Clone)]
struct RestoreResolution {
    anchor_snapshot_id: String,
    /// `Some` => Replay bis zu diesem Event; `None` => reiner Snapshot-Punkt-Restore.
    target_event_id: Option<i64>,
    target_tick: Option<u64>,
    exact: bool,
    granularity: &'static str,
}

/// #491: loest ein `OperatorRestoreCommand` in Anchor-Snapshot + (optionalen) Ziel-Cursor auf.
/// Reine Lese-Operationen auf dem EventStore + der Snapshot-Liste (nach `tick DESC`).
fn resolve_restore_target(
    cmd: &sentinel_common::OperatorRestoreCommand,
    event_store: &EventStore,
    snapshots: &[sentinel_common::SnapshotMeta],
) -> Result<RestoreResolution> {
    if let Some(sid) = &cmd.snapshot_id {
        return Ok(RestoreResolution {
            anchor_snapshot_id: sid.clone(),
            target_event_id: None,
            target_tick: None,
            exact: true,
            granularity: "snapshot",
        });
    }

    // Ziel-Event + Ziel-Tick bestimmen.
    let (target_event_id, target_tick) = if let Some(eid) = cmd.target_event_id {
        let tick = event_store
            .get_event_tick(eid)?
            .ok_or_else(|| anyhow!("target_event_id {eid} unbekannt"))?;
        (eid, tick)
    } else if let Some(t) = cmd.target_tick {
        match event_store.max_event_id_at_tick(t)? {
            Some(eid) => (eid, t),
            None => {
                // Kein Event <= t -> Ziel liegt vor dem ersten Event. Aeltester Snapshot, exact:false.
                let oldest = snapshots
                    .last()
                    .ok_or_else(|| anyhow!("kein Snapshot vorhanden"))?;
                return Ok(RestoreResolution {
                    anchor_snapshot_id: oldest.id.clone(),
                    target_event_id: None,
                    target_tick: None,
                    exact: false,
                    granularity: "snapshot",
                });
            }
        }
    } else {
        return Err(anyhow!(
            "Restore-Command ohne Ziel (validate uebersprungen?)"
        ));
    };

    let anchor = select_anchor_snapshot(snapshots, target_tick, target_event_id).ok_or_else(|| {
        anyhow!("kein Anchor-Snapshot vor Ziel (tick<{target_tick}, last_event_id<={target_event_id})")
    })?;

    // exact, wenn das Ziel-Event das letzte seines Ticks ist (sonst tick-granular).
    let exact = event_store.max_event_id_at_tick(target_tick)? == Some(target_event_id);

    Ok(RestoreResolution {
        anchor_snapshot_id: anchor.id.clone(),
        target_event_id: Some(target_event_id),
        target_tick: Some(target_tick),
        exact,
        granularity: if exact { "event" } else { "tick" },
    })
}

#[allow(clippy::too_many_arguments)]
fn commit_world_restore_stores(
    anchor_snapshot_id: &str,
    snapshot: &sentinel_common::WorldSnapshot,
    owner_epoch: u64,
    world: &mut bevy_ecs::prelude::World,
    schedule: &mut bevy_ecs::schedule::Schedule,
    replay_plan: Option<RestoreReplayPlan>,
    exact: bool,
    event_store: &Arc<EventStore>,
    state_store: &Arc<StateStore>,
    fs_layer: Option<&sentinel_fs::layer::LayerManager>,
    projection_db_path: &str,
    failure_point: RestoreCommitFailurePoint,
) -> Result<RestoreCommitReport> {
    state_store
        .restore_all_tables(&snapshot.redb)
        .context("redb Restore fehlgeschlagen")?;
    failure_point.fail_if(RestoreCommitFailurePoint::AfterRedb)?;

    if let Some(fs_metadata) = &snapshot.fs_metadata {
        let layer = fs_layer.expect("validated above");
        layer
            .meta()
            .restore_all_tables(fs_metadata)
            .context("sentinel-fs Restore fehlgeschlagen")?;
        failure_point.fail_if(RestoreCommitFailurePoint::AfterFs)?;
    }

    // ECS auf den ANCHOR-Zustand setzen.
    sentinel_ecs::restore_ecs_state(world, &snapshot.ecs);
    failure_point.fail_if(RestoreCommitFailurePoint::AfterEcs)?;

    // #491 (TM-3): optionales bounded Replay `(anchor, target]` auf der Live-World. Danach steht die
    // World am Ziel-Zustand; die Projection wird aus DIESEM Zustand geseedet (nicht aus dem Anchor).
    let (final_tick, final_sim_hour, replayed_inputs, target_event_id) =
        if let Some(plan) = &replay_plan {
            let report = crate::replay::run_bounded_replay(
                world,
                schedule,
                &plan.events,
                plan.anchor_tick,
                plan.target_tick,
            )
            .context("Bounded Replay fehlgeschlagen")?;
            let sim_hour = world
                .get_resource::<sentinel_ecs::SimulationTime>()
                .map(|t| t.sim_hour)
                .unwrap_or(snapshot.sim_hour);
            info!(
                anchor_tick = plan.anchor_tick,
                target_tick = plan.target_tick,
                ticks_replayed = report.ticks_replayed,
                inputs = report.inputs_injected,
                "Bounded Replay abgeschlossen"
            );
            (
                plan.target_tick,
                sim_hour,
                report.inputs_injected as u64,
                Some(plan.target_event_id),
            )
        } else {
            (snapshot.tick, snapshot.sim_hour, 0u64, None)
        };

    let max_event_id = event_store
        .get_latest_event_id()
        .unwrap_or(snapshot.last_event_id);
    // Projection-Seed: bei Replay aus dem Post-Replay-Zustand (Anchor-Snapshot geklont, nur ecs/tick
    // /sim_hour ersetzt — redb wird vom Seed nicht gelesen). Ohne Replay: direkt der Anchor.
    let seed_snapshot;
    let seed_ref: &sentinel_common::WorldSnapshot = if replay_plan.is_some() {
        let mut view = snapshot.clone();
        view.ecs = sentinel_ecs::snapshot_ecs_state(world);
        view.tick = final_tick;
        view.sim_hour = final_sim_hour;
        seed_snapshot = view;
        &seed_snapshot
    } else {
        snapshot
    };
    let projection_report = seed_projection_from_world_snapshot(
        projection_db_path,
        seed_ref,
        max_event_id,
        now_ms_i64(),
    )?;
    for (name, _) in &snapshot.projection_offsets {
        event_store
            .force_reset_offset(name, max_event_id)
            .with_context(|| format!("Projection-Offset fuer {name} setzen"))?;
    }
    failure_point.fail_if(RestoreCommitFailurePoint::AfterProjection)?;

    // #493 (TM-5): the jump back discards the "future" `(anchor, head]`. The anchor cursor is the
    // post-replay target (events up to the target were re-applied and stay alive) or, without replay,
    // the anchor snapshot's `last_event_id`. Bump the persistent restore generation and record the
    // dead id-interval so every read path excludes it and the pruner removes it in the retention
    // window. `push_dead_range` is a no-op when nothing was discarded (anchor == head).
    let anchor_event_id = target_event_id.unwrap_or(snapshot.last_event_id);
    event_store
        .increment_restore_generation()
        .context("Restore-Generation erhoehen fehlgeschlagen")?;
    event_store
        .push_dead_range(anchor_event_id, max_event_id)
        .context("Dead-Branch markieren fehlgeschlagen")?;

    let restore_event = sentinel_common::DomainEvent {
        event_id: uuid::Uuid::new_v4().to_string(),
        event_type: "snapshot_restored".to_string(),
        aggregate_id: "system".to_string(),
        payload: serde_json::json!({
            "anchor_snapshot_id": anchor_snapshot_id,
            "restored_tick": final_tick,
            "restored_sim_hour": final_sim_hour,
            "target_event_id": target_event_id,
            "exact": exact,
            "replayed_inputs": replayed_inputs,
            "agents_count": seed_ref.ecs.identities.len(),
            "owner_epoch": owner_epoch,
        })
        .to_string(),
        correlation_id: anchor_snapshot_id.to_string(),
        causation_id: None,
        operation_id: uuid::Uuid::new_v4().to_string(),
        tick: final_tick,
        timestamp_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        schema_version: 1,
        compensation_type: "none".to_string(),
    };
    event_store
        .append_with_outbox(&restore_event, "sentinel.events")
        .context("SnapshotRestored Event schreiben fehlgeschlagen")?;

    Ok(RestoreCommitReport {
        projection_report,
        final_tick,
        final_sim_hour,
        replayed_inputs,
    })
}

#[allow(clippy::too_many_arguments)]
fn rollback_world_restore_stores(
    pre_snapshot_id: &str,
    world: &mut bevy_ecs::prelude::World,
    event_store: &Arc<EventStore>,
    state_store: &Arc<StateStore>,
    fs_layer: Option<&sentinel_fs::layer::LayerManager>,
    data_dir: &std::path::Path,
    projection_db_path: &str,
) -> Result<()> {
    let bytes = event_store
        .load_world_snapshot(pre_snapshot_id)?
        .ok_or_else(|| anyhow!("Pre-Restore Snapshot nicht gefunden: {pre_snapshot_id}"))?;
    let snapshot = sentinel_common::decode_world_snapshot(&bytes)
        .with_context(|| format!("Pre-Restore Snapshot dekodieren: {pre_snapshot_id}"))?;
    if let Some(fs_metadata) = &snapshot.fs_metadata {
        validate_fs_metadata_blobs(data_dir, fs_metadata)?;
    }
    state_store
        .restore_all_tables(&snapshot.redb)
        .context("Rollback redb Restore fehlgeschlagen")?;
    if let Some(fs_metadata) = &snapshot.fs_metadata {
        let layer = fs_layer
            .ok_or_else(|| anyhow!("Rollback braucht sentinel-fs Layer, aber keiner ist aktiv"))?;
        layer
            .meta()
            .restore_all_tables(fs_metadata)
            .context("Rollback sentinel-fs Restore fehlgeschlagen")?;
    }
    sentinel_ecs::restore_ecs_state(world, &snapshot.ecs);
    let max_event_id = event_store
        .get_latest_event_id()
        .unwrap_or(snapshot.last_event_id);
    seed_projection_from_world_snapshot(projection_db_path, &snapshot, max_event_id, now_ms_i64())
        .context("Rollback Projection-Seeding fehlgeschlagen")?;
    for (name, _) in &snapshot.projection_offsets {
        event_store
            .force_reset_offset(name, max_event_id)
            .with_context(|| format!("Rollback Projection-Offset fuer {name} setzen"))?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn rollback_world_restore_after_commit_failure(
    commit_error: anyhow::Error,
    pre_snapshot_id: &str,
    restore_fence: &mut RestoreFence,
    world: &mut bevy_ecs::prelude::World,
    event_store: &Arc<EventStore>,
    state_store: &Arc<StateStore>,
    fs_layer: Option<&sentinel_fs::layer::LayerManager>,
    data_dir: &std::path::Path,
    projection_db_path: &str,
) -> Result<()> {
    match rollback_world_restore_stores(
        pre_snapshot_id,
        world,
        event_store,
        state_store,
        fs_layer,
        data_dir,
        projection_db_path,
    ) {
        Ok(()) => {
            restore_fence.end();
            Err(commit_error.context("Restore rollback succeeded"))
        }
        Err(rollback_error) => {
            error!(
                error = %rollback_error,
                "Restore-Rollback fehlgeschlagen — Fence bleibt aktiv"
            );
            Err(commit_error.context(format!(
                "critical restore rollback failure: {rollback_error}"
            )))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_world_restore_transfer(
    restore_cmd: &sentinel_common::OperatorRestoreCommand,
    restore_fence: &mut RestoreFence,
    snapshot_manager: &mut crate::snapshot::SnapshotManager,
    world: &mut bevy_ecs::prelude::World,
    schedule: &mut bevy_ecs::schedule::Schedule,
    runtime_orch: &mut RuntimeOrchestrator,
    sandbox: &SandboxEnforcer,
    sandbox_handles: &mut HashMap<AgentId, SandboxHandle>,
    ebpf_collector: &mut EbpfCollector,
    agent_processes: &mut HashMap<AgentId, sentinel_sandbox::AgentProcess>,
    agent_command: &[String],
    security_runtime_state: &operator_api::SharedSecurityRuntimeState,
    event_store: &Arc<EventStore>,
    state_store: &Arc<StateStore>,
    fs_layer: Option<&sentinel_fs::layer::LayerManager>,
    fs_mount: Option<&str>,
    data_dir: &std::path::Path,
    projection_db_path: &str,
    all_agents: &[AgentConfig],
    tick_count: &mut u64,
    sim_hour: &mut f32,
    current_shift: &mut u8,
) -> Result<()> {
    let started = Instant::now();
    let pre_snapshot_id = snapshot_manager
        .create_and_store(
            world,
            state_store,
            event_store,
            data_dir,
            fs_layer,
            fs_mount,
            *tick_count,
            *sim_hour,
        )
        .context("Pre-Restore Snapshot fatal fehlgeschlagen")?;
    info!(snapshot_id = %pre_snapshot_id, "Pre-Restore Snapshot erstellt (Rollback-Punkt)");

    // #491 (TM-3): Ziel aufloesen (Anchor-Snapshot + optionaler Replay-Cursor).
    let snapshots = event_store
        .list_world_snapshots()
        .context("Snapshot-Liste fuer Restore-Resolution")?;
    let resolution = resolve_restore_target(restore_cmd, event_store.as_ref(), &snapshots)?;

    let bytes = event_store
        .load_world_snapshot(&resolution.anchor_snapshot_id)
        .with_context(|| format!("Snapshot laden: {}", resolution.anchor_snapshot_id))?
        .ok_or_else(|| anyhow!("Snapshot nicht gefunden: {}", resolution.anchor_snapshot_id))?;
    let snapshot = sentinel_common::decode_world_snapshot(&bytes)
        .with_context(|| format!("Snapshot dekodieren: {}", resolution.anchor_snapshot_id))?;

    if let Some(fs_metadata) = &snapshot.fs_metadata {
        if fs_layer.is_none() {
            return Err(anyhow!(
                "sentinel-fs Restore angefordert, aber Layer nicht initialisiert"
            ));
        }
        validate_fs_metadata_blobs(data_dir, fs_metadata)?;
    }
    validate_projection_restore_schema(projection_db_path)?;

    // #491: Replay-Plan bilden — Legacy-Gate (Anchor muss v3 sein) + Sanity-Cap + Branch-Check
    // (keine `snapshot_restored`-Events in der Range). Jeder Fall, der kein exaktes Replay erlaubt,
    // faellt auf den Anchor als reinen Snapshot-Punkt zurueck (exact:false, ehrlich reportet).
    let mut exact = resolution.exact;
    let mut replay_plan: Option<RestoreReplayPlan> = None;
    if let (Some(target_event_id), Some(target_tick)) =
        (resolution.target_event_id, resolution.target_tick)
    {
        if snapshot.schema_version < 3 {
            warn!(anchor = %resolution.anchor_snapshot_id, "Anchor pre-v3 -> kein Replay (nearest, exact:false)");
            exact = false;
        } else if target_tick.saturating_sub(snapshot.tick) > REPLAY_TICK_CAP {
            warn!(
                span = target_tick.saturating_sub(snapshot.tick),
                cap = REPLAY_TICK_CAP,
                "Replay-Spanne ueber Cap -> nearest, exact:false"
            );
            exact = false;
        } else {
            let range = event_store
                .get_events_range(snapshot.last_event_id, target_event_id)
                .context("Replay-Range laden")?;
            if range.iter().any(|e| e.event_type == "snapshot_restored") {
                warn!("Replay-Range enthaelt snapshot_restored (nicht-lineare History) -> nearest, exact:false");
                exact = false;
            } else {
                replay_plan = Some(RestoreReplayPlan {
                    events: range,
                    anchor_tick: snapshot.tick,
                    target_tick,
                    target_event_id,
                });
            }
        }
    }

    let owner_epoch = restore_fence.begin();
    let target_cursor = replay_plan
        .as_ref()
        .map(|p| sentinel_common::StateTransferCursor {
            tick: p.target_tick,
            last_event_id: p.target_event_id,
        });
    let transfer = sentinel_common::FencedStateTransfer {
        scope: sentinel_common::StateTransferScope::World,
        owner_epoch,
        source_cursor: sentinel_common::StateTransferCursor {
            tick: snapshot.tick,
            last_event_id: snapshot.last_event_id,
        },
        snapshot_id: resolution.anchor_snapshot_id.clone(),
        cas_manifest: sentinel_common::CasManifest {
            blob_hashes: snapshot
                .fs_metadata
                .as_ref()
                .map(|metadata| {
                    metadata
                        .refcounts
                        .iter()
                        .filter(|(_, count)| *count > 0)
                        .map(|(hash, _)| *hash)
                        .collect()
                })
                .unwrap_or_default(),
        },
        projection_delta: sentinel_common::ProjectionDelta {
            last_event_id: snapshot.last_event_id,
        },
        route_update: sentinel_common::RouteUpdate::default(),
        target_cursor,
    };
    info!(
        owner_epoch = transfer.owner_epoch,
        anchor_snapshot_id = %transfer.snapshot_id,
        replay = replay_plan.is_some(),
        exact,
        granularity = resolution.granularity,
        "Restore-Fence aktiviert"
    );

    let anchor_snapshot_id = resolution.anchor_snapshot_id.clone();
    let commit_result = commit_world_restore_stores(
        &anchor_snapshot_id,
        &snapshot,
        owner_epoch,
        world,
        schedule,
        replay_plan,
        exact,
        event_store,
        state_store,
        fs_layer,
        projection_db_path,
        RestoreCommitFailurePoint::None,
    );

    // #491: finaler Zustand = Ziel-Tick (nach Replay) bzw. Anchor-Tick (ohne Replay).
    let final_tick;
    let final_sim_hour;
    match commit_result {
        Err(commit_error) => {
            error!(
                error = %commit_error,
                pre_snapshot_id = %pre_snapshot_id,
                "Restore-Commit fehlgeschlagen — Rollback auf Pre-Snapshot gestartet"
            );
            return rollback_world_restore_after_commit_failure(
                commit_error,
                &pre_snapshot_id,
                restore_fence,
                world,
                event_store,
                state_store,
                fs_layer,
                data_dir,
                projection_db_path,
            );
        }
        Ok(report) => {
            info!(
                agents_seeded = report.projection_report.agents_seeded,
                rooms_seeded = report.projection_report.rooms_seeded,
                tasks_seeded = report.projection_report.tasks_seeded,
                kpi_rows_seeded = report.projection_report.kpi_rows_seeded,
                watermarks_seeded = report.projection_report.watermarks_seeded,
                replayed_inputs = report.replayed_inputs,
                "Projection Snapshot-Seeding abgeschlossen"
            );
            final_tick = report.final_tick;
            final_sim_hour = report.final_sim_hour;
        }
    }

    let terminated = teardown_runtime_for_world_restore(
        runtime_orch,
        sandbox,
        sandbox_handles,
        ebpf_collector,
        agent_processes,
        security_runtime_state,
    );

    let mut respawned = 0u32;
    let mut respawn_errors = Vec::new();
    for (id, _) in &snapshot.ecs.identities {
        let Some(agent_cfg) = all_agents.iter().find(|cfg| cfg.identity.id == *id) else {
            respawn_errors.push(format!("Agent-Konfiguration fehlt fuer AGENT-{id:02}"));
            continue;
        };
        if spawn_agent_runtime_stack(
            runtime_orch,
            agent_cfg,
            sandbox,
            sandbox_handles,
            ebpf_collector,
            agent_processes,
            agent_command,
            security_runtime_state,
            fs_mount,
        ) {
            respawned += 1;
        } else {
            respawn_errors.push(format!("Runtime-Respawn fehlgeschlagen fuer AGENT-{id:02}"));
        }
    }

    *tick_count = final_tick;
    *sim_hour = final_sim_hour;
    *current_shift = detect_shift_from_sim_hour(final_sim_hour);
    if let Some(mut sim_time) = world.get_resource_mut::<sentinel_ecs::SimulationTime>() {
        sim_time.tick = sentinel_common::Tick(final_tick);
        sim_time.tick_count = final_tick;
        sim_time.sim_hour = final_sim_hour;
    }

    if let Some(receiver) = world.get_resource::<sentinel_ecs::ActionReceiver>() {
        if let Ok(rx) = receiver.0.lock() {
            let mut drained = 0u32;
            while rx.try_recv().is_ok() {
                drained += 1;
            }
            if drained > 0 {
                info!(
                    drained,
                    "Action-Channel geleert (Zukunfts-Events verworfen)"
                );
            }
        }
    }

    restore_fence.end();
    if respawn_errors.is_empty() {
        info!(
            anchor_snapshot_id = %anchor_snapshot_id,
            tick = final_tick,
            sim_hour = final_sim_hour,
            agents = snapshot.ecs.identities.len(),
            terminated,
            respawned,
            elapsed_ms = started.elapsed().as_millis(),
            "Hot-Swap Restore abgeschlossen"
        );
        Ok(())
    } else {
        Err(anyhow!(
            "Restore committed, aber PostCommit-Respawn ist degraded: {}",
            respawn_errors.join("; ")
        ))
    }
}

/// True, wenn der Agent gerade unter aktiver Control-Plane-Heilung steht (Runtime-Health „stale").
/// TOGAF §6 L3: solche Agents NICHT despawnen — Despawn wird deferred.
fn agent_under_active_healing(
    runtime_health: &crate::runtime_health::SharedRuntimeHealthState,
    agent_id: AgentId,
) -> bool {
    runtime_health
        .read()
        .map(|snapshot| {
            snapshot.agents.iter().any(|a| {
                a.agent_id == agent_id.0 && a.last_repair_status.as_deref() == Some("stale")
            })
        })
        .unwrap_or(false)
}

/// Aktualisiert Name/Rolle eines live-aktualisierten Agents in der Read-Projection (Dashboard).
/// Live-Component-Updates emittieren kein Event → die Projection wird hier gezielt nachgezogen.
fn update_agent_projection_identity(projection_db_path: &str, cfg: &AgentConfig) {
    if projection_db_path.is_empty() {
        return;
    }
    if let Ok(db) = sentinel_limbo::rusqlite::Connection::open(projection_db_path) {
        let _ = db.execute(
            "UPDATE agent_live_view SET name = ?2, role = ?3 WHERE agent_id = ?1",
            sentinel_limbo::rusqlite::params![
                cfg.identity.id as i64,
                cfg.identity.name,
                cfg.identity.role
            ],
        );
    }
}

/// ECS Tick-Loop auf dediziertem Thread.
///
/// Verwaltet den RuntimeOrchestrator (Lifecycle-Events, Shift-Wechsel, Snapshots)
/// UND die ECS World (Entity-Spawning, Simulation). Laeuft bis `shutdown` gesetzt wird.
/// Cluster 12 ProvisionNode worker (#495, G3): drains `ProvisionNode` commands and
/// bootstraps bare targets into cluster nodes. Runs **off** the ECS tick (pure infra
/// I/O — it never touches the world), one op at a time on its own thread, so a slow
/// SSH bootstrap never stalls the 1 Hz tick. Spawned only on the seed.
fn run_provision_worker(
    provision_rx: mpsc::Receiver<sentinel_common::OperatorProvisionCommand>,
    cluster_id: uuid::Uuid,
    pending_targets: Vec<sentinel_common::cluster::PendingBareNode>,
    binary_path: std::path::PathBuf,
    bootstrap_user: String,
    event_store: Arc<EventStore>,
) {
    use crate::provision_exec::{
        execute_provision_node, sanitize_alias, sha256_file, ProvisionPlan, ProvisionTiming,
        SshProvisionTransport,
    };
    use sentinel_common::provision::{validate_pending_target, ProvisionOp, ProvisionOpState};

    let now_ms = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    };

    // sha256 of the binary the seed pushes (the determinism-profile invariant, #494);
    // computed once. If it is unhashable, provisioning is disabled but we still drain
    // the channel so the operator endpoint gets a clean rejection rather than backing up.
    let binary_sha256 = match sha256_file(&binary_path) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, path = %binary_path.display(),
                "ProvisionNode: Binary nicht hashbar — Provisioning deaktiviert");
            while provision_rx.recv().is_ok() {
                warn!("ProvisionNode-Request ignoriert: Provision-Binary nicht verfuegbar");
            }
            return;
        }
    };

    // Idempotency (AC-S2): a completed op for an idempotency_key makes a re-run a no-op.
    let mut seen: std::collections::HashMap<String, ProvisionOpState> =
        std::collections::HashMap::new();

    while let Ok(cmd) = provision_rx.recv() {
        if matches!(
            seen.get(&cmd.idempotency_key),
            Some(ProvisionOpState::Completed)
        ) {
            info!(idempotency_key = %cmd.idempotency_key,
                "ProvisionNode: bereits abgeschlossen, no-op (AC-S2)");
            continue;
        }
        let now_unix_s = (now_ms() / 1000) as i64;
        // V14: host/identity come from the allowlist, never from the request.
        let pending =
            match validate_pending_target(&pending_targets, &cmd.pending_target_id, now_unix_s) {
                Ok(p) => p.clone(),
                Err(e) => {
                    warn!(error = %e, "ProvisionNode abgelehnt (Allowlist V14)");
                    continue;
                }
            };
        let alias = cmd
            .requested_alias
            .as_deref()
            .and_then(sanitize_alias)
            .or_else(|| sanitize_alias(&cmd.pending_target_id))
            .unwrap_or_else(|| "node".to_string());
        let node_id = sentinel_common::NodeId::new();
        let mut op = ProvisionOp::new(
            uuid::Uuid::new_v4(),
            cmd.pending_target_id.clone(),
            alias.clone(),
            cmd.idempotency_key.clone(),
            now_ms(),
        );
        let plan = ProvisionPlan {
            assigned_node_id: node_id,
            alias: alias.clone(),
            cluster_id,
            seed_endpoint: None, // LAN multicast discovery (Track-A default)
            binary_local_path: binary_path.clone(),
            binary_sha256: binary_sha256.clone(),
        };
        let work_dir = std::env::temp_dir();
        let transport = match SshProvisionTransport::new(&pending, &bootstrap_user, &work_dir, None)
        {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "ProvisionNode: Transport-Setup fehlgeschlagen");
                op.fail(format!("transport: {e}"), now_ms());
                seen.insert(cmd.idempotency_key, op.state);
                continue;
            }
        };
        info!(target = %cmd.pending_target_id, %alias, %node_id, "ProvisionNode: Bootstrap gestartet");
        match execute_provision_node(
            &mut op,
            &pending,
            &plan,
            &transport,
            ProvisionTiming::default(),
            &now_ms,
        ) {
            Ok(duration_ms) => {
                let payload = DomainEventPayload::NodeProvisioned {
                    node_id: node_id.to_string(),
                    alias: alias.clone(),
                    pending_target_id: cmd.pending_target_id.clone(),
                    target_ip: pending.target_ip.clone(),
                    duration_ms,
                };
                let event = DomainEvent::new(
                    payload.event_type_str(),
                    "cluster",
                    &payload.to_json(),
                    &format!("provision-{}", op.op_id),
                    0,
                );
                if let Err(e) = event_store.append_event(&event) {
                    warn!(error = %e, "NodeProvisioned-Event konnte nicht persistiert werden");
                }
                info!(%node_id, %alias, duration_ms, "ProvisionNode: Knoten provisioniert");
            }
            Err(e) => {
                warn!(error = %e, target = %cmd.pending_target_id,
                    "ProvisionNode fehlgeschlagen (Target quarantined, AC-B6)");
            }
        }
        seen.insert(cmd.idempotency_key, op.state);
    }
}

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
    mut all_agents: Vec<AgentConfig>,
    initial_shift: u8,
    tick_rate: Duration,
    time_scale: f32,
    phase_timing_enabled: bool,
    shutdown: Arc<AtomicBool>,
    mut controlplane: ControlplaneKernel,
    mut runtime_orch: RuntimeOrchestrator,
    sandbox: SandboxEnforcer,
    mut ebpf_collector: EbpfCollector,
    ebpf_tx: tokio::sync::mpsc::Sender<MetricsSnapshot>,
    mut episode_producer: EpisodeProducer,
    nightrun_rx: mpsc::Receiver<sentinel_common::OperatorNightrunCommand>,
    evolution_job_tx: Option<tokio::sync::mpsc::Sender<EvolutionJob>>,
    evolution_result_rx: Option<mpsc::Receiver<EvolutionResult>>,
    snapshot_rx: mpsc::Receiver<sentinel_common::OperatorSnapshotCommand>,
    restore_rx: mpsc::Receiver<sentinel_common::OperatorRestoreCommand>,
    config_apply_rx: mpsc::Receiver<sentinel_common::OperatorConfigApplyCommand>,
    migrate_rx: mpsc::Receiver<sentinel_common::OperatorMigrateCommand>,
    config_dir: std::path::PathBuf,
    config_apply_max_agents: usize,
    config_apply_validation: sentinel_common::agent_config::AgentConfigValidation,
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
    llm_circuit_open: Arc<AtomicBool>,
    llm_activity_ticks: Arc<Mutex<HashMap<AgentId, u64>>>,
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

    // Per-Phase-Timing (#381): Boundary-Marker + PhaseTimings-Resource (opt-in).
    // Die Resource ueberlebt restore_ecs_state (das despawnt nur Entities).
    if phase_timing_enabled {
        sentinel_ecs::install_phase_timing(&mut world, &mut schedule);
    }

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
    // #75: kept for the post-spawn isolation verifier (AgentIsolationFailed event).
    let event_store_for_isolation = Arc::clone(&event_store);
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

                // #75: captured inside the handle-borrow block; netns isolation
                // is verified after the borrow is released.
                let mut agent_process_started = false;
                let mut started_child_pid: Option<u32> = None;

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
                                // #75: sandboxed child PID (bwrap --info-fd) for
                                // netns verification — NOT the supervisor `pid`.
                                let proc_child_pid = proc.child_pid;
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

                                // #75: defer netns isolation check until the
                                // handle borrow is released.
                                agent_process_started = true;
                                started_child_pid = proc_child_pid;
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

                // #75: verify full-cage isolation after the handle borrow is
                // released. ecs_tick_loop owns the event store, so a NotIsolated
                // agent also gets a string-typed AgentIsolationFailed event.
                if agent_process_started {
                    enforce_agent_netns_isolation(
                        agent_id,
                        &agent_cfg.identity.name,
                        started_child_pid,
                        &sandbox,
                        &mut sandbox_handles,
                        &mut ebpf_collector,
                        &mut agent_processes,
                        &security_runtime_state,
                        Some(event_store_for_isolation.as_ref()),
                    );
                }

                // #428 (Auflage B): Restart-Konsistenz. Ein aus dem Snapshot wiederhergestellter
                // Agent mit Status `Suspended` (= vor dem Restart pausiert) wird oben mit einem
                // frischen, *aktiven* bwrap-Prozess gespawnt — er wird hier unmittelbar wieder
                // eingefroren (Re-SIGSTOP), damit der Prozess NICHT weiterlaeuft. Das Aktiv-Fenster
                // ist µs-ms und liegt vor dem ersten ECS-Input. Bekannte Grenze: das
                // projektions-/UI-seitige Status-Label re-seedet beim Restart aus dem World-Snapshot
                // auf "active" (die ECS-Welt kennt kein Pause-Konzept) und re-synchronisiert beim
                // naechsten Pause/Resume; der Prozess ist real eingefroren (`T`), nicht aktiv.
                if agent_process_started
                    && runtime_orch
                        .agents()
                        .get(&agent_id)
                        .map(|handle| handle.status == sentinel_runtime::AgentStatus::Suspended)
                        .unwrap_or(false)
                {
                    if let Some(handle) = sandbox_handles.get(&agent_id) {
                        match suspend_agent_cgroup_processes(&handle.agent_name, handle.bwrap_pid) {
                            Ok(pids) => info!(
                                agent_id = %agent_id,
                                stopped_pids = pids.len(),
                                "Restored suspended agent re-eingefroren (#428 Re-SIGSTOP nach Restart)"
                            ),
                            Err(error) => warn!(
                                agent_id = %agent_id,
                                error = %error,
                                "Re-SIGSTOP fuer restored suspended agent fehlgeschlagen"
                            ),
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
    // Per-Phase-Histogramme (#381): leer wenn phase_timing deaktiviert.
    let phase_histograms: Vec<std::sync::Arc<sentinel_telemetry::Histogram>> =
        if phase_timing_enabled {
            sentinel_ecs::PHASE_NAMES
                .iter()
                .map(|phase| {
                    sentinel_telemetry::MetricsRegistry::global().histogram(
                        &sentinel_telemetry::phase_metric_name(phase),
                        &sentinel_telemetry::PHASE_DURATION_BOUNDARIES_MS,
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
    let mut restore_fence = RestoreFence::default();
    // #491 (TM-3): zuletzt aufgezeichnetes PSI-Band (cpu_above, mem_above). None = noch nichts
    // emittiert -> erster Tick setzt die Baseline. Nur Aenderungen werden als Event geschrieben.
    let mut psi_band: Option<(bool, bool)> = None;

    loop {
        let tick_start = Instant::now();

        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        if let Some(rx) = evolution_result_rx.as_ref() {
            drain_evolution_results(&state_store_for_sim, rx);
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
        let psi_cpu_avg10 = adaptive_tick.cpu_avg10();
        let psi_mem_avg10 = adaptive_tick.mem_avg10();
        if let Some(mut psi) = world.get_resource_mut::<sentinel_ecs::PsiMetrics>() {
            psi.cpu_avg10 = psi_cpu_avg10;
            psi.mem_avg10 = psi_mem_avg10;
        }

        // #491 (TM-3): PSI-Band als sparse Event aufzeichnen (nur bei Wechsel). apply_psi_stress
        // ist rein schwellenbasiert -> die zwei Booleans sind der exakte, deterministische
        // Replay-Input. Push in den EventBuffer VOR schedule.run -> persist_system schreibt das
        // Event mit GENAU diesem Tick; das Band wirkt im Replay ab demselben Tick (kein Off-by-one).
        let current_band = psi_band_from_metrics(psi_cpu_avg10, psi_mem_avg10);
        if psi_band != Some(current_band) {
            psi_band = Some(current_band);
            let payload = sentinel_common::DomainEventPayload::PsiBandChanged {
                cpu_above: current_band.0,
                mem_above: current_band.1,
            };
            if let Some(mut buffer) = world.get_resource_mut::<sentinel_ecs::EventBuffer>() {
                buffer.events.push(sentinel_common::DomainEvent::new(
                    payload.event_type_str(),
                    "world",
                    &payload.to_json(),
                    &format!("psi-{tick_count}"),
                    tick_count,
                ));
            }
        }

        // RuntimeOrchestrator Tick synchronisieren
        runtime_orch.set_tick(tick_count);

        // ECS Schedule ausfuehren (alle 12 Systems in Reihenfolge)
        schedule.run(&mut world);

        // Per-Phase-Dauern recorden (#381): 10x observe, ~25ns each — im Budget.
        if !phase_histograms.is_empty() {
            if let Some(timings) = world.get_resource::<sentinel_ecs::PhaseTimings>() {
                for (i, hist) in phase_histograms.iter().enumerate() {
                    if let Some(ms) = timings.duration_ms(i) {
                        hist.observe(ms);
                    }
                }
            }
        }

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
                    if restore_fence.is_active() {
                        let response = RuntimeReconcileResponse {
                            accepted: false,
                            dry_run: request.dry_run,
                            current_shift,
                            repair_last_status: "restore_fence_active".to_string(),
                            errors: vec!["Runtime-Reconcile ist waehrend Restore-Fence blockiert"
                                .to_string()],
                            ..RuntimeReconcileResponse::default()
                        };
                        let _ = response_tx.send(response);
                        continue;
                    }
                    let response = execute_runtime_reconcile(
                        tick_count,
                        current_shift,
                        &all_agents,
                        &mut world,
                        &mut runtime_orch,
                        &sandbox,
                        &mut sandbox_handles,
                        &mut ebpf_collector,
                        &mut agent_processes,
                        &agent_command,
                        &security_runtime_state,
                        &event_store_for_prune,
                        &runtime_health,
                        &projection_db_path,
                        operator_auth_required,
                        service_health_checker.worker_state(),
                        fs_mount.as_deref(),
                        request,
                        &mut respawn_backoff,
                        RuntimeReconcileSource::Operator,
                    );
                    let _ = response_tx.send(response);
                }
                RuntimeControlCommand::AnalysisFloodTest {
                    request,
                    response_tx,
                } => {
                    let enqueue_started = std::time::Instant::now();
                    for _ in 0..request.count {
                        platform_cp.enqueue_control_command(
                            crate::platform_controlplane::PlatformControlCommand::AnalyzeNow,
                        );
                    }

                    #[cfg(feature = "llm")]
                    for idx in 0..request.count {
                        let synthetic = crate::platform_controlplane::PlatformAnalysisRequest {
                            trigger: format!("flood_test_{idx}"),
                            tick: tick_count,
                            metrics: crate::platform_controlplane::metrics::PlatformMetrics {
                                tick: tick_count,
                                ..Default::default()
                            },
                            verify_results: HashMap::new(),
                            failed_interventions: Vec::new(),
                        };
                        let _ = platform_llm_analyzer.enqueue(synthetic);
                    }

                    let mut stats = platform_cp.analysis_queue_stats();
                    #[cfg(feature = "llm")]
                    {
                        let analyzer_stats = platform_llm_analyzer.queue_stats();
                        stats.depth = stats.depth.saturating_add(analyzer_stats.depth);
                        stats.dropped_total = stats
                            .dropped_total
                            .saturating_add(analyzer_stats.dropped_total);
                        stats.coalesced_total = stats
                            .coalesced_total
                            .saturating_add(analyzer_stats.coalesced_total);
                    }

                    let note = if cfg!(feature = "llm") {
                        "bounded controlplane and analyzer queues exercised"
                    } else {
                        "bounded controlplane queue exercised; llm feature disabled"
                    };
                    let enqueue_elapsed_ns = enqueue_started.elapsed().as_nanos() as u64;
                    let _ = response_tx.send(RuntimeAnalysisFloodTestResponse {
                        accepted: true,
                        requested: request.count,
                        queue_depth: stats.depth,
                        dropped_total: stats.dropped_total,
                        coalesced_total: stats.coalesced_total,
                        enqueue_elapsed_us: enqueue_elapsed_ns / 1_000,
                        enqueue_per_request_ns: enqueue_elapsed_ns
                            / u64::from(request.count.max(1)),
                        note: note.to_string(),
                    });
                }
                RuntimeControlCommand::PanicTest {
                    request,
                    response_tx,
                } => {
                    let response = if request.worker == "service_health" {
                        if service_health_checker.trigger_panic_test() {
                            info!(
                                worker = %request.worker,
                                "panic test triggered fuer Worker"
                            );
                            RuntimePanicTestResponse {
                                accepted: true,
                                worker: request.worker,
                                note: "panic-test dispatched".to_string(),
                            }
                        } else {
                            RuntimePanicTestResponse {
                                accepted: false,
                                worker: request.worker,
                                note: "service_health control channel unavailable".to_string(),
                            }
                        }
                    } else {
                        RuntimePanicTestResponse {
                            accepted: false,
                            worker: request.worker,
                            note: "worker unsupported".to_string(),
                        }
                    };
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
                            let bookkeeping_started = std::time::Instant::now();
                            let pid_before = tracked_pid_for_agent(
                                agent_id,
                                &sandbox_handles,
                                &agent_processes,
                                &security_runtime_state,
                            );
                            let bookkeeping_elapsed_ns =
                                bookkeeping_started.elapsed().as_nanos() as u64;
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
                                        bookkeeping_elapsed_ns,
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
                                        bookkeeping_elapsed_ns,
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
                            bookkeeping_elapsed_ns: 0,
                            note: "agent_id unbekannt".to_string(),
                        },
                    };
                    let _ = response_tx.send(response);
                }
                // #491 (TM-3): read-only State-Hash der Live-World. Wird hier (nach schedule.run
                // dieses Ticks) erhoben = "Ende des Ticks" — dieselbe Zyklus-Position wie der
                // Post-Replay-Hash, damit live@T und restore(@T)+replay vergleichbar sind.
                RuntimeControlCommand::StateHash { response_tx } => {
                    let hashes = sentinel_ecs::hash::state_hashes(&mut world);
                    let tick = world
                        .get_resource::<sentinel_ecs::SimulationTime>()
                        .map(|t| t.tick_count)
                        .unwrap_or(0);
                    let last_event_id = world
                        .get_resource::<sentinel_ecs::LimboEventStore>()
                        .and_then(|es| es.0.get_latest_event_id().ok())
                        .unwrap_or(0);
                    let _ = response_tx.send(crate::runtime_control::StateHashResponse {
                        strict: hashes.strict,
                        core: hashes.core,
                        tick,
                        last_event_id,
                    });
                }
                // #428: per-Agent Pause — SIGSTOP der Sandbox-Prozesse + Status->Suspended.
                // Nicht destruktiv: ECS-Entity + Memory/Evolution bleiben (KEIN teardown_agent_full).
                RuntimeControlCommand::Pause {
                    agent_id,
                    response_tx,
                } => {
                    let aid = AgentId(agent_id);
                    let aggregate_id = format!("AGENT-{agent_id:02}");
                    let response = if !runtime_orch.agents().contains_key(&aid) {
                        AgentLifecycleResponse {
                            accepted: false,
                            agent_id,
                            aggregate_id,
                            action: "pause".to_string(),
                            new_status: String::new(),
                            affected_pids: 0,
                            outcome: "not_found".to_string(),
                            note: "Agent nicht in der Runtime".to_string(),
                        }
                    } else {
                        match runtime_orch.pause_agent(aid) {
                            Ok(()) => {
                                let affected = sandbox_handles
                                    .get(&aid)
                                    .and_then(|h| {
                                        suspend_agent_cgroup_processes(&h.agent_name, h.bwrap_pid)
                                            .ok()
                                    })
                                    .map(|pids| pids.len())
                                    .unwrap_or(0);
                                info!(
                                    agent_id = %aid,
                                    affected_pids = affected,
                                    "Agent pausiert (#428 SIGSTOP; ECS-Entity + Memory bleiben)"
                                );
                                AgentLifecycleResponse {
                                    accepted: true,
                                    agent_id,
                                    aggregate_id,
                                    action: "pause".to_string(),
                                    new_status: "suspended".to_string(),
                                    affected_pids: affected,
                                    outcome: "ok".to_string(),
                                    note: "paused (SIGSTOP; ECS-Entity + Memory bleiben)".to_string(),
                                }
                            }
                            Err(error) => AgentLifecycleResponse {
                                accepted: false,
                                agent_id,
                                aggregate_id,
                                action: "pause".to_string(),
                                new_status: String::new(),
                                affected_pids: 0,
                                outcome: "invalid_transition".to_string(),
                                note: error.to_string(),
                            },
                        }
                    };
                    let _ = response_tx.send(response);
                }
                // #428: per-Agent Resume — SIGCONT + Status->Active. Gegenstueck zu Pause.
                RuntimeControlCommand::Resume {
                    agent_id,
                    response_tx,
                } => {
                    let aid = AgentId(agent_id);
                    let aggregate_id = format!("AGENT-{agent_id:02}");
                    let response = if !runtime_orch.agents().contains_key(&aid) {
                        AgentLifecycleResponse {
                            accepted: false,
                            agent_id,
                            aggregate_id,
                            action: "resume".to_string(),
                            new_status: String::new(),
                            affected_pids: 0,
                            outcome: "not_found".to_string(),
                            note: "Agent nicht in der Runtime".to_string(),
                        }
                    } else {
                        match runtime_orch.resume_agent(aid) {
                            Ok(()) => {
                                let affected = sandbox_handles
                                    .get(&aid)
                                    .and_then(|h| {
                                        resume_agent_cgroup_processes(&h.agent_name, h.bwrap_pid)
                                            .ok()
                                    })
                                    .map(|pids| pids.len())
                                    .unwrap_or(0);
                                info!(
                                    agent_id = %aid,
                                    affected_pids = affected,
                                    "Agent fortgesetzt (#428 SIGCONT)"
                                );
                                AgentLifecycleResponse {
                                    accepted: true,
                                    agent_id,
                                    aggregate_id,
                                    action: "resume".to_string(),
                                    new_status: "active".to_string(),
                                    affected_pids: affected,
                                    outcome: "ok".to_string(),
                                    note: "resumed (SIGCONT)".to_string(),
                                }
                            }
                            Err(error) => AgentLifecycleResponse {
                                accepted: false,
                                agent_id,
                                aggregate_id,
                                action: "resume".to_string(),
                                new_status: String::new(),
                                affected_pids: 0,
                                outcome: "invalid_transition".to_string(),
                                note: error.to_string(),
                            },
                        }
                    };
                    let _ = response_tx.send(response);
                }
                // #428: per-Agent destruktives Despawn — teardown_agent_full -> AgentDespawned.
                // Separater, bestaetigungs-gegateter Pfad (NICHT Pause).
                RuntimeControlCommand::Despawn {
                    agent_id,
                    response_tx,
                } => {
                    let aid = AgentId(agent_id);
                    let aggregate_id = format!("AGENT-{agent_id:02}");
                    let present = runtime_orch.agents().contains_key(&aid);
                    if present {
                        teardown_agent_full(
                            aid,
                            &mut world,
                            &mut runtime_orch,
                            &sandbox,
                            &mut sandbox_handles,
                            &mut ebpf_collector,
                            &mut agent_processes,
                            &security_runtime_state,
                        );
                        info!(
                            agent_id = %aid,
                            "Agent destruktiv entfernt (#428 teardown_agent_full -> AgentDespawned)"
                        );
                    }
                    let response = AgentLifecycleResponse {
                        accepted: present,
                        agent_id,
                        aggregate_id,
                        action: "despawn".to_string(),
                        new_status: if present {
                            "despawned".to_string()
                        } else {
                            String::new()
                        },
                        affected_pids: 0,
                        outcome: if present {
                            "ok".to_string()
                        } else {
                            "not_found".to_string()
                        },
                        note: if present {
                            "despawned (teardown_agent_full)".to_string()
                        } else {
                            "Agent nicht in der Runtime".to_string()
                        },
                    };
                    let _ = response_tx.send(response);
                }
            }
        }

        // Platform-Controlplane: Self-Healing (alle N Ticks)
        if !restore_fence.is_active()
            && sentinel_common::feature_flags::RuntimeFlags::global().platform_controlplane_enabled
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
                llm_circuit_open.as_ref(),
                llm_activity_ticks.as_ref(),
            );
            // #428 (Auflage A): pausierte (Suspended) Agents von der Stall->Restart-Regel ausnehmen.
            // Ein SIGSTOPpter Agent macht 0 Syscalls und wuerde sonst als "stalled" erkannt +
            // zwangs-restartet (was die Pause aufheben wuerde). SSOT = Runtime-Handle-Status, ueber
            // den Namen gematcht (stalled_agents sind eBPF-Namen); ueberlebt Restart via Snapshot.
            let suspended_agents: std::collections::HashSet<String> = runtime_orch
                .agents()
                .values()
                .filter(|handle| handle.status == sentinel_runtime::AgentStatus::Suspended)
                .map(|handle| handle.identity.name.clone())
                .collect();
            platform_cp.set_suspended_agents(suspended_agents);
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
                        // Auto-detect cutoff: behalte die 2 neuesten World-Snapshots (Restore-Puffer),
                        // prune alle Events davor.
                        if let Ok(snapshots) = event_store_for_prune.list_world_snapshots() {
                            if let Some(prune_point) =
                                crate::snapshot::prune_cutoff_from_ordered_snapshots(&snapshots)
                            {
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
        if should_run_periodic_runtime_reconcile_unfenced(
            platform_cp.config(),
            tick_count,
            &restore_fence,
        ) {
            let request = periodic_runtime_reconcile_request(platform_cp.config());
            let response = execute_runtime_reconcile(
                tick_count,
                current_shift,
                &all_agents,
                &mut world,
                &mut runtime_orch,
                &sandbox,
                &mut sandbox_handles,
                &mut ebpf_collector,
                &mut agent_processes,
                &agent_command,
                &security_runtime_state,
                &event_store_for_prune,
                &runtime_health,
                &projection_db_path,
                operator_auth_required,
                service_health_checker.worker_state(),
                fs_mount.as_deref(),
                request,
                &mut respawn_backoff,
                RuntimeReconcileSource::Periodic,
            );
            debug!(
                tick = tick_count,
                elapsed_us = response.elapsed_us,
                repairs = response.security_snapshots_removed
                    + response.unexpected_runtime_removed
                    + response.orphan_cgroups_removed
                    + response.respawned_agents,
                status = %response.repair_last_status,
                "Periodischer Runtime-Reconcile abgeschlossen"
            );
        }
        publish_platform_state_snapshot(
            &platform_state,
            tick_count,
            &platform_cp,
            &runtime_orch,
            &resource_manager,
        );
        let mut analysis_queue_stats = platform_cp.analysis_queue_stats();
        #[cfg(feature = "llm")]
        {
            let analyzer_stats = platform_llm_analyzer.queue_stats();
            analysis_queue_stats.depth = analysis_queue_stats
                .depth
                .saturating_add(analyzer_stats.depth);
            analysis_queue_stats.dropped_total = analysis_queue_stats
                .dropped_total
                .saturating_add(analyzer_stats.dropped_total);
            analysis_queue_stats.coalesced_total = analysis_queue_stats
                .coalesced_total
                .saturating_add(analyzer_stats.coalesced_total);
        }
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
            analysis_queue_stats,
        );

        // Prune: Empfange Cutoff von Operator-API, arbeite 1 Batch/Tick ab
        while let Ok(cutoff) = prune_rx.try_recv() {
            if restore_fence.is_active() {
                warn!(cutoff, "Prune waehrend Restore-Fence blockiert");
            } else {
                snapshot_manager.start_prune(cutoff);
            }
        }
        if !restore_fence.is_active() {
            snapshot_manager.prune_tick(&event_store_for_prune, tick_count);
        }

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
                    if let Some(proc_handle) = agent_processes.remove(agent_id) {
                        terminate_agent_process(proc_handle);
                    }

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
                let nightrun_event_store = world
                    .get_resource::<sentinel_ecs::LimboEventStore>()
                    .map(|es| Arc::clone(&es.0));
                let shift_run_id = nightrun_run_id("shift", tick_count, current_shift, new_shift);
                let shift_started = Instant::now();
                let mut shift_hash_chain = NightrunHashChain::new(&shift_run_id, &shift_run_id);
                let mut shift_event_emission_enabled = false;
                if let Some(ref event_store) = nightrun_event_store {
                    let payload = DomainEventPayload::NightRunStarted {
                        run_id: shift_run_id.clone(),
                        trigger_shift_set: current_shift,
                        agents_queued: removed.len() as u32,
                    };
                    match append_nightrun_event(
                        event_store,
                        payload,
                        "nightrun",
                        &shift_run_id,
                        tick_count,
                        Some(&mut shift_hash_chain),
                    ) {
                        Ok(_) => {
                            shift_event_emission_enabled = true;
                        }
                        Err(e) => {
                            warn!(
                                run_id = %shift_run_id,
                                error = %e,
                                "Schichtwechsel-Nightrun-Start-Event fehlgeschlagen"
                            );
                        }
                    }
                }
                let mut shift_episodes_processed = 0u32;
                let mut shift_episodes_consolidated = 0u32;
                let mut shift_agents_consolidated = 0u32;
                let mut shift_agents_failed = 0u32;
                let mut shift_nmda_scores: Vec<f64> = Vec::new();
                for agent_id in &removed {
                    let agent_name = all_agents
                        .iter()
                        .find(|a| AgentId(a.identity.id) == *agent_id)
                        .map(|a| a.identity.name.as_str());
                    if let Some(name) = agent_name {
                        let agent_started = Instant::now();
                        match episode_producer.hippocampus().consolidate_agent(name) {
                            Ok(result) => {
                                let episodes_processed = result.episodes_processed as u32;
                                let episodes_consolidated = result.episodes_consolidated as u32;
                                if episodes_processed > 0 {
                                    let nmda_scores = nmda_consolidated_scores(&result);
                                    let agent_stats = nmda_score_stats(&result.episode_scores);
                                    shift_episodes_processed += episodes_processed;
                                    shift_episodes_consolidated += episodes_consolidated;
                                    shift_agents_consolidated += 1;
                                    shift_nmda_scores.extend(result.episode_scores.iter().copied());

                                    info!(
                                        agent = name,
                                        episodes_processed,
                                        episodes_consolidated,
                                        selection_rate = format!(
                                            "{:.3}",
                                            nmda_selection_rate(
                                                episodes_processed,
                                                episodes_consolidated
                                            )
                                        ),
                                        nmda_threshold = NMDA_CONSOLIDATION_THRESHOLD,
                                        nmda_score_min = ?agent_stats.min,
                                        nmda_score_avg = ?agent_stats.avg,
                                        nmda_score_max = ?agent_stats.max,
                                        "Schichtwechsel-NMDA-Agent-Selektion"
                                    );

                                    if shift_event_emission_enabled {
                                        if let Some(ref event_store) = nightrun_event_store {
                                            let payload = DomainEventPayload::AgentConsolidated {
                                                run_id: shift_run_id.clone(),
                                                agent_name: name.to_string(),
                                                episodes_processed,
                                                episodes_consolidated,
                                                duration_ms: agent_started.elapsed().as_millis()
                                                    as u64,
                                            };
                                            let aggregate_id = format!("AGENT-{:02}", agent_id.0);
                                            if let Err(e) = append_nightrun_event(
                                                event_store,
                                                payload,
                                                &aggregate_id,
                                                &shift_run_id,
                                                tick_count,
                                                Some(&mut shift_hash_chain),
                                            ) {
                                                shift_event_emission_enabled = false;
                                                warn!(
                                                    run_id = %shift_run_id,
                                                    agent = name,
                                                    error = %e,
                                                    "Schichtwechsel-AgentConsolidated-Event fehlgeschlagen"
                                                );
                                            }
                                        }
                                    }

                                    if episodes_consolidated > 0 {
                                        let narrative: String = result
                                            .consolidated_summaries
                                            .iter()
                                            .map(|(s, _score)| s.as_str())
                                            .collect::<Vec<_>>()
                                            .join("; ");

                                        let agent_role = all_agents
                                            .iter()
                                            .find(|a| AgentId(a.identity.id) == *agent_id)
                                            .map(|a| a.identity.role.as_str())
                                            .unwrap_or("Mitarbeiter");
                                        let queued = queue_evolution_job(
                                            &evolution_job_tx,
                                            EvolutionJob {
                                                agent_id: *agent_id,
                                                agent_name: name.to_string(),
                                                agent_role: agent_role.to_string(),
                                                narrative: narrative.clone(),
                                                source: EvolutionSource::ShiftTransition,
                                            },
                                        );
                                        if !queued {
                                            write_evolution_narrative_only(
                                                &state_store_for_sim,
                                                *agent_id,
                                                name,
                                                EvolutionSource::ShiftTransition,
                                                &narrative,
                                            );
                                        }

                                        if let Some(ref store) = redb_store {
                                            // NMDA scores nach redb schreiben
                                            if !nmda_scores.is_empty() {
                                                let avg_score: f64 =
                                                    nmda_scores.iter().sum::<f64>()
                                                        / nmda_scores.len() as f64;
                                                match store.set_nmda_scores(*agent_id, &nmda_scores)
                                                {
                                                    Ok(()) => {
                                                        info!(
                                                            agent = name,
                                                            nmda_count = nmda_scores.len(),
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
                                                match store.set_agent_facts(*agent_id, &facts_json)
                                                {
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
                            }
                            Err(e) => {
                                shift_agents_failed += 1;
                                if shift_event_emission_enabled {
                                    if let Some(ref event_store) = nightrun_event_store {
                                        let payload =
                                            DomainEventPayload::AgentConsolidationFailed {
                                                run_id: shift_run_id.clone(),
                                                agent_name: name.to_string(),
                                                error: e.to_string(),
                                            };
                                        let aggregate_id = format!("AGENT-{:02}", agent_id.0);
                                        if let Err(event_err) = append_nightrun_event(
                                            event_store,
                                            payload,
                                            &aggregate_id,
                                            &shift_run_id,
                                            tick_count,
                                            Some(&mut shift_hash_chain),
                                        ) {
                                            shift_event_emission_enabled = false;
                                            warn!(
                                                run_id = %shift_run_id,
                                                agent = name,
                                                error = %event_err,
                                                "Schichtwechsel-AgentConsolidationFailed-Event fehlgeschlagen"
                                            );
                                        }
                                    }
                                }
                                warn!(agent = name, error = %e, "Schichtwechsel-Konsolidierung fehlgeschlagen");
                            }
                        }
                    }
                }
                let shift_stats = nmda_score_stats(&shift_nmda_scores);
                if shift_episodes_processed > 0 {
                    info!(
                        old_shift = current_shift,
                        new_shift,
                        agents_removed = removed.len(),
                        episodes_processed = shift_episodes_processed,
                        episodes_consolidated = shift_episodes_consolidated,
                        selection_rate = format!(
                            "{:.3}",
                            nmda_selection_rate(
                                shift_episodes_processed,
                                shift_episodes_consolidated
                            )
                        ),
                        nmda_threshold = NMDA_CONSOLIDATION_THRESHOLD,
                        nmda_max_consolidation_episodes = NMDA_MAX_CONSOLIDATION_EPISODES,
                        nmda_score_min = ?shift_stats.min,
                        nmda_score_avg = ?shift_stats.avg,
                        nmda_score_max = ?shift_stats.max,
                        "Schichtwechsel-NMDA-Selektion abgeschlossen"
                    );
                }
                if shift_event_emission_enabled {
                    if let Some(ref event_store) = nightrun_event_store {
                        let hash_chain_final = shift_hash_chain.current_hash();
                        let payload = DomainEventPayload::NightRunCompleted {
                            run_id: shift_run_id.clone(),
                            trigger_shift_set: current_shift,
                            agents_consolidated: shift_agents_consolidated,
                            agents_failed: shift_agents_failed,
                            agents_skipped: 0,
                            total_episodes: shift_episodes_processed,
                            total_episodes_consolidated: shift_episodes_consolidated,
                            nmda_selection_rate: Some(nmda_selection_rate(
                                shift_episodes_processed,
                                shift_episodes_consolidated,
                            )),
                            nmda_threshold: Some(NMDA_CONSOLIDATION_THRESHOLD),
                            nmda_max_consolidation_episodes: Some(
                                NMDA_MAX_CONSOLIDATION_EPISODES as u32,
                            ),
                            nmda_score_min: shift_stats.min,
                            nmda_score_avg: shift_stats.avg,
                            nmda_score_max: shift_stats.max,
                            duration_ms: shift_started.elapsed().as_millis() as u64,
                            hash_chain: Some(hash_chain_final.clone()),
                        };
                        if let Err(e) = append_nightrun_event(
                            event_store,
                            payload,
                            "nightrun",
                            &shift_run_id,
                            tick_count,
                            None,
                        ) {
                            warn!(
                                run_id = %shift_run_id,
                                hash_chain = %hash_chain_final,
                                error = %e,
                                "Schichtwechsel-Nightrun-Completed-Event fehlgeschlagen"
                            );
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
                // #529: Post-Shift-Anker erzwingen. Der periodische Snapshot-Block weiter unten
                // (im selben Tick, nach Despawn+Respawn) erfasst dann den Post-Shift-Zustand, sodass
                // jeder Restore auf ein Ziel >= diesem Shift-Tick den Post-Shift-Anker waehlt und das
                // Replay-Fenster nie ueber die Schichtgrenze laeuft (vgl. SPIKE-529).
                snapshot_manager.mark_shift_snapshot_pending();
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
            let trigger_shift_set = nightrun_cmd.shift_set.unwrap_or(current_shift);
            let operator_run_id =
                nightrun_run_id("operator", tick_count, current_shift, trigger_shift_set);
            let operator_started = Instant::now();
            let operator_event_store = world
                .get_resource::<sentinel_ecs::LimboEventStore>()
                .map(|es| Arc::clone(&es.0));
            let mut operator_hash_chain =
                NightrunHashChain::new(&operator_run_id, &operator_run_id);
            let mut operator_event_emission_enabled = false;
            if !nightrun_cmd.dry_run {
                if let Some(ref event_store) = operator_event_store {
                    let payload = DomainEventPayload::NightRunStarted {
                        run_id: operator_run_id.clone(),
                        trigger_shift_set,
                        agents_queued: target_agents.len() as u32,
                    };
                    match append_nightrun_event(
                        event_store,
                        payload,
                        "nightrun",
                        &operator_run_id,
                        tick_count,
                        Some(&mut operator_hash_chain),
                    ) {
                        Ok(_) => {
                            operator_event_emission_enabled = true;
                        }
                        Err(e) => {
                            warn!(
                                run_id = %operator_run_id,
                                error = %e,
                                "Operator-Nightrun-Start-Event fehlgeschlagen"
                            );
                        }
                    }
                }
            }
            let mut episodes_processed_total = 0u32;
            let mut episodes_consolidated_total = 0u32;
            let mut agents_consolidated_total = 0u32;
            let mut agents_failed_total = 0u32;
            let mut operator_nmda_scores: Vec<f64> = Vec::new();
            let mut evolution_jobs_queued = 0u32;
            let mut evolution_entries: Vec<(String, u32)> = Vec::new();
            for agent_cfg in &target_agents {
                let name = &agent_cfg.identity.name;
                if nightrun_cmd.dry_run {
                    info!(agent = %name, "Nightrun dry-run: wuerde konsolidieren");
                    continue;
                }
                let agent_started = Instant::now();
                match episode_producer.hippocampus().consolidate_agent(name) {
                    Ok(result) if result.episodes_processed > 0 => {
                        let episodes_processed = result.episodes_processed as u32;
                        let episodes_consolidated = result.episodes_consolidated as u32;
                        let agent_stats = nmda_score_stats(&result.episode_scores);
                        episodes_processed_total += episodes_processed;
                        episodes_consolidated_total += episodes_consolidated;
                        agents_consolidated_total += 1;
                        operator_nmda_scores.extend(result.episode_scores.iter().copied());

                        info!(
                            agent = %name,
                            episodes_processed,
                            episodes_consolidated,
                            selection_rate = format!(
                                "{:.3}",
                                nmda_selection_rate(episodes_processed, episodes_consolidated)
                            ),
                            nmda_threshold = NMDA_CONSOLIDATION_THRESHOLD,
                            nmda_score_min = ?agent_stats.min,
                            nmda_score_avg = ?agent_stats.avg,
                            nmda_score_max = ?agent_stats.max,
                            "Nightrun-NMDA-Agent-Selektion"
                        );

                        if operator_event_emission_enabled {
                            if let Some(ref event_store) = operator_event_store {
                                let payload = DomainEventPayload::AgentConsolidated {
                                    run_id: operator_run_id.clone(),
                                    agent_name: name.to_string(),
                                    episodes_processed,
                                    episodes_consolidated,
                                    duration_ms: agent_started.elapsed().as_millis() as u64,
                                };
                                let aggregate_id = format!("AGENT-{:02}", agent_cfg.identity.id);
                                if let Err(e) = append_nightrun_event(
                                    event_store,
                                    payload,
                                    &aggregate_id,
                                    &operator_run_id,
                                    tick_count,
                                    Some(&mut operator_hash_chain),
                                ) {
                                    operator_event_emission_enabled = false;
                                    warn!(
                                        run_id = %operator_run_id,
                                        agent = %name,
                                        error = %e,
                                        "Operator-AgentConsolidated-Event fehlgeschlagen"
                                    );
                                }
                            }
                        }

                        if episodes_consolidated > 0 {
                            evolution_entries.push((name.to_string(), episodes_consolidated));
                            let narrative = result
                                .consolidated_summaries
                                .iter()
                                .map(|(summary, _score)| summary.as_str())
                                .collect::<Vec<_>>()
                                .join("; ");
                            let queued = queue_evolution_job(
                                &evolution_job_tx,
                                EvolutionJob {
                                    agent_id: AgentId(agent_cfg.identity.id),
                                    agent_name: name.to_string(),
                                    agent_role: agent_cfg.identity.role.clone(),
                                    narrative: narrative.clone(),
                                    source: EvolutionSource::Nightrun,
                                },
                            );
                            if queued {
                                evolution_jobs_queued += 1;
                            } else {
                                write_evolution_narrative_only(
                                    &state_store_for_sim,
                                    AgentId(agent_cfg.identity.id),
                                    name,
                                    EvolutionSource::Nightrun,
                                    &narrative,
                                );
                            }
                        }
                    }
                    Ok(_) => {} // Keine Episodes = nichts zu tun
                    Err(e) => {
                        agents_failed_total += 1;
                        if operator_event_emission_enabled {
                            if let Some(ref event_store) = operator_event_store {
                                let payload = DomainEventPayload::AgentConsolidationFailed {
                                    run_id: operator_run_id.clone(),
                                    agent_name: name.to_string(),
                                    error: e.to_string(),
                                };
                                let aggregate_id = format!("AGENT-{:02}", agent_cfg.identity.id);
                                if let Err(event_err) = append_nightrun_event(
                                    event_store,
                                    payload,
                                    &aggregate_id,
                                    &operator_run_id,
                                    tick_count,
                                    Some(&mut operator_hash_chain),
                                ) {
                                    operator_event_emission_enabled = false;
                                    warn!(
                                        run_id = %operator_run_id,
                                        agent = %name,
                                        error = %event_err,
                                        "Operator-AgentConsolidationFailed-Event fehlgeschlagen"
                                    );
                                }
                            }
                        }
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
                                    format!("Nightrun-Konsolidierung: {episodes} Episoden konsolidiert"),
                                    0.0_f64,
                                    "night_run",
                                    now_ms,
                                ],
                            ).is_ok() {
                                written += 1;
                                if let Err(retention_err) =
                                    retain_personality_evolution_agent_field(
                                        &evo_db,
                                        &agent_id,
                                        "memory_consolidation",
                                    )
                                {
                                    warn!(
                                        agent = %agent_id,
                                        error = %retention_err,
                                        "personality_evolution: agent-field retention fehlgeschlagen"
                                    );
                                }
                            }
                        }
                        if written > 0 {
                            if let Err(retention_err) = retain_personality_evolution_global(&evo_db)
                            {
                                warn!(
                                    error = %retention_err,
                                    "personality_evolution: global retention fehlgeschlagen"
                                );
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

            let operator_stats = nmda_score_stats(&operator_nmda_scores);
            info!(
                agents = target_agents.len(),
                episodes_processed = episodes_processed_total,
                episodes_consolidated = episodes_consolidated_total,
                selection_rate = format!(
                    "{:.3}",
                    nmda_selection_rate(episodes_processed_total, episodes_consolidated_total)
                ),
                nmda_threshold = NMDA_CONSOLIDATION_THRESHOLD,
                nmda_max_consolidation_episodes = NMDA_MAX_CONSOLIDATION_EPISODES,
                nmda_score_min = ?operator_stats.min,
                nmda_score_avg = ?operator_stats.avg,
                nmda_score_max = ?operator_stats.max,
                evolution_jobs_queued,
                dry_run = nightrun_cmd.dry_run,
                "Nightrun abgeschlossen"
            );
            if operator_event_emission_enabled {
                if let Some(ref event_store) = operator_event_store {
                    let hash_chain_final = operator_hash_chain.current_hash();
                    let payload = DomainEventPayload::NightRunCompleted {
                        run_id: operator_run_id.clone(),
                        trigger_shift_set,
                        agents_consolidated: agents_consolidated_total,
                        agents_failed: agents_failed_total,
                        agents_skipped: 0,
                        total_episodes: episodes_processed_total,
                        total_episodes_consolidated: episodes_consolidated_total,
                        nmda_selection_rate: Some(nmda_selection_rate(
                            episodes_processed_total,
                            episodes_consolidated_total,
                        )),
                        nmda_threshold: Some(NMDA_CONSOLIDATION_THRESHOLD),
                        nmda_max_consolidation_episodes: Some(
                            NMDA_MAX_CONSOLIDATION_EPISODES as u32,
                        ),
                        nmda_score_min: operator_stats.min,
                        nmda_score_avg: operator_stats.avg,
                        nmda_score_max: operator_stats.max,
                        duration_ms: operator_started.elapsed().as_millis() as u64,
                        hash_chain: Some(hash_chain_final.clone()),
                    };
                    if let Err(e) = append_nightrun_event(
                        event_store,
                        payload,
                        "nightrun",
                        &operator_run_id,
                        tick_count,
                        None,
                    ) {
                        warn!(
                            run_id = %operator_run_id,
                            hash_chain = %hash_chain_final,
                            error = %e,
                            "Operator-Nightrun-Completed-Event fehlgeschlagen"
                        );
                    }
                }
            }
        }

        // Time Machine: Periodische World Snapshots
        if !restore_fence.is_active() && snapshot_manager.should_create_snapshot(tick_count) {
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
                            if let Err(e) = snapshot_manager.maintain(&es, fs_layer.as_deref()) {
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
            if restore_fence.is_active() {
                warn!("Manueller Snapshot waehrend Restore-Fence blockiert");
                continue;
            }
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
            info!(cmd = ?restore_cmd, "Hot-Swap Restore gestartet");
            let event_store_for_restore = world
                .get_resource::<sentinel_ecs::LimboEventStore>()
                .map(|es| Arc::clone(&es.0));
            let state_store_for_restore = world
                .get_resource::<sentinel_ecs::RedbStateStore>()
                .map(|rs| rs.store.clone());

            if let (Some(es), Some(ss)) = (event_store_for_restore, state_store_for_restore) {
                let data_dir = std::path::Path::new(&events_db_path_str)
                    .parent()
                    .unwrap_or(std::path::Path::new("/opt/sentinel/data"));
                if let Err(error) = execute_world_restore_transfer(
                    &restore_cmd,
                    &mut restore_fence,
                    &mut snapshot_manager,
                    &mut world,
                    &mut schedule,
                    &mut runtime_orch,
                    &sandbox,
                    &mut sandbox_handles,
                    &mut ebpf_collector,
                    &mut agent_processes,
                    &agent_command,
                    &security_runtime_state,
                    &es,
                    &ss,
                    fs_layer.as_deref(),
                    fs_mount.as_deref(),
                    data_dir,
                    &projection_db_path,
                    &all_agents,
                    &mut tick_count,
                    &mut sim_hour,
                    &mut current_shift,
                ) {
                    error!(
                        cmd = ?restore_cmd,
                        error = %error,
                        fenced = restore_fence.is_active(),
                        "Hot-Swap Restore fehlgeschlagen"
                    );
                }
            } else {
                error!("Hot-Swap Restore nicht moeglich: EventStore oder StateStore fehlt");
            }
        }

        // Runtime Config-Apply (#425): Firma zur Laufzeit aendern — Live-Diff oder Fresh-Load.
        // Laeuft zwischen Ticks (nach schedule.run) → tick-synchron.
        while let Ok(apply_cmd) = config_apply_rx.try_recv() {
            if restore_fence.is_active() {
                warn!("Config-Apply waehrend Restore-Fence blockiert");
                continue;
            }
            // Defensive Re-Validierung (Endpoint hat bereits 4xx geliefert; hier nur Schutz).
            if let Err(errors) = crate::config_apply::validate_config_apply(
                &apply_cmd,
                config_apply_max_agents,
                config_apply_validation,
            ) {
                warn!(
                    error_count = errors.len(),
                    "Config-Apply im Tick-Loop abgelehnt (Re-Validierung) — keine Mutation"
                );
                continue;
            }
            let mode = apply_cmd.mode;
            info!(
                ?mode,
                agents = apply_cmd.agents.len(),
                rooms = apply_cmd.building.rooms.len(),
                "Config-Apply gestartet (tick-synchron)"
            );

            // 1. Pre-Apply Safety-Snapshot (Rollback-Punkt).
            {
                let es = world
                    .get_resource::<sentinel_ecs::LimboEventStore>()
                    .map(|e| Arc::clone(&e.0));
                let ss = world
                    .get_resource::<sentinel_ecs::RedbStateStore>()
                    .map(|r| r.store.clone());
                if let (Some(es), Some(ss)) = (es, ss) {
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
                        Ok(id) => info!(snapshot_id = %id, "Pre-Apply Safety-Snapshot erstellt"),
                        Err(e) => {
                            warn!(error = %e, "Pre-Apply Snapshot fehlgeschlagen (Apply wird fortgesetzt)")
                        }
                    }
                }
            }

            // 2. Raum-Maps idempotent neu bauen (Layout-Aenderungen wirken sofort).
            sentinel_ecs::rebuild_room_maps(&mut world, &apply_cmd.building);

            let mut spawned = 0u32;
            let mut updated = 0u32;
            let mut despawned = 0u32;
            let mut deferred_ids: Vec<AgentId> = Vec::new();
            // IDs der live-geaenderten Agents → gezielte Gateway-DNA-Invalidierung (#440).
            let mut changed_ids: Vec<u16> = Vec::new();

            match mode {
                sentinel_common::ApplyMode::Fresh => {
                    // Fresh-Load: gesamte Agent-Welt abbauen, dann Schicht-Agents neu spawnen.
                    for agent_id in world_agent_ids(&mut world) {
                        teardown_agent_full(
                            agent_id,
                            &mut world,
                            &mut runtime_orch,
                            &sandbox,
                            &mut sandbox_handles,
                            &mut ebpf_collector,
                            &mut agent_processes,
                            &security_runtime_state,
                        );
                        despawned += 1;
                    }
                    for cfg in agents_for_shift(&apply_cmd.agents, current_shift) {
                        if spawn_agent_full(
                            &mut runtime_orch,
                            &mut world,
                            cfg,
                            &sandbox,
                            &mut sandbox_handles,
                            &mut ebpf_collector,
                            &mut agent_processes,
                            &agent_command,
                            &security_runtime_state,
                            fs_mount.as_deref(),
                        ) {
                            spawned += 1;
                        }
                    }
                }
                sentinel_common::ApplyMode::Live => {
                    let diff =
                        crate::config_apply::compute_agent_diff(&all_agents, &apply_cmd.agents);
                    // Neue Agents: nur spawnen wenn in aktueller Schicht (sonst beim Schichtwechsel).
                    for cfg in &diff.spawn {
                        if (current_shift == 0 || cfg.identity.shift_set == current_shift)
                            && spawn_agent_full(
                                &mut runtime_orch,
                                &mut world,
                                cfg,
                                &sandbox,
                                &mut sandbox_handles,
                                &mut ebpf_collector,
                                &mut agent_processes,
                                &agent_command,
                                &security_runtime_state,
                                fs_mount.as_deref(),
                            )
                        {
                            spawned += 1;
                        }
                    }
                    // Geaenderte Agents: live aktualisieren, KEIN Despawn (Memory/Bio/Evolution bleibt).
                    for cfg in &diff.update {
                        if crate::config_apply::apply_agent_update(&mut world, cfg) {
                            updated += 1;
                            changed_ids.push(cfg.identity.id);
                            update_agent_projection_identity(&projection_db_path, cfg);
                        }
                    }
                    // Entfernte Agents: despawnen — aber CP-Heilung nicht stoeren (§6 L3 → deferren).
                    for agent_id in &diff.despawn {
                        if agent_under_active_healing(&runtime_health, *agent_id) {
                            warn!(agent_id = %agent_id, "Despawn deferred: Agent unter aktiver Control-Plane-Heilung (TOGAF §6 L3)");
                            deferred_ids.push(*agent_id);
                            continue;
                        }
                        teardown_agent_full(
                            *agent_id,
                            &mut world,
                            &mut runtime_orch,
                            &sandbox,
                            &mut sandbox_handles,
                            &mut ebpf_collector,
                            &mut agent_processes,
                            &security_runtime_state,
                        );
                        despawned += 1;
                    }
                }
            }

            // 3. Persistenz (config_dir Write-Back) — Daemon ist alleiniger Schreiber (#420).
            let persisted = match crate::config_persist::persist_company_config(
                &config_dir,
                &apply_cmd.agents,
                &apply_cmd.building,
                &tick_count.to_string(),
            ) {
                Ok(result) => {
                    info!(
                        agents = result.agents_written,
                        removed = result.agents_removed,
                        "Config in config_dir persistiert (ueberlebt Restart)"
                    );
                    true
                }
                Err(e) => {
                    error!(error = %e, "Config-Persistenz fehlgeschlagen — Laufzeit-Welt ist bereits aktualisiert (Safety-Snapshot erlaubt Rollback)");
                    false
                }
            };

            // 4. Angewandte Config fuer den naechsten Diff uebernehmen. Deferred (CP-Heilung) Agents
            //    bleiben erhalten, damit ihr Despawn beim naechsten Apply erneut versucht wird.
            let deferred_configs: Vec<AgentConfig> = deferred_ids
                .iter()
                .filter_map(|id| all_agents.iter().find(|a| a.identity.id == id.0).cloned())
                .collect();
            all_agents = apply_cmd.agents.clone();
            all_agents.extend(deferred_configs);

            // 5. ConfigApplied-DomainEvent (Audit + durabler Trigger fuer Gateway-DNA-Reload #440).
            let payload = DomainEventPayload::ConfigApplied {
                mode: format!("{mode:?}").to_lowercase(),
                spawned,
                updated,
                despawned,
                rooms_changed: apply_cmd.building.rooms.len() as u32,
                persisted,
            };
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let event = DomainEvent::new(
                payload.event_type_str(),
                "company",
                &payload.to_json(),
                &format!("config-apply-{now_ms}"),
                tick_count,
            );
            if let Some(es) = world
                .get_resource::<sentinel_ecs::LimboEventStore>()
                .map(|e| Arc::clone(&e.0))
            {
                if let Err(e) = es.append_event(&event) {
                    warn!(error = %e, "ConfigApplied-Event konnte nicht persistiert werden");
                }
            }

            // 6. Gateway-DNA-Cache-Invalidierung triggern (#440). Realer Trigger: fire-and-forget
            //    HTTP-POST an den Gateway-Control-Plane in einem detached Thread, damit der Tick-Loop
            //    NICHT blockiert. No-op wenn der Gateway aus ist (Connection refused → still ignoriert,
            //    VM-Default token-safe). Leere agent_ids (Fresh/Spawn-only) ⇒ Gateway invalidiert alle.
            if spawned + updated > 0 {
                let gateway_url = std::env::var("CORTEX_GATEWAY_URL")
                    .unwrap_or_else(|_| "http://localhost:8081".to_string());
                let ids = changed_ids.clone();
                std::thread::spawn(move || {
                    let body = serde_json::json!({ "agent_ids": ids });
                    match reqwest::blocking::Client::builder()
                        .timeout(Duration::from_secs(2))
                        .build()
                    {
                        Ok(client) => {
                            let url = format!("{gateway_url}/control/dna/invalidate");
                            match client.post(&url).json(&body).send() {
                                Ok(resp) => info!(
                                    status = %resp.status(),
                                    "Gateway-DNA-Invalidierung getriggert (#440)"
                                ),
                                Err(_) => info!(
                                    "Gateway-DNA-Invalidierung: Gateway nicht erreichbar — No-op (erwartet wenn inaktiv)"
                                ),
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "reqwest-Client fuer DNA-Invalidierung nicht baubar")
                        }
                    }
                });
                info!(
                    spawned,
                    updated,
                    changed = changed_ids.len(),
                    "Gateway-DNA-Cache-Invalidierung getriggert (#440, fire-and-forget)"
                );
            }

            info!(
                ?mode,
                spawned,
                updated,
                despawned,
                deferred = deferred_ids.len(),
                persisted,
                "Config-Apply abgeschlossen"
            );
        }

        // Nano-Container Live-Migration via Operator-API (#413): orchestrator-getriebener,
        // lokaler Snapshot->Restore-Handoff einer ECS-native-Instanz (Time-Machine-Audit).
        // Migriert wird die aus dem Live-Welt-Snapshot abgeleitete Instanz — die Live-Daemon-Welt
        // bleibt unangetastet (LOKAL). Cross-node/Netzwerk-Migration ist out-of-scope (Multi-Node-gated).
        while let Ok(migrate_cmd) = migrate_rx.try_recv() {
            let reason = if migrate_cmd.reason.trim().is_empty() {
                "manual".to_string()
            } else {
                migrate_cmd.reason.clone()
            };
            info!(%reason, "Nano-Container Live-Migration gestartet (lokal, ECS-native)");

            // 1. Read-only ECS-Snapshot der Live-Welt ziehen (Welt bleibt unangetastet).
            let ecs_snapshot = sentinel_ecs::snapshot_ecs_state(&mut world);
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let workload_id = format!("ecs-world-migrate-{now_ms}");

            // 2. Handoff via NanoRuntime-Vertrag (snapshot(source)->restore(target)->terminate) +
            //    lokale Migrations-Latenz messen.
            let started = std::time::Instant::now();
            match sentinel_runtime::migrate_ecs_native_instance(
                &ecs_snapshot,
                config_apply_max_agents,
                &workload_id,
            ) {
                Ok(outcome) => {
                    let duration_ms = started.elapsed().as_millis() as u64;

                    // 3. Telemetrie-Counter (Migrations-Gesamtzahl).
                    sentinel_telemetry::MetricsRegistry::global()
                        .counter("sentinel_daemon_migrations_total")
                        .increment();

                    // 4. MigrationCompleted-DomainEvent (Audit-Trail in der Time Machine).
                    let payload = DomainEventPayload::MigrationCompleted {
                        runtime_key: outcome.runtime_key,
                        workload_id: workload_id.clone(),
                        agent_count: outcome.agent_count,
                        from_handle: outcome.from_handle,
                        to_handle: outcome.to_handle,
                        duration_ms,
                        reason: reason.clone(),
                    };
                    let event = DomainEvent::new(
                        payload.event_type_str(),
                        "platform",
                        &payload.to_json(),
                        &format!("migrate-{now_ms}"),
                        tick_count,
                    );
                    if let Some(es) = world
                        .get_resource::<sentinel_ecs::LimboEventStore>()
                        .map(|e| Arc::clone(&e.0))
                    {
                        if let Err(e) = es.append_event(&event) {
                            warn!(error = %e, "MigrationCompleted-Event konnte nicht persistiert werden");
                        }
                    }
                    info!(
                        agents = outcome.agent_count,
                        duration_ms,
                        %reason,
                        "Nano-Container Live-Migration abgeschlossen (lokal)"
                    );
                }
                Err(e) => {
                    warn!(error = %e, "Nano-Container Live-Migration fehlgeschlagen");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controlplane::config::ControlplaneConfig;
    use crate::controlplane::store::ControlplaneStore;
    use sentinel_common::agent_config::{
        BackgroundConfig, IdentityConfig, PersonalityConfig, PreferencesConfig,
    };
    use sentinel_common::components::{BioState, Mood, Position, TaskState};
    use sentinel_common::{
        DomainEventPayload, EcsSnapshot, Emotion, EventType, FsMetadataDump, OperatorChaosCommand,
        OperatorCommand, RedbDump, SnapshotTier, TaskId, TaskStatus, WorldSnapshot,
    };
    use sentinel_ebpf::loader::MonitoringMode;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    static PROJECTION_RESTART_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn snap_meta(id: &str, tick: u64, last_event_id: i64) -> sentinel_common::SnapshotMeta {
        sentinel_common::SnapshotMeta {
            id: id.to_string(),
            tier: SnapshotTier::Hourly,
            tick,
            sim_hour: 0.0,
            last_event_id,
            payload_size_bytes: 0,
            created_at_ms: 0,
        }
    }

    #[test]
    fn select_anchor_requires_tick_strictly_before_target() {
        // #491 VM-Befund: in Ruhephasen teilen sich Snapshots dieselbe last_event_id. Ein Anchor
        // mit tick > target_tick ist verboten (sonst leeres/negatives Replay -> Anchor-Zustand statt
        // Ziel). tick == target_tick ist erlaubt (#529, eigener Test). Liste ist tick DESC.
        let snapshots = vec![
            snap_meta("newer", 2_540_056, 12_034_085), // gleiche le wie Ziel, aber tick NACH Ziel
            snap_meta("valid", 2_539_906, 12_034_085), // tick VOR Ziel -> korrekter Anchor
            snap_meta("older", 2_536_000, 12_030_000),
        ];
        let anchor = select_anchor_snapshot(&snapshots, 2_540_040, 12_034_085)
            .expect("ein gueltiger Anchor existiert");
        assert_eq!(
            anchor.id, "valid",
            "darf NICHT den tick-zu-neuen 'newer' waehlen"
        );

        // Ziel vor dem aeltesten Snapshot -> kein gueltiger Anchor.
        assert!(select_anchor_snapshot(&snapshots, 2_535_000, 12_029_000).is_none());

        // Direkt nach 'older': dessen le<=Ziel und tick<Ziel.
        let a2 = select_anchor_snapshot(&snapshots, 2_537_000, 12_031_000).expect("anchor");
        assert_eq!(a2.id, "older");
    }

    #[test]
    fn select_anchor_accepts_snapshot_exactly_at_target_tick() {
        // #529: ein Snapshot GENAU am Ziel-Tick (z.B. der erzwungene Post-Shift-Anker) ist ein
        // gueltiger Anchor -> leeres Replay-Fenster = exakter Ziel-Zustand, kein Replay ueber die
        // Schichtgrenze. (Bug #528 bleibt gefixt: tick > Ziel ist weiter ausgeschlossen.)
        let snapshots = vec![
            snap_meta("at_target", 2_540_040, 12_034_085), // tick == Ziel
            snap_meta("pre_shift", 2_539_900, 12_033_000), // vor dem Shift
        ];
        let anchor = select_anchor_snapshot(&snapshots, 2_540_040, 12_034_085).expect("anchor");
        assert_eq!(
            anchor.id, "at_target",
            "Snapshot am Ziel-Tick muss als Anchor gewaehlt werden (leeres Replay = exakt)"
        );
    }

    #[test]
    fn select_anchor_picks_post_shift_anchor_for_post_shift_target() {
        // #529-Kern: nach einem Schichtwechsel existiert ein erzwungener Post-Shift-Anker am
        // Shift-Tick. Fuer JEDES Ziel >= Shift-Tick waehlt select_anchor diesen (nicht den
        // Pre-Shift-Anker) -> das Fenster (anker, ziel] liegt innerhalb der neuen Schicht, kreuzt die
        // Schichtgrenze NIE. (DESC-Liste, find() nimmt den naechstgelegenen <= Ziel.)
        let snapshots = vec![
            snap_meta("post_shift", 2_540_000, 12_034_000), // erzwungener Anker am Shift-Tick
            snap_meta("pre_shift", 2_536_400, 12_030_000),  // letzter Pre-Shift-Intervall-Anker
        ];
        // Ziel mitten in der neuen Schicht.
        let a = select_anchor_snapshot(&snapshots, 2_540_500, 12_034_400).expect("anchor");
        assert_eq!(
            a.id, "post_shift",
            "Post-Shift-Ziel muss den Post-Shift-Anker waehlen -> kein Cross-Shift-Replay"
        );
        // Ziel exakt am Shift-Tick -> ebenfalls Post-Shift-Anker (leeres Replay).
        let a0 = select_anchor_snapshot(&snapshots, 2_540_000, 12_034_000).expect("anchor");
        assert_eq!(a0.id, "post_shift");
    }

    #[test]
    fn psi_band_matches_bio_thresholds() {
        // #491 (TM-3): Band-Ableitung nutzt exakt die Bio-Schwellen, strikt `>`.
        assert_eq!(psi_band_from_metrics(0.0, 0.0), (false, false));
        // Wert exakt auf der Schwelle -> false (konsistent mit apply_psi_stress).
        assert_eq!(
            psi_band_from_metrics(
                sentinel_ecs::PSI_CPU_STRESS_THRESHOLD,
                sentinel_ecs::PSI_MEM_STRESS_THRESHOLD
            ),
            (false, false)
        );
        assert_eq!(psi_band_from_metrics(99.0, 99.0), (true, true));
        assert_eq!(psi_band_from_metrics(99.0, 0.0), (true, false));
        assert_eq!(psi_band_from_metrics(0.0, 99.0), (false, true));
    }

    #[test]
    fn psi_band_emits_only_on_change() {
        // #491 (TM-3): sparse Emission — dieselbe Tracking-Logik wie der Tick-Loop. Nur echte
        // Band-Wechsel (inkl. der None->Baseline am ersten Tick) erzeugen ein Event.
        let metrics = [
            (0.0, 0.0),   // Baseline (false,false) -> emit 1
            (10.0, 20.0), // weiter (false,false) -> kein emit
            (99.0, 0.0),  // (true,false) -> emit 2
            (80.0, 10.0), // weiter (true,false) -> kein emit
            (99.0, 99.0), // (true,true) -> emit 3
            (0.0, 0.0),   // (false,false) -> emit 4
        ];
        let mut prev: Option<(bool, bool)> = None;
        let mut emits = 0;
        for (cpu, mem) in metrics {
            let band = psi_band_from_metrics(cpu, mem);
            if prev != Some(band) {
                prev = Some(band);
                emits += 1;
            }
        }
        assert_eq!(emits, 4, "nur 4 Band-Wechsel bei 6 Ticks");
    }

    #[test]
    fn judge_alert_agent_id_uses_configured_bounds() {
        let bounds = AgentIdBounds::new(120);

        assert_eq!(
            parse_judge_alert_agent_id("AGENT-75", bounds).unwrap(),
            AgentId(75)
        );
        assert!(parse_judge_alert_agent_id("AGENT-121", bounds).is_err());
        assert!(parse_judge_alert_agent_id("agent-75", bounds).is_err());
    }

    #[test]
    fn degraded_agent_recorded_in_security_runtime_state() {
        // #378/#375: Agenten ohne vollstaendige Sandbox (bwrap-Start-Fehler,
        // kein cgroup oder Sandbox-Setup-Fehler) muessen trotzdem im
        // Security-Runtime-State erscheinen — sonst verliert die Operator-
        // Security-API degradierte Agenten komplett aus der Sicht.
        let state: operator_api::SharedSecurityRuntimeState = Default::default();
        record_security_runtime_snapshot(&state, AgentId(7), "Thomas Mueller", None, None);

        let guard = state.read().unwrap();
        let snap = guard.get(&7).expect("degradierter Agent muss erfasst sein");
        assert_eq!(snap.agent_id, 7);
        assert_eq!(snap.aggregate_id, "AGENT-07");
        assert_eq!(snap.bwrap_pid, None, "degraded => kein bwrap-PID");
        assert_eq!(snap.fs_mount, None);
        assert_eq!(
            snap.home_host_path, "/ram/agents/Thomas Mueller",
            "ohne fs_mount faellt der Home-Pfad auf /ram/agents/<name> zurueck"
        );
    }

    #[test]
    fn healthy_agent_records_pid_and_mount_home() {
        // Kontrast zum degraded-Pfad: mit bwrap-PID + Mount wird der Home-Pfad
        // aus Mount + aggregate_id gebildet (nicht der /ram-Fallback).
        let state: operator_api::SharedSecurityRuntimeState = Default::default();
        record_security_runtime_snapshot(
            &state,
            AgentId(3),
            "Lisa Brenner",
            Some(4242),
            Some("/cas/mnt"),
        );

        let guard = state.read().unwrap();
        let snap = guard.get(&3).expect("Agent muss erfasst sein");
        assert_eq!(snap.bwrap_pid, Some(4242));
        assert_eq!(snap.fs_mount.as_deref(), Some("/cas/mnt"));
        assert_eq!(snap.home_host_path, "/cas/mnt/AGENT-03");
    }

    fn record_projection_restart(_service_name: &str) -> bool {
        PROJECTION_RESTART_CALLS.fetch_add(1, Ordering::SeqCst);
        true
    }

    fn projection_service_active(_service_name: &str) -> bool {
        true
    }

    #[test]
    fn nightrun_events_persist_replayable_hash_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");
        let event_store = EventStore::open(events_path.to_str().unwrap()).unwrap();
        let run_id = "shift-tick-42-shift-1-to-2";
        let mut chain = NightrunHashChain::new(run_id, run_id);

        append_nightrun_event(
            &event_store,
            DomainEventPayload::NightRunStarted {
                run_id: run_id.to_string(),
                trigger_shift_set: 1,
                agents_queued: 1,
            },
            "nightrun",
            run_id,
            42,
            Some(&mut chain),
        )
        .unwrap();
        append_nightrun_event(
            &event_store,
            DomainEventPayload::AgentConsolidated {
                run_id: run_id.to_string(),
                agent_name: "Anna".to_string(),
                episodes_processed: 3,
                episodes_consolidated: 1,
                duration_ms: 7,
            },
            "AGENT-01",
            run_id,
            42,
            Some(&mut chain),
        )
        .unwrap();
        let expected_hash = chain.current_hash();
        append_nightrun_event(
            &event_store,
            DomainEventPayload::NightRunCompleted {
                run_id: run_id.to_string(),
                trigger_shift_set: 1,
                agents_consolidated: 1,
                agents_failed: 0,
                agents_skipped: 0,
                total_episodes: 3,
                total_episodes_consolidated: 1,
                nmda_selection_rate: Some(1.0 / 3.0),
                nmda_threshold: Some(NMDA_CONSOLIDATION_THRESHOLD),
                nmda_max_consolidation_episodes: Some(NMDA_MAX_CONSOLIDATION_EPISODES as u32),
                nmda_score_min: Some(0.1),
                nmda_score_avg: Some(0.2),
                nmda_score_max: Some(0.3),
                duration_ms: 9,
                hash_chain: Some(expected_hash.clone()),
            },
            "nightrun",
            run_id,
            42,
            None,
        )
        .unwrap();

        let events = event_store.get_events_by_correlation(run_id, 10).unwrap();
        assert_eq!(events.len(), 3);

        let mut replay_chain = NightrunHashChain::new(run_id, run_id);
        for event in events
            .iter()
            .filter(|event| event.event_type != "nightrun_completed")
        {
            replay_chain.extend(event);
        }
        assert_eq!(replay_chain.current_hash(), expected_hash);

        let completed = events
            .iter()
            .find(|event| event.event_type == "nightrun_completed")
            .unwrap();
        let payload: DomainEventPayload = serde_json::from_str(&completed.payload).unwrap();
        match payload {
            DomainEventPayload::NightRunCompleted {
                hash_chain,
                total_episodes,
                total_episodes_consolidated,
                nmda_selection_rate,
                nmda_threshold,
                nmda_max_consolidation_episodes,
                nmda_score_min,
                nmda_score_avg,
                nmda_score_max,
                ..
            } => {
                assert_eq!(hash_chain.as_deref(), Some(expected_hash.as_str()));
                assert_eq!(total_episodes, 3);
                assert_eq!(total_episodes_consolidated, 1);
                assert_eq!(nmda_selection_rate, Some(1.0 / 3.0));
                assert_eq!(nmda_threshold, Some(NMDA_CONSOLIDATION_THRESHOLD));
                assert_eq!(
                    nmda_max_consolidation_episodes,
                    Some(NMDA_MAX_CONSOLIDATION_EPISODES as u32)
                );
                assert_eq!(nmda_score_min, Some(0.1));
                assert_eq!(nmda_score_avg, Some(0.2));
                assert_eq!(nmda_score_max, Some(0.3));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    /// #493 AC-5 (couples #491): a discarded future must NEVER enter the bounded-replay hash chain.
    /// The live timeline is A,B,E,F; C,D are the old future a restore discards (dead interval (2,4]).
    /// Replaying the guarded range must reproduce the ground-truth forward hash of A,B,E,F exactly
    /// (STRICT/CORE) with C,D never contributing — the most dangerous Time-Machine break if regressed.
    #[test]
    fn dead_branch_excluded_from_replay_hash_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let event_store = EventStore::open(tmp.path().join("events.db").to_str().unwrap()).unwrap();
        let run_id = "replay-dead-boundary";
        let mk = |name: &str, tick: u64| {
            sentinel_common::DomainEvent::new(
                "agent_action_received",
                "AGENT-01",
                &format!("{{\"step\":\"{name}\"}}"),
                run_id,
                tick,
            )
        };

        let a = mk("A", 1);
        event_store.append_event(&a).unwrap();
        let b = mk("B", 2);
        event_store.append_event(&b).unwrap();
        // old future C,D (ids 3,4) — discarded by the restore below
        let c = mk("C", 3);
        event_store.append_event(&c).unwrap();
        let d = mk("D", 4);
        event_store.append_event(&d).unwrap();

        event_store.increment_restore_generation().unwrap();
        event_store.push_dead_range(2, 4).unwrap(); // (2,4] dead -> ids 3,4

        // live continuation E,F (ids 5,6)
        let e = mk("E", 3);
        event_store.append_event(&e).unwrap();
        let f = mk("F", 4);
        event_store.append_event(&f).unwrap();

        // ground-truth forward hash over the LIVE timeline A,B,E,F (C,D never applied)
        let mut truth = NightrunHashChain::new(run_id, run_id);
        for ev in [&a, &b, &e, &f] {
            truth.extend(ev);
        }

        // replay the full range through the guarded read (the #491 bounded-replay input)
        let replayed = event_store.get_events_range(0, 6).unwrap();
        assert_eq!(replayed.len(), 4, "C,D excluded from the replay input");
        let mut chain = NightrunHashChain::new(run_id, run_id);
        for ev in &replayed {
            chain.extend(ev);
        }
        assert_eq!(
            chain.current_hash(),
            truth.current_hash(),
            "STRICT/CORE: a discarded future never replays into the restored world"
        );
        for dead in [&c, &d] {
            assert!(
                !replayed.iter().any(|ev| ev.event_id == dead.event_id),
                "dead event {} replayed",
                dead.event_id
            );
        }
    }

    /// Erstellt EbpfCollector + tokio mpsc Sender fuer Tests (Userspace mode, kein tokio noetig).
    fn test_ebpf() -> (EbpfCollector, tokio::sync::mpsc::Sender<MetricsSnapshot>) {
        let collector = EbpfCollector::new(MonitoringMode::Userspace);
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        (collector, tx)
    }

    #[test]
    fn mountinfo_contains_mountpoint_matches_exact_sentinel_fuse_mount() {
        let mountinfo = "\
35 23 0:33 / / rw,relatime - overlay overlay rw\n\
92 35 0:56 / /opt/sentinel/fs rw,nosuid,nodev - fuse sentinel-fs rw,user_id=0\n";

        assert!(mountinfo_contains_mountpoint(
            mountinfo,
            std::path::Path::new("/opt/sentinel/fs")
        ));
        assert!(!mountinfo_contains_mountpoint(
            mountinfo,
            std::path::Path::new("/opt/sentinel/fs-missing")
        ));
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
            runtime: Default::default(),
            capabilities: Default::default(),
        }
    }

    fn empty_redb_dump() -> RedbDump {
        RedbDump {
            agent_states: Vec::new(),
            room_states: Vec::new(),
            personalities: Vec::new(),
            relationships: Vec::new(),
            voice_styles: Vec::new(),
            behavioral_notes: Vec::new(),
            narrative_summaries: Vec::new(),
            evolution_versions: Vec::new(),
            nmda_scores: Vec::new(),
            agent_facts: Vec::new(),
            sim_meta: Vec::new(),
            api_patterns: Vec::new(),
        }
    }

    fn restore_snapshot_with_one_agent() -> WorldSnapshot {
        WorldSnapshot {
            snapshot_id: "snap-restore-test".to_string(),
            schema_version: WorldSnapshot::SCHEMA_VERSION,
            tick: 420,
            sim_hour: 9.5,
            timestamp_ms: 1_700_000_000_000,
            tier: SnapshotTier::Hourly,
            last_event_id: 321,
            redb: empty_redb_dump(),
            ecs: EcsSnapshot {
                positions: vec![(
                    1,
                    Position {
                        room_id: "labor".to_string(),
                        in_transit: false,
                        transit_target: None,
                        transit_remaining_ms: 0,
                        transit_correlation_id: None,
                        transit_route: Vec::new(),
                        transit_total_ms: 0,
                        transit_paused: false,
                        transit_pause_tick: 0,
                        transit_source: None,
                    },
                )],
                bio_states: vec![(
                    1,
                    BioState {
                        hunger: 33.0,
                        energy: 66.0,
                        caffeine_mg: 12.0,
                        bladder: 22.0,
                        stress: 11.0,
                        social_need: 44.0,
                        comfort: 77.0,
                    },
                )],
                personalities: Vec::new(),
                moods: vec![(
                    1,
                    Mood {
                        valence: 0.4,
                        arousal: 0.3,
                        dominant_emotion: Emotion::Focused,
                    },
                )],
                perception_states: Vec::new(),
                work_contexts: Vec::new(),
                agent_capabilities: Vec::new(),
                event_queues: Vec::new(),
                identities: vec![(
                    1,
                    AgentIdentity {
                        agent_id: AgentId(1),
                        name: "Restore Agent".to_string(),
                        role: "Operator".to_string(),
                    },
                )],
                shift_infos: vec![(
                    1,
                    ShiftInfo {
                        shift_set: 2,
                        shift_start_hour: 14,
                        shift_end_hour: 22,
                        is_on_duty: true,
                    },
                )],
                relationships: Vec::new(),
                llm_configs: Vec::new(),
                task_states: vec![TaskState {
                    task_id: TaskId(42),
                    title: "Restore Task".to_string(),
                    description: "Projection restore fixture".to_string(),
                    assigned_to: AgentId(1),
                    assigned_by: Some(AgentId(2)),
                    parent_task: None,
                    status: TaskStatus::InProgress,
                    created_tick: 400,
                    updated_tick: 420,
                    result: Some("seeded".to_string()),
                }],
                sim_tick: 420,
                sim_hour: 9.5,
                sim_delta_seconds: 1.0,
                active_chaos_json: Vec::new(),
                active_stimuli_json: Vec::new(),
                autonomy_cooldowns: Vec::new(),
                smells_json: Vec::new(),
                room_chat_json: Vec::new(),
                gaia_json: Vec::new(),
                broadcast_json: Vec::new(),
            },
            projection_offsets: vec![("sentinel-projection".to_string(), 321)],
            fs_metadata: None,
        }
    }

    fn restore_snapshot_for_agent(
        snapshot_id: &str,
        agent_id: u16,
        name: &str,
        room_id: &str,
        tick: u64,
        last_event_id: i64,
        redb_agent_state: &[u8],
        fs_metadata: Option<FsMetadataDump>,
    ) -> WorldSnapshot {
        let mut snapshot = restore_snapshot_with_one_agent();
        snapshot.snapshot_id = snapshot_id.to_string();
        snapshot.tick = tick;
        snapshot.sim_hour = 8.0 + (agent_id as f32 / 100.0);
        snapshot.last_event_id = last_event_id;
        snapshot.redb.agent_states = vec![(agent_id, redb_agent_state.to_vec())];
        snapshot.ecs.identities = vec![(
            agent_id,
            AgentIdentity {
                agent_id: AgentId(agent_id),
                name: name.to_string(),
                role: "Operator".to_string(),
            },
        )];
        snapshot.ecs.positions = vec![(
            agent_id,
            Position {
                room_id: room_id.to_string(),
                in_transit: false,
                transit_target: None,
                transit_remaining_ms: 0,
                transit_correlation_id: None,
                transit_route: Vec::new(),
                transit_total_ms: 0,
                transit_paused: false,
                transit_pause_tick: 0,
                transit_source: None,
            },
        )];
        snapshot.ecs.bio_states = Vec::new();
        snapshot.ecs.moods = Vec::new();
        snapshot.ecs.shift_infos = Vec::new();
        snapshot.ecs.task_states = Vec::new();
        snapshot.ecs.sim_tick = tick;
        snapshot.projection_offsets = vec![("sentinel-projection".to_string(), last_event_id)];
        snapshot.fs_metadata = fs_metadata;
        snapshot
    }

    fn save_world_snapshot_fixture(event_store: &EventStore, snapshot: &WorldSnapshot) {
        let bytes = sentinel_common::encode_world_snapshot(snapshot).unwrap();
        event_store
            .save_world_snapshot(
                &snapshot.snapshot_id,
                &snapshot.tier.to_string(),
                snapshot.tick,
                snapshot.sim_hour,
                snapshot.last_event_id,
                &bytes,
            )
            .unwrap();
    }

    #[test]
    fn should_run_periodic_runtime_reconcile_respects_due_rules() {
        let mut config = crate::config::PlatformControlplaneConfig::default();

        assert!(!should_run_periodic_runtime_reconcile(&config, 0));
        assert!(!should_run_periodic_runtime_reconcile(&config, 59));
        assert!(should_run_periodic_runtime_reconcile(&config, 60));
        assert!(should_run_periodic_runtime_reconcile(&config, 120));

        config.runtime_reconcile_enabled = false;
        assert!(!should_run_periodic_runtime_reconcile(&config, 60));

        config.runtime_reconcile_enabled = true;
        config.runtime_reconcile_interval_ticks = 0;
        assert!(should_run_periodic_runtime_reconcile(&config, 1));
        assert!(should_run_periodic_runtime_reconcile(&config, 2));
        assert!(!should_run_periodic_runtime_reconcile(&config, 0));
    }

    #[test]
    fn restore_fence_blocks_due_periodic_runtime_reconcile() {
        let config = crate::config::PlatformControlplaneConfig::default();
        let mut fence = RestoreFence::default();

        assert!(should_run_periodic_runtime_reconcile_unfenced(
            &config, 60, &fence
        ));

        let owner_epoch = fence.begin();
        assert_eq!(owner_epoch, 1);
        assert!(fence.is_active());
        assert!(!should_run_periodic_runtime_reconcile_unfenced(
            &config, 60, &fence
        ));

        fence.end();
        assert!(should_run_periodic_runtime_reconcile_unfenced(
            &config, 60, &fence
        ));
    }

    #[test]
    fn restore_validate_rejects_missing_cas_blob_before_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let missing_hash = [7_u8; 32];
        let missing_metadata = FsMetadataDump {
            refcounts: vec![(missing_hash, 1)],
            ..FsMetadataDump::default()
        };

        let err = validate_fs_metadata_blobs(tmp.path(), &missing_metadata)
            .expect_err("fehlender CAS-Blob muss vor Commit scheitern");
        assert!(
            err.to_string().contains("fehlende CAS-Blobs"),
            "unexpected error: {err:?}"
        );

        let cas = sentinel_fs::cas::CasStore::open(tmp.path()).unwrap();
        let (existing_hash, _) = cas.store(b"restore blob").unwrap();
        let valid_metadata = FsMetadataDump {
            refcounts: vec![(existing_hash, 1), (missing_hash, 0)],
            ..FsMetadataDump::default()
        };
        validate_fs_metadata_blobs(tmp.path(), &valid_metadata)
            .expect("existierender Blob und zero-ref Trash duerfen validieren");
    }

    fn run_mid_commit_rollback_case(failure_point: RestoreCommitFailurePoint) {
        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");
        let state_path = tmp.path().join("state.redb");
        let projection_path = tmp.path().join("projection.db");
        let fs_meta_path = tmp.path().join("fs-meta.redb");
        let event_store = Arc::new(EventStore::open(events_path.to_str().unwrap()).unwrap());
        let state_store = Arc::new(StateStore::open(state_path.to_str().unwrap()).unwrap());
        let projection_store =
            sentinel_projection::ReadModelStore::open(projection_path.to_str().unwrap()).unwrap();
        drop(projection_store);

        let cas = sentinel_fs::cas::CasStore::open(tmp.path()).unwrap();
        let (pre_hash, _) = cas.store(b"pre fs blob").unwrap();
        let (target_hash, _) = cas.store(b"target fs blob").unwrap();
        let fs_meta = sentinel_fs::metadata::MetadataStore::open(&fs_meta_path).unwrap();
        let fs_layer = sentinel_fs::layer::LayerManager::new(cas, fs_meta);

        let pre_snapshot = restore_snapshot_for_agent(
            "pre-snapshot",
            2,
            "Pre Agent",
            "pre-room",
            100,
            10,
            b"pre-redb-state",
            Some(FsMetadataDump {
                refcounts: vec![(pre_hash, 1)],
                ..FsMetadataDump::default()
            }),
        );
        let target_snapshot = restore_snapshot_for_agent(
            "target-snapshot",
            1,
            "Target Agent",
            "target-room",
            200,
            20,
            b"target-redb-state",
            Some(FsMetadataDump {
                refcounts: vec![(target_hash, 1)],
                ..FsMetadataDump::default()
            }),
        );
        save_world_snapshot_fixture(&event_store, &pre_snapshot);

        state_store
            .restore_all_tables(&pre_snapshot.redb)
            .expect("pre redb fixture");
        fs_layer
            .meta()
            .restore_all_tables(pre_snapshot.fs_metadata.as_ref().unwrap())
            .expect("pre fs fixture");

        let (mut world, mut schedule) = create_simulation_world();
        sentinel_ecs::restore_ecs_state(&mut world, &pre_snapshot.ecs);
        seed_projection_from_world_snapshot(
            projection_path.to_str().unwrap(),
            &pre_snapshot,
            pre_snapshot.last_event_id,
            1,
        )
        .expect("pre projection fixture");

        // #491: reiner Snapshot-Punkt-Restore (kein Replay) — Failure-Injection wie zuvor.
        let commit_error = commit_world_restore_stores(
            &target_snapshot.snapshot_id,
            &target_snapshot,
            7,
            &mut world,
            &mut schedule,
            None,
            true,
            &event_store,
            &state_store,
            Some(&fs_layer),
            projection_path.to_str().unwrap(),
            failure_point,
        )
        .expect_err("failure injection muss Commit abbrechen");
        assert!(
            commit_error
                .to_string()
                .contains("injected restore commit failure"),
            "unexpected commit error: {commit_error:?}"
        );

        rollback_world_restore_stores(
            &pre_snapshot.snapshot_id,
            &mut world,
            &event_store,
            &state_store,
            Some(&fs_layer),
            tmp.path(),
            projection_path.to_str().unwrap(),
        )
        .expect("mid-commit rollback to pre snapshot");

        assert_eq!(
            state_store.get_agent_state(AgentId(2)).unwrap().as_deref(),
            Some(b"pre-redb-state".as_slice())
        );
        assert!(state_store.get_agent_state(AgentId(1)).unwrap().is_none());
        assert_eq!(world_agent_ids(&mut world), vec![AgentId(2)]);

        let fs_dump = fs_layer.meta().dump_all_tables().unwrap();
        assert_eq!(fs_dump.refcounts, vec![(pre_hash, 1)]);

        let db = sentinel_limbo::rusqlite::Connection::open(&projection_path).unwrap();
        let pre_agent_name: String = db
            .query_row(
                "SELECT name FROM agent_live_view WHERE agent_id = 2",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pre_agent_name, "Pre Agent");
        let target_projection_rows: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM agent_live_view WHERE agent_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(target_projection_rows, 0);

        let events = event_store.get_all_events().unwrap();
        assert!(
            events
                .iter()
                .all(|event| event.event_type != "snapshot_restored"),
            "Rollbackbarer Mid-Commit-Fehler darf kein SnapshotRestored-Event hinterlassen"
        );
    }

    #[test]
    fn mid_commit_failures_roll_back_to_pre_snapshot_without_mixed_state() {
        for failure_point in [
            RestoreCommitFailurePoint::AfterRedb,
            RestoreCommitFailurePoint::AfterFs,
            RestoreCommitFailurePoint::AfterEcs,
            RestoreCommitFailurePoint::AfterProjection,
        ] {
            run_mid_commit_rollback_case(failure_point);
        }
    }

    #[test]
    fn rollback_failure_keeps_restore_fence_active_and_reports_critical() {
        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");
        let state_path = tmp.path().join("state.redb");
        let projection_path = tmp.path().join("projection.db");
        let event_store = Arc::new(EventStore::open(events_path.to_str().unwrap()).unwrap());
        let state_store = Arc::new(StateStore::open(state_path.to_str().unwrap()).unwrap());
        let projection_store =
            sentinel_projection::ReadModelStore::open(projection_path.to_str().unwrap()).unwrap();
        drop(projection_store);
        let (mut world, _schedule) = create_simulation_world();
        let mut fence = RestoreFence::default();
        fence.begin();

        let err = rollback_world_restore_after_commit_failure(
            anyhow!("commit failed"),
            "missing-pre-snapshot",
            &mut fence,
            &mut world,
            &event_store,
            &state_store,
            None,
            tmp.path(),
            projection_path.to_str().unwrap(),
        )
        .expect_err("fehlender Pre-Snapshot muss kritischen Rollback-Fehler liefern");

        assert!(
            fence.is_active(),
            "Fence muss bei Rollback-Fehler aktiv bleiben"
        );
        assert!(
            format!("{err:?}").contains("critical restore rollback failure"),
            "unexpected rollback error: {err:?}"
        );
    }

    #[test]
    fn projection_restore_seed_resets_future_views_and_seeds_snapshot_state() {
        let tmp = tempfile::tempdir().unwrap();
        let projection_path = tmp.path().join("projection.db");
        let projection_store =
            sentinel_projection::ReadModelStore::open(projection_path.to_str().unwrap()).unwrap();
        drop(projection_store);

        let db = sentinel_limbo::rusqlite::Connection::open(&projection_path).unwrap();
        db.execute(
            "INSERT INTO agent_live_view
             (agent_id, name, role, shift_set, status, current_room, last_event_id, updated_at)
             VALUES (99, 'Stale', 'Old', 1, 'active', 'stale-room', 999, 1)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO room_live_view
             (room_id, occupant_count, transit_count, temperature, co2_ppm, noise_db, active_smells, last_event_id, updated_at)
             VALUES ('stale-room', 99, 0, 19.0, 500.0, 30.0, 'old-smell', 999, 1)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO kpi_1m
             (bucket_start, active_agents, total_actions, total_transits, chaos_events,
              tick_count, shift_changes, nightrun_events, last_event_id, updated_at)
             VALUES (60, 99, 9, 8, 7, 6, 5, 4, 999, 1)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO task_kanban
             (task_id, title, assigned_to, assigned_by, parent_task, status, result, last_event_id, updated_at)
             VALUES (999, 'Stale Task', 99, NULL, NULL, 'done', 'future', 999, 1)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT OR REPLACE INTO projection_watermarks (projection_name, last_event_id, updated_at)
             VALUES ('sentinel-projection', 999, 1)",
            [],
        )
        .unwrap();
        drop(db);

        let snapshot = restore_snapshot_with_one_agent();
        let report = seed_projection_from_world_snapshot(
            projection_path.to_str().unwrap(),
            &snapshot,
            321,
            654,
        )
        .expect("Projection Restore Seed");

        assert_eq!(report.agents_seeded, 1);
        assert_eq!(report.rooms_seeded, 1);
        assert_eq!(report.tasks_seeded, 1);
        assert_eq!(report.kpi_rows_seeded, 1);
        assert_eq!(report.watermarks_seeded, 1);

        let db = sentinel_limbo::rusqlite::Connection::open(&projection_path).unwrap();
        let stale_rows: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM agent_live_view WHERE agent_id = 99",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stale_rows, 0);
        let stale_task_rows: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM task_kanban WHERE task_id = 999",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stale_task_rows, 0);

        let agent_row: (String, String, String, i64, f64, f64, f64, String, i64) = db
            .query_row(
                "SELECT name, role, current_room, shift_set, hunger, energy, caffeine_mg, mood, last_event_id
                 FROM agent_live_view WHERE agent_id = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(agent_row.0, "Restore Agent");
        assert_eq!(agent_row.1, "Operator");
        assert_eq!(agent_row.2, "labor");
        assert_eq!(agent_row.3, 2);
        assert_eq!(agent_row.4, 33.0);
        assert_eq!(agent_row.5, 66.0);
        assert_eq!(agent_row.6, 12.0);
        assert_eq!(agent_row.7, "Focused");
        assert_eq!(agent_row.8, 321);

        let room_row: (i64, Option<f64>, Option<f64>, Option<f64>, Option<String>) = db
            .query_row(
                "SELECT occupant_count, temperature, co2_ppm, noise_db, active_smells
                 FROM room_live_view WHERE room_id = 'labor'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(room_row.0, 1);
        assert_eq!(room_row.1, None, "RoomPhysicsState bleibt out of scope");
        assert_eq!(room_row.2, None, "RoomPhysicsState bleibt out of scope");
        assert_eq!(room_row.3, None, "RoomPhysicsState bleibt out of scope");
        assert_eq!(room_row.4, None, "Room smells bleiben out of scope");

        let kpi_row: (i64, i64, i64, i64) = db
            .query_row(
                "SELECT active_agents, total_actions, tick_count, last_event_id FROM kpi_1m",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(kpi_row, (1, 0, 420, 321));

        let task_row: (String, i64, Option<i64>, String, Option<String>, i64) = db
            .query_row(
                "SELECT title, assigned_to, assigned_by, status, result, last_event_id
                 FROM task_kanban WHERE task_id = 42",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(task_row.0, "Restore Task");
        assert_eq!(task_row.1, 1);
        assert_eq!(task_row.2, Some(2));
        assert_eq!(task_row.3, "in_progress");
        assert_eq!(task_row.4.as_deref(), Some("seeded"));
        assert_eq!(task_row.5, 321);

        let watermark: i64 = db
            .query_row(
                "SELECT last_event_id FROM projection_watermarks WHERE projection_name = 'sentinel-projection'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(watermark, 321);
    }

    #[test]
    fn world_restore_teardown_removes_runtime_fragments_without_deleting_ecs() {
        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");
        let event_store = Arc::new(EventStore::open(events_path.to_str().unwrap()).unwrap());
        let mut runtime_orch = RuntimeOrchestrator::new(10).with_event_store(event_store);
        let (mut world, _schedule) = create_simulation_world();
        spawn_agent(
            &mut world,
            AgentId(1),
            "Restore Agent",
            "Operator",
            2,
            "labor",
        );

        runtime_orch
            .spawn_agent(
                AgentIdentity {
                    agent_id: AgentId(1),
                    name: "Restore Agent".to_string(),
                    role: "Operator".to_string(),
                },
                ShiftInfo {
                    shift_set: 2,
                    shift_start_hour: 14,
                    shift_end_hour: 22,
                    is_on_duty: true,
                },
                "labor",
            )
            .unwrap();
        let security_runtime_state: operator_api::SharedSecurityRuntimeState = Default::default();
        record_security_runtime_snapshot(
            &security_runtime_state,
            AgentId(1),
            "Restore Agent",
            None,
            None,
        );

        let sandbox = test_sandbox();
        let (mut ebpf_collector, _ebpf_tx) = test_ebpf();
        let mut sandbox_handles = HashMap::new();
        let mut agent_processes = HashMap::new();

        let removed = teardown_runtime_for_world_restore(
            &mut runtime_orch,
            &sandbox,
            &mut sandbox_handles,
            &mut ebpf_collector,
            &mut agent_processes,
            &security_runtime_state,
        );

        assert_eq!(removed, 1);
        assert!(!runtime_orch.agents().contains_key(&AgentId(1)));
        assert!(!security_runtime_state.read().unwrap().contains_key(&1));
        assert!(
            world_agent_ids(&mut world).contains(&AgentId(1)),
            "World-Restore darf restored ECS nicht als Runtime-Cleanup loeschen"
        );
    }

    #[test]
    fn spawn_agent_full_split_preserves_startup_shift_and_config_apply_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");
        let event_store = Arc::new(EventStore::open(events_path.to_str().unwrap()).unwrap());
        let mut runtime_orch = RuntimeOrchestrator::new(10).with_event_store(event_store);
        let (mut world, _schedule) = create_simulation_world();
        let sandbox = test_sandbox();
        let (mut ebpf_collector, _ebpf_tx) = test_ebpf();
        let mut sandbox_handles = HashMap::new();
        let mut agent_processes = HashMap::new();
        let security_runtime_state: operator_api::SharedSecurityRuntimeState = Default::default();
        let agent_command = vec!["true".to_string()];

        let cases = [
            ("startup", test_agent_config(1, "Startup Agent", "Ops", 1)),
            ("shift", test_agent_config(2, "Shift Agent", "Ops", 2)),
            (
                "config_apply",
                test_agent_config(3, "Config Apply Agent", "Ops", 3),
            ),
        ];

        for (path_name, cfg) in cases {
            assert!(
                spawn_agent_full(
                    &mut runtime_orch,
                    &mut world,
                    &cfg,
                    &sandbox,
                    &mut sandbox_handles,
                    &mut ebpf_collector,
                    &mut agent_processes,
                    &agent_command,
                    &security_runtime_state,
                    None,
                ),
                "{path_name} spawn path failed"
            );
            let agent_id = AgentId(cfg.identity.id);
            assert!(
                runtime_orch.agents().contains_key(&agent_id),
                "{path_name} missing RuntimeOrchestrator entry"
            );
            assert!(
                world_agent_ids(&mut world).contains(&agent_id),
                "{path_name} missing ECS entity"
            );
            assert!(
                security_runtime_state
                    .read()
                    .unwrap()
                    .contains_key(&agent_id.0),
                "{path_name} missing security runtime snapshot"
            );
            if let Some(handle) = sandbox_handles.get(&agent_id) {
                assert_eq!(handle.agent_name, cfg.identity.name);
            }
        }
    }

    #[test]
    fn periodic_runtime_reconcile_request_is_explicit_and_configured() {
        let config = crate::config::PlatformControlplaneConfig {
            runtime_reconcile_respawn_missing: false,
            runtime_reconcile_projection_rebuild: false,
            ..crate::config::PlatformControlplaneConfig::default()
        };

        let request = periodic_runtime_reconcile_request(&config);

        assert!(!request.dry_run);
        assert!(!request.respawn_missing);
        assert!(!request.projection_rebuild);
    }

    #[test]
    fn runtime_reconcile_records_operator_and_periodic_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");
        let projection_path = tmp.path().join("projection.db");
        let event_store = Arc::new(EventStore::open(events_path.to_str().unwrap()).unwrap());
        let (mut world, _schedule) = create_simulation_world();
        let sandbox = test_sandbox();
        let (mut ebpf_collector, _ebpf_tx) = test_ebpf();
        let mut runtime_orch =
            RuntimeOrchestrator::new(10).with_event_store(Arc::clone(&event_store));
        let mut sandbox_handles = HashMap::new();
        let mut agent_processes = HashMap::new();
        let security_runtime_state = Arc::new(RwLock::new(HashMap::new()));
        let runtime_health = Arc::new(RwLock::new(
            crate::runtime_health::RuntimeHealthSnapshot::default(),
        ));
        let mut respawn_backoff = RespawnBackoffTracker::new(3);
        let mut reconcile_ctx = RuntimeReconcileContext {
            tick_count: 10,
            current_shift: 1,
            all_agents: &[],
            world: &mut world,
            runtime_orch: &mut runtime_orch,
            sandbox: &sandbox,
            sandbox_handles: &mut sandbox_handles,
            ebpf_collector: &mut ebpf_collector,
            agent_processes: &mut agent_processes,
            agent_command: &[],
            security_runtime_state: &security_runtime_state,
            event_store: &event_store,
            runtime_health: &runtime_health,
            projection_db_path: &projection_path,
            operator_auth_required: false,
            service_health_state: crate::service_health::ServiceHealthWorkerSnapshot::default(),
            fs_mount: None,
            data_dir: tmp.path(),
            restart_service_fn: record_projection_restart,
            is_service_active_fn: projection_service_active,
        };
        let request = RuntimeReconcileRequest {
            dry_run: true,
            projection_rebuild: false,
            respawn_missing: false,
        };

        let operator_response = run_runtime_reconcile(
            &mut reconcile_ctx,
            request.clone(),
            &mut respawn_backoff,
            RuntimeReconcileSource::Operator,
        );
        assert!(operator_response.accepted);
        {
            let snapshot = runtime_health.read().unwrap();
            assert_eq!(snapshot.reconcile_runs_total, 1);
            assert_eq!(snapshot.auto_reconcile_runs_total, 0);
            assert_eq!(snapshot.last_reconcile_tick, 10);
            assert_eq!(snapshot.last_reconcile_source, "operator");
        }

        reconcile_ctx.tick_count = 60;
        let periodic_response = run_runtime_reconcile(
            &mut reconcile_ctx,
            request,
            &mut respawn_backoff,
            RuntimeReconcileSource::Periodic,
        );
        assert!(periodic_response.accepted);
        {
            let snapshot = runtime_health.read().unwrap();
            assert_eq!(snapshot.reconcile_runs_total, 2);
            assert_eq!(snapshot.auto_reconcile_runs_total, 1);
            assert_eq!(snapshot.last_reconcile_tick, 60);
            assert_eq!(snapshot.last_reconcile_source, "periodic");
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
    fn runtime_reconcile_projection_seed_persists_event_and_live_view() {
        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");
        let projection_path = tmp.path().join("projection.db");
        let event_store = EventStore::open(events_path.to_str().unwrap()).unwrap();
        let projection_store =
            sentinel_projection::ReadModelStore::open(projection_path.to_str().unwrap()).unwrap();
        drop(projection_store);

        let agent_cfg = test_agent_config(46, "Ralf Steinbach", "Operations", 1);
        let row_id = emit_runtime_projection_spawn_event(&event_store, &agent_cfg, 2095680)
            .expect("projection seed event");
        upsert_agent_projection_seed(&projection_path, &agent_cfg, row_id)
            .expect("projection seed upsert");

        let events = event_store.get_events_since(0, 10).unwrap();
        assert!(
            events.iter().any(|event| {
                event.event_type == "agent_spawned"
                    && event.aggregate_id == "AGENT-46"
                    && event.payload.contains("\"agent_id\":46")
            }),
            "Projection-Seed muss als AgentSpawned-Event rebuild-faehig persistieren"
        );

        let db = sentinel_limbo::rusqlite::Connection::open(&projection_path).unwrap();
        let row: (String, String, String, String, i64) = db
            .query_row(
                "SELECT name, role, status, current_room, last_event_id
                 FROM agent_live_view
                 WHERE agent_id = 46",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0, "Ralf Steinbach");
        assert_eq!(row.1, "Operations");
        assert_eq!(row.2, "active");
        assert_eq!(row.3, "empfang");
        assert_eq!(row.4, row_id);
    }

    #[test]
    fn runtime_reconcile_projection_despawn_marks_live_view_inactive() {
        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");
        let projection_path = tmp.path().join("projection.db");
        let event_store = EventStore::open(events_path.to_str().unwrap()).unwrap();
        let projection_store =
            sentinel_projection::ReadModelStore::open(projection_path.to_str().unwrap()).unwrap();
        drop(projection_store);

        let db = sentinel_limbo::rusqlite::Connection::open(&projection_path).unwrap();
        db.execute(
            "INSERT INTO agent_live_view
               (agent_id, name, role, shift_set, status, current_room, in_transit, last_event_id, updated_at)
             VALUES (39, 'Victoria Lehmann', 'Support', 3, 'active', 'empfang', 1, 1, 1)",
            [],
        )
        .unwrap();

        let row_id = emit_runtime_projection_despawn_event(&event_store, AgentId(39), 2096700)
            .expect("projection despawn event");
        mark_agent_projection_despawned(&projection_path, AgentId(39), row_id)
            .expect("projection despawn update");

        let events = event_store.get_events_since(0, 10).unwrap();
        assert!(
            events.iter().any(|event| {
                event.event_type == "agent_despawned"
                    && event.aggregate_id == "AGENT-39"
                    && event.payload.contains("\"agent_id\":39")
            }),
            "Projection-only Despawn muss als AgentDespawned-Event rebuild-faehig persistieren"
        );

        let row: (String, i64, Option<String>) = db
            .query_row(
                "SELECT status, in_transit, transit_target
                 FROM agent_live_view
                 WHERE agent_id = 39",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row.0, "despawned");
        assert_eq!(row.1, 0);
        assert_eq!(row.2, None);
    }

    #[test]
    fn test_runtime_reconcile_skips_projection_restart_when_rebuild_can_run_in_place() {
        PROJECTION_RESTART_CALLS.store(0, Ordering::SeqCst);

        let tmp = tempfile::tempdir().unwrap();
        let events_path = tmp.path().join("events.db");
        let projection_path = tmp.path().join("projection.db");
        let rebuild_request_path = tmp.path().join(".projection-rebuild-request");
        let event_store = Arc::new(EventStore::open(events_path.to_str().unwrap()).unwrap());
        let projection_store =
            sentinel_projection::ReadModelStore::open(projection_path.to_str().unwrap()).unwrap();
        {
            let txn = projection_store.begin_transaction().unwrap();
            txn.begin().unwrap();
            txn.upsert_agent(7, "Projection Ghost", "Tester", 1, "active", 1)
                .unwrap();
            txn.commit().unwrap();
        }
        drop(projection_store);

        let (mut world, _schedule) = create_simulation_world();
        let sandbox = test_sandbox();
        let (mut ebpf_collector, _ebpf_tx) = test_ebpf();
        let mut runtime_orch =
            RuntimeOrchestrator::new(10).with_event_store(Arc::clone(&event_store));
        let mut sandbox_handles = HashMap::new();
        let mut agent_processes = HashMap::new();
        let security_runtime_state = Arc::new(RwLock::new(HashMap::new()));
        let runtime_health = Arc::new(RwLock::new(
            crate::runtime_health::RuntimeHealthSnapshot::default(),
        ));
        let mut respawn_backoff = RespawnBackoffTracker::new(3);
        let mut reconcile_ctx = RuntimeReconcileContext {
            tick_count: 123,
            current_shift: 1,
            all_agents: &[],
            world: &mut world,
            runtime_orch: &mut runtime_orch,
            sandbox: &sandbox,
            sandbox_handles: &mut sandbox_handles,
            ebpf_collector: &mut ebpf_collector,
            agent_processes: &mut agent_processes,
            agent_command: &[],
            security_runtime_state: &security_runtime_state,
            event_store: &event_store,
            runtime_health: &runtime_health,
            projection_db_path: &projection_path,
            operator_auth_required: false,
            service_health_state: crate::service_health::ServiceHealthWorkerSnapshot::default(),
            fs_mount: None,
            data_dir: tmp.path(),
            restart_service_fn: record_projection_restart,
            is_service_active_fn: projection_service_active,
        };

        let response = run_runtime_reconcile(
            &mut reconcile_ctx,
            RuntimeReconcileRequest {
                dry_run: false,
                projection_rebuild: true,
                respawn_missing: false,
            },
            &mut respawn_backoff,
            RuntimeReconcileSource::Operator,
        );

        assert!(response.projection_drift_before);
        assert!(!response.projection_restart_attempted);
        assert!(!response.projection_restart_succeeded);
        assert!(response.projection_rebuild_requested);
        assert_eq!(PROJECTION_RESTART_CALLS.load(Ordering::SeqCst), 0);
        assert!(rebuild_request_path.exists());
        let events = event_store.get_events_since(0, 100).unwrap();
        assert!(
            events.iter().any(|event| {
                event.event_type == "agent_despawned"
                    && event.payload.contains("runtime_reconcile_projection_only")
            }),
            "Projection-only Ghost-Agent muss als Despawn-Event in den append-only Truth-Stream"
        );
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
            true,
            shutdown,
            controlplane,
            runtime_orch,
            test_sandbox(),
            ebpf_collector,
            ebpf_tx,
            ep,
            mpsc::channel::<sentinel_common::OperatorNightrunCommand>().1,
            None,
            None,
            mpsc::channel::<sentinel_common::OperatorSnapshotCommand>().1,
            mpsc::channel::<sentinel_common::OperatorRestoreCommand>().1,
            mpsc::channel::<sentinel_common::OperatorConfigApplyCommand>().1,
            mpsc::channel::<sentinel_common::OperatorMigrateCommand>().1,
            std::path::PathBuf::from("/tmp/sentinel-test-config-apply"),
            10,
            sentinel_common::agent_config::AgentConfigValidation::default(),
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
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(HashMap::new())),
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
                true,
                shutdown,
                controlplane,
                runtime_orch,
                test_sandbox(),
                ebpf_collector,
                ebpf_tx,
                ep,
                mpsc::channel::<sentinel_common::OperatorNightrunCommand>().1,
                None,
                None,
                mpsc::channel::<sentinel_common::OperatorSnapshotCommand>().1,
                mpsc::channel::<sentinel_common::OperatorRestoreCommand>().1,
                mpsc::channel::<sentinel_common::OperatorConfigApplyCommand>().1,
                mpsc::channel::<sentinel_common::OperatorMigrateCommand>().1,
                std::path::PathBuf::from("/tmp/sentinel-test-config-apply"),
                10,
                sentinel_common::agent_config::AgentConfigValidation::default(),
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
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(HashMap::new())),
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
                true,
                shutdown,
                controlplane,
                runtime_orch,
                test_sandbox(),
                ebpf_collector,
                ebpf_tx,
                ep,
                mpsc::channel::<sentinel_common::OperatorNightrunCommand>().1,
                None,
                None,
                mpsc::channel::<sentinel_common::OperatorSnapshotCommand>().1,
                mpsc::channel::<sentinel_common::OperatorRestoreCommand>().1,
                mpsc::channel::<sentinel_common::OperatorConfigApplyCommand>().1,
                mpsc::channel::<sentinel_common::OperatorMigrateCommand>().1,
                std::path::PathBuf::from("/tmp/sentinel-test-config-apply"),
                10,
                sentinel_common::agent_config::AgentConfigValidation::default(),
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
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(HashMap::new())),
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
                true,
                shutdown,
                controlplane,
                runtime_orch,
                test_sandbox(),
                ebpf_collector,
                ebpf_tx,
                ep,
                mpsc::channel::<sentinel_common::OperatorNightrunCommand>().1,
                None,
                None,
                mpsc::channel::<sentinel_common::OperatorSnapshotCommand>().1,
                mpsc::channel::<sentinel_common::OperatorRestoreCommand>().1,
                mpsc::channel::<sentinel_common::OperatorConfigApplyCommand>().1,
                mpsc::channel::<sentinel_common::OperatorMigrateCommand>().1,
                std::path::PathBuf::from("/tmp/sentinel-test-config-apply"),
                10,
                sentinel_common::agent_config::AgentConfigValidation::default(),
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
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(HashMap::new())),
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
                ..
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

    #[test]
    fn world_agent_ids_lists_all_spawned() {
        // Fresh-Load Reset enumeriert alle ECS-Agents (#425).
        let (mut world, _) = create_simulation_world();
        spawn_agent(&mut world, AgentId(1), "A", "Dev", 1, "empfang");
        spawn_agent(&mut world, AgentId(2), "B", "PM", 1, "empfang");
        let mut ids: Vec<u16> = world_agent_ids(&mut world).iter().map(|a| a.0).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2]);
    }

    fn health_agent(
        agent_id: u16,
        repair_status: &str,
    ) -> crate::runtime_health::RuntimeHealthAgentSnapshot {
        crate::runtime_health::RuntimeHealthAgentSnapshot {
            agent_id,
            aggregate_id: format!("AGENT-{agent_id:02}"),
            name: format!("Agent{agent_id}"),
            runtime_present: true,
            projection_present: true,
            tracked_pid: None,
            tracked_pid_alive: false,
            tracked_pid_state: None,
            cgroup_live_pid_count: 0,
            security_runtime_present: true,
            last_repair_status: Some(repair_status.to_string()),
        }
    }

    #[test]
    fn agent_under_active_healing_defers_only_stale() {
        // TOGAF §6 L3: Agents unter aktiver CP-Heilung ("stale") NICHT despawnen.
        let health = Arc::new(RwLock::new(
            crate::runtime_health::RuntimeHealthSnapshot::default(),
        ));
        {
            let mut h = health.write().unwrap();
            h.agents.push(health_agent(5, "stale"));
            h.agents.push(health_agent(6, "healthy"));
        }
        assert!(
            agent_under_active_healing(&health, AgentId(5)),
            "stale -> defer"
        );
        assert!(
            !agent_under_active_healing(&health, AgentId(6)),
            "healthy -> despawn ok"
        );
        assert!(
            !agent_under_active_healing(&health, AgentId(99)),
            "unknown -> despawn ok"
        );
    }

    #[test]
    fn personality_evolution_agent_field_retention_uses_insert_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evolution.db");
        let db = sentinel_limbo::rusqlite::Connection::open(path).unwrap();
        db.execute_batch(
            "CREATE TABLE personality_evolution (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id TEXT NOT NULL,
                tick INTEGER NOT NULL,
                field TEXT NOT NULL,
                change_type TEXT NOT NULL,
                old_value TEXT,
                new_value TEXT NOT NULL,
                reason TEXT NOT NULL,
                nmda_score REAL,
                source TEXT NOT NULL DEFAULT 'night_run',
                created_at_ms INTEGER NOT NULL
            );
            CREATE INDEX idx_evolution_agent_field_id
            ON personality_evolution(agent_id, field, id DESC);",
        )
        .unwrap();

        db.execute(
            "INSERT INTO personality_evolution
             (agent_id, tick, field, change_type, new_value, reason, source, created_at_ms)
             VALUES ('AGENT-01', 1780475609399, 'memory_consolidation', 'night_run', 'legacy', 'legacy ms tick', 'night_run', 1)",
            [],
        )
        .unwrap();
        for tick in 0..PERSONALITY_EVOLUTION_PER_AGENT_FIELD_KEEP {
            db.execute(
                "INSERT INTO personality_evolution
                 (agent_id, tick, field, change_type, new_value, reason, source, created_at_ms)
                 VALUES ('AGENT-01', ?1, 'memory_consolidation', 'night_run', 'real', 'real sim tick', 'night_run', ?2)",
                sentinel_limbo::rusqlite::params![1_900_000 + tick, 2 + tick],
            )
            .unwrap();
        }

        retain_personality_evolution_agent_field(&db, "AGENT-01", "memory_consolidation").unwrap();

        let (count, max_tick): (i64, i64) = db
            .query_row(
                "SELECT count(*), max(tick)
                 FROM personality_evolution
                 WHERE agent_id = 'AGENT-01' AND field = 'memory_consolidation'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, PERSONALITY_EVOLUTION_PER_AGENT_FIELD_KEEP);
        assert!(
            max_tick < 1_000_000_000,
            "legacy millisecond tick was retained: {max_tick}"
        );
    }
}
