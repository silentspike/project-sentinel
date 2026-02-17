//! Acceptance Tests fuer Issue #16: sentinel-sandbox
//!
//! Testet BwrapConfig (to_args, for_agent), CgroupLimits (default),
//! parse_psi und cgroup_path.

use sentinel_sandbox::{BwrapConfig, CgroupLimits};

// AC #16.02: to_args() enthaelt --ro-bind, --bind (writable), --tmpfs, --unshare-all
#[test]
fn ac_16_02_bwrap_args() {
    let config = BwrapConfig::for_agent("testuser");
    let args = config.to_args();

    assert!(
        args.contains(&"--ro-bind".to_string()),
        "to_args() must contain '--ro-bind', got: {:?}",
        args
    );
    // Writable binds use --bind (not --rw-bind)
    assert!(
        args.contains(&"--bind".to_string()),
        "to_args() must contain '--bind' for writable mounts, got: {:?}",
        args
    );
    assert!(
        args.contains(&"--tmpfs".to_string()),
        "to_args() must contain '--tmpfs', got: {:?}",
        args
    );
    assert!(
        args.contains(&"--unshare-all".to_string()),
        "to_args() must contain '--unshare-all', got: {:?}",
        args
    );
}

// AC #16.03: for_agent("thomas") hat ro-bind /work/company, rw-bind fuer thomas
#[test]
fn ac_16_03_for_agent_standard() {
    let config = BwrapConfig::for_agent("thomas");
    let args = config.to_args();

    // Readonly bind: /work/company -> /company
    assert!(
        args.contains(&"/work/company".to_string()),
        "for_agent should include /work/company as ro-bind source, got: {:?}",
        args
    );

    // Writable bind: /ram/agents/thomas -> /home/thomas
    assert!(
        args.contains(&"/ram/agents/thomas".to_string()),
        "for_agent('thomas') should include /ram/agents/thomas as writable bind, got: {:?}",
        args
    );
    assert!(
        args.contains(&"/home/thomas".to_string()),
        "for_agent('thomas') should include /home/thomas as writable bind dest, got: {:?}",
        args
    );

    // Hostname soll sentinel-thomas enthalten
    assert_eq!(
        config.hostname, "sentinel-thomas",
        "hostname should be 'sentinel-thomas'"
    );
}

// AC #16.04: CgroupLimits::default() hat die dokumentierten Werte
#[test]
fn ac_16_04_cgroup_limits() {
    let limits = CgroupLimits::default();

    assert_eq!(
        limits.cpu_quota_us, 100_000,
        "cpu_quota_us default should be 100000"
    );
    assert_eq!(
        limits.memory_bytes,
        256 * 1024 * 1024,
        "memory_bytes default should be 256MB"
    );
    assert_eq!(limits.io_max_iops, 300, "io_max_iops default should be 300");
    assert_eq!(
        limits.io_max_bps,
        10 * 1024 * 1024,
        "io_max_bps default should be 10MB/s"
    );
}

// AC #16.05: parse_psi() mit Sample-Input, verify avg10/avg60/avg300/total
#[test]
fn ac_16_05_psi_reader() {
    let sample_psi =
        "some avg10=1.50 avg60=2.30 avg300=0.10 total=12345\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0";

    let metrics = sentinel_sandbox::cgroups::parse_psi(sample_psi)
        .expect("parse_psi should succeed with valid input");

    assert!(
        (metrics.avg10 - 1.50).abs() < f64::EPSILON,
        "avg10 should be 1.50, got: {}",
        metrics.avg10
    );
    assert!(
        (metrics.avg60 - 2.30).abs() < f64::EPSILON,
        "avg60 should be 2.30, got: {}",
        metrics.avg60
    );
    assert!(
        (metrics.avg300 - 0.10).abs() < f64::EPSILON,
        "avg300 should be 0.10, got: {}",
        metrics.avg300
    );
    assert_eq!(metrics.total, 12345, "total should be 12345");
}

// AC #16.06: cgroup_path("thomas") enthaelt "sentinel/thomas"
#[test]
fn ac_16_06_cgroup_path() {
    let path = sentinel_sandbox::cgroups::cgroup_path("thomas");

    assert!(
        path.contains("sentinel/thomas"),
        "cgroup_path('thomas') should contain 'sentinel/thomas', got: '{}'",
        path
    );
}

// AC #73.01: discover_block_device findet whole-disk Device fuer Root-Filesystem
#[test]
fn ac_73_01_discover_block_device() {
    // /proc/self/mountinfo exists on all Linux systems
    if std::path::Path::new("/proc/self/mountinfo").exists() {
        let device = sentinel_sandbox::cgroups::discover_block_device("/");
        assert!(
            device.is_some(),
            "discover_block_device('/') should find a device"
        );
        let dev = device.unwrap();
        assert!(
            dev.contains(':'),
            "device should be in MAJ:MIN format, got: '{dev}'"
        );
        // Must be a whole-disk device (not a partition) for io.max compatibility
        let sysfs_partition = format!("/sys/dev/block/{dev}/partition");
        assert!(
            !std::path::Path::new(&sysfs_partition).exists(),
            "device {dev} must be a whole-disk device (not a partition) for io.max"
        );
    }
}

// AC #73.02: format_io_max erzeugt korrektes cgroup v2 io.max Format mit Device-Prefix
#[test]
fn ac_73_02_io_max_format() {
    let limits = CgroupLimits::default();
    let io_max = sentinel_sandbox::cgroups::format_io_max("8:0", &limits);

    // Must start with device
    assert!(
        io_max.starts_with("8:0 "),
        "io.max must start with device prefix, got: '{io_max}'"
    );
    // Must contain all four limit fields
    assert!(io_max.contains("rbps="), "missing rbps in: {io_max}");
    assert!(io_max.contains("wbps="), "missing wbps in: {io_max}");
    assert!(io_max.contains("riops="), "missing riops in: {io_max}");
    assert!(io_max.contains("wiops="), "missing wiops in: {io_max}");
    // Verify actual values match defaults
    assert!(
        io_max.contains("riops=300"),
        "riops should be 300, got: {io_max}"
    );
    assert!(
        io_max.contains("rbps=10485760"),
        "rbps should be 10MB/s, got: {io_max}"
    );
}

// AC #73.N1: Bestehende CPU/Memory Limits in CgroupLimits::default() unveraendert
#[test]
fn ac_73_n1_existing_limits_unchanged() {
    let limits = CgroupLimits::default();
    // These must match Issue #16 AC values exactly
    assert_eq!(limits.cpu_quota_us, 100_000, "CPU quota must stay 100000");
    assert_eq!(limits.cpu_period_us, 100_000, "CPU period must stay 100000");
    assert_eq!(
        limits.memory_bytes,
        256 * 1024 * 1024,
        "Memory must stay 256MB"
    );
    assert_eq!(limits.io_max_iops, 300, "IO IOPS must stay 300");
    assert_eq!(
        limits.io_max_bps,
        10 * 1024 * 1024,
        "IO BPS must stay 10MB/s"
    );
}
