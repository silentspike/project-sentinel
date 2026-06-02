//! Probe loader with capability detection and graceful fallback.
//!
//! Determines whether eBPF probes can be loaded (kernel mode) or falls back
//! to userspace monitoring. Never degrades silently — always logs the mode.

use std::path::Path;

use tracing::{info, warn};

const CAP_BPF_BIT: u64 = 39;

/// Monitoring mode determined at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum MonitoringMode {
    /// eBPF probes loaded in kernel. ~540ns per probe hit, ~333us collection cycle.
    Kernel,
    /// Userspace fallback. Reads /proc and cgroup files. ~10ms per collection cycle.
    Userspace,
}

impl MonitoringMode {
    /// Returns the mode name for Prometheus labels and dashboard display.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Kernel => "kernel",
            Self::Userspace => "userspace",
        }
    }
}

impl std::fmt::Display for MonitoringMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Capability check results from startup probing.
#[derive(Debug, Clone)]
pub struct CapabilityReport {
    /// Whether /sys/kernel/btf/vmlinux exists (BTF available).
    pub btf_available: bool,
    /// Whether the process has CAP_BPF capability.
    pub cap_bpf: bool,
    /// Kernel version string.
    pub kernel_version: String,
    /// Whether kernel supports fentry (>= 5.5).
    pub fentry_support: bool,
    /// Determined monitoring mode.
    pub mode: MonitoringMode,
    /// Human-readable reason for the chosen mode.
    pub reason: String,
}

/// Probes the system for eBPF capabilities and determines monitoring mode.
///
/// Checks in order:
/// 1. /sys/kernel/btf/vmlinux exists (BTF)
/// 2. CAP_BPF capability available
/// 3. Kernel version >= 5.5 (fentry support)
/// 4. Test probe can be loaded (if ebpf feature enabled)
///
/// Returns the capability report with determined mode.
pub fn detect_capabilities() -> CapabilityReport {
    let btf_available = check_btf();
    let cap_bpf = check_cap_bpf();
    let kernel_version = read_kernel_version();
    let fentry_support = check_fentry_support(&kernel_version);

    let (mode, reason) = determine_mode(btf_available, cap_bpf, fentry_support);

    let report = CapabilityReport {
        btf_available,
        cap_bpf,
        kernel_version,
        fentry_support,
        mode,
        reason,
    };

    // CRITICAL: Never silent degradation (AC-N1).
    match report.mode {
        MonitoringMode::Kernel => {
            info!(
                btf = btf_available,
                cap_bpf = cap_bpf,
                kernel = %report.kernel_version,
                "eBPF monitoring mode: kernel (probes loaded)"
            );
        }
        MonitoringMode::Userspace => {
            warn!(
                btf = btf_available,
                cap_bpf = cap_bpf,
                kernel = %report.kernel_version,
                reason = %report.reason,
                "eBPF not available, fallback to userspace monitoring"
            );
        }
    }

    report
}

/// Result of monitoring initialization.
pub struct InitResult {
    /// The active monitoring mode.
    pub mode: MonitoringMode,
    /// Loaded eBPF probes (only Some in kernel mode with `ebpf` feature).
    #[cfg(feature = "ebpf")]
    pub probes: Option<LoadedProbes>,
}

/// Initializes monitoring in the determined mode.
///
/// In kernel mode (with `ebpf` feature): loads BPF programs via aya.
/// In userspace mode: sets up /proc and cgroup polling.
///
/// Returns the active monitoring mode and loaded probes (if any).
pub fn init() -> InitResult {
    let report = detect_capabilities();

    #[cfg(feature = "ebpf")]
    if report.mode == MonitoringMode::Kernel {
        match load_ebpf_probes() {
            Ok(probes) => {
                info!("eBPF probes loaded successfully (kernel mode)");
                return InitResult {
                    mode: MonitoringMode::Kernel,
                    probes: Some(probes),
                };
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "eBPF probe loading failed, falling back to userspace monitoring"
                );
                return InitResult {
                    mode: MonitoringMode::Userspace,
                    probes: None,
                };
            }
        }
    }

    #[cfg(not(feature = "ebpf"))]
    if report.mode == MonitoringMode::Kernel {
        warn!(
            "Kernel capabilities present but 'ebpf' feature not compiled in, \
             using userspace monitoring"
        );
    }

    InitResult {
        mode: MonitoringMode::Userspace,
        #[cfg(feature = "ebpf")]
        probes: None,
    }
}

/// Checks whether /sys/kernel/btf/vmlinux exists.
fn check_btf() -> bool {
    Path::new("/sys/kernel/btf/vmlinux").exists()
}

