//! I/O profiling per cgroup at the block layer.
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
        self.read_ops + self.write_ops
    }

    /// Total throughput in bytes (read + write).
    pub fn total_bytes(&self) -> u64 {
        self.read_bytes + self.write_bytes
    }
}

/// Tracks I/O operations per cgroup.
#[derive(Debug, Default)]
pub struct IoProfiler {
    /// Maps cgroup_id -> accumulated I/O metrics.
    metrics: HashMap<u64, IoMetrics>,
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
        entry.read_ops += 1;
        entry.read_bytes += bytes;
    }

    /// Records a write operation for the given cgroup.
    pub fn record_write(&mut self, cgroup_id: u64, cgroup_name: &str, bytes: u64) {
        let entry = self.metrics.entry(cgroup_id).or_insert_with(|| IoMetrics {
            cgroup_name: cgroup_name.to_string(),
            ..Default::default()
        });
        entry.write_ops += 1;
        entry.write_bytes += bytes;
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
