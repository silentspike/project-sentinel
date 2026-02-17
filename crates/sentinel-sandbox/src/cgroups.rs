//! cgroups v2 resource limits and PSI monitoring.

use anyhow::{Context, Result};
use tracing::{info, warn};

// PsiMetrics and parse_psi live in sentinel-common for cross-crate reuse.
pub use sentinel_common::psi::{parse_psi, PsiMetrics};

/// Default mount path for agent storage (tmpfs on VM).
const AGENT_STORAGE_PATH: &str = "/ram";
/// Fallback mount path when /ram is not available.
const FALLBACK_STORAGE_PATH: &str = "/";

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

/// Discovers the whole-disk block device (major:minor) backing a given mount path.
///
/// Parses `/proc/self/mountinfo` to find the device for the filesystem
/// containing `mount_path`. If the device is a partition (e.g. `8:1` = sda1),
/// resolves it to the whole disk device (e.g. `8:0` = sda) because cgroup v2
/// `io.max` only accepts whole-disk devices.
///
/// Falls back to `/` if `mount_path` is not found.
pub fn discover_block_device(mount_path: &str) -> Option<String> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let dev = discover_device_from_mountinfo(&mountinfo, mount_path)
        .or_else(|| discover_device_from_mountinfo(&mountinfo, FALLBACK_STORAGE_PATH))?;
    // Resolve partition to whole-disk device for io.max compatibility
    Some(resolve_to_whole_disk(&dev).unwrap_or(dev))
}

/// Resolves a partition device (e.g. `8:1`) to its whole-disk device (e.g. `8:0`).
///
/// Uses `/sys/dev/block/MAJ:MIN/partition` to detect partitions and reads
/// the parent device from `/sys/dev/block/MAJ:MIN/../dev`.
fn resolve_to_whole_disk(dev: &str) -> Option<String> {
    let partition_path = format!("/sys/dev/block/{dev}/partition");
    if std::path::Path::new(&partition_path).exists() {
        // It's a partition — read the parent (whole disk) device number
        let parent_dev_path = format!("/sys/dev/block/{dev}/../dev");
        let parent_dev = std::fs::read_to_string(parent_dev_path).ok()?;
        Some(parent_dev.trim().to_string())
    } else {
        None // Already a whole-disk device
    }
}

/// Parses mountinfo content to find the device for a specific mount point.
///
/// mountinfo format (per line):
/// `ID PARENT_ID MAJ:MIN ROOT MOUNT_POINT OPTIONS ... - FS_TYPE SOURCE OPTIONS`
/// Field 3 (0-indexed: 2) is the device in `major:minor` format.
/// Field 5 (0-indexed: 4) is the mount point.
fn discover_device_from_mountinfo(mountinfo: &str, mount_point: &str) -> Option<String> {
    for line in mountinfo.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 5 && fields[4] == mount_point {
            return Some(fields[2].to_string());
        }
    }
    None
}

/// Enables the IO controller in a cgroup's subtree_control.
///
/// Writes `"+io"` to `{cgroup_path}/cgroup.subtree_control`.
/// Best-effort: returns `true` on success, `false` on failure.
pub fn enable_io_controller(parent_cgroup_path: &str) -> bool {
    let subtree_path = format!("{parent_cgroup_path}/cgroup.subtree_control");
    match std::fs::write(&subtree_path, "+io") {
        Ok(_) => {
            info!("Enabled IO controller in {subtree_path}");
            true
        }
        Err(e) => {
            warn!("Failed to enable IO controller in {subtree_path}: {e}");
            false
        }
    }
}

/// Checks whether the IO controller is enabled in a cgroup's subtree_control.
pub fn io_controller_enabled(cgroup_path: &str) -> bool {
    let subtree_path = format!("{cgroup_path}/cgroup.subtree_control");
    std::fs::read_to_string(&subtree_path)
        .map(|s| s.split_whitespace().any(|c| c == "io"))
        .unwrap_or(false)
}

