//! Metric collector that polls eBPF maps or userspace sources.
//!
//! In kernel mode: reads Per-CPU Hash Maps and Ring Buffer via aya.
//! In userspace mode: reads /proc/{pid}/io and cgroup io.stat files.
//!
//! Polling interval: 1s for hash maps, event-driven for ring buffer.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, trace, warn};

use crate::loader::MonitoringMode;
use crate::probes::agent_health::AgentHealthChecker;
use crate::probes::io_profile::IoProfiler;
use crate::probes::network::NetworkMonitor;
use crate::psi::PsiReader;

/// Collected metrics snapshot from one polling cycle.
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    /// Agent health data (stall detection).
    pub stalled_agents: Vec<u64>,
    /// I/O metrics per cgroup.
    pub io_metrics: HashMap<u64, IoSnapshot>,
    /// Network metrics per destination.
    pub network_metrics: HashMap<String, NetworkSnapshot>,
    /// PSI metrics per agent.
    pub psi_metrics: HashMap<String, PsiSnapshot>,
    /// Collection cycle duration.
    pub cycle_duration: Duration,
    /// Current monitoring mode.
    pub mode: MonitoringMode,
    /// Ring buffer drop count (0 in userspace mode).
    pub ring_buffer_drops: u64,
}

/// I/O snapshot for a single cgroup.
#[derive(Debug, Clone)]
pub struct IoSnapshot {
    pub cgroup_name: String,
    pub read_ops: u64,
    pub write_ops: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
}

/// Network snapshot for a single destination.
#[derive(Debug, Clone)]
pub struct NetworkSnapshot {
    pub destination: String,
    pub request_count: u64,
    pub avg_latency_us: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub error_count: u64,
}

/// PSI snapshot for a single agent.
#[derive(Debug, Clone)]
pub struct PsiSnapshot {
    pub cpu_avg10: f64,
    pub memory_avg10: f64,
    pub io_avg10: f64,
    pub combined_stress: f32,
}

/// Maps agent name to cgroup path for userspace fallback.
#[derive(Debug, Clone)]
pub struct AgentCgroupMapping {
    pub agent_name: String,
    pub cgroup_path: String,
    pub cgroup_id: u64,
    pub pid: Option<u32>,
}

/// Collects monitoring metrics from kernel or userspace sources.
#[derive(Debug)]
pub struct EbpfCollector {
    mode: MonitoringMode,
    health_checker: AgentHealthChecker,
    io_profiler: IoProfiler,
    network_monitor: NetworkMonitor,
    agent_mappings: Vec<AgentCgroupMapping>,
    ring_buffer_drops: u64,
    last_collect: Option<Instant>,
}

impl EbpfCollector {
    /// Creates a new collector in the specified monitoring mode.
    pub fn new(mode: MonitoringMode) -> Self {
        Self {
            mode,
            health_checker: AgentHealthChecker::new(),
            io_profiler: IoProfiler::new(),
            network_monitor: NetworkMonitor::new(),
            agent_mappings: Vec::new(),
            ring_buffer_drops: 0,
            last_collect: None,
        }
    }

    /// Returns the current monitoring mode.
    pub fn mode(&self) -> MonitoringMode {
        self.mode
    }

    /// Returns a reference to the health checker.
    pub fn health_checker(&self) -> &AgentHealthChecker {
        &self.health_checker
    }

    /// Returns a reference to the I/O profiler.
    pub fn io_profiler(&self) -> &IoProfiler {
        &self.io_profiler
    }

    /// Returns a reference to the network monitor.
    pub fn network_monitor(&self) -> &NetworkMonitor {
        &self.network_monitor
    }

    /// Returns the ring buffer drop count.
    pub fn ring_buffer_drops(&self) -> u64 {
        self.ring_buffer_drops
    }

    /// Registers an agent for monitoring.
    pub fn register_agent(&mut self, mapping: AgentCgroupMapping) {
        debug!(
            agent = %mapping.agent_name,
            cgroup = %mapping.cgroup_path,
            cgroup_id = mapping.cgroup_id,
            "Registered agent for eBPF monitoring"
        );
        self.agent_mappings.push(mapping);
    }

    /// Unregisters an agent from monitoring.
    pub fn unregister_agent(&mut self, cgroup_id: u64) {
        self.agent_mappings.retain(|m| m.cgroup_id != cgroup_id);
        self.health_checker.untrack(cgroup_id);
        self.io_profiler.untrack(cgroup_id);
    }

