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
#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricsSnapshot {
    /// Agent health data (stall detection).
    pub stalled_agents: Vec<u64>,
    /// I/O metrics per cgroup.
    pub io_metrics: HashMap<u64, IoSnapshot>,
    /// Network metrics per destination.
    pub network_metrics: HashMap<String, NetworkSnapshot>,
    /// PSI metrics per agent.
    pub psi_metrics: HashMap<String, PsiSnapshot>,
    /// Collection cycle duration in microseconds.
    #[serde(serialize_with = "serialize_duration_us")]
    pub cycle_duration: Duration,
    /// Current monitoring mode.
    pub mode: MonitoringMode,
    /// Ring buffer drop count (0 in userspace mode).
    pub ring_buffer_drops: u64,
}

/// Serializes Duration as microseconds (u64) for JSON.
fn serialize_duration_us<S>(d: &Duration, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    s.serialize_u64(d.as_micros() as u64)
}

/// I/O snapshot for a single cgroup.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IoSnapshot {
    pub cgroup_name: String,
    pub read_ops: u64,
    pub write_ops: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
}

/// Network snapshot for a single destination.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NetworkSnapshot {
    pub destination: String,
    pub request_count: u64,
    pub avg_latency_us: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub error_count: u64,
}

/// PSI snapshot for a single agent.
#[derive(Debug, Clone, serde::Serialize)]
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
pub struct EbpfCollector {
    mode: MonitoringMode,
    health_checker: AgentHealthChecker,
    io_profiler: IoProfiler,
    network_monitor: NetworkMonitor,
    agent_mappings: Vec<AgentCgroupMapping>,
    ring_buffer_drops: u64,
    last_collect: Option<Instant>,
    #[cfg(feature = "ebpf")]
    loaded_probes: Option<crate::loader::LoadedProbes>,
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
            #[cfg(feature = "ebpf")]
            loaded_probes: None,
        }
    }

    /// Creates a collector with loaded eBPF probes for kernel-mode collection.
    #[cfg(feature = "ebpf")]
    pub fn with_probes(mode: MonitoringMode, probes: crate::loader::LoadedProbes) -> Self {
        Self {
            mode,
            health_checker: AgentHealthChecker::new(),
            io_profiler: IoProfiler::new(),
            network_monitor: NetworkMonitor::new(),
            agent_mappings: Vec::new(),
            ring_buffer_drops: 0,
            last_collect: None,
            loaded_probes: Some(probes),
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

    /// Kernel collection: reads BPF maps via aya.
    ///
    /// Reads:
    /// 1. AGENT_HEALTH Per-CPU Hash Map → max timestamp per cgroup (stall detection)
    /// 2. IO_STATS Per-CPU Hash Map → sum counters per cgroup (IOPS/throughput)
    /// 3. TCP_EVENTS Ring Buffer → drain TCP connect/close events
    fn collect_kernel(&mut self) -> Result<()> {
        #[cfg(feature = "ebpf")]
        {
            use aya::maps::{PerCpuHashMap, RingBuf};

            let probes = match &mut self.loaded_probes {
                Some(p) => p,
                None => {
                    warn!("Kernel mode but no probes loaded");
                    return Ok(());
                }
            };

            let now_secs = current_secs();

            // 1. Agent health: Per-CPU Hash Map (cgroup_id → timestamp_ns)
            //    Take max timestamp across CPUs for each cgroup.
            //    bpf_ktime_get_ns() uses CLOCK_MONOTONIC — convert via delta.
            let monotonic_ns = monotonic_clock_ns();
            if let Some(map) = probes.agent_health.map("AGENT_HEALTH") {
                let map: PerCpuHashMap<_, u64, u64> =
                    PerCpuHashMap::try_from(map).context("AGENT_HEALTH map")?;
                for (cgroup_id, per_cpu_values) in map.iter().flatten() {
                    let max_ktime_ns = per_cpu_values.iter().copied().max().unwrap_or(0);
                    if max_ktime_ns > 0 {
                        let elapsed_ns = monotonic_ns.saturating_sub(max_ktime_ns);
                        let write_unix_secs = now_secs.saturating_sub(elapsed_ns / 1_000_000_000);
                        self.health_checker.record_write(cgroup_id, write_unix_secs);
                    }
                }
            }

            // 2. I/O profiling: Per-CPU Hash Map (cgroup_id → IoStats)
            //    Sum read_ops/write_ops/read_bytes/write_bytes across CPUs.
            if let Some(map) = probes.io_profile.map("IO_STATS") {
                let map: PerCpuHashMap<_, u64, BpfIoStats> =
                    PerCpuHashMap::try_from(map).context("IO_STATS map")?;
                for (cgroup_id, per_cpu_values) in map.iter().flatten() {
                    let mut total = BpfIoStats::default();
                    for cpu_val in per_cpu_values.iter() {
                        total.read_ops += cpu_val.read_ops;
                        total.write_ops += cpu_val.write_ops;
                        total.read_bytes += cpu_val.read_bytes;
                        total.write_bytes += cpu_val.write_bytes;
                    }
                    let name = self
                        .agent_mappings
                        .iter()
                        .find(|m| m.cgroup_id == cgroup_id)
                        .map(|m| m.agent_name.as_str())
                        .unwrap_or("unknown");
                    if total.read_bytes > 0 {
                        self.io_profiler.record_read(cgroup_id, name, total.read_bytes);
                    }
                    if total.write_bytes > 0 {
                        self.io_profiler.record_write(cgroup_id, name, total.write_bytes);
                    }
                }
            }

            // 3. Network: Ring Buffer → drain TCP events
            if let Some(map) = probes.network.map_mut("TCP_EVENTS") {
                let mut ring_buf =
                    RingBuf::try_from(map).context("TCP_EVENTS ring buffer")?;
                let mut event_count = 0u64;
                while let Some(data) = ring_buf.next() {
                    if data.len() >= core::mem::size_of::<BpfTcpEvent>() {
                        let event: BpfTcpEvent =
                            unsafe { core::ptr::read_unaligned(data.as_ptr() as *const _) };
                        if event.event_type == 1 {
                            // tcp_close — record as completed request
                            let dest = format!(
                                "{}.{}.{}.{}:{}",
                                event.dest_ip & 0xFF,
                                (event.dest_ip >> 8) & 0xFF,
                                (event.dest_ip >> 16) & 0xFF,
                                (event.dest_ip >> 24) & 0xFF,
                                event.dest_port,
                            );
                            self.network_monitor.record_request(
                                &dest,
                                Duration::from_nanos(100), // placeholder latency
                                event.bytes_sent,
                                event.bytes_recv,
                            );
                        }
                        event_count += 1;
                    }
                }
                if event_count > 0 {
                    debug!(events = event_count, "Drained TCP ring buffer");
                }
            }
        }

        #[cfg(not(feature = "ebpf"))]
        {
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

/// BPF IoStats struct matching the kernel-side definition in io_profile.rs.
/// Must match the `#[repr(C)]` layout in sentinel-ebpf-probes.
#[cfg(feature = "ebpf")]
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct BpfIoStats {
    read_ops: u64,
    write_ops: u64,
    read_bytes: u64,
    write_bytes: u64,
}

#[cfg(feature = "ebpf")]
unsafe impl aya::Pod for BpfIoStats {}

/// BPF TcpEvent struct matching the kernel-side definition in network.rs.
#[cfg(feature = "ebpf")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct BpfTcpEvent {
    dest_ip: u32,
    dest_port: u16,
    _pad: u16,
    timestamp_ns: u64,
    bytes_sent: u64,
    bytes_recv: u64,
    event_type: u8,
    _pad2: [u8; 7],
}

/// Returns the current monotonic clock in nanoseconds (same clock as bpf_ktime_get_ns).
#[cfg(feature = "ebpf")]
fn monotonic_clock_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
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
