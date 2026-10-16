//! Acceptance Tests fuer sentinel-sandbox.
//!
//! Issue #16: BwrapConfig (to_args, for_agent), CgroupLimits, parse_psi, cgroup_path.
//! Issue #73: IO Delegation (discover_block_device, format_io_max).
//! Issue #75: Full network cage (no --share-net) + post-spawn isolation verifier.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sentinel_sandbox::{
    BwrapConfig, CgroupLimits, IsolationStatus, SandboxEnforcer, SpawnedSandbox,
};

// Match the spawn-info budget, but inspect state on every poll: a zombie still
// fails until its host-side parent actually reaps this exact proc identity.
const REAP_DEADLINE: Duration = Duration::from_secs(5);
const REAP_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, PartialEq)]
struct ProcState {
    pid: u32,
    parent: u32,
    state: char,
    start_ticks: u64,
}

fn parse_proc_state(stat: &str) -> io::Result<ProcState> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid proc stat identity");
    let (pid, rest) = stat.split_once(" (").ok_or_else(invalid)?;
    // comm may itself contain spaces and closing parentheses.
    let (_, fields) = rest.rsplit_once(") ").ok_or_else(invalid)?;
    let fields = fields.split_whitespace().collect::<Vec<_>>();
    Ok(ProcState {
        pid: pid.parse().map_err(|_| invalid())?,
        parent: fields
            .get(1)
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?,
        state: match *fields.first().ok_or_else(invalid)? {
            "R" => 'R',
            "S" => 'S',
            "D" => 'D',
            "Z" => 'Z',
            "T" => 'T',
            "t" => 't',
            "X" => 'X',
            "x" => 'x',
            "I" => 'I',
            "P" => 'P',
            _ => return Err(invalid()),
        },
        start_ticks: fields
            .get(19)
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?,
    })
}

#[derive(Debug)]
struct InitIdentity {
    pid: u32,
    start_ticks: u64,
    proc_dir: File,
}

impl InitIdentity {
    fn capture(pid: u32) -> io::Result<Self> {
        let mut identity = Self {
            pid,
            start_ticks: 0,
            proc_dir: File::open(format!("/proc/{pid}"))?,
        };
        let state = identity.read_state()?;
        if state.pid != pid {
            return Err(io::Error::other("sandbox init identity changed"));
        }
        identity.start_ticks = state.start_ticks;
        Ok(identity)
    }

    fn path(&self, name: &str) -> PathBuf {
        // Pin the original proc directory: never follow a recycled numeric PID.
        PathBuf::from(format!(
            "/proc/self/fd/{}/{name}",
            self.proc_dir.as_raw_fd()
        ))
    }

    fn read_state(&self) -> io::Result<ProcState> {
        parse_proc_state(&std::fs::read_to_string(self.path("stat"))?)
    }

    fn duplicate(&self) -> io::Result<Self> {
        Ok(Self {
            pid: self.pid,
            start_ticks: self.start_ticks,
            proc_dir: self.proc_dir.try_clone()?,
        })
    }

    fn live_netns(&self) -> io::Result<PathBuf> {
        let state = self.read_state()?;
        if state.pid != self.pid
            || state.start_ticks != self.start_ticks
            || matches!(state.state, 'Z' | 'X' | 'x')
        {
            return Err(io::Error::other(format!(
                "sandbox init is not live: {state:?}"
            )));
        }
        std::fs::read_link(self.path("ns/net"))
    }

    fn wait_reaped(&self) -> io::Result<()> {
        let deadline = Instant::now() + REAP_DEADLINE;
        loop {
            match self.read_state() {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
                Ok(state) => {
                    if state.pid != self.pid || state.start_ticks != self.start_ticks {
                        return Err(io::Error::other(format!(
                            "pinned init identity changed: {state:?}"
                        )));
                    }
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(io::ErrorKind::TimedOut,
                            format!("sandbox init not reaped after {REAP_DEADLINE:?}: {state:?}, host parent={}", state.parent)));
                    }
                }
            }
            std::thread::sleep(REAP_POLL_INTERVAL);
        }
    }
}

struct OwnedCage {
    process: SpawnedSandbox,
    init: Option<InitIdentity>,
    cleanup_attempted: bool,
}

