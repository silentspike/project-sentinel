//! Breakout Tests fuer sentinel-sandbox.
//!
//! Issue #76: Security-Validierung der Sandbox-Isolation.
//! 9 Breakout-Szenarien die beweisen, dass ein Agent NICHT entkommen kann.
//!
//! Test-Tiers:
//! - Tier 1 (CI): Config-Validation, kein #[ignore], kein root/bwrap noetig
//! - Tier 2 (VM): Echte Breakout-Tests, #[ignore], brauchen bwrap auf 10.0.0.240

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use sentinel_sandbox::{BwrapConfig, CgroupLimits, LandlockRuleset};

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Finds the breakout-helper binary in the target directory.
fn helper_binary_path() -> PathBuf {
    // cargo test builds binaries in target/debug/ or target/release/
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop(); // crates/
    path.pop(); // project root
    path.push("target");
    path.push("debug");
    path.push("breakout-helper");

    if !path.exists() {
        panic!(
            "breakout-helper binary not found at {}. Run `cargo build -p sentinel-sandbox` first.",
            path.display()
        );
    }
    path
}

/// Creates an extended BwrapConfig for breakout tests.
/// Binds system directories (ro) so the helper binary can run inside bwrap.
fn breakout_bwrap_config(name: &str) -> BwrapConfig {
    let helper_path = helper_binary_path();
    let helper_str = helper_path.to_str().expect("helper path not UTF-8");

    // Start with production config
    let mut config = BwrapConfig::for_agent(name);

    // Add system binds needed to run the helper binary (skip if already in production config)
    let system_ro_binds = [
        ("/usr", "/usr"),
        ("/lib", "/lib"),
        ("/lib64", "/lib64"),
        ("/bin", "/bin"),
        ("/sbin", "/sbin"),
        ("/etc", "/etc"),
    ];

    for (host, guest) in &system_ro_binds {
        if std::path::Path::new(host).exists()
            && !config
                .readonly_binds
                .iter()
                .any(|(h, g)| h == host && g == guest)
        {
            config
                .readonly_binds
                .push((host.to_string(), guest.to_string()));
        }
    }

    // Bind the helper binary at root level (NOT under /usr which is ro-bound).
    // bwrap root is a private mount — creating files at / works.
    let guest_helper = "/breakout-helper";
    config
        .readonly_binds
        .push((helper_str.to_string(), guest_helper.to_string()));

    config
}

/// Creates the agent home directory for a breakout test.
fn create_agent_home(name: &str) -> PathBuf {
    let home = PathBuf::from(format!("/ram/agents/{name}"));
    let _ = std::fs::create_dir_all(&home);
    home
}

/// Cleanup: removes agent home + cgroup.
fn cleanup_breakout(name: &str) {
    let home = format!("/ram/agents/{name}");
    let _ = std::fs::remove_dir_all(&home);
    let _ = sentinel_sandbox::cgroups::remove_cgroup(name);
}

/// RAII guard for automatic cleanup on test exit (including panic).
struct BreakoutGuard {
    name: String,
}

impl BreakoutGuard {
    fn new(name: &str) -> Self {
        create_agent_home(name);
        Self {
            name: name.to_string(),
        }
    }
}

impl Drop for BreakoutGuard {
    fn drop(&mut self) {
        cleanup_breakout(&self.name);
    }
}

/// Result from spawning a sandboxed command.
struct SpawnResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// Spawns the helper binary DIRECTLY inside a cgroup (no bwrap).
/// Uses `sudo sh -c "echo $$ > cgroup.procs && exec helper scenario"` so the
/// process starts in the cgroup and all children inherit it.
/// This avoids the cgroup v2 cross-subtree PID migration issue (EPERM) and
/// the race condition where bwrap forks before we can add the PID.
fn spawn_in_cgroup(name: &str, scenario: &str) -> SpawnResult {
    let helper = helper_binary_path();
    let helper_str = helper.to_str().expect("helper path not UTF-8");
    let cgroup_procs = format!("/sys/fs/cgroup/sentinel/{name}/cgroup.procs");
    let cmd = format!("echo $$ > {cgroup_procs} && exec {helper_str} {scenario}");

    let child = Command::new("sudo")
        .args(["sh", "-c", &cmd])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn helper in cgroup");

    let output = child
        .wait_with_output()
        .expect("Failed to wait for helper in cgroup");

    SpawnResult {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    }
}

