//! I/O profiling from authoritative agent-parent cgroup io.stat snapshots.
//!
//! Tracks read/write IOPS and throughput per cgroup for IOPS budget monitoring.

use std::collections::HashMap;

/// I/O metrics for a single cgroup.
#[derive(Debug, Clone, Default)]
pub struct IoMetrics {
    /// Human-readable cgroup name.
    pub cgroup_name: String,
    /// Total read operations since last reset.
    pub read_ops: u64,
    /// Total write operations since last reset.
    pub write_ops: u64,
    /// Total bytes read since last reset.
    pub read_bytes: u64,
    /// Total bytes written since last reset.
    pub write_bytes: u64,
}

impl IoMetrics {
    /// Total IOPS (read + write).
    pub fn total_iops(&self) -> u64 {
        self.read_ops.saturating_add(self.write_ops)
    }

    /// Total throughput in bytes (read + write).
    pub fn total_bytes(&self) -> u64 {
        self.read_bytes.saturating_add(self.write_bytes)
    }
}

/// Tracks I/O operations per cgroup.
#[derive(Debug, Default)]
pub struct IoProfiler {
    /// Maps cgroup_id -> accumulated I/O metrics.
    metrics: HashMap<u64, IoMetrics>,
    counter_snapshots: HashMap<u64, [u64; 4]>,
    device_snapshots: HashMap<u64, HashMap<String, [u64; 4]>>,
}

impl IoProfiler {
    /// Creates a new I/O profiler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a read operation for the given cgroup.
    pub fn record_read(&mut self, cgroup_id: u64, cgroup_name: &str, bytes: u64) {
        let entry = self.metrics.entry(cgroup_id).or_insert_with(|| IoMetrics {
            cgroup_name: cgroup_name.to_string(),
            ..Default::default()
        });
        entry.read_ops = entry.read_ops.saturating_add(1);
        entry.read_bytes = entry.read_bytes.saturating_add(bytes);
    }

    /// Records a write operation for the given cgroup.
    pub fn record_write(&mut self, cgroup_id: u64, cgroup_name: &str, bytes: u64) {
        let entry = self.metrics.entry(cgroup_id).or_insert_with(|| IoMetrics {
            cgroup_name: cgroup_name.to_string(),
            ..Default::default()
        });
        entry.write_ops = entry.write_ops.saturating_add(1);
        entry.write_bytes = entry.write_bytes.saturating_add(bytes);
    }

    /// Accounts cumulative counters once. A decreased counter starts a new epoch.
    pub fn record_counter_snapshot(
        &mut self,
        cgroup_id: u64,
        cgroup_name: &str,
        counters: [u64; 4],
    ) {
        let previous = self
            .counter_snapshots
            .insert(cgroup_id, counters)
            .unwrap_or_default();
        let delta: [u64; 4] =
            std::array::from_fn(|i| counters[i].checked_sub(previous[i]).unwrap_or(counters[i]));
        let entry = self.metrics.entry(cgroup_id).or_insert_with(|| IoMetrics {
            cgroup_name: cgroup_name.to_string(),
            ..Default::default()
        });
        entry.read_ops = entry.read_ops.saturating_add(delta[0]);
        entry.write_ops = entry.write_ops.saturating_add(delta[1]);
        entry.read_bytes = entry.read_bytes.saturating_add(delta[2]);
        entry.write_bytes = entry.write_bytes.saturating_add(delta[3]);
    }

    /// Accounts exact rios/wios/rbytes/wbytes deltas per device, never poll counts.
    /// Missing devices retain their frontier so a later reappearance is not replayed.
    pub fn record_cgroup_snapshot(
        &mut self,
        cgroup_id: u64,
        cgroup_name: &str,
        devices: &HashMap<String, [u64; 4]>,
    ) {
        let frontiers = self.device_snapshots.entry(cgroup_id).or_default();
        let entry = self.metrics.entry(cgroup_id).or_insert_with(|| IoMetrics {
            cgroup_name: cgroup_name.to_string(),
            ..Default::default()
        });
        for (device, counters) in devices {
            let previous = frontiers
                .insert(device.clone(), *counters)
                .unwrap_or_default();
            let delta: [u64; 4] = std::array::from_fn(|i| {
                counters[i].checked_sub(previous[i]).unwrap_or(counters[i])
            });
            entry.read_ops = entry.read_ops.saturating_add(delta[0]);
            entry.write_ops = entry.write_ops.saturating_add(delta[1]);
            entry.read_bytes = entry.read_bytes.saturating_add(delta[2]);
            entry.write_bytes = entry.write_bytes.saturating_add(delta[3]);
        }
    }