impl OwnedCage {
    fn new(process: SpawnedSandbox) -> anyhow::Result<Self> {
        let mut cage = Self {
            process,
            init: None,
            cleanup_attempted: false,
        };
        let pid = cage.process.child_pid.ok_or_else(|| {
            anyhow::anyhow!("bwrap must report the sandboxed child PID via --info-fd")
        })?;
        cage.init = Some(InitIdentity::capture(pid)?);
        Ok(cage)
    }

    fn init(&self) -> &InitIdentity {
        self.init.as_ref().expect("captured sandbox init")
    }

    fn cleanup(&mut self) -> anyhow::Result<()> {
        self.cleanup_attempted = true;
        self.process.terminate();
        anyhow::ensure!(
            self.process.child.try_wait()?.is_some(),
            "owned supervisor must be reaped"
        );
        if let Some(init) = &self.init {
            init.wait_reaped()?;
        }
        Ok(())
    }
}

impl Drop for OwnedCage {
    fn drop(&mut self) {
        if !self.cleanup_attempted {
            if let Err(error) = self.cleanup() {
                eprintln!("owned cage cleanup failed during unwinding: {error:#}");
            }
        }
    }
}

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

// ================================================================
// Issue #75: Full network cage (no --share-net) + isolation verifier
// ================================================================

// AC #75.01: Agents are full-caged by default — BwrapConfig::for_agent() sets
// share_net=false and to_args() emits --unshare-all WITHOUT --share-net.
// Agents make no network calls; the daemon proxies LLM traffic to the gateway.
#[test]
fn ac_75_01_agent_default_full_cage() {
    let config = BwrapConfig::for_agent("thomas");
    assert!(
        !config.share_net,
        "BwrapConfig::for_agent() default must be share_net=false (#75 full cage)"
    );
    let args = config.to_args();
    assert!(
        args.contains(&"--unshare-all".to_string()),
        "full cage must keep --unshare-all"
    );
    assert!(
        !args.contains(&"--share-net".to_string()),
        "agents must NOT get --share-net (#75), args:\n{args:?}"
    );
}

// AC #75.N1: Existing bwrap/cgroup invariants remain unchanged.
#[test]
fn ac_75_n1_existing_invariants_unchanged() {
    let config = BwrapConfig::for_agent("test");
    let args = config.to_args();
    assert!(args.contains(&"--unshare-all".to_string()));
    assert!(args.contains(&"--die-with-parent".to_string()));
    assert!(args.contains(&"--ro-bind".to_string()));
    assert!(args.contains(&"--bind".to_string()));
    assert!(args.contains(&"--tmpfs".to_string()));

    let limits = CgroupLimits::default();
    assert_eq!(limits.cpu_quota_us, 100_000);
    assert_eq!(limits.memory_bytes, 256 * 1024 * 1024);
}

// VM-only (AC-1): a spawned agent lands in its OWN network namespace, distinct
// from this process. Reads the sandboxed child PID from bwrap --info-fd
// (SpawnedSandbox.child_pid — NOT the supervisor PID) and compares ns inodes.
// Requires bwrap + unprivileged user namespaces (deploy VM).
#[test]
#[ignore]
fn ac_75_vm_full_cage_distinct_netns() {
    let config = BwrapConfig::for_agent("vm-cage-test");
    let mut spawned = OwnedCage::new(
        config
            .spawn(&["/usr/bin/sleep".to_string(), "2".to_string()])
            .expect("bwrap spawn must succeed on the VM"),
    )
    .expect("capture full-cage init identity");

    let own = std::fs::read_link("/proc/self/ns/net").expect("read own netns");
    let agent = spawned.init().live_netns().expect("read live agent netns");
    assert_ne!(
        own, agent,
        "agent must run in a distinct net namespace (full cage), own={own:?} agent={agent:?}"
    );

    spawned
        .cleanup()
        .expect("full-cage test process must be reaped");
}