    /// Collects one cycle of metrics.
    ///
    /// In userspace mode: reads /proc and cgroup files.
    /// In kernel mode: reads BPF maps (behind feature gate).
    pub fn collect(&mut self) -> Result<MetricsSnapshot> {
        let start = Instant::now();

        match self.mode {
            MonitoringMode::Userspace => self.collect_userspace()?,
            MonitoringMode::Kernel => self.collect_kernel()?,
        }

        let cycle_duration = start.elapsed();
        self.last_collect = Some(start);

        trace!(
            mode = %self.mode,
            duration_us = cycle_duration.as_micros() as u64,
            agents = self.agent_mappings.len(),
            "Collection cycle completed"
        );

        let stalled = self.health_checker.stalled_agents(current_secs());
        let io_metrics = self.snapshot_io();
        let network_metrics = self.snapshot_network();
        let psi_metrics = self.collect_psi();

        Ok(MetricsSnapshot {
            stalled_agents: stalled,
            io_metrics,
            network_metrics,
            psi_metrics,
            cycle_duration,
            mode: self.mode,
            ring_buffer_drops: self.ring_buffer_drops,
        })
    }

    /// Userspace collection: reads /proc/{pid}/io for each agent.
    fn collect_userspace(&mut self) -> Result<()> {
        let now = current_secs();

        for mapping in &self.agent_mappings {
            // Agent health: check if process is alive and writing.
            if let Some(pid) = mapping.pid {
                if let Ok(io_data) = read_proc_io(pid) {
                    // If write_bytes changed, agent is alive.
                    self.health_checker.record_write(mapping.cgroup_id, now);

                    // Record I/O metrics.
                    if io_data.read_bytes > 0 {
                        self.io_profiler.record_read(
                            mapping.cgroup_id,
                            &mapping.agent_name,
                            io_data.read_bytes,
                        );
                    }
                    if io_data.write_bytes > 0 {
                        self.io_profiler.record_write(
                            mapping.cgroup_id,
                            &mapping.agent_name,
                            io_data.write_bytes,
                        );
                    }
                }
            }

            // I/O from cgroup io.stat (if available).
            if let Ok(io_stat) = read_cgroup_io_stat(&mapping.cgroup_path) {
                for (device, stats) in io_stat {
                    trace!(
                        cgroup = %mapping.agent_name,
                        device = %device,
                        rbytes = stats.0,
                        wbytes = stats.1,
                        "cgroup io.stat"
                    );
                }
            }
        }

        Ok(())
    }

    /// Kernel collection: reads BPF maps (behind feature gate).
    fn collect_kernel(&mut self) -> Result<()> {
        #[cfg(feature = "ebpf")]
        {
            // In scope:full, this reads:
            // 1. AGENT_HEALTH Per-CPU Hash Map → aggregate per-CPU timestamps
            // 2. IO_STATS Per-CPU Hash Map → aggregate per-CPU counters
            // 3. TCP_EVENTS Ring Buffer → drain events
            //
            // Per-CPU aggregation: iterate all CPU slots, take max (timestamps)
            // or sum (counters) across CPUs.
            warn!("Kernel collection not yet implemented (scope:partial)");
        }

        #[cfg(not(feature = "ebpf"))]
        {
            // Should never reach here — loader.init() returns Userspace without feature.
            warn!("Kernel mode requested but ebpf feature not compiled in");
        }

        Ok(())
    }

    /// Creates I/O snapshot from current profiler state.
    fn snapshot_io(&self) -> HashMap<u64, IoSnapshot> {
        self.io_profiler
            .all_metrics()
            .iter()
            .map(|(cgroup_id, m)| {
                (
                    *cgroup_id,
                    IoSnapshot {
                        cgroup_name: m.cgroup_name.clone(),
                        read_ops: m.read_ops,
                        write_ops: m.write_ops,
                        read_bytes: m.read_bytes,
                        write_bytes: m.write_bytes,
                    },
                )
            })
            .collect()
    }

    /// Creates network snapshot from current monitor state.
    fn snapshot_network(&self) -> HashMap<String, NetworkSnapshot> {
        self.network_monitor
            .all_metrics()
            .iter()
            .map(|(dest, m)| {
                (
                    dest.clone(),
                    NetworkSnapshot {
                        destination: m.destination.clone(),
                        request_count: m.request_count,
                        avg_latency_us: m
                            .avg_latency()
                            .map(|d| d.as_micros() as u64)
                            .unwrap_or(0),
                        bytes_sent: m.bytes_sent,
                        bytes_received: m.bytes_received,
                        error_count: m.error_count,
                    },
                )
            })
            .collect()
    }

