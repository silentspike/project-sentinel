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
    assert_eq!(
        limits.io_max_iops, 300,
        "io_max_iops default should be 300"
    );
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