// VM-only (AC-2): inside a full-caged agent, only loopback exists — no external
// reachability. Runs `ip -o link` inside the sandbox and asserts only `lo`.
#[test]
#[ignore]
fn ac_75_vm_only_loopback_inside_cage() {
    use std::io::Read;
    let config = BwrapConfig::for_agent("vm-cage-lo");
    // Use the production bind filtering and owned process-group cleanup. The
    // short-lived ip init may already be reaped before it can be proc-pinned.
    let mut cage = OwnedCage {
        process: config
            .spawn(&[
                "/usr/sbin/ip".to_string(),
                "-o".to_string(),
                "link".to_string(),
            ])
            .expect("bwrap spawn must succeed on the VM"),
        init: None,
        cleanup_attempted: false,
    };
    let mut output = Vec::new();
    cage.process
        .child
        .stdout
        .take()
        .expect("sandbox stdout must be piped")
        .take(65537)
        .read_to_end(&mut output)
        .expect("read sandbox link output");
    let status = cage.process.child.wait().expect("wait for sandbox ip");
    cage.cleanup().expect("loopback test process must be reaped");
    assert!(
        status.success(),
        "sandbox ip failed: {status}, stdout={output:?}"
    );
    assert!(output.len() <= 65536, "unexpectedly large link output");
    let links = std::str::from_utf8(&output).expect("ip link output must be UTF-8");
    let lines = links
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "expected exactly one loopback link: {links}");
    let fields = lines[0].split_whitespace().collect::<Vec<_>>();
    assert_eq!(fields.first(), Some(&"1:"), "unexpected link index: {links}");
    assert_eq!(fields.get(1), Some(&"lo:"), "unexpected interface: {links}");
    let flags = fields.get(2).expect("loopback link flags");
    assert!(
        flags.starts_with('<')
            && flags.ends_with('>')
            && flags[1..flags.len() - 1]
                .split(',')
                .any(|flag| flag == "LOOPBACK"),
        "missing loopback flag: {links}"
    );
    assert!(
        fields.contains(&"link/loopback"),
        "unexpected link kind: {links}"
    );
}

// VM-only (AC-4): deliberately sharing the host network must be detected as a
// cage breach. `with_shared_net` exists for non-agent diagnostics; production
// agent configs never call it.
#[test]
#[ignore = "requires deploy-VM bwrap/userns support"]
fn ac_75_vm_shared_net_is_detected_as_not_isolated() {
    let config = BwrapConfig::for_agent("vm-cage-negative").with_shared_net();
    let mut spawned = OwnedCage::new(
        config
            .spawn(&["/usr/bin/sleep".to_string(), "5".to_string()])
            .expect("shared-net fault-injection sandbox must start on the VM"),
    )
    .expect("capture fault-injection init identity");
    spawned
        .init()
        .live_netns()
        .expect("fault-injection init must be live");

    let (enforcer, _) = SandboxEnforcer::detect();
    assert_eq!(
        enforcer.verify_agent_netns_isolation(spawned.init().pid),
        IsolationStatus::NotIsolated,
        "a deliberately shared host namespace must be classified as NotIsolated"
    );

    spawned
        .cleanup()
        .expect("fault-injection process must be reaped");
}

// VM-only (AC-3/AC-6): a 60-agent burst must still produce one isolated
// network namespace per sandbox. This is a correctness stress test; timing is
// measured separately by the deploy-VM benchmark.
#[test]
#[ignore = "requires deploy-VM bwrap/userns support"]
fn ac_75_vm_60_concurrent_full_cages_are_distinct() {
    std::thread::scope(|scope| {
        let workers = (0..60)
            .map(|index| {
                let (release, released) = std::sync::mpsc::channel::<()>();
                let (ready, identity) = std::sync::mpsc::channel();
                let worker = scope.spawn(move || {
                    let process = BwrapConfig::for_agent(&format!("vm-cage-burst-{index}"))
                        .spawn(&["/usr/bin/sleep".to_string(), "30".to_string()])
                        .expect("full-cage burst process must start");
                    let mut cage = OwnedCage::new(process).expect("capture burst init identity");
                    // PDEATHSIG follows this creator thread. Keep it alive until
                    // inspection finishes; sender loss also releases it on panic.
                    if ready
                        .send(cage.init().duplicate().expect("pin burst init"))
                        .is_ok()
                    {
                        let _ = released.recv();
                    }
                    cage.cleanup()
                });
                (release, identity, worker)
            })
            .collect::<Vec<_>>();

        let daemon_netns = std::fs::read_link("/proc/self/ns/net").expect("read daemon netns");
        let identities = workers
            .iter()
            .map(|(_, identity, _)| {
                identity
                    .recv()
                    .expect("every burst worker must report its pinned init")
            })
            .collect::<Vec<InitIdentity>>();
        let netns = identities
            .iter()
            .map(|identity| identity.live_netns().expect("read live burst child netns"))
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(netns.len(), 60, "every burst sandbox needs a unique netns");
        assert!(
            !netns.contains(&daemon_netns),
            "no burst sandbox may share the daemon netns"
        );

        // Release every creator before joining any, including during unwinding
        // when dropping the senders disconnects every release receiver.
        for (release, _, _) in &workers {
            let _ = release.send(());
        }
        for (_, _, worker) in workers {
            worker
                .join()
                .expect("sandbox cleanup must not panic")
                .expect("the burst must reap its supervisor and original init");
        }
        assert!(identities.iter().all(|identity| {
            matches!(identity.read_state(), Err(error) if error.kind() == io::ErrorKind::NotFound)
        }), "the 60-sandbox burst must leave no sandboxed child behind");
    });
}