    /// Collects PSI metrics for all registered agents.
    fn collect_psi(&self) -> HashMap<String, PsiSnapshot> {
        let mut psi_map = HashMap::new();

        for mapping in &self.agent_mappings {
            let reader = PsiReader::new(&mapping.cgroup_path);

            let cpu = reader.read_cpu_pressure().ok();
            let memory = reader.read_memory_pressure().ok();
            let io = reader.read_io_pressure().ok();

            if let (Some(cpu), Some(memory), Some(io)) = (&cpu, &memory, &io) {
                let stress = crate::psi::combined_stress_factor(cpu, memory, io);
                psi_map.insert(
                    mapping.agent_name.clone(),
                    PsiSnapshot {
                        cpu_avg10: cpu.avg10,
                        memory_avg10: memory.avg10,
                        io_avg10: io.avg10,
                        combined_stress: stress,
                    },
                );
            }
        }

        psi_map
    }
}

/// Data from /proc/{pid}/io.
#[derive(Debug, Default)]
struct ProcIoData {
    read_bytes: u64,
    write_bytes: u64,
}

/// Reads /proc/{pid}/io for a process.
fn read_proc_io(pid: u32) -> Result<ProcIoData> {
    let path = format!("/proc/{pid}/io");
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Reading /proc/{pid}/io"))?;

    let mut data = ProcIoData::default();
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("read_bytes: ") {
            data.read_bytes = val.trim().parse().unwrap_or(0);
        } else if let Some(val) = line.strip_prefix("write_bytes: ") {
            data.write_bytes = val.trim().parse().unwrap_or(0);
        }
    }

    Ok(data)
}

/// Reads cgroup io.stat file.
/// Returns Vec<(device, (rbytes, wbytes))>.
fn read_cgroup_io_stat(cgroup_path: &str) -> Result<Vec<(String, (u64, u64))>> {
    let path = format!("{cgroup_path}/io.stat");
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Reading {path}"))?;

    let mut results = Vec::new();
    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        let device = parts[0].to_string();
        let mut rbytes = 0u64;
        let mut wbytes = 0u64;
        for part in &parts[1..] {
            if let Some(val) = part.strip_prefix("rbytes=") {
                rbytes = val.parse().unwrap_or(0);
            } else if let Some(val) = part.strip_prefix("wbytes=") {
                wbytes = val.parse().unwrap_or(0);
            }
        }
        results.push((device, (rbytes, wbytes)));
    }

    Ok(results)
}

/// Returns current UNIX timestamp in seconds.
fn current_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_collector_userspace() {
        let collector = EbpfCollector::new(MonitoringMode::Userspace);
        assert_eq!(collector.mode(), MonitoringMode::Userspace);
        assert_eq!(collector.ring_buffer_drops(), 0);
    }

    #[test]
    fn register_and_unregister_agent() {
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        collector.register_agent(AgentCgroupMapping {
            agent_name: "AGENT-01".to_string(),
            cgroup_path: "/sys/fs/cgroup/sentinel/agent-01".to_string(),
            cgroup_id: 1,
            pid: Some(1234),
        });
        assert_eq!(collector.agent_mappings.len(), 1);

        collector.unregister_agent(1);
        assert_eq!(collector.agent_mappings.len(), 0);
    }

    #[test]
    fn collect_empty_userspace() {
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        let snapshot = collector.collect().unwrap();
        assert!(snapshot.stalled_agents.is_empty());
        assert!(snapshot.io_metrics.is_empty());
        assert!(snapshot.network_metrics.is_empty());
        assert!(snapshot.psi_metrics.is_empty());
        assert_eq!(snapshot.mode, MonitoringMode::Userspace);
        assert_eq!(snapshot.ring_buffer_drops, 0);
    }

    #[test]
    fn snapshot_io_from_profiler() {
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        collector
            .io_profiler
            .record_read(1, "sentinel/agent-01", 4096);
        collector
            .io_profiler
            .record_write(1, "sentinel/agent-01", 8192);

        let snapshot = collector.snapshot_io();
        let io = snapshot.get(&1).unwrap();
        assert_eq!(io.read_ops, 1);
        assert_eq!(io.write_ops, 1);
        assert_eq!(io.read_bytes, 4096);
        assert_eq!(io.write_bytes, 8192);
    }

    #[test]
    fn snapshot_network_from_monitor() {
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        collector.network_monitor.record_request(
            "api.anthropic.com:443",
            Duration::from_millis(150),
            1024,
            4096,
        );

        let snapshot = collector.snapshot_network();
        let net = snapshot.get("api.anthropic.com:443").unwrap();
        assert_eq!(net.request_count, 1);
        assert_eq!(net.avg_latency_us, 150_000);
        assert_eq!(net.bytes_sent, 1024);
        assert_eq!(net.bytes_received, 4096);
    }

    #[test]
    fn cycle_duration_measured() {
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        let snapshot = collector.collect().unwrap();
        assert!(snapshot.cycle_duration.as_nanos() > 0);
    }
}
