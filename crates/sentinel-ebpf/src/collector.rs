//! Metric collector that polls eBPF maps or userspace sources.
//!
//! In kernel mode: reads Per-CPU Hash Maps and Ring Buffer via aya.
//! In userspace mode: reads /proc/{pid}/io and cgroup io.stat files.
//!
//! Polling interval: 1s for hash maps, event-driven for ring buffer.

use std::collections::HashMap;
use std::fs;
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
    /// Agent health data (stall detection, filtered to registered agents only).
    pub stalled_agents: Vec<StalledAgent>,
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

/// Agent stall info for a single agent.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StalledAgent {
    pub cgroup_id: u64,
    pub agent_name: String,
    pub seconds_since_write: u64,
}

/// Maps agent name to cgroup path for monitoring.
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
    /// Previous /proc/PID/io values for delta tracking.
    prev_proc_io: HashMap<u64, ProcIoData>,
    /// Previous cgroup io.stat values for delta tracking (cgroup_id -> (rbytes, wbytes)).
    prev_cgroup_io: HashMap<u64, (u64, u64)>,
    /// Whether we've already warned about /proc/PID/io permission denied.
    proc_io_permission_warned: bool,
    #[cfg(feature = "ebpf")]
    loaded_probes: Option<crate::loader::LoadedProbes>,
}

impl EbpfCollector {
    /// Creates a new collector in the specified monitoring mode.
    pub fn new(mode: MonitoringMode) -> Self {
        Self::new_with_stall_threshold(mode, AgentHealthChecker::default().threshold_secs())
    }

    /// Creates a new collector with an explicit stall threshold in seconds.
    pub fn new_with_stall_threshold(mode: MonitoringMode, stall_threshold_secs: u64) -> Self {
        Self {
            mode,
            health_checker: AgentHealthChecker::with_threshold(stall_threshold_secs),
            io_profiler: IoProfiler::new(),
            network_monitor: NetworkMonitor::new(),
            agent_mappings: Vec::new(),
            ring_buffer_drops: 0,
            last_collect: None,
            prev_proc_io: HashMap::new(),
            prev_cgroup_io: HashMap::new(),
            proc_io_permission_warned: false,
            #[cfg(feature = "ebpf")]
            loaded_probes: None,
        }
    }

    /// Creates a collector with loaded eBPF probes for kernel-mode collection.
    #[cfg(feature = "ebpf")]
    pub fn with_probes(mode: MonitoringMode, probes: crate::loader::LoadedProbes) -> Self {
        Self::with_probes_and_stall_threshold(
            mode,
            probes,
            AgentHealthChecker::default().threshold_secs(),
        )
    }

