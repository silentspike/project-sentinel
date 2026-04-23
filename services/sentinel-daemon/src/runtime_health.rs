use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, RwLock};

use sentinel_common::agent_config::AgentConfig;
use sentinel_common::AgentId;
use sentinel_projection::ReadModelStore;
use sentinel_runtime::RuntimeOrchestrator;
use sentinel_sandbox::cgroups::list_pids_in_cgroup;
use sentinel_sandbox::{AgentProcess, SandboxHandle};
use serde::{Deserialize, Serialize};

use crate::operator_api::SharedSecurityRuntimeState;
use crate::service_health::ServiceHealthWorkerSnapshot;
use crate::shift::agents_for_shift;

const CGROUP_ROOT: &str = "/sys/fs/cgroup/sentinel";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeWorkerState {
    pub running: bool,
    #[serde(default)]
    pub restart_count: u64,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub thread_name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeHealthAgentSnapshot {
    pub agent_id: u16,
    pub aggregate_id: String,
    pub name: String,
    pub runtime_present: bool,
    pub projection_present: bool,
    pub tracked_pid: Option<u32>,
    pub tracked_pid_alive: bool,
    pub tracked_pid_state: Option<String>,
    pub cgroup_live_pid_count: usize,
    pub security_runtime_present: bool,
    #[serde(default)]
    pub last_repair_status: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeHealthSnapshot {
    pub current_shift: u8,
    pub expected_active_agents: usize,
    pub runtime_agents: usize,
    pub projection_agents: usize,
    pub security_runtime_entries: usize,
    pub sandbox_handles: usize,
    pub tracked_processes: usize,
    pub live_cgroup_dirs: usize,
    pub stale_runtime_entries: usize,
    pub orphan_cgroups: usize,
    pub zombie_tracked_pids: usize,
    #[serde(default)]
    pub worker_states: BTreeMap<String, RuntimeWorkerState>,
    pub analysis_queue_depth: usize,
    pub analysis_queue_dropped_total: u64,
    pub analysis_queue_coalesced_total: u64,
    pub reconcile_runs_total: u64,
    pub reconcile_repairs_total: u64,
    pub respawn_failures: u64,
    #[serde(default)]
    pub last_repair_error: Option<String>,
    #[serde(default)]
    pub repair_last_status: Option<String>,
    pub operator_auth_required: bool,
    #[serde(default)]
    pub agents: Vec<RuntimeHealthAgentSnapshot>,
}

pub type SharedRuntimeHealthState = Arc<RwLock<RuntimeHealthSnapshot>>;

pub fn build_runtime_health_snapshot(
    all_agents: &[AgentConfig],
    current_shift: u8,
    runtime_orch: &RuntimeOrchestrator,
    sandbox_handles: &HashMap<AgentId, SandboxHandle>,
    agent_processes: &HashMap<AgentId, AgentProcess>,
    security_runtime_state: &SharedSecurityRuntimeState,
    projection_db_path: &Path,
    operator_auth_required: bool,
    service_health_state: ServiceHealthWorkerSnapshot,
    previous: Option<&RuntimeHealthSnapshot>,
) -> RuntimeHealthSnapshot {
    let expected_agents = agents_for_shift(all_agents, current_shift);
    let expected_active_ids = expected_agents
        .iter()
        .map(|cfg| cfg.identity.id)
        .collect::<BTreeSet<_>>();
    let expected_active_agents = expected_active_ids.len();
    let security_state = security_runtime_state
        .read()
        .map(|state| state.clone())
        .unwrap_or_default();
    let cgroup_snapshot = collect_cgroup_snapshot();
    let projection_store: Option<ReadModelStore> = projection_db_path
        .to_str()
        .and_then(|path| ReadModelStore::open(path).ok());
    let projection_agents = projection_store
        .as_ref()
        .and_then(|store| store.active_agent_count().ok())
        .unwrap_or_default()
        .max(0) as usize;
    let runtime_agents = runtime_orch.agent_count();
    let previous_agent_status = previous
        .map(|snapshot| {
            snapshot
                .agents
                .iter()
                .map(|agent| (agent.agent_id, agent.last_repair_status.clone()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    let mut agent_catalog = BTreeMap::<u16, (String, String)>::new();
    for cfg in expected_agents {
        agent_catalog.insert(
            cfg.identity.id,
            (
                format!("AGENT-{:02}", cfg.identity.id),
                cfg.identity.name.clone(),
            ),
        );
    }
    for (agent_id, handle) in runtime_orch.agents() {
        agent_catalog.entry(agent_id.0).or_insert_with(|| {
            (
                format!("AGENT-{:02}", agent_id.0),
                handle.identity.name.clone(),
            )
        });
    }
    for snapshot in security_state.values() {
        agent_catalog
            .entry(snapshot.agent_id)
            .or_insert_with(|| (snapshot.aggregate_id.clone(), snapshot.agent_name.clone()));
    }

    let mut stale_runtime_entries = 0usize;
    let mut zombie_tracked_pids = 0usize;
    let mut agents = Vec::with_capacity(agent_catalog.len());

    for (agent_id, (aggregate_id, name)) in agent_catalog {
        let expected_active = expected_active_ids.contains(&agent_id);
        let runtime_present = runtime_orch.agents().contains_key(&AgentId(agent_id));
        let security_snapshot = security_state.get(&agent_id);
        let security_runtime_present = security_snapshot.is_some();
        let sandbox_handle = sandbox_handles.get(&AgentId(agent_id));
        let agent_process = agent_processes.get(&AgentId(agent_id));
        let tracked_pid = security_snapshot
            .and_then(|snapshot| snapshot.bwrap_pid)
            .or_else(|| sandbox_handle.and_then(|handle| handle.bwrap_pid))
            .or_else(|| agent_process.map(|proc| proc.pid));
        let tracked_pid_state = tracked_pid
            .and_then(read_proc_state)
            .map(|state| state.to_string());
        let tracked_pid_alive = tracked_pid_state
            .as_deref()
            .is_some_and(|state| !matches!(state, "Z" | "X"));
        if tracked_pid_state.as_deref() == Some("Z") {
            zombie_tracked_pids += 1;
        }
        let cgroup_live_pid_count = cgroup_snapshot
            .live_pid_counts
            .get(&name)
            .copied()
            .unwrap_or(0);
        let projection_present = projection_store
            .as_ref()
            .and_then(|store| store.get_agent(agent_id).ok().flatten())
            .is_some_and(|view| view.status == "active");
        let healthy = runtime_present
            && projection_present
            && security_runtime_present
            && tracked_pid_alive
            && cgroup_live_pid_count > 0;
        let unexpected_extra = !expected_active
            && (runtime_present
                || projection_present
                || security_runtime_present
                || tracked_pid_alive
                || cgroup_live_pid_count > 0);
        let stale = if expected_active {
            !healthy
        } else {
            unexpected_extra
        };
        if stale {
            stale_runtime_entries += 1;
        }
        let last_repair_status = previous_agent_status
            .get(&agent_id)
            .cloned()
            .flatten()
            .or_else(|| {
                Some(if healthy {
                    "healthy".to_string()
                } else if expected_active {
                    "stale".to_string()
                } else {
                    "unexpected_runtime".to_string()
                })
            });
        agents.push(RuntimeHealthAgentSnapshot {
            agent_id,
            aggregate_id,
            name,
            runtime_present,
            projection_present,
            tracked_pid,
            tracked_pid_alive,
            tracked_pid_state,
            cgroup_live_pid_count,
            security_runtime_present,
            last_repair_status,
        });
    }

    let runtime_agent_names = runtime_orch
        .agents()
        .values()
        .map(|handle| handle.identity.name.clone())
        .collect::<BTreeSet<_>>();
    let orphan_cgroups = cgroup_snapshot
        .all_dirs
        .iter()
        .filter(|name| !runtime_agent_names.contains(*name))
        .count();
    let mut worker_states = previous
        .map(|snapshot| snapshot.worker_states.clone())
        .unwrap_or_default();
    worker_states.insert(
        "ecs_tick_loop".to_string(),
        RuntimeWorkerState {
            running: true,
            restart_count: worker_states
                .get("ecs_tick_loop")
                .map(|state| state.restart_count)
                .unwrap_or(0),
            last_error: worker_states
                .get("ecs_tick_loop")
                .and_then(|state| state.last_error.clone()),
            thread_name: "ecs-tick-loop".to_string(),
        },
    );
    worker_states.insert(
        "service_health".to_string(),
        RuntimeWorkerState {
            running: service_health_state.running,
            restart_count: worker_states
                .get("service_health")
                .map(|state| state.restart_count)
                .unwrap_or(service_health_state.restart_count),
            last_error: worker_states
                .get("service_health")
                .and_then(|state| state.last_error.clone())
                .or(service_health_state.last_error),
            thread_name: service_health_state.thread_name,
        },
    );

    let mut snapshot = RuntimeHealthSnapshot {
        current_shift,
        expected_active_agents,
        runtime_agents,
        projection_agents,
        security_runtime_entries: security_state.len(),
        sandbox_handles: sandbox_handles.len(),
        tracked_processes: agent_processes.len(),
        live_cgroup_dirs: cgroup_snapshot.live_cgroup_dirs,
        stale_runtime_entries,
        orphan_cgroups,
        zombie_tracked_pids,
        worker_states,
        analysis_queue_depth: 0,
        analysis_queue_dropped_total: 0,
        analysis_queue_coalesced_total: 0,
        reconcile_runs_total: 0,
        reconcile_repairs_total: 0,
        respawn_failures: 0,
        last_repair_error: None,
        repair_last_status: None,
        operator_auth_required,
        agents,
    };

    if let Some(previous) = previous {
        snapshot.analysis_queue_depth = previous.analysis_queue_depth;
        snapshot.analysis_queue_dropped_total = previous.analysis_queue_dropped_total;
        snapshot.analysis_queue_coalesced_total = previous.analysis_queue_coalesced_total;
        snapshot.reconcile_runs_total = previous.reconcile_runs_total;
        snapshot.reconcile_repairs_total = previous.reconcile_repairs_total;
        snapshot.respawn_failures = previous.respawn_failures;
        snapshot.last_repair_error = previous.last_repair_error.clone();
        snapshot.repair_last_status = previous.repair_last_status.clone().or_else(|| {
            Some(
                if snapshot.stale_runtime_entries == 0 && snapshot.orphan_cgroups == 0 {
                    "healthy".to_string()
                } else {
                    "drift_detected".to_string()
                },
            )
        });
    } else {
        snapshot.repair_last_status = Some(
            if snapshot.stale_runtime_entries == 0 && snapshot.orphan_cgroups == 0 {
                "healthy".to_string()
            } else {
                "drift_detected".to_string()
            },
        );
    }

    snapshot
}

#[allow(clippy::too_many_arguments)]
pub fn publish_runtime_health_snapshot(
    runtime_health: &SharedRuntimeHealthState,
    all_agents: &[AgentConfig],
    current_shift: u8,
    runtime_orch: &RuntimeOrchestrator,
    sandbox_handles: &HashMap<AgentId, SandboxHandle>,
    agent_processes: &HashMap<AgentId, AgentProcess>,
    security_runtime_state: &SharedSecurityRuntimeState,
    projection_db_path: &Path,
    operator_auth_required: bool,
    service_health_state: ServiceHealthWorkerSnapshot,
    analysis_queue_stats: crate::platform_controlplane::AnalysisQueueStats,
) {
    let previous = runtime_health.read().ok().map(|snapshot| snapshot.clone());
    let mut snapshot = build_runtime_health_snapshot(
        all_agents,
        current_shift,
        runtime_orch,
        sandbox_handles,
        agent_processes,
        security_runtime_state,
        projection_db_path,
        operator_auth_required,
        service_health_state,
        previous.as_ref(),
    );
    snapshot.analysis_queue_depth = analysis_queue_stats.depth;
    snapshot.analysis_queue_dropped_total = analysis_queue_stats.dropped_total;
    snapshot.analysis_queue_coalesced_total = analysis_queue_stats.coalesced_total;
    if let Ok(mut state) = runtime_health.write() {
        *state = snapshot;
    }
}

#[derive(Debug, Default)]
struct CgroupSnapshot {
    all_dirs: BTreeSet<String>,
    live_pid_counts: HashMap<String, usize>,
    live_cgroup_dirs: usize,
}

fn collect_cgroup_snapshot() -> CgroupSnapshot {
    let mut snapshot = CgroupSnapshot::default();
    let Ok(entries) = std::fs::read_dir(CGROUP_ROOT) else {
        return snapshot;
    };

    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        snapshot.all_dirs.insert(name.clone());
        let pid_count = list_pids_in_cgroup(&name)
            .map(|pids: Vec<u32>| pids.len())
            .unwrap_or(0);
        if pid_count > 0 {
            snapshot.live_cgroup_dirs += 1;
        }
        snapshot.live_pid_counts.insert(name, pid_count);
    }

    snapshot
}

fn read_proc_state(pid: u32) -> Option<char> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        let value = line.strip_prefix("State:")?.trim();
        value.chars().next()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::agent_config::{
        AgentConfig, BackgroundConfig, IdentityConfig, PersonalityConfig, PreferencesConfig,
    };
    use sentinel_common::components::{AgentIdentity, ShiftInfo};
    use tempfile::tempdir;

    fn test_agent(id: u16, shift_set: u8, name: &str) -> AgentConfig {
        AgentConfig {
            identity: IdentityConfig {
                id,
                name: name.to_string(),
                role: "Role".to_string(),
                department: "Dept".to_string(),
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
                neuroticism: 0.5,
                caffeine_tolerance: 0.5,
                morning_person: true,
            },
            preferences: PreferencesConfig {
                favorite_room: "empfang".to_string(),
                coffee_preference: "black".to_string(),
                lunch_time: "12:00".to_string(),
            },
            background: BackgroundConfig {
                bio: "bio".to_string(),
                quirks: Vec::new(),
            },
            capabilities: Default::default(),
        }
    }

    #[test]
    fn build_snapshot_marks_missing_projection_and_security_as_stale() {
        let mut runtime = RuntimeOrchestrator::new(30);
        runtime.set_tick(42);
        runtime
            .spawn_agent(
                AgentIdentity {
                    agent_id: AgentId(7),
                    name: "Test Agent".to_string(),
                    role: "Role".to_string(),
                },
                ShiftInfo {
                    shift_set: 1,
                    shift_start_hour: 6,
                    shift_end_hour: 14,
                    is_on_duty: true,
                },
                "empfang",
            )
            .unwrap();
        let tmp = tempdir().unwrap();
        let snapshot = build_runtime_health_snapshot(
            &[test_agent(7, 1, "Test Agent")],
            1,
            &runtime,
            &HashMap::new(),
            &HashMap::new(),
            &Arc::new(RwLock::new(HashMap::new())),
            &tmp.path().join("projection.db"),
            false,
            ServiceHealthWorkerSnapshot::default(),
            None,
        );

        assert_eq!(snapshot.expected_active_agents, 1);
        assert_eq!(snapshot.runtime_agents, 1);
        assert_eq!(snapshot.projection_agents, 0);
        assert_eq!(snapshot.stale_runtime_entries, 1);
        assert_eq!(snapshot.agents.len(), 1);
        assert!(snapshot.agents[0].runtime_present);
        assert!(!snapshot.agents[0].projection_present);
        assert!(!snapshot.agents[0].security_runtime_present);
    }
}