    /// Transfers the same stable parent's counters when its runtime ID changes.
    pub fn rekey(&mut self, old_id: u64, new_id: u64) {
        if old_id == new_id {
            return;
        }
        self.untrack(new_id);
        if let Some(metrics) = self.metrics.remove(&old_id) {
            self.metrics.insert(new_id, metrics);
        }
        if let Some(frontier) = self.counter_snapshots.remove(&old_id) {
            self.counter_snapshots.insert(new_id, frontier);
        }
        if let Some(frontiers) = self.device_snapshots.remove(&old_id) {
            self.device_snapshots.insert(new_id, frontiers);
        }
    }

    /// Returns I/O metrics for a specific cgroup.
    pub fn get_metrics(&self, cgroup_id: u64) -> Option<&IoMetrics> {
        self.metrics.get(&cgroup_id)
    }

    /// Returns all tracked cgroup metrics.
    pub fn all_metrics(&self) -> &HashMap<u64, IoMetrics> {
        &self.metrics
    }

    /// Resets all counters (call after exporting metrics).
    pub fn reset(&mut self) {
        for metrics in self.metrics.values_mut() {
            metrics.read_ops = 0;
            metrics.write_ops = 0;
            metrics.read_bytes = 0;
            metrics.write_bytes = 0;
        }
    }