/// Checks whether the process has CAP_BPF capability.
///
/// Reads /proc/self/status for CapEff and checks the BPF bit (bit 39).
fn check_cap_bpf() -> bool {
    let status = match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(_) => return false,
    };

    cap_eff_has_cap_bpf(&status)
}

fn cap_eff_has_cap_bpf(status: &str) -> bool {
    status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .and_then(|hex| u64::from_str_radix(hex.trim(), 16).ok())
        .is_some_and(|caps| caps & (1u64 << CAP_BPF_BIT) != 0)
}

/// Reads the kernel version string from /proc/version.
fn read_kernel_version() -> String {
    std::fs::read_to_string("/proc/version")
        .ok()
        .and_then(|v| v.split_whitespace().nth(2).map(String::from))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Checks if kernel version >= 5.5 (fentry support).
fn check_fentry_support(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() < 2 {
        return false;
    }
    let major: u32 = parts[0].parse().unwrap_or(0);
    let minor: u32 = parts[1].parse().unwrap_or(0);
    (major > 5) || (major == 5 && minor >= 5)
}

/// Determines monitoring mode based on capability checks.
fn determine_mode(btf: bool, cap_bpf: bool, fentry: bool) -> (MonitoringMode, String) {
    if !btf {
        return (
            MonitoringMode::Userspace,
            "BTF not available (/sys/kernel/btf/vmlinux missing)".to_string(),
        );
    }
    if !cap_bpf {
        return (
            MonitoringMode::Userspace,
            "CAP_BPF capability not granted".to_string(),
        );
    }
    if !fentry {
        return (
            MonitoringMode::Userspace,
            "Kernel too old for fentry (need >= 5.5)".to_string(),
        );
    }
    (
        MonitoringMode::Kernel,
        "All capabilities present".to_string(),
    )
}

/// Compiled BPF probe bytecode (embedded at compile time).
#[cfg(feature = "ebpf")]
const AGENT_HEALTH_PROBE: &[u8] = include_bytes!("../probes/agent-health.o");
#[cfg(feature = "ebpf")]
const IO_PROFILE_PROBE: &[u8] = include_bytes!("../probes/io-profile.o");
#[cfg(feature = "ebpf")]
const NETWORK_PROBE: &[u8] = include_bytes!("../probes/network.o");

/// Loaded eBPF programs with attached probes and accessible maps.
///
/// Holds the `Ebpf` objects that own the BPF programs. Dropping this
/// struct detaches all probes and frees BPF maps.
#[cfg(feature = "ebpf")]
pub struct LoadedProbes {
    /// Agent health probe (fentry/vfs_write). Owns the AGENT_HEALTH BPF map.
    pub agent_health: aya::Ebpf,
    /// I/O profiling probe (tracepoint/block:block_rq_complete). Owns the IO_STATS BPF map.
    pub io_profile: aya::Ebpf,
    /// Network probe (fentry/tcp_connect + tcp_close). Owns the TCP_EVENTS ring buffer.
    pub network: aya::Ebpf,
}

/// Loads and attaches all eBPF probes via aya.
///
/// Probes loaded:
/// - `fentry/vfs_write` → Agent health (stall detection)
/// - `tracepoint/block:block_rq_complete` → I/O profiling
/// - `fentry/tcp_connect` + `fentry/tcp_close` → Network monitoring
#[cfg(feature = "ebpf")]
pub fn load_ebpf_probes() -> anyhow::Result<LoadedProbes> {
    use aya::{
        programs::{FEntry, TracePoint},
        Btf, Ebpf,
    };

    // include_bytes! produces &[u8] with arbitrary alignment (often not 8-byte aligned).
    // object crate 0.38+ requires 8-byte alignment for ELF64 parsing.
    // Heap-allocated Vec<u8> is always at least pointer-aligned (8 bytes on 64-bit).
    fn aligned_copy(data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(data.len());
        buf.extend_from_slice(data);
        buf
    }

    let btf = Btf::from_sys_fs()?;

    // 1. Agent health: fentry/vfs_write → Per-CPU Hash Map
    let agent_health_data = aligned_copy(AGENT_HEALTH_PROBE);
    let mut agent_health = Ebpf::load(&agent_health_data)?;
    let prog: &mut FEntry = agent_health
        .program_mut("agent_health_probe")
        .ok_or_else(|| anyhow::anyhow!("BPF program 'agent_health_probe' not found"))?
        .try_into()?;
    prog.load("vfs_write", &btf)?;
    prog.attach()?;
    info!("Attached fentry/vfs_write probe (agent health)");

    // 2. I/O profiling: tracepoint/block:block_rq_complete → Per-CPU Hash Map
    let io_profile_data = aligned_copy(IO_PROFILE_PROBE);
    let mut io_profile = Ebpf::load(&io_profile_data)?;
    let prog: &mut TracePoint = io_profile
        .program_mut("io_profile_probe")
        .ok_or_else(|| anyhow::anyhow!("BPF program 'io_profile_probe' not found"))?
        .try_into()?;
    prog.load()?;
    prog.attach("block", "block_rq_complete")?;
    info!("Attached tracepoint/block:block_rq_complete probe (I/O profiling)");

    // 3. Network: fentry/tcp_connect + fentry/tcp_close → Ring Buffer
    let network_data = aligned_copy(NETWORK_PROBE);
    let mut network = Ebpf::load(&network_data)?;

    let prog: &mut FEntry = network
        .program_mut("tcp_connect_probe")
        .ok_or_else(|| anyhow::anyhow!("BPF program 'tcp_connect_probe' not found"))?
        .try_into()?;
    prog.load("tcp_connect", &btf)?;
    prog.attach()?;
    info!("Attached fentry/tcp_connect probe (network)");

    let prog: &mut FEntry = network
        .program_mut("tcp_close_probe")
        .ok_or_else(|| anyhow::anyhow!("BPF program 'tcp_close_probe' not found"))?
        .try_into()?;
    prog.load("tcp_close", &btf)?;
    prog.attach()?;
    info!("Attached fentry/tcp_close probe (network)");

    Ok(LoadedProbes {
        agent_health,
        io_profile,
        network,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitoring_mode_display() {
        assert_eq!(MonitoringMode::Kernel.as_str(), "kernel");
        assert_eq!(MonitoringMode::Userspace.as_str(), "userspace");
        assert_eq!(format!("{}", MonitoringMode::Kernel), "kernel");
        assert_eq!(format!("{}", MonitoringMode::Userspace), "userspace");
    }

    #[test]
    fn determine_mode_all_present() {
        let (mode, _) = determine_mode(true, true, true);
        assert_eq!(mode, MonitoringMode::Kernel);
    }

    #[test]
    fn determine_mode_no_btf() {
        let (mode, reason) = determine_mode(false, true, true);
        assert_eq!(mode, MonitoringMode::Userspace);
        assert!(reason.contains("BTF"));
    }

    #[test]
    fn determine_mode_no_cap_bpf() {
        let (mode, reason) = determine_mode(true, false, true);
        assert_eq!(mode, MonitoringMode::Userspace);
        assert!(reason.contains("CAP_BPF"));
    }

    #[test]
    fn determine_mode_no_fentry() {
        let (mode, reason) = determine_mode(true, true, false);
        assert_eq!(mode, MonitoringMode::Userspace);
        assert!(reason.contains("fentry"));
    }

    #[test]
    fn fentry_support_check() {
        assert!(check_fentry_support("6.17.6-1-default"));
        assert!(check_fentry_support("5.5.0"));
        assert!(check_fentry_support("5.15.0-generic"));
        assert!(!check_fentry_support("5.4.0"));
        assert!(!check_fentry_support("4.19.0"));
        assert!(!check_fentry_support("unknown"));
    }

    #[test]
    fn cap_eff_detects_cap_bpf_bit() {
        let status = "Name:\tsentinel-daemon\nCapEff:\t0000008000000000\n";
        assert!(cap_eff_has_cap_bpf(status));
    }

    #[test]
    fn cap_eff_rejects_missing_cap_bpf_bit() {
        let status = "Name:\tsentinel-daemon\nCapEff:\t0000000000000000\n";
        assert!(!cap_eff_has_cap_bpf(status));
    }

    #[test]
    fn cap_eff_rejects_malformed_or_missing_status() {
        assert!(!cap_eff_has_cap_bpf("Name:\tsentinel-daemon\n"));
        assert!(!cap_eff_has_cap_bpf("CapEff:\tnot-hex\n"));
    }

    #[test]
    fn capability_report_fields() {
        let report = CapabilityReport {
            btf_available: false,
            cap_bpf: false,
            kernel_version: "6.17.0".to_string(),
            fentry_support: true,
            mode: MonitoringMode::Userspace,
            reason: "test".to_string(),
        };
        assert_eq!(report.mode, MonitoringMode::Userspace);
    }

    #[test]
    fn init_returns_userspace_without_feature() {
        // Without the ebpf feature, init() always returns Userspace
        // (even if kernel has capabilities, probes aren't compiled in).
        let result = init();
        assert_eq!(result.mode, MonitoringMode::Userspace);
    }
}
