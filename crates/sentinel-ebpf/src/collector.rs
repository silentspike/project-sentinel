//! Metric collector that polls eBPF maps or userspace sources.
//!
//! In kernel mode: reads Per-CPU Hash Maps and Ring Buffer via aya.
//! In both modes: agent-parent cgroup io.stat owns I/O bytes and operations.
//! /proc and BPF activity supplement health only, never agent I/O totals.
//!
//! Polling interval: 1s for hash maps, event-driven for ring buffer.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
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
    /// Authoritative source, present only after a valid current-cycle observation.
    pub io_collection_source: Option<&'static str>,
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
    prev_proc_io: HashMap<u64, ProcIoBaseline>,
    /// Cgroups with a valid parent io.stat observation in the current cycle.
    observed_io_cgroups: HashSet<u64>,
    /// Real parent-directory identity; the same path can be removed and recreated.
    parent_identities: HashMap<u64, (u64, u64)>,
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
            observed_io_cgroups: HashSet::new(),
            parent_identities: HashMap::new(),
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
            observed_io_cgroups: HashSet::new(),
            parent_identities: HashMap::new(),
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

    /// Upserts only the exact stable agent name and parent path.
    /// An unchanged parent inode preserves I/O totals/frontiers across runtime IDs;
    /// foreign names/paths cannot replace an existing registration.
    ///
    /// Sets an initial health timestamp so the agent has a 30s grace period
    /// before stall detection kicks in. Without this, agents that haven't
    /// produced any I/O yet would never appear as stalled (no entry in
    /// `last_write`), but the first recorded write followed by 30s inactivity
    /// would immediately trigger a false stall.
    pub fn register_agent(&mut self, mapping: AgentCgroupMapping) {
        if self.agent_mappings.iter().any(|old| {
            (old.cgroup_id == mapping.cgroup_id
                || old.agent_name == mapping.agent_name
                || old.cgroup_path == mapping.cgroup_path)
                && (old.agent_name != mapping.agent_name || old.cgroup_path != mapping.cgroup_path)
        }) {
            warn!(agent = %mapping.agent_name, cgroup = %mapping.cgroup_path,
                "Refusing conflicting agent monitoring registration");
            return;
        }
        if let Some(index) = self.agent_mappings.iter().position(|old| {
            old.agent_name == mapping.agent_name && old.cgroup_path == mapping.cgroup_path
        }) {
            let old_id = self.agent_mappings[index].cgroup_id;
            if old_id == mapping.cgroup_id {
                let _ = self.refresh_parent_identity(old_id, &mapping.cgroup_path);
                if let Some(pid) = mapping.pid {
                    self.update_agent_pid(old_id, pid);
                }
                return;
            }
            let identity = read_parent_identity(&mapping.cgroup_path).ok();
            let verified_identity = self.parent_identities.remove(&old_id);
            // Unknown metadata quarantines the transferred frontier until a successful
            // observation confirms the identity; it is not evidence of a new parent.
            if identity.is_none() || identity == verified_identity {
                self.io_profiler.rekey(old_id, mapping.cgroup_id);
            } else {
                self.io_profiler.untrack(old_id);
            }
            self.observed_io_cgroups.remove(&old_id);
            if let Some(identity) = identity.or(verified_identity) {
                self.parent_identities.insert(mapping.cgroup_id, identity);
            }
            self.health_checker.untrack(old_id);
            self.prev_proc_io.remove(&old_id);
            self.agent_mappings.remove(index);
        }
        let now = current_secs();
        debug!(
            agent = %mapping.agent_name,
            cgroup = %mapping.cgroup_path,
            cgroup_id = mapping.cgroup_id,
            "Registered agent for eBPF monitoring"
        );
        self.health_checker.record_write(mapping.cgroup_id, now);
        let _ = self.refresh_parent_identity(mapping.cgroup_id, &mapping.cgroup_path);
        self.agent_mappings.push(mapping);
    }

    fn refresh_parent_identity(&mut self, cgroup_id: u64, path: &str) -> Result<()> {
        let identity = read_parent_identity(path)?;
        if self.parent_identities.get(&cgroup_id) != Some(&identity) {
            self.io_profiler.untrack(cgroup_id);
            self.observed_io_cgroups.remove(&cgroup_id);
            self.prev_proc_io.remove(&cgroup_id);
            self.health_checker.record_write(cgroup_id, current_secs());
            self.parent_identities.insert(cgroup_id, identity);
        }
        Ok(())
    }

    /// Updates the PID for a registered agent (after process start).
    ///
    /// Resolves current owned membership; a caller-provided PID is not ownership proof.
    pub fn update_agent_pid(&mut self, cgroup_id: u64, pid: u32) {
        let resolved_pid = self
            .agent_mappings
            .iter()
            .find(|mapping| mapping.cgroup_id == cgroup_id)
            .and_then(|mapping| resolve_agent_runtime_pid(&mapping.cgroup_path));
        self.set_agent_runtime_pid(cgroup_id, resolved_pid);
        debug!(cgroup_id, requested_pid = pid, pid = ?resolved_pid,
            "Agent PID reconciled with current cgroup membership");
    }

    fn set_agent_runtime_pid(&mut self, cgroup_id: u64, pid: Option<u32>) {
        if let Some(mapping) = self
            .agent_mappings
            .iter_mut()
            .find(|m| m.cgroup_id == cgroup_id)
        {
            if mapping.pid != pid || pid.is_none() {
                self.prev_proc_io.remove(&cgroup_id);
            }
            mapping.pid = pid;
        }
    }

    /// Unregisters an agent from monitoring.
    pub fn unregister_agent(&mut self, cgroup_id: u64) {
        self.agent_mappings.retain(|m| m.cgroup_id != cgroup_id);
        self.health_checker.untrack(cgroup_id);
        self.io_profiler.untrack(cgroup_id);
        self.prev_proc_io.remove(&cgroup_id);
        self.observed_io_cgroups.remove(&cgroup_id);
        self.parent_identities.remove(&cgroup_id);
    }

    /// Read-only ownership probe used by lifecycle reconciliation and tests.
    pub fn is_agent_registered(&self, cgroup_id: u64) -> bool {
        self.agent_mappings
            .iter()
            .any(|mapping| mapping.cgroup_id == cgroup_id)
    }

    /// Collects one cycle of metrics.
    ///
    /// In userspace mode: reads /proc and cgroup files.
    /// In kernel mode: reads BPF maps (behind feature gate).
    pub fn collect(&mut self) -> Result<MetricsSnapshot> {
        let start = Instant::now();

        if self.mode == MonitoringMode::Kernel {
            if let Err(error) = self.collect_kernel() {
                warn!(error = %error, "Kernel diagnostics unavailable; parent I/O accounting remains authoritative");
            }
        }
        // Apply fresh parent/proc activity after potentially older BPF health timestamps.
        // Parent io.stat remains the exclusive bytes/ops source in either mode.
        self.collect_userspace()?;

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
            io_collection_source: (!self.agent_mappings.is_empty()
                && self.observed_io_cgroups.len() == self.agent_mappings.len())
            .then_some("agent_cgroup_io_stat"),
            network_metrics,
            psi_metrics,
            cycle_duration,
            mode: self.mode,
            ring_buffer_drops: self.ring_buffer_drops,
        })
    }

    /// Parent io.stat is the exclusive bytes/ops source; proc data is liveness only.
    fn collect_userspace(&mut self) -> Result<()> {
        self.collect_userspace_at(current_secs())
    }

    fn collect_userspace_at(&mut self, now: u64) -> Result<()> {
        self.observed_io_cgroups.clear();
        let mappings = self.agent_mappings.clone();
        for mapping in &mappings {
            let observation = self
                .refresh_parent_identity(mapping.cgroup_id, &mapping.cgroup_path)
                .and_then(|()| {
                    let devices = read_cgroup_io_stat(&mapping.cgroup_path)?;
                    if Some(read_parent_identity(&mapping.cgroup_path)?)
                        != self.parent_identities.get(&mapping.cgroup_id).copied()
                    {
                        bail!("Parent cgroup identity changed during observation");
                    }
                    Ok(devices)
                });
            match observation {
                Ok(devices) => {
                    let previous = self
                        .io_profiler
                        .get_metrics(mapping.cgroup_id)
                        .map(|m| [m.read_ops, m.write_ops, m.read_bytes, m.write_bytes])
                        .unwrap_or_default();
                    self.io_profiler.record_cgroup_snapshot(
                        mapping.cgroup_id,
                        &mapping.agent_name,
                        &devices,
                    );
                    self.observed_io_cgroups.insert(mapping.cgroup_id);
                    if self
                        .io_profiler
                        .get_metrics(mapping.cgroup_id)
                        .is_some_and(|m| {
                            [m.read_ops, m.write_ops, m.read_bytes, m.write_bytes]
                                .iter()
                                .zip(previous)
                                .any(|(current, prior)| *current > prior)
                        })
                    {
                        self.health_checker.record_write(mapping.cgroup_id, now);
                    }
                }
                Err(error) => {
                    debug!(agent = %mapping.agent_name, error = %error,
                        "Agent parent io.stat unavailable; retaining frontier without exporting stale totals");
                }
            }

            let pid = resolve_agent_runtime_pid(&mapping.cgroup_path);
            self.set_agent_runtime_pid(mapping.cgroup_id, pid);
            if let Some(pid) = pid {
                match read_proc_observation(pid) {
                    Ok((start_time, data)) => {
                        let current_pid = resolve_agent_runtime_pid(&mapping.cgroup_path);
                        if current_pid == Some(pid) {
                            self.record_proc_health(mapping.cgroup_id, pid, start_time, data, now);
                        } else {
                            self.set_agent_runtime_pid(mapping.cgroup_id, current_pid);
                        }
                    }
                    Err(error) => {
                        // A gap invalidates liveness deltas, but never changes the parent I/O frontier.
                        self.prev_proc_io.remove(&mapping.cgroup_id);
                        if !self.proc_io_permission_warned
                            && error
                                .root_cause()
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
                        {
                            warn!(pid, "Cannot read process I/O for agent liveness");
                            self.proc_io_permission_warned = true;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn record_proc_health(
        &mut self,
        cgroup_id: u64,
        pid: u32,
        start_time: u64,
        data: ProcIoData,
        now: u64,
    ) {
        let previous = self.prev_proc_io.insert(
            cgroup_id,
            ProcIoBaseline {
                pid,
                start_time,
                data,
            },
        );
        if previous.is_some_and(|prev| {
            prev.pid == pid
                && prev.start_time == start_time
                && data
                    .counters()
                    .iter()
                    .zip(prev.data.counters())
                    .any(|(current, prior)| {
                        current.checked_sub(prior).is_some_and(|delta| delta > 0)
                    })
        }) {
            self.health_checker.record_write(cgroup_id, now);
        }
    }

    /// Kernel collection: reads BPF maps via aya.
    ///
    /// Reads:
    /// 1. AGENT_HEALTH Per-CPU Hash Map → max timestamp per cgroup (stall detection)
    /// 2. TCP_EVENTS Ring Buffer -> drain TCP connect/close events
    ///
    /// IO_STATS is not an agent-owned counter source.
    ///
    /// BPF maps contain ALL system cgroups. Only registered Sentinel agents
    /// are processed — system cgroups (sshd, systemd, etc.) are filtered out.
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

            // IO_STATS deliberately excluded: block completion runs in a task that
            // need not own the request. Only parent io.stat can attribute agent I/O.

            // 3. Network: Ring Buffer → drain TCP events
            if let Some(map) = probes.network.map_mut("TCP_EVENTS") {
                let mut ring_buf = RingBuf::try_from(map).context("TCP_EVENTS ring buffer")?;
                let mut event_count = 0u64;
                while let Some(data) = ring_buf.next() {
                    if data.len() >= core::mem::size_of::<BpfTcpEvent>() {
                        // SAFETY: the ring-buffer sample is at least the size of
                        // `BpfTcpEvent`; `read_unaligned` is used because kernel
                        // ring-buffer bytes do not guarantee Rust struct alignment.
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
            .filter(|(cgroup_id, _)| self.observed_io_cgroups.contains(*cgroup_id))
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

            let cpu = match reader.read_measured_cpu_pressure() {
                Ok(v) => Some(v),
                Err(e) => {
                    log_psi_error(&mapping.agent_name, "cpu.pressure", &e);
                    None
                }
            };
            let memory = match reader.read_measured_memory_pressure() {
                Ok(v) => Some(v),
                Err(e) => {
                    log_psi_error(&mapping.agent_name, "memory.pressure", &e);
                    None
                }
            };
            let io = match reader.read_measured_io_pressure() {
                Ok(v) => Some(v),
                Err(e) => {
                    log_psi_error(&mapping.agent_name, "io.pressure", &e);
                    None
                }
            };

            // A composite measurement requires all three real inputs; fallbacks
            // belong to BioEngine/scheduling, not the telemetry export.
            if let (Some(cpu), Some(memory), Some(io)) = (cpu, memory, io) {
                let stress = crate::psi::combined_stress_factor(&cpu, &memory, &io);
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

fn resolve_agent_runtime_pid(cgroup_path: &str) -> Option<u32> {
    let runtime_path = format!("{cgroup_path}/runtime");
    let entries = match fs::read_to_string(format!("{runtime_path}/cgroup.procs")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Legacy layouts may have the runtime directly in the parent. An
            // existing but unreadable/empty runtime leaf never authorizes fallback.
            match fs::metadata(&runtime_path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::read_to_string(format!("{cgroup_path}/cgroup.procs")).ok()?
                }
                _ => return None,
            }
        }
        Err(_) => return None,
    };
    let mut candidates = Vec::new();
    for line in entries.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let pid = match line.parse::<u32>() {
            Ok(pid) if pid > 0 => pid,
            _ => continue,
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

impl ProcIoData {
    fn counters(self) -> [u64; 4] {
        [self.rchar, self.wchar, self.read_bytes, self.write_bytes]
    }
}

#[derive(Debug, Clone, Copy)]
struct ProcIoBaseline {
    pid: u32,
    start_time: u64,
    data: ProcIoData,
}

fn read_parent_identity(path: &str) -> Result<(u64, u64)> {
    let metadata = fs::metadata(path).with_context(|| format!("Reading parent cgroup {path}"))?;
    if !metadata.is_dir() {
        bail!("Parent cgroup is not a directory: {path}");
    }
    Ok((metadata.dev(), metadata.ino()))
}

fn read_proc_start_time(pid: u32) -> Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let (_, fields) = stat.rsplit_once(')').context("Invalid process stat")?;
    fields
        .split_whitespace()
        .nth(19)
        .context("Missing process start time")?
        .parse()
        .context("Invalid process start time")
}

fn read_proc_observation(pid: u32) -> Result<(u64, ProcIoData)> {
    let start_time = read_proc_start_time(pid)?;
    let data = read_proc_io(pid)?;
    if read_proc_start_time(pid)? != start_time {
        bail!("Process identity changed during observation");
    }
    Ok((start_time, data))
}

/// Reads /proc/{pid}/io for a process.
fn read_proc_io(pid: u32) -> Result<ProcIoData> {
    let path = format!("/proc/{pid}/io");
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("Reading /proc/{pid}/io"))?;

    let mut counters = [None; 4];
    for line in content.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let index = match key {
            "rchar" => 0,
            "wchar" => 1,
            "read_bytes" => 2,
            "write_bytes" => 3,
            _ => continue,
        };
        if counters[index].is_some() {
            bail!("Duplicate process I/O counter");
        }
        counters[index] = Some(
            value
                .trim()
                .parse::<u64>()
                .context("Invalid process I/O counter")?,
        );
    }
    Ok(ProcIoData {
        rchar: counters[0].context("Missing process rchar")?,
        wchar: counters[1].context("Missing process wchar")?,
        read_bytes: counters[2].context("Missing process read_bytes")?,
        write_bytes: counters[3].context("Missing process write_bytes")?,
    })
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

/// Reads authoritative parent cgroup counters in rios/wios/rbytes/wbytes order.
fn read_cgroup_io_stat(cgroup_path: &str) -> Result<HashMap<String, [u64; 4]>> {
    let path = format!("{cgroup_path}/io.stat");
    let content = fs::read_to_string(&path).with_context(|| format!("Reading {path}"))?;
    parse_cgroup_io_stat(&content)
}

fn parse_cgroup_io_stat(content: &str) -> Result<HashMap<String, [u64; 4]>> {
    let mut devices = HashMap::new();
    let mut totals = [0u64; 4];
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let Some(device) = parts.next() else { continue };
        let (major, minor) = device.split_once(':').context("Invalid io.stat device")?;
        for component in [major, minor] {
            if component.is_empty() || !component.bytes().all(|c| c.is_ascii_digit()) {
                bail!("Invalid io.stat device: {device}");
            }
            component
                .parse::<u32>()
                .context("Invalid io.stat device number")?;
        }
        let mut counters = [None; 4];
        let mut fields = HashSet::new();
        for part in parts {
            let (key, value) = part.split_once('=').context("Invalid io.stat field")?;
            if key.is_empty()
                || !fields.insert(key)
                || value.is_empty()
                || !value.bytes().all(|c| c.is_ascii_digit())
            {
                bail!("Invalid or duplicate io.stat field: {part}");
            }
            let value = value.parse::<u64>().context("Invalid io.stat counter")?;
            let index = match key {
                "rios" => Some(0),
                "wios" => Some(1),
                "rbytes" => Some(2),
                "wbytes" => Some(3),
                _ => None,
            };
            if let Some(index) = index {
                counters[index] = Some(value);
            }
        }
        // Linux blkcg_print_one_stat legitimately emits only the device when all
        // read/write counters are zero. A partially populated row is not that case.
        let counters = if fields.is_empty() {
            [0; 4]
        } else {
            [
                counters[0].context("Missing io.stat rios")?,
                counters[1].context("Missing io.stat wios")?,
                counters[2].context("Missing io.stat rbytes")?,
                counters[3].context("Missing io.stat wbytes")?,
            ]
        };
        if devices.insert(device.to_string(), counters).is_some() {
            bail!("Duplicate io.stat device: {device}");
        }
        for (total, value) in totals.iter_mut().zip(counters) {
            *total = total.checked_add(value).context("io.stat total overflow")?;
        }
    }
    Ok(devices)
}

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
    // SAFETY: `ts` is a valid writable timespec pointer and CLOCK_MONOTONIC is
    // a valid clock id on Linux. On failure, return a conservative zero value.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    if rc != 0 {
        return 0;
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
        collector.health_checker.untrack(100);
        collector.health_checker.record_write(100, old_time); // registered, stalled
        collector.health_checker.record_write(999, old_time); // unregistered, stalled

        let snapshot = collector.collect().unwrap();
        // Only registered agent should appear in stalled list
        assert_eq!(snapshot.stalled_agents.len(), 1);
        assert_eq!(snapshot.stalled_agents[0].agent_name, "AGENT-01");
        assert_eq!(snapshot.stalled_agents[0].cgroup_id, 100);
        assert!(snapshot.stalled_agents[0].seconds_since_write >= 30);
    }

    struct IoFixture(std::path::PathBuf);

    impl IoFixture {
        fn new(contents: &str) -> Self {
            let root = std::env::var_os("RUNNER_TEMP")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| "/work/tmp/project-sentinel".into());
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = root.join(format!("ebpf-io-{}-{nonce}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("io.stat"), contents).unwrap();
            Self(path)
        }
    }

    impl Drop for IoFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mapping(fixture: &IoFixture, cgroup_id: u64, pid: Option<u32>) -> AgentCgroupMapping {
        AgentCgroupMapping {
            agent_name: "io-fixture".into(),
            cgroup_path: fixture.0.to_str().unwrap().into(),
            cgroup_id,
            pid,
        }
    }

    fn counters(snapshot: &MetricsSnapshot, id: u64) -> [u64; 4] {
        let io = snapshot.io_metrics.get(&id).unwrap();
        [io.read_ops, io.write_ops, io.read_bytes, io.write_bytes]
    }

    #[test]
    fn stale_kernel_timestamp_cannot_rewind_parent_activity_in_the_next_cycle() {
        let fixture = IoFixture::new("8:0 rbytes=100 wbytes=0 rios=1 wios=0\n");
        let mut collector = EbpfCollector::new(MonitoringMode::Kernel);
        collector.register_agent(mapping(&fixture, 1, None));
        collector.health_checker.untrack(1);
        // First cycle: old kernel activity followed by new authoritative parent I/O.
        collector.health_checker.record_write(1, 90);
        collector.collect_userspace_at(100).unwrap();
        assert_eq!(
            collector.health_checker.seconds_since_last_write(1, 100),
            Some(0)
        );
        // Second cycle: the kernel repeats its old timestamp, and no new parent I/O occurs.
        collector.health_checker.record_write(1, 90);
        collector.collect_userspace_at(110).unwrap();
        assert_eq!(
            collector.health_checker.seconds_since_last_write(1, 110),
            Some(10)
        );
        assert_eq!(collector.snapshot_io()[&1].read_bytes, 100);
    }

    #[test]
    fn metadata_gap_preserves_missing_device_history_with_or_without_runtime_rekey() {
        for rekey in [false, true] {
            let fixture = IoFixture::new(
                "8:0 rbytes=100 wbytes=0 rios=1 wios=0\n8:1 rbytes=200 wbytes=0 rios=2 wios=0\n",
            );
            let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
            collector.register_agent(mapping(&fixture, 1, None));
            assert_eq!(counters(&collector.collect().unwrap(), 1), [3, 0, 300, 0]);
            let identity = collector.parent_identities[&1];
            let parked = IoFixture(fixture.0.with_extension("metadata-gap"));
            fs::rename(&fixture.0, &parked.0).unwrap();
            let unavailable = collector.collect().unwrap();
            assert_eq!(unavailable.io_collection_source, None);
            assert!(unavailable.io_metrics.is_empty());
            assert_eq!(collector.parent_identities.get(&1), Some(&identity));
            let id = if rekey {
                collector.register_agent(mapping(&fixture, 2, None));
                assert!(!collector.is_agent_registered(1));
                assert!(collector.is_agent_registered(2));
                assert!(collector.collect().unwrap().io_collection_source.is_none());
                2
            } else {
                1
            };
            assert_eq!(collector.parent_identities.get(&id), Some(&identity));
            fs::write(
                parked.0.join("io.stat"),
                "8:0 rbytes=150 wbytes=0 rios=2 wios=0\n",
            )
            .unwrap();
            fs::rename(&parked.0, &fixture.0).unwrap();
            let recovered = collector.collect().unwrap();
            assert_eq!(recovered.io_collection_source, Some("agent_cgroup_io_stat"));
            // A advances from 100 to 150; disappeared B's 200 remains accounted, not replayed/lost.
            assert_eq!(counters(&recovered, id), [4, 0, 350, 0]);
            assert_eq!(counters(&collector.collect().unwrap(), id), [4, 0, 350, 0]);
        }
    }

    #[test]
    fn unknown_metadata_rekey_resets_only_after_a_different_parent_is_observed() {
        let fixture = IoFixture::new("8:0 rbytes=100 wbytes=0 rios=1 wios=0\n");
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        collector.register_agent(mapping(&fixture, 1, None));
        collector.collect().unwrap();
        let identity = collector.parent_identities[&1];
        let parked = IoFixture(fixture.0.with_extension("old-parent"));
        fs::rename(&fixture.0, &parked.0).unwrap();
        collector.register_agent(mapping(&fixture, 2, None));
        assert_eq!(collector.parent_identities.get(&2), Some(&identity));
        assert_eq!(
            collector.io_profiler.get_metrics(2).unwrap().read_bytes,
            100
        );
        assert!(collector.collect().unwrap().io_collection_source.is_none());
        fs::create_dir(&fixture.0).unwrap();
        fs::write(
            fixture.0.join("io.stat"),
            "8:0 rbytes=50 wbytes=0 rios=1 wios=0\n",
        )
        .unwrap();
        let recovered = collector.collect().unwrap();
        assert_ne!(collector.parent_identities[&2], identity);
        assert_eq!(counters(&recovered, 2), [1, 0, 50, 0]);
        assert_eq!(recovered.io_collection_source, Some("agent_cgroup_io_stat"));
    }

    #[test]
    fn explicitly_empty_runtime_membership_cannot_use_an_unrelated_pid_for_health() {
        let fixture = IoFixture::new("");
        let pid = std::process::id();
        fs::create_dir(fixture.0.join("runtime")).unwrap();
        fs::write(fixture.0.join("runtime/cgroup.procs"), "").unwrap();
        fs::write(fixture.0.join("cgroup.procs"), format!("{pid}\n")).unwrap();
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        collector.register_agent(mapping(&fixture, 1, Some(pid)));
        collector.health_checker.untrack(1);
        collector.health_checker.record_write(1, 0);
        collector.record_proc_health(
            1,
            pid,
            read_proc_start_time(pid).unwrap(),
            ProcIoData::default(),
            0,
        );
        collector.update_agent_pid(1, pid);
        assert!(collector.prev_proc_io.is_empty());
        assert_eq!(collector.agent_mappings[0].pid, None);
        collector.collect_userspace_at(100).unwrap();
        collector.collect_userspace_at(110).unwrap();
        assert_eq!(
            collector.health_checker.seconds_since_last_write(1, 110),
            Some(110)
        );
        assert_eq!(collector.health_checker.stalled_agents(110), vec![1]);
        assert!(collector.prev_proc_io.is_empty());
        assert_eq!(collector.snapshot_io()[&1].read_bytes, 0);
    }

    fn write_pressure(fixture: &IoFixture, file: &str, avg10: u64) {
        fs::write(
            fixture.0.join(file),
            format!(
            "some avg10={avg10} avg60=0 avg300=0 total=0\nfull avg10=0 avg60=0 avg300=0 total=0\n",
        ),
        )
        .unwrap();
    }

    #[test]
    fn composite_psi_requires_all_actual_valid_inputs_and_recovers_measured_zero() {
        let fixture = IoFixture::new("");
        let partial = IoFixture::new("");
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        collector.register_agent(mapping(&fixture, 1, None));
        let mut other = mapping(&partial, 2, None);
        other.agent_name = "partial-pressure".into();
        collector.register_agent(other);
        write_pressure(&partial, "cpu.pressure", 0);
        write_pressure(&fixture, "cpu.pressure", 80);
        write_pressure(&fixture, "memory.pressure", 40);
        let missing = collector.collect().unwrap();
        assert!(missing.psi_metrics.is_empty());
        assert!(!crate::exporter::MetricsExporter::export_snapshot(&missing)
            .contains("sentinel_agent_cpu_pressure_stress{"));
        write_pressure(&fixture, "io.pressure", 20);
        let observed = collector.collect().unwrap();
        assert_eq!(observed.psi_metrics.len(), 1);
        assert!(!observed.psi_metrics.contains_key("partial-pressure"));
        assert!((observed.psi_metrics["io-fixture"].combined_stress - 0.56).abs() < 0.0001);
        assert!(crate::exporter::MetricsExporter::export_snapshot(&observed)
            .contains("sentinel_agent_cpu_pressure_stress{agent=\"io-fixture\"} 0.5600"));
        for invalid in [
            "some",
            "some avg10=0 avg60=0 total=0",
            "some avg10=0 avg60=0 avg300=0 total=0 avg10=1",
            "some avg10=NaN avg60=0 avg300=0 total=0",
            "some avg10=101 avg60=0 avg300=0 total=0",
        ] {
            fs::write(fixture.0.join("io.pressure"), invalid).unwrap();
            let unavailable = collector.collect().unwrap();
            assert!(unavailable.psi_metrics.is_empty(), "{invalid}");
            assert!(
                !crate::exporter::MetricsExporter::export_snapshot(&unavailable)
                    .contains("sentinel_agent_cpu_pressure_stress{")
            );
        }
        for file in ["cpu.pressure", "memory.pressure", "io.pressure"] {
            write_pressure(&fixture, file, 0);
        }
        let zero = collector.collect().unwrap();
        assert_eq!(zero.psi_metrics.len(), 1);
        assert_eq!(zero.psi_metrics["io-fixture"].combined_stress, 0.0);
        assert!(crate::exporter::MetricsExporter::export_snapshot(&zero)
            .contains("sentinel_agent_cpu_pressure_stress{agent=\"io-fixture\"} 0.0000"));
    }

    #[test]
    fn parent_cgroup_covers_runtime_and_commands_in_both_modes_once() {
        let fixture = IoFixture::new("8:0 rbytes=100 wbytes=300 rios=7 wios=11\n");
        for (child, stat) in [
            ("runtime", "8:0 rbytes=60 wbytes=100 rios=3 wios=4\n"),
            ("commands", "8:0 rbytes=40 wbytes=200 rios=4 wios=7\n"),
        ] {
            fs::create_dir(fixture.0.join(child)).unwrap();
            fs::write(fixture.0.join(child).join("io.stat"), stat).unwrap();
        }
        for mode in [MonitoringMode::Userspace, MonitoringMode::Kernel] {
            let mut collector = EbpfCollector::new(mode);
            collector.register_agent(mapping(&fixture, 1, Some(std::process::id())));
            let snapshot = collector.collect().unwrap();
            assert_eq!(snapshot.io_collection_source, Some("agent_cgroup_io_stat"));
            assert_eq!(counters(&snapshot, 1), [7, 11, 100, 300]);
            assert_eq!(
                counters(&collector.collect().unwrap(), 1),
                [7, 11, 100, 300]
            );
            assert_eq!(collector.io_profiler.all_metrics().len(), 1);
        }
    }

    #[test]
    fn proc_unavailable_and_recovery_never_overlap_parent_io() {
        let fixture = IoFixture::new("8:0 rbytes=1234 wbytes=5678 rios=5 wios=9\n");
        fs::write(
            fixture.0.join("cgroup.procs"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        collector.register_agent(mapping(&fixture, 1, Some(std::process::id())));
        assert_eq!(
            counters(&collector.collect().unwrap(), 1),
            [5, 9, 1234, 5678]
        );
        fs::write(fixture.0.join("cgroup.procs"), format!("{}\n", u32::MAX)).unwrap();
        collector.update_agent_pid(1, u32::MAX);
        assert_eq!(
            counters(&collector.collect().unwrap(), 1),
            [5, 9, 1234, 5678]
        );
        assert!(!collector.prev_proc_io.contains_key(&1));
        fs::write(
            fixture.0.join("io.stat"),
            "8:0 rbytes=1254 wbytes=5708 rios=8 wios=13\n",
        )
        .unwrap();
        fs::write(
            fixture.0.join("cgroup.procs"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        collector.update_agent_pid(1, std::process::id());
        assert_eq!(
            counters(&collector.collect().unwrap(), 1),
            [8, 13, 1254, 5708]
        );
        assert_eq!(
            counters(&collector.collect().unwrap(), 1),
            [8, 13, 1254, 5708]
        );
    }

    #[test]
    fn empty_device_only_and_explicit_zero_stats_are_real_observations() {
        for contents in ["", "8:0\n", "8:0 rbytes=0 wbytes=0 rios=0 wios=0\n"] {
            let fixture = IoFixture::new(contents);
            let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
            collector.register_agent(mapping(&fixture, 1, None));
            let snapshot = collector.collect().unwrap();
            assert_eq!(snapshot.io_collection_source, Some("agent_cgroup_io_stat"));
            assert_eq!(counters(&snapshot, 1), [0; 4]);
            let output = crate::exporter::MetricsExporter::export_snapshot(&snapshot);
            assert!(
                output.contains("sentinel_io_collection_source{source=\"agent_cgroup_io_stat\"} 1")
            );
            assert!(output.contains("sentinel_io_ops_total{cgroup_id=\"1\",cgroup_name=\"io-fixture\",direction=\"read\"} 0"));
        }
    }

    #[test]
    fn missing_and_malformed_stats_hide_stale_source_and_retain_exact_frontier() {
        let fixture = IoFixture::new("8:0 rbytes=100 wbytes=200 rios=3 wios=4\n");
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        collector.register_agent(mapping(&fixture, 1, Some(std::process::id())));
        assert_eq!(counters(&collector.collect().unwrap(), 1), [3, 4, 100, 200]);
        fs::remove_file(fixture.0.join("io.stat")).unwrap();
        let missing = collector.collect().unwrap();
        assert!(missing.io_metrics.is_empty());
        assert_eq!(missing.io_collection_source, None);
        for contents in [
            "8:0 rbytes=100 wbytes=200\n",
            "8:0 rbytes=bad wbytes=200 rios=3 wios=4\n",
            "8:0 rbytes=100 wbytes=200 rios=3 wios=4\n8:1 rios=1\n",
        ] {
            fs::write(fixture.0.join("io.stat"), contents).unwrap();
            let snapshot = collector.collect().unwrap();
            assert!(snapshot.io_metrics.is_empty());
            assert_eq!(snapshot.io_collection_source, None);
            assert!(
                !crate::exporter::MetricsExporter::export_snapshot(&snapshot)
                    .contains("sentinel_io_collection_source")
            );
        }
        fs::write(
            fixture.0.join("io.stat"),
            "8:0 rbytes=150 wbytes=250 rios=5 wios=7\n",
        )
        .unwrap();
        assert_eq!(counters(&collector.collect().unwrap(), 1), [5, 7, 150, 250]);
    }

    #[test]
    fn partial_registered_agent_coverage_never_claims_global_source() {
        let observed = IoFixture::new("");
        let missing = IoFixture::new("");
        fs::remove_file(missing.0.join("io.stat")).unwrap();
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        assert_eq!(collector.collect().unwrap().io_collection_source, None);
        collector.register_agent(mapping(&observed, 1, None));
        let mut other = mapping(&missing, 2, None);
        other.agent_name = "other-agent".into();
        collector.register_agent(other);
        let snapshot = collector.collect().unwrap();
        assert_eq!(snapshot.io_metrics.len(), 1);
        assert_eq!(snapshot.io_collection_source, None);
        assert!(
            !crate::exporter::MetricsExporter::export_snapshot(&snapshot)
                .contains("sentinel_io_collection_source")
        );
        fs::write(missing.0.join("io.stat"), "").unwrap();
        assert_eq!(
            collector.collect().unwrap().io_collection_source,
            Some("agent_cgroup_io_stat")
        );
    }

    #[test]
    fn parser_rejects_partial_malformed_duplicate_and_overflow_samples() {
        for contents in [
            "not-a-device\n",
            "8:0 rbytes=1\n",
            "8:0 rbytes=-1 wbytes=0 rios=0 wios=0\n",
            "8:0 rbytes=1.5 wbytes=0 rios=0 wios=0\n",
            "8:0 rbytes=1 wbytes=0 rios=bad wios=0\n",
            "8:0 rbytes=1 wbytes=0 rios=0 wios=0 rbytes=2\n",
            "8:0 rbytes=1 wbytes=0 rios=0 wios=0 dios=bad\n",
            "8:0\n8:0\n",
            "8:0 rbytes=18446744073709551615 wbytes=0 rios=0 wios=0\n8:1 rbytes=1 wbytes=0 rios=0 wios=0\n",
        ] {
            assert!(parse_cgroup_io_stat(contents).is_err(), "{contents}");
        }
        let devices =
            parse_cgroup_io_stat("8:0 wios=9 rios=7 wbytes=200 rbytes=100 dbytes=0 dios=0\n8:1\n")
                .unwrap();
        assert_eq!(devices["8:0"], [7, 9, 100, 200]);
        assert_eq!(devices["8:1"], [0; 4]);
    }

    #[test]
    fn runtime_reincarnation_transfers_same_parent_frontier_not_lifetime_total_twice() {
        let fixture = IoFixture::new("8:0 rbytes=100 wbytes=200 rios=3 wios=4\n");
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        collector.register_agent(mapping(&fixture, 1, None));
        collector.collect().unwrap();
        collector.record_proc_health(1, 100, 10, ProcIoData::default(), current_secs());
        collector.register_agent(mapping(&fixture, 2, None));
        assert!(!collector.is_agent_registered(1));
        assert!(collector.is_agent_registered(2));
        assert!(!collector.prev_proc_io.contains_key(&1));
        assert!(collector
            .health_checker
            .seconds_since_last_write(1, current_secs())
            .is_none());
        let snapshot = collector.collect().unwrap();
        assert!(!snapshot.io_metrics.contains_key(&1));
        assert_eq!(counters(&snapshot, 2), [3, 4, 100, 200]);
        fs::write(
            fixture.0.join("io.stat"),
            "8:0 rbytes=150 wbytes=250 rios=5 wios=7\n",
        )
        .unwrap();
        assert_eq!(counters(&collector.collect().unwrap(), 2), [5, 7, 150, 250]);
    }

    #[test]
    fn recreated_parent_path_does_not_inherit_old_inode_counters() {
        let fixture = IoFixture::new("8:0 rbytes=100 wbytes=200 rios=3 wios=4\n");
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        collector.register_agent(mapping(&fixture, 1, None));
        collector.collect().unwrap();
        let retired = IoFixture(fixture.0.with_extension("retired"));
        fs::rename(&fixture.0, &retired.0).unwrap();
        fs::create_dir(&fixture.0).unwrap();
        fs::write(
            fixture.0.join("io.stat"),
            "8:0 rbytes=20 wbytes=30 rios=1 wios=2\n",
        )
        .unwrap();
        assert_ne!(
            read_parent_identity(fixture.0.to_str().unwrap()).unwrap(),
            read_parent_identity(retired.0.to_str().unwrap()).unwrap()
        );
        collector.register_agent(mapping(&fixture, 2, None));
        assert!(!collector.is_agent_registered(1));
        assert_eq!(counters(&collector.collect().unwrap(), 2), [1, 2, 20, 30]);
    }

    #[test]
    fn exact_owner_upsert_is_idempotent_and_foreign_name_or_path_cannot_replace_it() {
        let fixture = IoFixture::new("8:0 rbytes=100 wbytes=200 rios=3 wios=4\n");
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        collector.register_agent(mapping(&fixture, 1, None));
        collector.collect().unwrap();
        collector.health_checker.untrack(1);
        collector
            .health_checker
            .record_write(1, current_secs().saturating_sub(60));
        collector.register_agent(mapping(&fixture, 1, None));
        assert_eq!(collector.collect().unwrap().stalled_agents.len(), 1);
        let mut foreign = mapping(&fixture, 2, None);
        foreign.agent_name = "foreign".into();
        collector.register_agent(foreign);
        let mut foreign = mapping(&fixture, 2, None);
        foreign.cgroup_path.push_str("/foreign");
        collector.register_agent(foreign);
        assert!(collector.is_agent_registered(1));
        assert!(!collector.is_agent_registered(2));
        assert_eq!(collector.agent_mappings.len(), 1);
        assert_eq!(counters(&collector.collect().unwrap(), 1), [3, 4, 100, 200]);
    }

    #[test]
    fn runtime_pid_preferred_over_parent_and_commands_sibling() {
        let fixture = IoFixture::new("");
        fs::create_dir(fixture.0.join("runtime")).unwrap();
        fs::create_dir(fixture.0.join("commands")).unwrap();
        fs::write(fixture.0.join("cgroup.procs"), "4001\n").unwrap();
        fs::write(fixture.0.join("runtime/cgroup.procs"), "2001\n").unwrap();
        fs::write(fixture.0.join("commands/cgroup.procs"), "9001\n").unwrap();
        assert_eq!(
            resolve_agent_runtime_pid(fixture.0.to_str().unwrap()),
            Some(2001)
        );
        fs::write(fixture.0.join("runtime/cgroup.procs"), "").unwrap();
        assert_eq!(resolve_agent_runtime_pid(fixture.0.to_str().unwrap()), None);
        fs::remove_file(fixture.0.join("runtime/cgroup.procs")).unwrap();
        assert_eq!(resolve_agent_runtime_pid(fixture.0.to_str().unwrap()), None);
        fs::write(fixture.0.join("runtime/cgroup.procs"), "0\ninvalid\n").unwrap();
        assert_eq!(resolve_agent_runtime_pid(fixture.0.to_str().unwrap()), None);
    }

    #[test]
    fn pid_update_and_pid_reuse_reset_proc_liveness_baseline_not_parent_io() {
        let fixture = IoFixture::new("8:0 rbytes=100 wbytes=200 rios=3 wios=4\n");
        fs::write(fixture.0.join("cgroup.procs"), "1001\n").unwrap();
        let mut collector = EbpfCollector::new(MonitoringMode::Userspace);
        collector.register_agent(mapping(&fixture, 1, Some(1001)));
        collector.collect().unwrap();
        let sample = ProcIoData {
            rchar: 100,
            wchar: 200,
            ..Default::default()
        };
        collector.record_proc_health(1, 1001, 10, sample, 100);
        fs::write(fixture.0.join("cgroup.procs"), "1002\n").unwrap();
        collector.update_agent_pid(1, 1002);
        assert!(!collector.prev_proc_io.contains_key(&1));
        collector.health_checker.untrack(1);
        collector.health_checker.record_write(1, 100);
        collector.record_proc_health(1, 1002, 20, sample, 101);
        let previous_health = collector.health_checker.seconds_since_last_write(1, 1000);
        collector.record_proc_health(
            1,
            1002,
            21,
            ProcIoData {
                rchar: 1000,
                wchar: 2000,
                ..Default::default()
            },
            102,
        );
        assert_eq!(
            collector.health_checker.seconds_since_last_write(1, 1000),
            previous_health
        );
        assert_eq!(collector.prev_proc_io[&1].start_time, 21);
        collector.record_proc_health(
            1,
            1002,
            21,
            ProcIoData {
                rchar: 1001,
                wchar: 2001,
                ..Default::default()
            },
            103,
        );
        assert_eq!(
            collector.health_checker.seconds_since_last_write(1, 1000),
            Some(897)
        );
        assert_eq!(counters(&collector.collect().unwrap(), 1), [3, 4, 100, 200]);
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
        collector.observed_io_cgroups.insert(1);

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