#[test]
fn proc_identity_parser_handles_parentheses_and_rejects_truncation() {
    let stat = "42 (sleep (test) name) S 7 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1234";
    assert_eq!(
        parse_proc_state(stat).unwrap(),
        ProcState {
            pid: 42,
            parent: 7,
            state: 'S',
            start_ticks: 1234,
        }
    );
    for invalid in ["", "42 (sleep) S 7", "42 (sleep) ? 7", "x (sleep) S 7"] {
        assert!(
            parse_proc_state(invalid).is_err(),
            "must reject {invalid:?}"
        );
    }
}

fn owned_sleep_fixture() -> SpawnedSandbox {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new("/usr/bin/sleep");
    command.arg("30").process_group(0);
    // SAFETY: only an async-signal-safe prctl is called before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().expect("spawn owned lifecycle fixture");
    SpawnedSandbox {
        child_pid: Some(child.id()),
        child,
    }
}

#[test]
fn owned_cage_cleanup_reaps_on_assertion_unwind_without_touching_foreign_process() {
    let mut foreign = OwnedCage::new(owned_sleep_fixture()).unwrap();
    let cage = OwnedCage::new(owned_sleep_fixture()).unwrap();
    let identity = cage.init().duplicate().unwrap();
    let result = std::panic::catch_unwind(move || {
        let _cage = cage;
        panic!("injected inspection assertion failure");
    });
    assert!(result.is_err());
    identity
        .wait_reaped()
        .expect("panic must reap the owned process");
    foreign
        .init()
        .live_netns()
        .expect("foreign process must remain live");
    foreign.cleanup().unwrap();
}

#[test]
fn creator_thread_stays_alive_until_inspection_and_reaping_finish() {
    std::thread::scope(|scope| {
        let (release, released) = std::sync::mpsc::channel::<()>();
        let (ready, identity) = std::sync::mpsc::channel();
        let worker = scope.spawn(move || {
            let mut cage = OwnedCage::new(owned_sleep_fixture()).unwrap();
            if ready.send(cage.init().duplicate().unwrap()).is_ok() {
                let _ = released.recv();
            }
            cage.cleanup()
        });
        let identity = identity.recv().unwrap();
        identity
            .live_netns()
            .expect("creator must remain alive while inspecting its child");
        assert!(
            !worker.is_finished(),
            "creator must await release, not return the handle"
        );
        drop(release);
        worker.join().unwrap().unwrap();
        identity.wait_reaped().unwrap();
    });
}

#[test]
fn pinned_proc_identity_does_not_follow_a_different_process() {
    let mut first = OwnedCage::new(owned_sleep_fixture()).unwrap();
    let retained = first.init().duplicate().unwrap();
    first.cleanup().unwrap();
    let mut second = OwnedCage::new(owned_sleep_fixture()).unwrap();
    retained
        .wait_reaped()
        .expect("retained proc directory must stay gone");
    second
        .init()
        .live_netns()
        .expect("new process must remain live");
    second.cleanup().unwrap();
}
