//! Probe loader with capability detection and graceful fallback.
//!
//! Determines whether eBPF probes can be loaded (kernel mode) or falls back
//! to userspace monitoring. Never degrades silently — always logs the mode.

use std::path::Path;

use tracing::{info, warn};

/// Monitoring mode determined at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitoringMode {
    /// eBPF probes loaded in kernel. Near-zero overhead.
    Kernel,
    /// Userspace fallback. Reads /proc and cgroup files. Higher overhead (~10ms).
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

/// Initializes monitoring in the determined mode.
///
/// In kernel mode (with `ebpf` feature): loads BPF programs via aya.
/// In userspace mode: sets up /proc and cgroup polling.
///
/// Returns the active monitoring mode.
pub fn init() -> MonitoringMode {
    let report = detect_capabilities();

    #[cfg(feature = "ebpf")]
    if report.mode == MonitoringMode::Kernel {
        match load_ebpf_probes() {
            Ok(()) => {
                info!("eBPF probes loaded successfully");
                return MonitoringMode::Kernel;
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "eBPF probe loading failed, falling back to userspace monitoring"
                );
                return MonitoringMode::Userspace;
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

    MonitoringMode::Userspace
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

    for line in status.lines() {
        if let Some(hex) = line.strip_prefix("CapEff:\t") {
            if let Ok(caps) = u64::from_str_radix(hex.trim(), 16) {
                // CAP_BPF = bit 39
                return caps & (1u64 << 39) != 0;
            }
        }
    }

    false
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

/// Loads eBPF probes via aya (only with `ebpf` feature).
#[cfg(feature = "ebpf")]
fn load_ebpf_probes() -> anyhow::Result<()> {
    // In scope:full, this will:
    // 1. Load compiled BPF .o files from embedded bytes or filesystem
    // 2. Attach fentry probes to vfs_write, tcp_connect, tcp_close
    // 3. Attach tracepoint to block:block_rq_complete
    // 4. Return handles for map access
    //
    // For scope:partial, we detect capabilities but don't load probes.
    // The aya dependency is compiled but probe objects are not yet available.
    anyhow::bail!("Probe objects not yet available (scope:partial)")
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
        let mode = init();
        assert_eq!(mode, MonitoringMode::Userspace);
    }
}