/// Formats an `io.max` line for cgroups v2.
///
/// Format: `"MAJ:MIN rbps=X wbps=X riops=Y wiops=Y"`
pub fn format_io_max(device: &str, limits: &CgroupLimits) -> String {
    format!(
        "{device} rbps={bps} wbps={bps} riops={iops} wiops={iops}",
        bps = limits.io_max_bps,
        iops = limits.io_max_iops
    )
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
        match discover_block_device(AGENT_STORAGE_PATH) {
            Some(device) => {
                let io_max = format_io_max(&device, limits);
                match std::fs::write(&io_max_path, &io_max) {
                    Ok(_) => {
                        info!("cgroup {name}: io.max = {io_max}");
                        true
                    }
                    Err(e) => {
                        warn!(
                            "cgroup {name}: io.max write failed (controller not delegated?): {e}"
                        );
                        false
                    }
                }
            }
            None => {
                warn!("cgroup {name}: block device discovery failed, skipping io.max");
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

    #[test]
    fn discover_device_from_mountinfo_finds_root() {
        let sample = "22 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw\n\
                       30 22 0:26 / /tmp rw,nosuid,nodev shared:14 - tmpfs tmpfs rw\n\
                       45 22 252:0 / /ram rw,nosuid,nodev shared:20 - tmpfs tmpfs rw,size=2097152k";
        assert_eq!(
            discover_device_from_mountinfo(sample, "/"),
            Some("8:2".to_string())
        );
    }

    #[test]
    fn discover_device_from_mountinfo_finds_ram() {
        let sample = "22 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw\n\
                       45 22 252:0 / /ram rw,nosuid,nodev shared:20 - tmpfs tmpfs rw,size=2097152k";
        assert_eq!(
            discover_device_from_mountinfo(sample, "/ram"),
            Some("252:0".to_string())
        );
    }

    #[test]
    fn discover_device_from_mountinfo_not_found() {
        let sample = "22 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw";
        assert_eq!(discover_device_from_mountinfo(sample, "/nonexistent"), None);
    }

    #[test]
    fn format_io_max_includes_device() {
        let limits = CgroupLimits::default();
        let result = format_io_max("8:0", &limits);
        assert!(
            result.starts_with("8:0 "),
            "io.max must start with device, got: {result}"
        );
        assert!(result.contains("rbps=10485760"), "must contain rbps");
        assert!(result.contains("riops=300"), "must contain riops");
        assert!(result.contains("wiops=300"), "must contain wiops");
        assert!(result.contains("wbps=10485760"), "must contain wbps");
    }

    #[test]
    fn format_io_max_custom_limits() {
        let limits = CgroupLimits {
            io_max_iops: 500,
            io_max_bps: 20 * 1024 * 1024,
            ..CgroupLimits::default()
        };
        let result = format_io_max("252:0", &limits);
        assert_eq!(
            result,
            "252:0 rbps=20971520 wbps=20971520 riops=500 wiops=500"
        );
    }

    #[test]
    fn io_controller_enabled_false_for_nonexistent() {
        assert!(!io_controller_enabled(
            "/sys/fs/cgroup/nonexistent-sentinel-test"
        ));
    }

    #[test]
    fn discover_block_device_reads_proc() {
        // /proc/self/mountinfo should always exist on Linux
        if std::path::Path::new("/proc/self/mountinfo").exists() {
            // Root filesystem should always have a device
            let device = discover_block_device("/");
            assert!(device.is_some(), "should discover device for /");
            let dev = device.unwrap();
            assert!(
                dev.contains(':'),
                "device should be in MAJ:MIN format, got: {dev}"
            );
        }
    }

    #[test]
    fn resolve_to_whole_disk_none_for_nonexistent() {
        // A device that doesn't exist should return None
        assert_eq!(resolve_to_whole_disk("999:999"), None);
    }

    #[test]
    fn resolve_to_whole_disk_on_real_system() {
        // On real systems, check if 8:1 resolves to 8:0 (sda1 -> sda)
        if std::path::Path::new("/sys/dev/block/8:1/partition").exists() {
            let parent = resolve_to_whole_disk("8:1");
            assert!(parent.is_some(), "8:1 should resolve to parent device");
            assert_eq!(
                parent.unwrap(),
                "8:0",
                "sda1 (8:1) should resolve to sda (8:0)"
            );
        }
    }
}
