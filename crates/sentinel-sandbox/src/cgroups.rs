//! cgroups v2 resource limits and PSI monitoring.

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

/// Erzeugt den cgroup v2 Pfad fuer einen Agenten.
pub fn cgroup_path(name: &str) -> String {
    format!("/sys/fs/cgroup/sentinel/{name}")
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
    #[ignore]
    fn cgroup_create_remove() {
        // Echte cgroup erstellen/loeschen (braucht root)
    }

    #[test]
    #[ignore]
    fn read_psi_real() {
        // Echte PSI-Datei lesen (/proc/pressure/cpu)
    }

    #[test]
    #[ignore]
    fn cgroup_remove_nonexistent_ok() {
        // Nicht-existierende cgroup entfernen soll nicht paniken
    }
}
