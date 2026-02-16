//! cgroups v2 resource limits and PSI monitoring.

use anyhow::{Context, Result};
use tracing::{info, warn};

// PsiMetrics and parse_psi live in sentinel-common for cross-crate reuse.
pub use sentinel_common::psi::{parse_psi, PsiMetrics};

/// Resource limits fuer cgroups v2.
#[derive(Debug, Clone)]
pub struct CgroupLimits {
    pub cpu_quota_us: u64,  // 100000 = 100% einer CPU
    pub cpu_period_us: u64, // 100000 = 100ms
    pub memory_bytes: u64,  // 256 * 1024 * 1024 = 256MB
    pub io_max_iops: u32,   // 300
    pub io_max_bps: u64,    // 10 * 1024 * 1024 = 10MB/s
}

impl Default for CgroupLimits {
    fn default() -> Self {
        Self {
            cpu_quota_us: 100_000,
            cpu_period_us: 100_000,
            memory_bytes: 256 * 1024 * 1024,
            io_max_iops: 300,
            io_max_bps: 10 * 1024 * 1024,
        }
    }
}

/// Result of creating a cgroup — tracks which controllers are available.
#[derive(Debug)]
pub struct CgroupSetup {
    /// Whether the IO controller is delegated and io.max can be enforced.
    pub io_available: bool,
}

/// Erzeugt den cgroup v2 Pfad fuer einen Agenten.
pub fn cgroup_path(name: &str) -> String {
    format!("/sys/fs/cgroup/sentinel/{name}")
}

/// Creates a cgroup v2 for an agent with resource limits.
///
/// Creates the cgroup directory and writes cpu, memory, and io limits.
/// IO limits are best-effort — if the IO controller is not delegated, they are skipped.
pub fn create_cgroup(name: &str, limits: &CgroupLimits) -> Result<CgroupSetup> {
    let path = cgroup_path(name);
    std::fs::create_dir_all(&path)
        .with_context(|| format!("Failed to create cgroup dir {path}"))?;

    // CPU quota
    let cpu_max = format!("{} {}", limits.cpu_quota_us, limits.cpu_period_us);
    std::fs::write(format!("{path}/cpu.max"), &cpu_max)
        .with_context(|| format!("Failed to write cpu.max for {name}"))?;
    info!("cgroup {name}: cpu.max = {cpu_max}");

    // Memory limit
    std::fs::write(
        format!("{path}/memory.max"),
        limits.memory_bytes.to_string(),
    )
    .with_context(|| format!("Failed to write memory.max for {name}"))?;

    // IO limits (best-effort — controller may not be delegated)
    let io_max_path = format!("{path}/io.max");
    let io_available = if std::path::Path::new(&io_max_path).exists() {
        // io.max format: "MAJ:MIN rbps=X wbps=X riops=Y wiops=Y"
        // We apply symmetric read/write limits
        let io_max = format!(
            "rbps={bps} wbps={bps} riops={iops} wiops={iops}",
            bps = limits.io_max_bps,
            iops = limits.io_max_iops
        );
        match std::fs::write(&io_max_path, &io_max) {
            Ok(_) => {
                info!("cgroup {name}: io.max set");
                true
            }
            Err(e) => {
                warn!("cgroup {name}: io.max write failed (controller not delegated?): {e}");
                false
            }
        }
    } else {
        warn!("cgroup {name}: io.max not available (IO controller not delegated)");
        false
    };

    Ok(CgroupSetup { io_available })
}

/// Removes a cgroup for an agent.
///
/// Fails gracefully if the cgroup does not exist.
pub fn remove_cgroup(name: &str) -> Result<()> {
    let path = cgroup_path(name);
    if std::path::Path::new(&path).exists() {
        std::fs::remove_dir(&path).with_context(|| format!("Failed to remove cgroup {path}"))?;
        info!("Removed cgroup for {name}");
    }
    Ok(())
}

/// Adds a process to an agent's cgroup.
pub fn add_pid_to_cgroup(name: &str, pid: u32) -> Result<()> {
    let path = format!("{}/cgroup.procs", cgroup_path(name));
    std::fs::write(&path, pid.to_string())
        .with_context(|| format!("Failed to add PID {pid} to cgroup {name}"))
}

/// Sets the OOM score adjustment for a process.
///
/// -1000 = immortal (ECS core), +1000 = first to kill.
pub fn set_oom_score(pid: u32, score: i32) -> Result<()> {
    let path = format!("/proc/{pid}/oom_score_adj");
    std::fs::write(&path, score.to_string())
        .with_context(|| format!("Failed to set oom_score_adj for PID {pid}"))
}

/// Reads PSI metrics from an agent's cgroup.
///
/// resource: "cpu", "memory", or "io"
pub fn read_psi_from_cgroup(name: &str, resource: &str) -> Result<PsiMetrics> {
    let path = format!("{}/{resource}.pressure", cgroup_path(name));
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read PSI {resource} for cgroup {name}"))?;
    parse_psi(&content).with_context(|| format!("Failed to parse PSI {resource} for cgroup {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let limits = CgroupLimits::default();
        assert_eq!(limits.cpu_quota_us, 100_000);
        assert_eq!(limits.memory_bytes, 256 * 1024 * 1024);
        assert_eq!(limits.io_max_iops, 300);
        assert_eq!(limits.io_max_bps, 10 * 1024 * 1024);
    }

    #[test]
    fn cgroup_default_limits() {
        let limits = CgroupLimits::default();
        assert_eq!(limits.cpu_quota_us, 100_000);
        assert_eq!(limits.cpu_period_us, 100_000);
        assert_eq!(limits.memory_bytes, 256 * 1024 * 1024);
        assert_eq!(limits.io_max_iops, 300);
        assert_eq!(limits.io_max_bps, 10 * 1024 * 1024);
    }

    #[test]
    fn cgroup_path_format() {
        assert_eq!(cgroup_path("thomas"), "/sys/fs/cgroup/sentinel/thomas");
    }

    #[test]
    fn psi_parse() {
        let content =
            "some avg10=1.50 avg60=2.30 avg300=0.10 total=12345\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0";
        let metrics = parse_psi(content).unwrap();
        assert_eq!(metrics.avg10, 1.50);
        assert_eq!(metrics.avg60, 2.30);
        assert_eq!(metrics.avg300, 0.10);
        assert_eq!(metrics.total, 12345);
    }

    #[test]
    fn cgroup_setup_fields() {
        let setup = CgroupSetup { io_available: true };
        assert!(setup.io_available);
    }

    #[test]
    #[ignore] // Needs cgroup v2 root access (VM only)
    fn cgroup_create_remove() {
        let limits = CgroupLimits::default();
        let setup = create_cgroup("test-agent", &limits).unwrap();
        assert!(std::path::Path::new("/sys/fs/cgroup/sentinel/test-agent").exists());
        remove_cgroup("test-agent").unwrap();
        assert!(!std::path::Path::new("/sys/fs/cgroup/sentinel/test-agent").exists());
        let _ = setup;
    }

    #[test]
    #[ignore] // Needs /proc access
    fn read_psi_real() {
        // Echte PSI-Datei lesen (/proc/pressure/cpu)
    }

    #[test]
    fn remove_nonexistent_ok() {
        // Removing a non-existent cgroup should succeed silently
        assert!(remove_cgroup("does-not-exist-xyz").is_ok());
    }
}