    /// Creates a collector with loaded eBPF probes and an explicit stall threshold.
    #[cfg(feature = "ebpf")]
    pub fn with_probes_and_stall_threshold(
        mode: MonitoringMode,
        probes: crate::loader::LoadedProbes,
        stall_threshold_secs: u64,
    ) -> Self {
        Self {
            mode,
            health_checker: AgentHealthChecker::with_threshold(stall_threshold_secs),
            io_profiler: IoProfiler::new(),
            network_monitor: NetworkMonitor::new(),
            agent_mappings: Vec::new(),
            ring_buffer_drops: 0,
            last_collect: None,
            prev_proc_io: HashMap::new(),
            prev_cgroup_io: HashMap::new(),
            proc_io_permission_warned: false,
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
    ///
    /// Sets an initial health timestamp so the agent has a 30s grace period
    /// before stall detection kicks in. Without this, agents that haven't
    /// produced any I/O yet would never appear as stalled (no entry in
    /// `last_write`), but the first recorded write followed by 30s inactivity
    /// would immediately trigger a false stall.
    pub fn register_agent(&mut self, mapping: AgentCgroupMapping) {
        let now = current_secs();
        debug!(
            agent = %mapping.agent_name,
            cgroup = %mapping.cgroup_path,
            cgroup_id = mapping.cgroup_id,
            "Registered agent for eBPF monitoring"
        );
        self.health_checker.record_write(mapping.cgroup_id, now);
        self.agent_mappings.push(mapping);
    }

    /// Updates the PID for a registered agent (after process start).
    ///
    /// Enables userspace I/O tracking via `/proc/{pid}/io`.
    pub fn update_agent_pid(&mut self, cgroup_id: u64, pid: u32) {
        if let Some(mapping) = self
            .agent_mappings
            .iter_mut()
            .find(|m| m.cgroup_id == cgroup_id)
        {
            let resolved_pid = resolve_agent_runtime_pid(&mapping.cgroup_path).unwrap_or(pid);
            mapping.pid = Some(resolved_pid);
            debug!(
                agent = %mapping.agent_name,
                requested_pid = pid,
                pid = resolved_pid,
                "Agent PID updated for eBPF monitoring"
            );
        }
    }

    /// Unregisters an agent from monitoring.
    pub fn unregister_agent(&mut self, cgroup_id: u64) {
        self.agent_mappings.retain(|m| m.cgroup_id != cgroup_id);
        self.health_checker.untrack(cgroup_id);
        self.io_profiler.untrack(cgroup_id);
        self.prev_proc_io.remove(&cgroup_id);
        self.prev_cgroup_io.remove(&cgroup_id);
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

        let now_secs = current_secs();
        let stalled_ids = self.health_checker.stalled_agents(now_secs);
        let stalled = stalled_ids
            .into_iter()
            .filter_map(|cgroup_id| {
                // Only report stalled agents that are registered (skip orphaned entries)
                let agent_name = self
                    .agent_mappings
                    .iter()
                    .find(|m| m.cgroup_id == cgroup_id)
                    .map(|m| m.agent_name.clone())?;
                let seconds = self
                    .health_checker
                    .seconds_since_last_write(cgroup_id, now_secs)
                    .unwrap_or(0);
                Some(StalledAgent {
                    cgroup_id,
                    agent_name,
                    seconds_since_write: seconds,
                })
            })
            .collect();
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

    /// Userspace collection: reads /proc/{pid}/io and cgroup io.stat for each agent.
    fn collect_userspace(&mut self) -> Result<()> {
        let now = current_secs();

        // Collect into a vec first to satisfy borrow checker (self.prev_* is mutated below)
        let mappings: Vec<_> = self.agent_mappings.clone();

        for mapping in &mappings {
            // Agent health + I/O: check /proc/{pid}/io for VFS-level activity.
            if let Some(pid) = mapping.pid {
                match read_proc_io(pid) {
                    Ok(io_data) => {
                        let prev = self.prev_proc_io.entry(mapping.cgroup_id).or_default();
                        // Use rchar/wchar (VFS-level) as primary metric — captures
                        // buffered I/O that never reaches the block layer.
                        let delta_read = if io_data.rchar > 0 {
                            io_data.rchar.saturating_sub(prev.rchar)
                        } else {
                            io_data.read_bytes.saturating_sub(prev.read_bytes)
                        };
                        let delta_write = if io_data.wchar > 0 {
                            io_data.wchar.saturating_sub(prev.wchar)
                        } else {
                            io_data.write_bytes.saturating_sub(prev.write_bytes)
                        };
                        *prev = io_data;

                        // Only mark agent as alive if there's actual I/O activity.
                        if delta_read > 0 || delta_write > 0 {
                            self.health_checker.record_write(mapping.cgroup_id, now);
                        }

                        if delta_read > 0 {
                            self.io_profiler.record_read(
                                mapping.cgroup_id,
                                &mapping.agent_name,
                                delta_read,
                            );
                        }
                        if delta_write > 0 {
                            self.io_profiler.record_write(
                                mapping.cgroup_id,
                                &mapping.agent_name,
                                delta_write,
                            );
                        }
                    }
                    Err(e) => {
                        if !self.proc_io_permission_warned {
                            let is_permission = e
                                .root_cause()
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|io| {
                                    io.kind() == std::io::ErrorKind::PermissionDenied
                                });
                            if is_permission {
                                warn!(
                                    pid,
                                    "Cannot read /proc/{pid}/io: permission denied. \
                                     Add AmbientCapabilities=CAP_SYS_PTRACE to systemd unit."
                                );
                                self.proc_io_permission_warned = true;
                            }
                        }
                    }
                }
            }

            // I/O from cgroup io.stat (if available).
            if let Ok(io_stat) = read_cgroup_io_stat(&mapping.cgroup_path) {
                // Aggregate across devices
                let total_rbytes: u64 = io_stat.iter().map(|(_, (r, _))| r).sum();
                let total_wbytes: u64 = io_stat.iter().map(|(_, (_, w))| w).sum();

                for (device, stats) in &io_stat {
                    trace!(
                        cgroup = %mapping.agent_name,
                        device = %device,
                        rbytes = stats.0,
                        wbytes = stats.1,
                        "cgroup io.stat"
                    );
                }

                // Delta tracking for cgroup io.stat
                let prev = self
                    .prev_cgroup_io
                    .entry(mapping.cgroup_id)
                    .or_insert((0, 0));
                let delta_read = total_rbytes.saturating_sub(prev.0);
                let delta_write = total_wbytes.saturating_sub(prev.1);
                *prev = (total_rbytes, total_wbytes);

                if delta_read > 0 {
                    self.io_profiler.record_read(
                        mapping.cgroup_id,
                        &mapping.agent_name,
                        delta_read,
                    );
                }
                if delta_write > 0 {
                    self.io_profiler.record_write(
                        mapping.cgroup_id,
                        &mapping.agent_name,
                        delta_write,
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
    ///
    /// BPF maps contain ALL system cgroups. Only registered Sentinel agents
    /// are processed — system cgroups (sshd, systemd, etc.) are filtered out.
    fn collect_kernel(&mut self) -> Result<()> {
        #[cfg(feature = "ebpf")]
        {
            use aya::maps::{PerCpuHashMap, RingBuf};
            use std::collections::HashSet;

            let probes = match &mut self.loaded_probes {
                Some(p) => p,
                None => {
                    warn!("Kernel mode but no probes loaded");
                    return Ok(());
                }
            };

            let now_secs = current_secs();

            // Build registered cgroup set for O(1) lookup.
            // Only cgroup_ids registered via register_agent() are processed.
            let registered_cgroups: HashSet<u64> =
                self.agent_mappings.iter().map(|m| m.cgroup_id).collect();

            // 1. Agent health: Per-CPU Hash Map (cgroup_id → timestamp_ns)
            //    Take max timestamp across CPUs for each cgroup.
            //    bpf_ktime_get_ns() uses CLOCK_MONOTONIC — convert via delta.
            let monotonic_ns = monotonic_clock_ns();
            if let Some(map) = probes.agent_health.map("AGENT_HEALTH") {
                let map: PerCpuHashMap<_, u64, u64> =
                    PerCpuHashMap::try_from(map).context("AGENT_HEALTH map")?;
                let mut total_entries = 0u64;
                let mut matched_entries = 0u64;
                for (cgroup_id, per_cpu_values) in map.iter().flatten() {
                    total_entries += 1;
                    // Skip system cgroups — only track registered Sentinel agents
                    if !registered_cgroups.contains(&cgroup_id) {
                        continue;
                    }
                    matched_entries += 1;
                    let max_ktime_ns = per_cpu_values.iter().copied().max().unwrap_or(0);
                    if max_ktime_ns > 0 {
                        let elapsed_ns = monotonic_ns.saturating_sub(max_ktime_ns);
                        let write_unix_secs = now_secs.saturating_sub(elapsed_ns / 1_000_000_000);
                        self.health_checker.record_write(cgroup_id, write_unix_secs);
                    }
                }
                debug!(
                    total = total_entries,
                    matched = matched_entries,
                    registered = registered_cgroups.len(),
                    "AGENT_HEALTH BPF map iterated"
                );
            }

            // 2. I/O profiling: Per-CPU Hash Map (cgroup_id → IoStats)
            //    Sum read_ops/write_ops/read_bytes/write_bytes across CPUs.
            //    Only processes registered agent cgroups.
            if let Some(map) = probes.io_profile.map("IO_STATS") {
                let map: PerCpuHashMap<_, u64, BpfIoStats> =
                    PerCpuHashMap::try_from(map).context("IO_STATS map")?;
                for (cgroup_id, per_cpu_values) in map.iter().flatten() {
                    // Skip system cgroups
                    if !registered_cgroups.contains(&cgroup_id) {
                        continue;
                    }
                    let mut total = BpfIoStats::default();
                    for cpu_val in per_cpu_values.iter() {
                        total.read_ops += cpu_val.read_ops;
                        total.write_ops += cpu_val.write_ops;
                        total.read_bytes += cpu_val.read_bytes;
                        total.write_bytes += cpu_val.write_bytes;
                    }
                    // Safe unwrap: cgroup_id is in registered_cgroups, so it's in agent_mappings
                    let name = self
                        .agent_mappings
                        .iter()
                        .find(|m| m.cgroup_id == cgroup_id)
                        .map(|m| m.agent_name.as_str())
                        .unwrap_or("unknown");
                    if total.read_bytes > 0 {
                        self.io_profiler
                            .record_read(cgroup_id, name, total.read_bytes);
                    }
                    if total.write_bytes > 0 {
                        self.io_profiler
                            .record_write(cgroup_id, name, total.write_bytes);
                    }
                }
            }

            // 3. Network: Ring Buffer → drain TCP events
            if let Some(map) = probes.network.map_mut("TCP_EVENTS") {
                let mut ring_buf = RingBuf::try_from(map).context("TCP_EVENTS ring buffer")?;
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

        // 4. Supplement with cgroup io.stat + /proc/{pid}/io (available in both modes).
        //    BPF block:block_rq_complete only tracks block device I/O.
        //    cgroup io.stat provides cgroup-level I/O regardless of BPF.
        //    /proc/{pid}/io provides VFS-level I/O (buffered, pipes, page cache).
        //    Both also update health_checker for stall detection — critical because
        //    the BPF fentry/vfs_write probe may not see activity for agents doing
        //    only buffered I/O through pipes.
        let supplement_now = current_secs();
        let mappings: Vec<_> = self.agent_mappings.clone();
        for mapping in &mappings {
            if let Ok(io_stat) = read_cgroup_io_stat(&mapping.cgroup_path) {
                let total_rbytes: u64 = io_stat.iter().map(|(_, (r, _))| r).sum();
                let total_wbytes: u64 = io_stat.iter().map(|(_, (_, w))| w).sum();

                // Delta tracking for cgroup io.stat
                let prev = self
                    .prev_cgroup_io
                    .entry(mapping.cgroup_id)
                    .or_insert((0, 0));
                let delta_read = total_rbytes.saturating_sub(prev.0);
                let delta_write = total_wbytes.saturating_sub(prev.1);
                *prev = (total_rbytes, total_wbytes);

                if delta_read > 0 {
                    self.io_profiler.record_read(
                        mapping.cgroup_id,
                        &mapping.agent_name,
                        delta_read,
                    );
                }
                if delta_write > 0 {
                    self.io_profiler.record_write(
                        mapping.cgroup_id,
                        &mapping.agent_name,
                        delta_write,
                    );
                }
                // cgroup I/O activity → agent is alive (supplements BPF stall detection)
                if delta_read > 0 || delta_write > 0 {
                    self.health_checker
                        .record_write(mapping.cgroup_id, supplement_now);
                }
            }

            // Also try /proc/PID/io if pid is known (supplements BPF block I/O).
            // Uses VFS-level rchar/wchar which includes page cache hits — critical for
            // agent processes that do buffered I/O never reaching the block layer.
            if let Some(pid) = mapping.pid {
                match read_proc_io(pid) {
                    Ok(io_data) => {
                        let prev = self.prev_proc_io.entry(mapping.cgroup_id).or_default();
                        // Use rchar/wchar (VFS-level) as primary, fall back to read_bytes/write_bytes
                        let delta_read = if io_data.rchar > 0 {
                            io_data.rchar.saturating_sub(prev.rchar)
                        } else {
                            io_data.read_bytes.saturating_sub(prev.read_bytes)
                        };
                        let delta_write = if io_data.wchar > 0 {
                            io_data.wchar.saturating_sub(prev.wchar)
                        } else {
                            io_data.write_bytes.saturating_sub(prev.write_bytes)
                        };
                        *prev = io_data;

                        if delta_read > 0 {
                            self.io_profiler.record_read(
                                mapping.cgroup_id,
                                &mapping.agent_name,
                                delta_read,
                            );
                        }
                        if delta_write > 0 {
                            self.io_profiler.record_write(
                                mapping.cgroup_id,
                                &mapping.agent_name,
                                delta_write,
                            );
                        }
                        // VFS-level I/O activity → agent is alive
                        if delta_read > 0 || delta_write > 0 {
                            self.health_checker
                                .record_write(mapping.cgroup_id, supplement_now);
                        }
                    }
                    Err(e) => {
                        if !self.proc_io_permission_warned {
                            let is_permission = e
                                .root_cause()
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|io| {
                                    io.kind() == std::io::ErrorKind::PermissionDenied
                                });
                            if is_permission {
                                warn!(
                                    pid,
                                    "Cannot read /proc/{pid}/io: permission denied. \
                                     Add AmbientCapabilities=CAP_SYS_PTRACE to systemd unit \
                                     for VFS-level I/O metrics."
                                );
                                self.proc_io_permission_warned = true;
                            } else {
                                debug!(
                                    agent = %mapping.agent_name,
                                    pid,
                                    error = %e,
                                    "/proc/{pid}/io read failed"
                                );
                            }
                        }
                    }
                }
            }
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
                        avg_latency_us: m.avg_latency().map(|d| d.as_micros() as u64).unwrap_or(0),
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

            let cpu = match reader.read_cpu_pressure() {
                Ok(v) => Some(v),
                Err(e) => {
                    log_psi_error(&mapping.agent_name, "cpu.pressure", &e);
                    None
                }
            };
            let memory = match reader.read_memory_pressure() {
                Ok(v) => Some(v),
                Err(e) => {
                    log_psi_error(&mapping.agent_name, "memory.pressure", &e);
                    None
                }
            };
            let io = match reader.read_io_pressure() {
                Ok(v) => Some(v),
                Err(e) => {
                    log_psi_error(&mapping.agent_name, "io.pressure", &e);
                    None
                }
            };

            // Partial PSI: use whatever is available, default missing values to 0.
            // Previously required ALL three (cpu+mem+io) to succeed — a single
            // missing file caused the entire agent to be skipped (P4 regression).
            if cpu.is_some() || memory.is_some() || io.is_some() {
                let zero = sentinel_common::psi::PsiMetrics::default();
                let cpu_ref = cpu.as_ref().unwrap_or(&zero);
                let mem_ref = memory.as_ref().unwrap_or(&zero);
                let io_ref = io.as_ref().unwrap_or(&zero);
                let stress = crate::psi::combined_stress_factor(cpu_ref, mem_ref, io_ref);
                psi_map.insert(
                    mapping.agent_name.clone(),
                    PsiSnapshot {
                        cpu_avg10: cpu_ref.avg10,
                        memory_avg10: mem_ref.avg10,
                        io_avg10: io_ref.avg10,
                        combined_stress: stress,
                    },
                );
            }
        }

        psi_map
    }
}

fn resolve_agent_runtime_pid(cgroup_path: &str) -> Option<u32> {
    let procs_path = format!("{cgroup_path}/cgroup.procs");
    let entries = fs::read_to_string(procs_path).ok()?;
    let mut candidates = Vec::new();
    for line in entries.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let pid = match line.parse::<u32>() {
            Ok(pid) => pid,
            Err(_) => continue,
        };
        let comm = fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        candidates.push((pid, comm));
    }
    select_agent_runtime_pid(&candidates)
}

fn select_agent_runtime_pid(candidates: &[(u32, String)]) -> Option<u32> {
    candidates
        .iter()
        .filter(|(_, comm)| comm == "agent-runtime")
        .map(|(pid, _)| *pid)
        .max()
        .or_else(|| candidates.iter().map(|(pid, _)| *pid).max())
}

/// Data from /proc/{pid}/io.
///
/// Both VFS-level (rchar/wchar) and block-level (read_bytes/write_bytes) counters.
/// VFS-level includes page cache hits and is the primary metric for agent activity,
/// since LLM agent processes do mostly buffered I/O that never reaches the block layer.
#[derive(Debug, Default, Clone, Copy)]
struct ProcIoData {
    /// VFS-level bytes read (includes page cache hits).
    rchar: u64,
    /// VFS-level bytes written (includes page cache).
    wchar: u64,
    /// Block-level bytes read (actual disk I/O).
    read_bytes: u64,
    /// Block-level bytes written (actual disk I/O).
    write_bytes: u64,
}

/// Reads /proc/{pid}/io for a process.
fn read_proc_io(pid: u32) -> Result<ProcIoData> {
    let path = format!("/proc/{pid}/io");
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("Reading /proc/{pid}/io"))?;

    let mut data = ProcIoData::default();
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("rchar: ") {
            data.rchar = val.trim().parse().unwrap_or(0);
        } else if let Some(val) = line.strip_prefix("wchar: ") {
            data.wchar = val.trim().parse().unwrap_or(0);
        } else if let Some(val) = line.strip_prefix("read_bytes: ") {
            data.read_bytes = val.trim().parse().unwrap_or(0);
        } else if let Some(val) = line.strip_prefix("write_bytes: ") {
            data.write_bytes = val.trim().parse().unwrap_or(0);
        }
    }

    Ok(data)
}

/// Logs PSI read errors with appropriate severity.
///
/// PermissionDenied → warn (misconfigured permissions),
/// NotFound → debug (cgroup may not exist yet),
/// Other → warn (unexpected error).
fn log_psi_error(agent: &str, file: &str, error: &anyhow::Error) {
    let io_error = error.root_cause().downcast_ref::<std::io::Error>();

    match io_error.map(|e| e.kind()) {
        Some(std::io::ErrorKind::PermissionDenied) => {
            warn!(agent, file, "PSI permission denied");
        }
        Some(std::io::ErrorKind::NotFound) => {
            debug!(agent, file, "PSI file not found (cgroup may not exist yet)");
        }
        _ => {
            warn!(agent, file, error = %error, "PSI read failed");
        }
    }
}

/// Reads cgroup io.stat file.
/// Returns Vec<(device, (rbytes, wbytes))>.
fn read_cgroup_io_stat(cgroup_path: &str) -> Result<Vec<(String, (u64, u64))>> {
    let path = format!("{cgroup_path}/io.stat");
    let content = std::fs::read_to_string(&path).with_context(|| format!("Reading {path}"))?;

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
    fn stalled_agents_only_registered() {
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        // Register only one agent
        collector.register_agent(AgentCgroupMapping {
            agent_name: "AGENT-01".to_string(),
            cgroup_path: "/sys/fs/cgroup/sentinel/agent-01".to_string(),
            cgroup_id: 100,
            pid: None,
        });
        // Record writes for both registered and unregistered cgroups
        let old_time = current_secs().saturating_sub(60);
        collector.health_checker.record_write(100, old_time); // registered, stalled
        collector.health_checker.record_write(999, old_time); // unregistered, stalled

        let snapshot = collector.collect().unwrap();
        // Only registered agent should appear in stalled list
        assert_eq!(snapshot.stalled_agents.len(), 1);
        assert_eq!(snapshot.stalled_agents[0].agent_name, "AGENT-01");
        assert_eq!(snapshot.stalled_agents[0].cgroup_id, 100);
        assert!(snapshot.stalled_agents[0].seconds_since_write >= 30);
    }

    #[test]
    fn delta_tracking_prevents_double_counting() {
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        // Simulate cgroup io.stat delta tracking
        let prev = collector.prev_cgroup_io.entry(1).or_insert((0, 0));
        assert_eq!(*prev, (0, 0));

        // First "read": cumulative = 1000
        let delta = 1000u64.saturating_sub(prev.0);
        assert_eq!(delta, 1000);
        *prev = (1000, 0);

        // Second "read": cumulative = 1500
        let delta = 1500u64.saturating_sub(prev.0);
        assert_eq!(delta, 500); // Only 500 new bytes
        *prev = (1500, 0);
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

    #[test]
    fn proc_io_data_includes_rchar_wchar() {
        let data = ProcIoData {
            rchar: 7015,
            wchar: 8,
            read_bytes: 0,
            write_bytes: 0,
        };
        // VFS-level metrics should be used when available
        assert!(data.rchar > 0);
        assert_eq!(data.read_bytes, 0); // Block-level can be zero for cached I/O
    }

    #[test]
    fn health_only_on_actual_io_delta() {
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        collector.register_agent(AgentCgroupMapping {
            agent_name: "AGENT-01".to_string(),
            cgroup_path: "/sys/fs/cgroup/sentinel/agent-01".to_string(),
            cgroup_id: 100,
            pid: None, // No PID → no /proc read → no health update
        });

        // With no PID and no cgroup io.stat, agent has only the initial timestamp
        // from register_agent() — which is fresh (now), so not stalled yet.
        let snapshot = collector.collect().unwrap();
        assert!(snapshot.stalled_agents.is_empty());
    }

    #[test]
    fn proc_io_permission_warned_flag() {
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        assert!(!collector.proc_io_permission_warned);
        collector.proc_io_permission_warned = true;
        assert!(collector.proc_io_permission_warned);
    }

    #[test]
    fn log_psi_error_handles_permission_denied() {
        // Verify the function handles different error kinds without panicking
        let perm_err = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        ));
        log_psi_error("AGENT-01", "cpu.pressure", &perm_err);

        let not_found =
            anyhow::Error::new(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"));
        log_psi_error("AGENT-01", "cpu.pressure", &not_found);

        let other = anyhow::Error::msg("unexpected");
        log_psi_error("AGENT-01", "cpu.pressure", &other);
    }

    #[test]
    fn register_sets_initial_health_timestamp() {
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        collector.register_agent(AgentCgroupMapping {
            agent_name: "AGENT-01".to_string(),
            cgroup_path: "/sys/fs/cgroup/sentinel/agent-01".to_string(),
            cgroup_id: 100,
            pid: None,
        });
        // Agent has initial timestamp → not stalled immediately
        let snapshot = collector.collect().unwrap();
        assert!(
            snapshot.stalled_agents.is_empty(),
            "Freshly registered agent must not be stalled"
        );
    }

    #[test]
    fn select_agent_runtime_pid_prefers_inner_runtime() {
        let candidates = vec![
            (1001, "bwrap".to_string()),
            (1002, "bwrap".to_string()),
            (1009, "agent-runtime".to_string()),
        ];
        assert_eq!(select_agent_runtime_pid(&candidates), Some(1009));
    }

    #[test]
    fn select_agent_runtime_pid_falls_back_to_highest_pid() {
        let candidates = vec![
            (2001, "bwrap".to_string()),
            (2007, "landlock-wrappe".to_string()),
        ];
        assert_eq!(select_agent_runtime_pid(&candidates), Some(2007));
    }
}
