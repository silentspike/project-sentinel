use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeReconcileRequest {
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub projection_rebuild: bool,
    #[serde(default)]
    pub respawn_missing: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeReconcileResponse {
    pub accepted: bool,
    pub dry_run: bool,
    pub current_shift: u8,
    pub stale_agents_before: usize,
    pub stale_agents_after: usize,
    pub orphan_cgroups_before: usize,
    pub orphan_cgroups_after: usize,
    pub security_snapshots_removed: usize,
    pub unexpected_runtime_removed: usize,
    pub orphan_cgroups_removed: usize,
    pub respawned_agents: usize,
    pub respawn_skipped_backoff: usize,
    pub respawn_blocked_agents: usize,
    pub projection_rebuild_requested: bool,
    pub respawn_failures_total: u64,
    pub repair_last_status: String,
    #[serde(default)]
    pub repaired_agents: Vec<String>,
    #[serde(default)]
    pub blocked_agents: Vec<String>,
    #[serde(default)]
    pub errors: Vec<String>,
}

#[derive(Debug)]
pub enum RuntimeControlCommand {
    Reconcile {
        request: RuntimeReconcileRequest,
        response_tx: mpsc::SyncSender<RuntimeReconcileResponse>,
    },
    PanicTest {
        request: RuntimePanicTestRequest,
        response_tx: mpsc::SyncSender<RuntimePanicTestResponse>,
    },
    StallRestartTest {
        request: RuntimeStallRestartTestRequest,
        response_tx: mpsc::SyncSender<RuntimeStallRestartTestResponse>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStallRestartTestRequest {
    pub agent_id: u16,
    pub mode: String,
    pub stall_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStallRestartTestResponse {
    pub accepted: bool,
    pub agent_id: u16,
    pub aggregate_id: String,
    pub agent_name: String,
    pub mode: String,
    pub stall_secs: u64,
    pub pid_before: Option<u32>,
    pub pid_after: Option<u32>,
    pub runtime_present_after: bool,
    pub security_runtime_present_after: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimePanicTestRequest {
    pub worker: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimePanicTestResponse {
    pub accepted: bool,
    pub worker: String,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespawnRetryDecision {
    Ready,
    BackoffActive { retry_at_tick: u64 },
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RespawnAttemptState {
    failures: u8,
    next_retry_tick: u64,
}

#[derive(Debug, Default)]
pub struct RespawnBackoffTracker {
    states: HashMap<u16, RespawnAttemptState>,
    max_failures: u8,
}

impl RespawnBackoffTracker {
    pub fn new(max_failures: u8) -> Self {
        Self {
            states: HashMap::new(),
            max_failures,
        }
    }

    pub fn decision(&self, agent_id: u16, current_tick: u64) -> RespawnRetryDecision {
        match self.states.get(&agent_id).copied() {
            None => RespawnRetryDecision::Ready,
            Some(state) if state.failures >= self.max_failures => RespawnRetryDecision::Blocked,
            Some(state) if current_tick < state.next_retry_tick => {
                RespawnRetryDecision::BackoffActive {
                    retry_at_tick: state.next_retry_tick,
                }
            }
            Some(_) => RespawnRetryDecision::Ready,
        }
    }

    pub fn record_success(&mut self, agent_id: u16) {
        self.states.remove(&agent_id);
    }

    pub fn record_failure(&mut self, agent_id: u16, current_tick: u64) -> RespawnRetryDecision {
        let entry = self.states.entry(agent_id).or_insert(RespawnAttemptState {
            failures: 0,
            next_retry_tick: current_tick,
        });
        entry.failures = entry.failures.saturating_add(1);
        if entry.failures >= self.max_failures {
            entry.next_retry_tick = current_tick.saturating_add(4);
            RespawnRetryDecision::Blocked
        } else {
            let backoff_ticks = 1u64 << (entry.failures - 1);
            entry.next_retry_tick = current_tick.saturating_add(backoff_ticks);
            RespawnRetryDecision::BackoffActive {
                retry_at_tick: entry.next_retry_tick,
            }
        }
    }
}

pub fn write_projection_rebuild_request(data_dir: &Path, tick: u64) -> Result<()> {
    let request_path = data_dir.join(".projection-rebuild-request");
    let payload = serde_json::json!({
        "requested_by": "runtime_reconcile",
        "tick": tick,
    });
    std::fs::write(&request_path, serde_json::to_vec_pretty(&payload)?).with_context(|| {
        format!(
            "Projection-Rebuild-Request konnte nicht geschrieben werden: {}",
            request_path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respawn_backoff_tracker_applies_exponential_backoff_until_blocked() {
        let mut tracker = RespawnBackoffTracker::new(3);

        assert_eq!(tracker.decision(7, 10), RespawnRetryDecision::Ready);
        assert_eq!(
            tracker.record_failure(7, 10),
            RespawnRetryDecision::BackoffActive { retry_at_tick: 11 }
        );
        assert_eq!(
            tracker.decision(7, 10),
            RespawnRetryDecision::BackoffActive { retry_at_tick: 11 }
        );
        assert_eq!(tracker.decision(7, 11), RespawnRetryDecision::Ready);

        assert_eq!(
            tracker.record_failure(7, 11),
            RespawnRetryDecision::BackoffActive { retry_at_tick: 13 }
        );
        assert_eq!(
            tracker.decision(7, 12),
            RespawnRetryDecision::BackoffActive { retry_at_tick: 13 }
        );
        assert_eq!(tracker.decision(7, 13), RespawnRetryDecision::Ready);

        assert_eq!(tracker.record_failure(7, 13), RespawnRetryDecision::Blocked);
        assert_eq!(tracker.decision(7, 13), RespawnRetryDecision::Blocked);

        tracker.record_success(7);
        assert_eq!(tracker.decision(7, 14), RespawnRetryDecision::Ready);
    }

    #[test]
    fn write_projection_rebuild_request_persists_request_file() {
        let dir = tempfile::tempdir().unwrap();
        write_projection_rebuild_request(dir.path(), 77).unwrap();
        let payload =
            std::fs::read_to_string(dir.path().join(".projection-rebuild-request")).unwrap();
        assert!(payload.contains("\"requested_by\": \"runtime_reconcile\""));
        assert!(payload.contains("\"tick\": 77"));
    }
}