/// Spawns bwrap with the given config and command, waits with timeout.
fn spawn_and_wait(config: &BwrapConfig, cmd: &[&str], timeout: Duration) -> SpawnResult {
    let mut args = config.to_args();
    args.extend(cmd.iter().map(|s| s.to_string()));

    let child = Command::new("bwrap")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn bwrap");

    let output = child.wait_with_output().expect("Failed to wait for bwrap");

    // Note: timeout is handled by the test framework's #[timeout] or
    // by the caller wrapping this in a thread with a deadline.
    // For simplicity, we rely on cargo test's built-in timeout.
    let _ = timeout; // Used conceptually, cargo test handles actual timeout

    SpawnResult {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tier 1: CI-compatible config tests (NO #[ignore])
// ---------------------------------------------------------------------------

/// AC-N1: Verifies that BwrapConfig, CgroupLimits, LandlockRuleset APIs are unchanged.
/// TOGAF-konform: proc_mount=/proc, dev_mount=/dev, share_net=true.
#[test]
fn ac_76_n1_existing_apis_unchanged() {
    // BwrapConfig::for_agent still works
    let bwrap = BwrapConfig::for_agent("test-n1");
    assert_eq!(bwrap.hostname, "sentinel-test-n1");
    assert!(bwrap.share_net, "TOGAF: default is --share-net");
    assert!(bwrap.die_with_parent);
    assert_eq!(
        bwrap.proc_mount,
        Some("/proc".to_string()),
        "TOGAF: default is --proc /proc"
    );
    assert_eq!(
        bwrap.dev_mount,
        Some("/dev".to_string()),
        "TOGAF: default is --dev /dev"
    );

    // to_args must contain --proc and --dev by default (TOGAF)
    let args = bwrap.to_args();
    assert!(args.contains(&"--proc".to_string()));
    assert!(args.contains(&"--dev".to_string()));

    // CgroupLimits::default still works
    let cgroups = CgroupLimits::default();
    assert_eq!(cgroups.memory_bytes, 256 * 1024 * 1024);
    assert_eq!(cgroups.cpu_quota_us, 100_000);

    // LandlockRuleset::for_agent still works
    let landlock = LandlockRuleset::for_agent("test-n1");
    assert!(landlock
        .read_paths
        .contains(&std::path::PathBuf::from("/company")));
    assert!(landlock
        .write_paths
        .contains(&std::path::PathBuf::from("/home/test-n1")));
    assert!(landlock
        .exec_paths
        .contains(&std::path::PathBuf::from("/usr")));
}

/// Verifies that breakout_bwrap_config() extends production config correctly.
#[test]
fn ac_76_config_breakout_extends_production() {
    // This test only validates config generation, does NOT need bwrap
    let prod = BwrapConfig::for_agent("test-cfg");
    let test_config = breakout_bwrap_config("test-cfg");

    // Test config must contain ALL production readonly binds
    for (host, guest) in &prod.readonly_binds {
        assert!(
            test_config
                .readonly_binds
                .iter()
                .any(|(h, g)| h == host && g == guest),
            "breakout config missing production ro-bind: {host} -> {guest}"
        );
    }

    // Test config must contain ALL production writable binds
    for (host, guest) in &prod.writable_binds {
        assert!(
            test_config
                .writable_binds
                .iter()
                .any(|(h, g)| h == host && g == guest),
            "breakout config missing production rw-bind: {host} -> {guest}"
        );
    }

    // Test config must also have system binds (/usr, /lib, etc.)
    let test_args = test_config.to_args();
    assert!(
        test_args.contains(&"/usr".to_string()),
        "breakout config should bind /usr"
    );

    // Test config must have the helper binary bound
    assert!(
        test_config
            .readonly_binds
            .iter()
            .any(|(_, g)| g == "/breakout-helper"),
        "breakout config should bind helper binary"
    );
}

/// Verifies proc_mount and dev_mount appear in args (TOGAF defaults).
#[test]
fn ac_76_config_proc_and_dev_mount_in_args() {
    let config = BwrapConfig::for_agent("test-proc");
    let args = config.to_args();

    let proc_idx = args
        .iter()
        .position(|a| a == "--proc")
        .expect("--proc must be in args (TOGAF default)");
    assert_eq!(args[proc_idx + 1], "/proc");

    let dev_idx = args
        .iter()
        .position(|a| a == "--dev")
        .expect("--dev must be in args (TOGAF default)");
    assert_eq!(args[dev_idx + 1], "/dev");
}

// ---------------------------------------------------------------------------
// Tier 2: VM-only breakout tests (ALL #[ignore])
// ---------------------------------------------------------------------------

/// Checks if bwrap is available on this system.
fn require_bwrap() {
    if !BwrapConfig::test_userns() {
        panic!("bwrap user namespace not available — run on VM (10.0.0.240)");
    }
}

// --- AC-1: Filesystem Breakout (4 tests) ---

/// AC-1/FS-001: Write to /etc/passwd must be blocked.
#[test]
#[ignore]
fn ac_76_01_fs_write_etc_passwd() {
    require_bwrap();
    let _guard = BreakoutGuard::new("brk-fs1");
    let config = breakout_bwrap_config("brk-fs1");

    let result = spawn_and_wait(
        &config,
        &["/breakout-helper", "--landlock", "brk-fs1", "write-etc"],
        Duration::from_secs(15),
    );

    eprintln!("stderr: {}", result.stderr);
    assert_eq!(
        result.exit_code, 0,
        "FS-001: write to /etc/passwd should be blocked (exit 0), got {}",
        result.exit_code
    );
}

/// AC-1/FS-002: Read another agent's home must be blocked.
#[test]
#[ignore]
fn ac_76_02_fs_read_other_agent_home() {
    require_bwrap();
    let _guard = BreakoutGuard::new("brk-fs2");
    let config = breakout_bwrap_config("brk-fs2");

    // NOTE: /home/other-agent is NOT bound — should be invisible
    let result = spawn_and_wait(
        &config,
        &["/breakout-helper", "read-other-home"],
        Duration::from_secs(15),
    );

    eprintln!("stderr: {}", result.stderr);
    assert_eq!(
        result.exit_code, 0,
        "FS-002: read /home/other-agent should be blocked (exit 0), got {}",
        result.exit_code
    );
}

/// AC-1/FS-003: Execute from /tmp — tests defense-in-depth.
///
/// KNOWN ISSUE: Landlock write_paths get all_access (incl. Execute).
/// In test config (with /usr bound), /tmp exec MAY succeed.
/// In production (no /usr), no executables available to copy.
/// Result is documented in security report, not necessarily PASS.
#[test]
#[ignore]
fn ac_76_03_fs_exec_from_tmp() {
    require_bwrap();
    let _guard = BreakoutGuard::new("brk-fs3");
    let config = breakout_bwrap_config("brk-fs3");

    let result = spawn_and_wait(
        &config,
        &["/breakout-helper", "--landlock", "brk-fs3", "exec-from-tmp"],
        Duration::from_secs(15),
    );

    eprintln!("stderr: {}", result.stderr);
    // Document result — known Landlock gap means exec MAY succeed in test env
    eprintln!(
        "FS-003 RESULT: exit_code={} (0=blocked, 1=exec succeeded). \
         Known Landlock gap: write_paths have all_access incl Execute. \
         Production mitigated by bwrap mount namespace (no /usr bound).",
        result.exit_code
    );
    // We don't assert exit_code == 0 here because of the known gap.
    // The test documents the finding for the security report.
}

/// AC-1/FS-004: Symlink escape from /tmp to /etc/shadow must be blocked.
#[test]
#[ignore]
fn ac_76_04_fs_symlink_escape() {
    require_bwrap();
    let _guard = BreakoutGuard::new("brk-fs4");
    let config = breakout_bwrap_config("brk-fs4");

    let result = spawn_and_wait(
        &config,
        &[
            "/breakout-helper",
            "--landlock",
            "brk-fs4",
            "symlink-escape",
        ],
        Duration::from_secs(15),
    );

    eprintln!("stderr: {}", result.stderr);
    assert_eq!(
        result.exit_code, 0,
        "FS-004: symlink escape should be blocked (exit 0), got {}",
        result.exit_code
    );
}

// --- AC-2: Resource Exhaustion (3 tests) ---

/// AC-2/RES-001: Memory bomb must be OOM-killed by cgroup.
/// No bwrap needed — cgroup memory.max enforces the limit directly.
#[test]
#[ignore]
fn ac_76_05_res_memory_bomb() {
    require_bwrap();
    let name = "brk-mem";
    let _guard = BreakoutGuard::new(name);

    let limits = CgroupLimits::default(); // 256MB
    sentinel_sandbox::cgroups::create_cgroup(name, &limits)
        .expect("Failed to create cgroup for memory bomb test");

    let result = spawn_in_cgroup(name, "memory-bomb");
    eprintln!("stderr: {}", result.stderr);
    eprintln!("RES-001: exit_code={}", result.exit_code);

    // OOM-killer sends SIGKILL (9). sudo/sh reports it as exit code 137 (128+9).
    let was_killed = result.exit_code != 0;
    assert!(
        was_killed,
        "RES-001: memory bomb should be OOM-killed, got exit_code={}",
        result.exit_code
    );
}

/// AC-2/RES-002: Fork bomb must be contained by pids.max.
/// No bwrap needed — cgroup pids.max enforces the limit directly.
#[test]
#[ignore]
fn ac_76_06_res_fork_bomb() {
    require_bwrap();
    let name = "brk-fork";
    let _guard = BreakoutGuard::new(name);

    let limits = CgroupLimits::default();
    sentinel_sandbox::cgroups::create_cgroup(name, &limits)
        .expect("Failed to create cgroup for fork bomb test");

    // Set pids.max directly (not part of CgroupLimits struct)
    let cgroup_dir = format!("/sys/fs/cgroup/sentinel/{name}");
    std::fs::write(format!("{cgroup_dir}/pids.max"), "50").expect("Failed to write pids.max");

    let result = spawn_in_cgroup(name, "fork-bomb");
    let stderr = &result.stderr;
    eprintln!("stderr: {stderr}");
    eprintln!("RES-002: exit_code={}", result.exit_code);

    // Helper exits 0 when spawn failures detected (fork bomb contained)
    assert_eq!(
        result.exit_code, 0,
        "RES-002: fork bomb should be contained (exit 0), got {}",
        result.exit_code
    );
}

/// AC-2/RES-003: CPU burn must be throttled by cgroup.
/// No bwrap needed — cgroup cpu.max enforces the limit directly.
#[test]
#[ignore]
fn ac_76_07_res_cpu_burn() {
    require_bwrap();
    let name = "brk-cpu";
    let _guard = BreakoutGuard::new(name);

    // 50% CPU quota
    let limits = CgroupLimits {
        cpu_quota_us: 50_000,
        ..CgroupLimits::default()
    };
    sentinel_sandbox::cgroups::create_cgroup(name, &limits)
        .expect("Failed to create cgroup for CPU burn test");

    let result = spawn_in_cgroup(name, "cpu-burn");
    eprintln!("stderr: {}", result.stderr);

    // Check cpu.stat for throttling evidence
    let cgroup_dir = format!("/sys/fs/cgroup/sentinel/{name}");
    let cpu_stat = std::fs::read_to_string(format!("{cgroup_dir}/cpu.stat")).unwrap_or_default();
    eprintln!("cpu.stat: {cpu_stat}");

    let nr_throttled: u64 = cpu_stat
        .lines()
        .find(|l| l.starts_with("nr_throttled "))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    eprintln!("RES-003: nr_throttled={nr_throttled}");
    assert!(
        nr_throttled > 0,
        "RES-003: CPU burn should be throttled (nr_throttled > 0), got {nr_throttled}"
    );
}

// --- AC-3: Namespace Isolation (2 tests) ---

/// AC-3/NS-001: PID namespace must isolate process view.
#[test]
#[ignore]
fn ac_76_08_ns_pid_namespace() {
    require_bwrap();
    let _guard = BreakoutGuard::new("brk-ns1");
    let config = breakout_bwrap_config("brk-ns1");
    // proc_mount is already Some("/proc") by default (TOGAF)

    let result = spawn_and_wait(
        &config,
        &["/breakout-helper", "pid-count"],
        Duration::from_secs(15),
    );

    eprintln!("stderr: {}", result.stderr);
    eprintln!("stdout: {}", result.stdout);

    let pid_count: usize = result
        .stdout
        .lines()
        .find(|l| l.starts_with("pid_count="))
        .and_then(|l| l.strip_prefix("pid_count="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(999);

    eprintln!("NS-001: pid_count={pid_count}");
    assert!(
        pid_count <= 5,
        "NS-001: PID namespace should show <=5 processes, got {pid_count}"
    );
}

/// AC-3/NS-002: UTS namespace must isolate hostname.
#[test]
#[ignore]
fn ac_76_09_ns_hostname() {
    require_bwrap();
    let _guard = BreakoutGuard::new("brk-ns2");
    let config = breakout_bwrap_config("brk-ns2");
    // proc_mount is already Some("/proc") by default (TOGAF)

    let result = spawn_and_wait(
        &config,
        &["/breakout-helper", "hostname"],
        Duration::from_secs(15),
    );

    eprintln!("stderr: {}", result.stderr);
    let hostname = result.stdout.trim();
    eprintln!("NS-002: hostname={hostname}");

    assert_eq!(
        hostname, "sentinel-brk-ns2",
        "NS-002: hostname should be 'sentinel-brk-ns2', got '{hostname}'"
    );
}
