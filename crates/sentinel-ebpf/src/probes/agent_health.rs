//! Agent health monitoring via write() syscall tracking.
//!
//! Detects stalled agents by tracking the last write() syscall per cgroup.
//! If an agent has not performed a write() in `threshold_secs`, it is considered stalled.

use std::collections::HashMap;

/// Default threshold in seconds before an agent is considered stalled.
const DEFAULT_STALL_THRESHOLD_SECS: u64 = 30;

/// Tracks write() activity per agent cgroup for stall detection.
#[derive(Debug)]
pub struct AgentHealthChecker {
    /// Maps cgroup_id -> timestamp of last write() syscall (unix seconds).
    last_write: HashMap<u64, u64>,
    /// Seconds without write() before an agent is considered stalled.
    threshold_secs: u64,
}

impl AgentHealthChecker {
    /// Creates a new health checker with default stall threshold (30s).
    pub fn new() -> Self {
        Self {
            last_write: HashMap::new(),
            threshold_secs: DEFAULT_STALL_THRESHOLD_SECS,
        }
    }

    /// Creates a new health checker with custom stall threshold.
    pub fn with_threshold(threshold_secs: u64) -> Self {
        Self {
            last_write: HashMap::new(),
            threshold_secs,
        }
    }

    /// Records a write() syscall for the given cgroup.
    pub fn record_write(&mut self, cgroup_id: u64, timestamp_secs: u64) {
        self.last_write.insert(cgroup_id, timestamp_secs);
    }

    /// Returns the list of stalled cgroup IDs at the given time.
    pub fn stalled_agents(&self, now_secs: u64) -> Vec<u64> {
        self.last_write
            .iter()
            .filter(|(_, last_write)| now_secs.saturating_sub(**last_write) > self.threshold_secs)
            .map(|(cgroup_id, _)| *cgroup_id)
            .collect()
    }

    /// Returns seconds since last write for a given cgroup, or None if not tracked.
    pub fn seconds_since_last_write(&self, cgroup_id: u64, now_secs: u64) -> Option<u64> {
        self.last_write
            .get(&cgroup_id)
            .map(|last| now_secs.saturating_sub(*last))
    }

    /// Returns the stall threshold in seconds.
    pub fn threshold_secs(&self) -> u64 {
        self.threshold_secs
    }

    /// Returns the number of tracked cgroups.
    pub fn tracked_count(&self) -> usize {
        self.last_write.len()
    }

    /// Removes a cgroup from tracking (agent stopped).
    pub fn untrack(&mut self, cgroup_id: u64) {
        self.last_write.remove(&cgroup_id);
    }
}

impl Default for AgentHealthChecker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_checker_has_no_tracked_agents() {
        let checker = AgentHealthChecker::new();
        assert_eq!(checker.tracked_count(), 0);
        assert!(checker.stalled_agents(1000).is_empty());
    }

    #[test]
    fn record_write_tracks_agent() {
        let mut checker = AgentHealthChecker::new();
        checker.record_write(1, 1000);
        assert_eq!(checker.tracked_count(), 1);
        assert_eq!(checker.seconds_since_last_write(1, 1000), Some(0));
    }

    #[test]
    fn agent_not_stalled_within_threshold() {
        let mut checker = AgentHealthChecker::new();
        checker.record_write(1, 990);
        // 10s since last write, threshold is 30s
        assert!(checker.stalled_agents(1000).is_empty());
    }

    #[test]
    fn agent_stalled_after_threshold() {
        let mut checker = AgentHealthChecker::new();
        checker.record_write(1, 960);
        checker.record_write(2, 990);
        // Agent 1: 45s since write (stalled), Agent 2: 15s (ok)
        let stalled = checker.stalled_agents(1005);
        assert_eq!(stalled.len(), 1);
        assert!(stalled.contains(&1));
    }

    #[test]
    fn custom_threshold() {
        let mut checker = AgentHealthChecker::with_threshold(10);
        checker.record_write(1, 985);
        // 15s > 10s threshold
        let stalled = checker.stalled_agents(1000);
        assert_eq!(stalled, vec![1]);
    }

    #[test]
    fn untrack_removes_agent() {
        let mut checker = AgentHealthChecker::new();
        checker.record_write(1, 1000);
        checker.untrack(1);
        assert_eq!(checker.tracked_count(), 0);
        assert_eq!(checker.seconds_since_last_write(1, 1000), None);
    }

    #[test]
    fn multiple_writes_update_timestamp() {
        let mut checker = AgentHealthChecker::new();
        checker.record_write(1, 900);
        checker.record_write(1, 995);
        assert_eq!(checker.seconds_since_last_write(1, 1000), Some(5));
        assert!(checker.stalled_agents(1000).is_empty());
    }
}