    /// Removes a cgroup from tracking.
    pub fn untrack(&mut self, cgroup_id: u64) {
        self.metrics.remove(&cgroup_id);
        self.counter_snapshots.remove(&cgroup_id);
        self.device_snapshots.remove(&cgroup_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_profiler_is_empty() {
        let profiler = IoProfiler::new();
        assert!(profiler.all_metrics().is_empty());
    }

    #[test]
    fn device_frontiers_survive_empty_observations_export_reset_and_runtime_rekey() {
        let mut profiler = IoProfiler::new();
        let devices = HashMap::from([("8:0".into(), [3, 2, 4096, 512])]);
        profiler.record_cgroup_snapshot(1, "agent", &devices);
        profiler.record_cgroup_snapshot(1, "agent", &HashMap::new());
        profiler.rekey(1, 2);
        profiler.record_cgroup_snapshot(2, "agent", &devices);
        assert!(profiler.get_metrics(1).is_none());
        assert_eq!(profiler.get_metrics(2).unwrap().read_bytes, 4096);
        assert_eq!(profiler.get_metrics(2).unwrap().read_ops, 3);
        profiler.reset();
        profiler.record_cgroup_snapshot(2, "agent", &devices);
        assert_eq!(profiler.get_metrics(2).unwrap().total_iops(), 0);
        assert_eq!(profiler.get_metrics(2).unwrap().total_bytes(), 0);
    }

    #[test]
    fn device_resets_and_disappearance_do_not_hide_another_devices_deltas() {
        let mut profiler = IoProfiler::new();
        profiler.record_cgroup_snapshot(
            1,
            "agent",
            &HashMap::from([
                ("8:0".into(), [10, 20, 1000, 2000]),
                ("8:1".into(), [2, 3, 200, 300]),
            ]),
        );
        profiler.record_cgroup_snapshot(
            1,
            "agent",
            &HashMap::from([("8:0".into(), [1, 2, 100, 200])]),
        );
        profiler.record_cgroup_snapshot(
            1,
            "agent",
            &HashMap::from([
                ("8:0".into(), [1, 2, 100, 200]),
                ("8:1".into(), [3, 4, 250, 350]),
            ]),
        );
        let metrics = profiler.get_metrics(1).unwrap();
        assert_eq!(
            [
                metrics.read_ops,
                metrics.write_ops,
                metrics.read_bytes,
                metrics.write_bytes
            ],
            [14, 26, 1350, 2550]
        );
    }

    #[test]
    fn cumulative_kernel_samples_are_counted_once_with_exact_operations() {
        let mut profiler = IoProfiler::new();
        profiler.record_counter_snapshot(1, "agent", [3, 2, 4096, 512]);
        profiler.record_counter_snapshot(1, "agent", [3, 2, 4096, 512]);
        profiler.record_counter_snapshot(1, "agent", [5, 3, 8192, 1024]);
        let metrics = profiler.get_metrics(1).unwrap();
        assert_eq!(
            [
                metrics.read_ops,
                metrics.write_ops,
                metrics.read_bytes,
                metrics.write_bytes
            ],
            [5, 3, 8192, 1024]
        );
    }

    #[test]
    fn kernel_counter_reset_and_untrack_do_not_lose_new_events() {
        let mut profiler = IoProfiler::new();
        profiler.record_counter_snapshot(1, "agent", [3, 2, 4096, 512]);
        profiler.record_counter_snapshot(1, "agent", [1, 1, 100, 50]);
        let metrics = profiler.get_metrics(1).unwrap();
        assert_eq!(
            [
                metrics.read_ops,
                metrics.write_ops,
                metrics.read_bytes,
                metrics.write_bytes
            ],
            [4, 3, 4196, 562]
        );
        profiler.untrack(1);
        profiler.record_counter_snapshot(1, "replacement", [1, 1, 100, 50]);
        assert_eq!(profiler.get_metrics(1).unwrap().read_bytes, 100);
        assert_eq!(profiler.get_metrics(1).unwrap().cgroup_name, "replacement");
    }

    #[test]
    fn export_reset_preserves_kernel_sample_frontier() {
        let mut profiler = IoProfiler::new();
        profiler.record_counter_snapshot(1, "agent", [3, 2, 4096, 512]);
        profiler.reset();
        profiler.record_counter_snapshot(1, "agent", [3, 2, 4096, 512]);
        assert_eq!(profiler.get_metrics(1).unwrap().total_bytes(), 0);
        profiler.record_counter_snapshot(1, "agent", [4, 2, 4196, 512]);
        assert_eq!(profiler.get_metrics(1).unwrap().read_bytes, 100);
    }

    #[test]
    fn huge_kernel_samples_and_aggregate_totals_do_not_overflow() {
        let mut profiler = IoProfiler::new();
        profiler.record_counter_snapshot(1, "agent", [u64::MAX; 4]);
        profiler.record_counter_snapshot(1, "agent", [1; 4]);
        let metrics = profiler.get_metrics(1).unwrap();
        assert_eq!(metrics.read_bytes, u64::MAX);
        assert_eq!(metrics.total_bytes(), u64::MAX);
        assert_eq!(metrics.total_iops(), u64::MAX);
    }

    #[test]
    fn record_read_creates_entry() {
        let mut profiler = IoProfiler::new();
        profiler.record_read(1, "sentinel/agent-01", 4096);
        let metrics = profiler.get_metrics(1).unwrap();
        assert_eq!(metrics.read_ops, 1);
        assert_eq!(metrics.read_bytes, 4096);
        assert_eq!(metrics.write_ops, 0);
        assert_eq!(metrics.cgroup_name, "sentinel/agent-01");
    }

    #[test]
    fn record_write_creates_entry() {
        let mut profiler = IoProfiler::new();
        profiler.record_write(1, "sentinel/agent-01", 8192);
        let metrics = profiler.get_metrics(1).unwrap();
        assert_eq!(metrics.write_ops, 1);
        assert_eq!(metrics.write_bytes, 8192);
        assert_eq!(metrics.read_ops, 0);
    }

    #[test]
    fn accumulates_operations() {
        let mut profiler = IoProfiler::new();
        profiler.record_read(1, "ecs", 4096);
        profiler.record_read(1, "ecs", 4096);
        profiler.record_write(1, "ecs", 8192);
        let metrics = profiler.get_metrics(1).unwrap();
        assert_eq!(metrics.read_ops, 2);
        assert_eq!(metrics.write_ops, 1);
        assert_eq!(metrics.total_iops(), 3);
        assert_eq!(metrics.total_bytes(), 4096 + 4096 + 8192);
    }

    #[test]
    fn multiple_cgroups() {
        let mut profiler = IoProfiler::new();
        profiler.record_read(1, "ecs", 4096);
        profiler.record_read(2, "redb", 8192);
        assert_eq!(profiler.all_metrics().len(), 2);
        assert_eq!(profiler.get_metrics(1).unwrap().read_bytes, 4096);
        assert_eq!(profiler.get_metrics(2).unwrap().read_bytes, 8192);
    }

    #[test]
    fn reset_clears_counters() {
        let mut profiler = IoProfiler::new();
        profiler.record_read(1, "ecs", 4096);
        profiler.record_write(1, "ecs", 8192);
        profiler.reset();
        let metrics = profiler.get_metrics(1).unwrap();
        assert_eq!(metrics.read_ops, 0);
        assert_eq!(metrics.write_ops, 0);
        assert_eq!(metrics.read_bytes, 0);
        assert_eq!(metrics.write_bytes, 0);
        // Name preserved
        assert_eq!(metrics.cgroup_name, "ecs");
    }

    #[test]
    fn untrack_removes_cgroup() {
        let mut profiler = IoProfiler::new();
        profiler.record_read(1, "ecs", 4096);
        profiler.untrack(1);
        assert!(profiler.get_metrics(1).is_none());
    }
}
