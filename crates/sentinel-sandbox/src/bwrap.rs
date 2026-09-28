//! Bubblewrap sandbox configuration.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

/// File descriptor bwrap writes its sandbox info JSON to (`--info-fd`).
const INFO_FD: RawFd = 3;
const WORKBENCH_BLOCK_FD: RawFd = 4;
const COMMAND_PROC: &str = "/run/sentinel-command-proc";

/// Total deadline for the accumulating `--info-fd` read loop, so a stuck bwrap
/// cannot block the daemon's spawn path indefinitely. Generous on purpose: the
/// eight info-JSON writes happen back-to-back once bwrap reaches the info dump,
/// so the budget only covers reaching that point under a heavy spawn burst.
const INFO_FD_TIMEOUT_MS: libc::c_int = 5000;
const WORKBENCH_ENVIRONMENT: [(&str, &str); 4] = [
    ("HOME", "/workspace"),
    ("LANG", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"),
    ("PATH", "/usr/bin:/bin"),
];

/// Result of spawning a bwrap sandbox.
///
/// `child` is the bwrap **supervisor** process (host/root netns by design,
/// used for cgroup membership and SIGTERM). `child_pid` is bwrap's sandbox
/// init process in the agent namespaces, reported by `--info-fd`. The command
/// normally runs as its direct child; attested workbench startup resolves and
/// verifies that runtime separately. `None` means bwrap did not report init.
#[derive(Debug)]
pub struct SpawnedSandbox {
    pub child: Child,
    pub child_pid: Option<u32>,
}

impl SpawnedSandbox {
    /// Reaps the owned supervisor; --die-with-parent terminates its namespace.
    pub fn terminate(&mut self) {
        terminate_sandbox_process(&mut self.child);
    }
}

pub(crate) fn terminate_sandbox_process(child: &mut Child) {
    match child.try_wait() {
        Ok(Some(_status)) => {}
        Ok(None) => {
            // The unreaped supervisor owns this group ID. Never signal the
            // cached init PID: it may already identify an unrelated process.
            let group = match i32::try_from(child.id()) {
                Ok(pid) => nix::unistd::Pid::from_raw(pid),
                Err(_) => {
                    warn!("sandbox supervisor PID exceeds pid_t");
                    return;
                }
            };
            match nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {
                    if let Err(error) = child.wait() {
                        warn!(%error, "sandbox supervisor could not be reaped");
                    }
                }
                Err(error) => warn!(%error, "owned sandbox group could not be terminated"),
            }
        }
        Err(error) => warn!(%error, "sandbox supervisor state could not be queried"),
    }
}

/// Bubblewrap sandbox configuration fuer einen einzelnen Agenten.
#[derive(Debug, Clone)]
pub struct BwrapConfig {
    pub hostname: String,
    pub readonly_binds: Vec<(String, String)>, // (host, guest)
    pub writable_binds: Vec<(String, String)>, // (host, guest)
    /// Read-only child mounts installed after writable parents (workbench inputs).
    pub readonly_overlay_binds: Vec<(String, String)>,
    pub tmpfs: Vec<String>,
    pub share_net: bool,
    pub die_with_parent: bool,
    /// Clear the parent daemon environment before starting the sandbox.
    pub clear_environment: bool,
    /// Trusted command-controller bind, never supplied by an employee request.
    pub command_boundary: Option<(String, u64)>,
    /// Agent-parent cgroup namespace and pinned runtime membership control.
    /// Created before controller delegation; never derived from tool input.
    workbench_namespace: Option<Arc<(File, File)>>,
    /// Missing host binds are fatal for profiles whose isolation contract is
    /// defined by those exact paths (the agent workbench).
    pub require_all_binds: bool,
    /// Mount /proc inside the sandbox (TOGAF: --proc /proc).
    pub proc_mount: Option<String>,
    /// Mount /dev inside the sandbox (TOGAF: --dev /dev).
    pub dev_mount: Option<String>,
}

impl BwrapConfig {
    /// Standard-Sandbox-Config fuer einen Agenten (TOGAF-konform).
    ///
    /// Minimale Namespace-Isolation:
    /// - System-Binaries readonly (/usr, /lib, /lib64 — noetig fuer agent-runtime + Deps)
    /// - Firmendaten readonly unter /company (TOGAF: --ro-bind /work/company /company)
    /// - DNS-Resolution readonly (/etc/resolv.conf)
    /// - Agent-Home writable (TOGAF: --bind /ram/agents/{name} /home/{name})
    /// - /tmp als tmpfs, /proc und /dev gemountet
    /// - Full network cage (#75): agents make NO network calls; the daemon
    ///   proxies all LLM traffic to the Cortex Gateway on the host.
    ///
    /// Landlock (Defense-in-Depth) schraenkt Zugriff innerhalb des Namespace weiter ein.
    pub fn for_agent(name: &str) -> Self {
        Self {
            hostname: hostname_for_agent(name),
            readonly_binds: vec![
                // System-Binaries + Libraries (noetig fuer agent-runtime Execution)
                ("/usr".to_string(), "/usr".to_string()),
                ("/lib".to_string(), "/lib".to_string()),
                ("/lib64".to_string(), "/lib64".to_string()),
                // DNS-Resolution (Landlock: read /etc/resolv.conf)
                (
                    "/etc/resolv.conf".to_string(),
                    "/etc/resolv.conf".to_string(),
                ),
                // Firmendaten readonly (TOGAF: --ro-bind /work/company /company)
                ("/work/company".to_string(), "/company".to_string()),
            ],
            writable_binds: vec![
                // Agent-Home writable (TOGAF: --bind /ram/agents/{name} /home/{name})
                (format!("/ram/agents/{name}"), format!("/home/{name}")),
            ],
            readonly_overlay_binds: Vec::new(),
            tmpfs: vec!["/tmp".to_string()],
            // #75 full cage: agents make NO network calls (agent-runtime has no
            // network code); the daemon proxies all LLM traffic to the Cortex
            // Gateway on the host. No --share-net -> own netns, loopback only.
            share_net: false,
            die_with_parent: true,
            clear_environment: false,
            command_boundary: None,
            workbench_namespace: None,
            require_all_binds: false,
            // TOGAF: --proc /proc
            proc_mount: Some("/proc".to_string()),
            // TOGAF: --dev /dev
            dev_mount: Some("/dev".to_string()),
        }
    }

    /// Replaces the default agent-home writable bind with a sentinel-fs FUSE mount path.
    ///
    /// Default: `/ram/agents/{name}` → `/home/{name}`
    /// With FS mount: `{fs_mount}/{host_agent_dir}` → `/home/{guest_name}`
    ///
    /// This enables CoW-backed per-agent filesystems via sentinel-fs FUSE.
    pub fn with_fs_mount(mut self, fs_mount: &str, host_agent_dir: &str, guest_name: &str) -> Self {
        self.writable_binds
            .retain(|(_, guest)| !guest.starts_with("/home/"));
        self.writable_binds.push((
            format!("{fs_mount}/{host_agent_dir}"),
            format!("/home/{guest_name}"),
        ));
        self
    }

    /// Mounts the agent-owned workbench roots at the stable protocol paths.
    ///
    /// The backing directories remain inside the same per-agent filesystem;
    /// the additional binds do not expose any host path outside that boundary.
    pub fn with_workbench_roots(mut self, host_agent_root: &Path) -> Self {
        self.readonly_overlay_binds.push((
            host_agent_root
                .join("inputs")
                .to_string_lossy()
                .into_owned(),
            "/workspace/.inputs".to_string(),
        ));
        self.writable_binds.push((
            host_agent_root
                .join("workspaces")
                .to_string_lossy()
                .into_owned(),
            "/workspace".to_string(),
        ));
        self.writable_binds.push((
            host_agent_root
                .join("artifacts")
                .to_string_lossy()
                .into_owned(),
            "/artifacts".to_string(),
        ));
        self
    }

    /// Route native tools and direct file tools through the same private FUSE workspace.
    /// Inputs and trusted completion artifacts retain their separate host boundaries.
    pub fn with_workbench_workspace(mut self, workspace: &Path) -> Self {
        self.writable_binds
            .retain(|(_, guest)| guest != "/workspace");
        self.writable_binds.push((
            workspace.to_string_lossy().into_owned(),
            "/workspace".to_string(),
        ));
        self
    }

    pub fn with_command_boundary(mut self, commands: &Path, budget_bytes: u64) -> Self {
        self.writable_binds.push((
            commands.to_string_lossy().into_owned(),
            "/run/sentinel-command-cgroups".to_owned(),
        ));
        if let Some(agent) = commands
            .parent()
            .filter(|_| commands.file_name() == Some(std::ffi::OsStr::new("commands")))
        {
            // Join before spawning the namespace broker so every trusted
            // descendant starts inside the cumulative agent runtime leaf.
            self.writable_binds.push((
                agent
                    .join("runtime/cgroup.procs")
                    .to_string_lossy()
                    .into_owned(),
                "/run/sentinel-runtime-cgroup.procs".to_owned(),
            ));
        }
        self.command_boundary = Some(("/run/sentinel-command-cgroups".to_owned(), budget_bytes));
        self
    }

    pub fn with_workbench_namespace(mut self, namespace: File, membership: File) -> Self {
        self.workbench_namespace = Some(Arc::new((namespace, membership)));
        self
    }

    /// Removes broad host-data binds that are not part of the workbench profile.
    pub fn for_workbench(mut self) -> Self {
        self.readonly_binds
            .retain(|(_, guest)| guest != "/company" && guest != "/etc/resolv.conf");
        self.writable_binds
            .retain(|(_, guest)| !guest.starts_with("/home/"));
        self.clear_environment = true;
        self.require_all_binds = true;
        self
    }

    /// Returns a config with shared host network.
    ///
    /// NOT used for agents — the agent default is full cage (#75). Kept for
    /// non-agent / diagnostic sandboxes that legitimately need host network.
    pub fn with_shared_net(mut self) -> Self {
        self.share_net = true;
        self
    }

    /// Tests whether bwrap user namespace creation works.
    ///
    /// Some systems (e.g. AppArmor) block unprivileged user namespaces.
    /// Returns true if bwrap can create a minimal sandbox.
    pub fn test_userns() -> bool {
        Command::new("bwrap")
            .args(["--unshare-user", "--ro-bind", "/", "/", "true"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Spawns a bwrap sandbox process with the configured isolation.
    ///
    /// Returns a [`SpawnedSandbox`] holding the bwrap supervisor `Child` plus
    /// the sandbox init PID (from bwrap `--info-fd`). The caller manages the
    /// process tree; the supervisor stays in the root netns, while init and its
    /// command child live in the sandbox namespaces.
    pub fn spawn(&self, command: &[String]) -> Result<SpawnedSandbox> {
        let config = self.with_existing_host_binds()?;
        anyhow::ensure!(
            config.command_boundary.is_none() || config.workbench_namespace.is_some(),
            "native commands require an agent-parent cgroup namespace"
        );
        let mut args = config.to_args();
        let startup_barrier = if config.workbench_namespace.is_some() {
            let (read, write) = nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC)
                .context("Failed to create native namespace startup barrier")?;
            Some((reserve_above_protocol_fds(read, WORKBENCH_BLOCK_FD)?, write))
        } else {
            None
        };
        if startup_barrier.is_some() {
            args.extend(["--block-fd".to_owned(), WORKBENCH_BLOCK_FD.to_string()]);
        }
        // bwrap writes `{"child-pid": N, ...}` to --info-fd once the sandbox is
        // set up. Options must precede the command.
        args.push("--info-fd".to_string());
        args.push(INFO_FD.to_string());
        args.extend(command.iter().cloned());

        log_bwrap_spawn(args.len());

        // Pipe for bwrap's --info-fd. Both ends CLOEXEC; the write end is
        // re-published at INFO_FD in the child via pre_exec (clearing CLOEXEC
        // on that descriptor only).
        let (info_read, info_write) = nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC)
            .context("Failed to create bwrap --info-fd pipe")?;
        let info_write = reserve_above_protocol_fds(info_write, WORKBENCH_BLOCK_FD)?;
        let write_fd = info_write.as_raw_fd();

        let mut cmd = Command::new("bwrap");
        let workbench_namespace = config.workbench_namespace.clone();
        let barrier_read = startup_barrier.as_ref().map(|(read, _)| read.as_raw_fd());
        if config.clear_environment {
            cmd.env_clear().envs(WORKBENCH_ENVIRONMENT);
            if let Some((root, budget)) = &config.command_boundary {
                cmd.env("SENTINEL_COMMAND_CGROUP_ROOT", root)
                    .env("SENTINEL_WORKSPACE_BUDGET_BYTES", budget.to_string());
            }
        }
        cmd.args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // Protocol failures are represented by typed, public-safe frames.
            // Inheriting child stderr would bypass that redaction boundary.
            .stderr(std::process::Stdio::null());
        // SAFETY: the closure runs in the forked child before exec and only
        // calls setns/write/setpgid/fcntl/dup2 on already-pinned descriptors.
        unsafe {
            cmd.pre_exec(move || {
                if let Some(ownership) = &workbench_namespace {
                    // The namespace must cover runtime AND sibling command
                    // leaves. Move while source and destination are visible in
                    // the host namespace, then enter the narrower agent namespace.
                    if libc::write(ownership.1.as_raw_fd(), b"0".as_ptr().cast(), 1) != 1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::setns(ownership.0.as_raw_fd(), libc::CLONE_NEWCGROUP) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if write_fd == INFO_FD {
                    let flags = libc::fcntl(write_fd, libc::F_GETFD);
                    if flags < 0
                        || libc::fcntl(write_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                } else if libc::dup2(write_fd, INFO_FD) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if let Some(read) = barrier_read {
                    if read == WORKBENCH_BLOCK_FD {
                        let flags = libc::fcntl(read, libc::F_GETFD);
                        if flags < 0
                            || libc::fcntl(read, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                        {
                            return Err(std::io::Error::last_os_error());
                        }
                    } else if libc::dup2(read, WORKBENCH_BLOCK_FD) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }

        let spawn_result = cmd.spawn();
        // Parent never writes to the info pipe.
        drop(info_write);
        let mut child = spawn_result.context("Failed to spawn bwrap process")?;

        // Takes ownership of read_fd and closes it.
        let child_pid = read_child_pid_from_info_fd(info_read.into_raw_fd(), INFO_FD_TIMEOUT_MS);
        if let Some((read, write)) = startup_barrier {
            drop(read);
            let setup = child_pid
                .context("Native sandbox init identity was not reported")
                .and_then(|pid| install_private_command_proc(&mut child, pid))
                .and_then(|_| {
                    nix::unistd::write(&write, b"1")
                        .map(|_| ())
                        .map_err(Into::into)
                });
            drop(write);
            if let Err(error) = setup {
                terminate_sandbox_process(&mut child);
                return Err(error).context("Native command namespace preparation failed");
            }
        }
        if child_pid.is_none() {
            warn!(
                "bwrap did not report its sandbox init PID via --info-fd; \
                 netns isolation verification will be skipped for this agent"
            );
        }

        Ok(SpawnedSandbox { child, child_pid })
    }

    fn with_existing_host_binds(&self) -> Result<Self> {
        let mut config = self.clone();
        if self.require_all_binds {
            for (host, guest) in self
                .readonly_binds
                .iter()
                .chain(&self.writable_binds)
                .chain(&self.readonly_overlay_binds)
            {
                anyhow::ensure!(
                    Path::new(host).exists(),
                    "required bwrap bind '{host}' -> '{guest}' is missing"
                );
            }
            return Ok(config);
        }
        config.readonly_binds.retain(|(host, guest)| {
            let exists = Path::new(host).exists();
            if !exists {
                warn!(
                    host = host.as_str(),
                    guest = guest.as_str(),
                    "Skipping bwrap readonly bind because host path is missing"
                );
            }
            exists
        });
        config.writable_binds.retain(|(host, guest)| {
            let exists = Path::new(host).exists();
            if !exists {
                warn!(
                    host = host.as_str(),
                    guest = guest.as_str(),
                    "Skipping bwrap writable bind because host path is missing"
                );
            }
            exists
        });
        config.readonly_overlay_binds.retain(|(host, guest)| {
            let exists = Path::new(host).exists();
            if !exists {
                warn!(
                    host = host.as_str(),
                    guest = guest.as_str(),
                    "Skipping bwrap readonly overlay because host path is missing"
                );
            }
            exists
        });
        Ok(config)
    }

    /// Generiert bwrap CLI-Argumente.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = if self.workbench_namespace.is_some() {
            // Cgroup isolation already uses the pinned agent-parent namespace.
            // Re-unsharing from runtime would hide the sibling command leaves.
            [
                "--unshare-user",
                "--unshare-ipc",
                "--unshare-pid",
                "--unshare-uts",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        } else {
            vec!["--unshare-all".to_string()]
        };

        if self.workbench_namespace.is_some() && !self.share_net {
            args.push("--unshare-net".to_owned());
        } else if self.share_net && self.workbench_namespace.is_none() {
            args.push("--share-net".to_string());
        }

        if self.die_with_parent {
            args.push("--die-with-parent".to_string());
        }

        args.push("--hostname".to_string());
        args.push(self.hostname.clone());
        if self.workbench_namespace.is_some() {
            args.extend(["--dir".to_owned(), COMMAND_PROC.to_owned()]);
        }

        // readonly binds
        for (host, guest) in &self.readonly_binds {
            args.push("--ro-bind".to_string());
            args.push(host.clone());
            args.push(guest.clone());
        }

        // writable binds
        for (host, guest) in &self.writable_binds {
            args.push("--bind".to_string());
            args.push(host.clone());
            args.push(guest.clone());
        }

        // readonly overlays must follow their writable parent mount.
        for (host, guest) in &self.readonly_overlay_binds {
            args.push("--ro-bind".to_string());
            args.push(host.clone());
            args.push(guest.clone());
        }

        // tmpfs
        for path in &self.tmpfs {
            args.push("--tmpfs".to_string());
            args.push(path.clone());
        }

        // proc mount (TOGAF: --proc /proc)
        if let Some(ref p) = self.proc_mount {
            args.push("--proc".to_string());
            args.push(p.clone());
        }

        // dev mount (TOGAF: --dev /dev)
        if let Some(ref d) = self.dev_mount {
            args.push("--dev".to_string());
            args.push(d.clone());
        }

        args
    }
}

fn reserve_above_protocol_fds(mut descriptor: OwnedFd, highest: RawFd) -> Result<OwnedFd> {
    // Publishing --info-fd must not overwrite the startup barrier's source.
    // Keep lower slots occupied until dup allocates a collision-free source.
    let mut reserved = Vec::new();
    while descriptor.as_raw_fd() <= highest {
        let duplicate = descriptor
            .try_clone()
            .context("Failed to reserve startup barrier descriptor")?;
        reserved.push(descriptor);
        descriptor = duplicate;
    }
    nix::fcntl::fcntl(
        &descriptor,
        nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
    )
    .context("Failed to close startup barrier source on exec")?;
    Ok(descriptor)
}

fn install_private_command_proc(supervisor: &mut Child, init_pid: u32) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    anyhow::ensure!(
        nix::unistd::geteuid().is_root(),
        "Native mount preparation requires the privileged host enforcer"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let init = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(format!("/proc/{init_pid}"))?;
    let init_path = format!("/proc/{}/fd/{}", std::process::id(), init.as_raw_fd());
    let root_path = format!("{init_path}/root");
    loop {
        anyhow::ensure!(
            supervisor.try_wait()?.is_none(),
            "Native sandbox exited before mount preparation"
        );
        verify_owned_init_parent(&init_path, supervisor.id())?;
        if Path::new(&root_path)
            .join(COMMAND_PROC.trim_start_matches('/'))
            .is_dir()
        {
            break;
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "Native sandbox mount root was not ready"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // Pin the owned init's mount/PID namespaces and pivoted root. Never pass
    // mutable workload paths or host /proc into the employee sandbox.
    let mount_namespace = File::open(format!("{init_path}/ns/mnt"))?;
    let pid_namespace = File::open(format!("{init_path}/ns/pid"))?;
    let root = File::open(&root_path)?;
    for (file, own) in [
        (&mount_namespace, "/proc/self/ns/mnt"),
        (&pid_namespace, "/proc/self/ns/pid"),
    ] {
        anyhow::ensure!(
            file.metadata()?.ino() != std::fs::metadata(own)?.ino(),
            "Native namespace was not isolated"
        );
    }
    for program in ["/usr/bin/nsenter", "/usr/bin/mount"] {
        let metadata = std::fs::symlink_metadata(program)?;
        anyhow::ensure!(
            metadata.is_file()
                && metadata.uid() == 0
                // The packaged mount binary may be setuid root. This helper
                // already runs as host root; never grant it to candidate code.
                && metadata.mode() & 0o3022 == 0
                && metadata.mode() & 0o111 != 0,
            "Trusted namespace setup executable is unavailable"
        );
    }
    let parent = std::process::id();
    let pinned = |file: &File| format!("/proc/{parent}/fd/{}", file.as_raw_fd());
    let child = Command::new("/usr/bin/nsenter")
        .arg(format!("--mount={}", pinned(&mount_namespace)))
        .arg(format!("--pid={}", pinned(&pid_namespace)))
        .arg(format!("--root={}", pinned(&root)))
        .args([
            "--wdns=/",
            "--",
            "/usr/bin/mount",
            "--internal-only",
            "-t",
            "proc",
            "-o",
            "nosuid,nodev,noexec",
            "proc",
            COMMAND_PROC,
        ])
        .env_clear()
        .envs(WORKBENCH_ENVIRONMENT)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("Failed to start private proc setup")?;
    let mut helper = OwnedProcSetup(Some(child));
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        use nix::sys::wait::{waitid, Id, WaitPidFlag, WaitStatus};
        let child = helper
            .0
            .as_ref()
            .context("Private proc setup ownership was lost")?;
        let pid = nix::unistd::Pid::from_raw(i32::try_from(child.id())?);
        let status = waitid(
            Id::Pid(pid),
            WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
        )
        .context("Private proc setup could not be observed")?;
        match status {
            WaitStatus::Exited(_, _) | WaitStatus::Signaled(_, _, _) => {
                let status = helper.finish()?;
                anyhow::ensure!(status.success(), "Private command proc mount failed");
                anyhow::ensure!(
                    supervisor.try_wait()?.is_none(),
                    "Native sandbox exited during mount preparation"
                );
                verify_owned_init_parent(&init_path, supervisor.id())?;
                return verify_private_command_proc(&root, &pid_namespace);
            }
            WaitStatus::StillAlive if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5))
            }
            _ => {
                helper.finish()?;
                anyhow::bail!("Private command proc setup timed out");
            }
        }
    }
}

fn verify_owned_init_parent(init_path: &str, supervisor: u32) -> Result<()> {
    let identity = std::fs::read_to_string(format!("{init_path}/stat"))?;
    let parent = identity
        .rsplit_once(") ")
        .and_then(|(_, fields)| fields.split_whitespace().nth(1))
        .and_then(|pid| pid.parse::<u32>().ok());
    anyhow::ensure!(
        parent == Some(supervisor),
        "Native sandbox init is not the owned supervisor child"
    );
    Ok(())
}

struct OwnedProcSetup(Option<Child>);

impl OwnedProcSetup {
    fn finish(&mut self) -> Result<std::process::ExitStatus> {
        self.finish_with_inspection(setup_group_has_live_members)
    }

    fn finish_with_inspection(
        &mut self,
        mut inspect: impl FnMut(i32, Instant) -> Result<bool>,
    ) -> Result<std::process::ExitStatus> {
        let child = self
            .0
            .as_mut()
            .context("Private proc setup ownership was lost")?;
        let group = i32::try_from(child.id()).context("Private proc setup PID exceeds pid_t")?;
        // waitid(WNOWAIT) retains the leader, so this group ID cannot be reused.
        match nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(group),
            nix::sys::signal::Signal::SIGKILL,
        ) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
            Err(error) => {
                return Err(error).context("Private proc setup group could not be stopped")
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        let quiescence = (|| -> Result<()> {
            while inspect(group, deadline)? {
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "Private proc setup group did not quiesce"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        })();
        // Even failed inspection must reap the killed leader. Its error remains
        // fatal; the caller then tears down the owned sandbox PID namespace.
        let status = child
            .wait()
            .context("Private proc setup leader could not be reaped")?;
        self.0.take();
        quiescence?;
        Ok(status)
    }
}

impl Drop for OwnedProcSetup {
    fn drop(&mut self) {
        if self.0.is_none() {
            return;
        }
        if let Err(error) = self.finish() {
            warn!(%error, "Owned private proc setup cleanup remains incomplete");
        }
    }
}

fn setup_group_has_live_members(group: i32, deadline: Instant) -> Result<bool> {
    anyhow::ensure!(
        Instant::now() < deadline,
        "Private proc setup inspection timed out"
    );
    for (index, entry) in std::fs::read_dir("/proc")?.enumerate() {
        anyhow::ensure!(
            index < 65536 && Instant::now() < deadline,
            "Private proc setup inspection exceeded its bound"
        );
        let entry = entry?;
        if !entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        let identity = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(identity) => identity,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(libc::ESRCH) =>
            {
                continue
            }
            Err(error) => {
                return Err(error).context("Private proc setup group could not be inspected")
            }
        };
        let fields: Vec<_> = identity
            .rsplit_once(") ")
            .context("Private proc setup process identity was malformed")?
            .1
            .split_whitespace()
            .collect();
        let actual = fields
            .get(2)
            .and_then(|value| value.parse::<i32>().ok())
            .context("Private proc setup process group was malformed")?;
        if actual == group
            && !fields
                .first()
                .is_some_and(|state| matches!(*state, "Z" | "X"))
        {
            return Ok(true);
        }
    }
    anyhow::ensure!(
        Instant::now() < deadline,
        "Private proc setup inspection timed out"
    );
    Ok(false)
}

fn verify_private_command_proc(root: &File, pid_namespace: &File) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = format!(
        "/proc/{}/fd/{}{COMMAND_PROC}",
        std::process::id(),
        root.as_raw_fd()
    );
    let procfs = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .context("Private command procfs was not mounted")?;
    anyhow::ensure!(
        nix::sys::statfs::fstatfs(&procfs)?.filesystem_type() == nix::sys::statfs::PROC_SUPER_MAGIC,
        "Private command proc mount is not procfs"
    );
    let visible_init = std::fs::metadata(format!("{path}/1/ns/pid"))?;
    let expected = pid_namespace.metadata()?;
    anyhow::ensure!(
        visible_init.dev() == expected.dev() && visible_init.ino() == expected.ino(),
        "Private procfs does not expose the exact owned agent PID namespace"
    );
    Ok(())
}

fn log_bwrap_spawn(argument_count: usize) {
    // Command content is intentionally excluded: executable paths and arguments
    // may contain invocation data or credentials supplied by the caller.
    info!(argument_count, "Spawning bwrap with direct argv");
}

fn hostname_for_agent(name: &str) -> String {
    const MAX_HOSTNAME_LEN: usize = 63;
    let mut token = String::with_capacity(name.len());
    let mut previous_was_dash = false;

    for ch in name.chars() {
        let next = if ch.is_ascii_alphanumeric() {
            previous_was_dash = false;
            Some(ch.to_ascii_lowercase())
        } else if !previous_was_dash {
            previous_was_dash = true;
            Some('-')
        } else {
            None
        };
        if let Some(ch) = next {
            token.push(ch);
        }
    }

    let token = token.trim_matches('-');
    let token = if token.is_empty() { "agent" } else { token };
    let mut hostname = format!("sentinel-{token}");
    hostname.truncate(MAX_HOSTNAME_LEN);
    while hostname.ends_with('-') {
        hostname.pop();
    }
    if hostname.is_empty() {
        "sentinel-agent".to_string()
    } else {
        hostname
    }
}

/// Reads the sandbox-init Host PID from bwrap's `--info-fd` pipe.
///
/// bwrap does NOT write the info JSON in one `write()`: bubblewrap 0.9.0 emits
/// it across eight syscalls (one per field — verified with strace), so the
/// first chunk (`{\n    "child-pid": N`) is not valid JSON on its own. A single
/// poll+read can therefore catch the JSON mid-sequence under load, which is why
/// ~2/26 agents were skipped during a restart burst (#75 follow-up). This reads
/// in a loop, accumulating bytes and re-parsing until a complete JSON object is
/// available, bounded by a total deadline. Takes ownership of `read_fd` (a
/// `File` closes it on every return path). Returns `None` — and logs the exact
/// failure mode — on deadline, EOF before complete JSON, or a poll/read error;
/// the caller then skips verification while the bwrap exit code stays the
/// primary fail-closed signal.
fn read_child_pid_from_info_fd(read_fd: RawFd, timeout_ms: libc::c_int) -> Option<u32> {
    // SAFETY: read_fd is a valid open fd; File takes ownership and closes it on drop.
    let mut file = unsafe { File::from_raw_fd(read_fd) };
    let fd = file.as_raw_fd();
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(0) as u64);
    let mut acc: Vec<u8> = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            warn!(
                bytes = acc.len(),
                "bwrap --info-fd: deadline reached without a complete JSON (child PID not reported)"
            );
            return None;
        }
        let remaining_ms = remaining.as_millis().min(libc::c_int::MAX as u128) as libc::c_int;
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: single valid pollfd; poll does not retain the pointer.
        let prc = unsafe { libc::poll(&mut pfd, 1, remaining_ms) };
        if prc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            warn!(error = %err, "bwrap --info-fd: poll error (child PID not reported)");
            return None;
        }
        if prc == 0 {
            warn!(
                bytes = acc.len(),
                "bwrap --info-fd: poll timeout, no data within deadline (child PID not reported)"
            );
            return None;
        }

        match file.read(&mut chunk) {
            Ok(0) => {
                warn!(
                    bytes = acc.len(),
                    "bwrap --info-fd: EOF before a complete JSON (child PID not reported)"
                );
                return None;
            }
            Ok(n) => {
                acc.extend_from_slice(&chunk[..n]);
                if let Some(pid) = parse_child_pid_bytes(&acc) {
                    debug!(
                        child_pid = pid,
                        bytes = acc.len(),
                        "bwrap --info-fd: child PID parsed"
                    );
                    return Some(pid);
                }
                // Incomplete JSON so far — keep accumulating.
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                warn!(error = %e, "bwrap --info-fd: read error (child PID not reported)");
                return None;
            }
        }
    }
}

/// Extracts the `child-pid` field from an accumulated info-JSON byte buffer.
///
/// Returns `None` while the buffer is not yet valid UTF-8 / complete JSON, so
/// the read loop keeps accumulating.
fn parse_child_pid_bytes(buf: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(buf).ok()?;
    parse_child_pid(text)
}

/// Extracts the `child-pid` field from bwrap's info JSON.
fn parse_child_pid(json: &str) -> Option<u32> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value
        .get("child-pid")
        .and_then(serde_json::Value::as_u64)
        .map(|pid| pid as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct SharedLogWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for SharedLogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn spawn_log_excludes_command_content() {
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = SharedLogWriter(std::sync::Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let secret_marker = "SECRET_ARGUMENT_MUST_NOT_APPEAR";

        tracing::subscriber::with_default(subscriber, || {
            let command = [secret_marker.to_string()];
            log_bwrap_spawn(command.len());
        });

        let captured = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(captured.contains("argument_count=1"));
        assert!(!captured.contains(secret_marker));
    }

    #[test]
    fn bwrap_command_structure() {
        let config = BwrapConfig::for_agent("test");
        let args = config.to_args();
        assert!(args.contains(&"--unshare-all".to_string()));
        assert!(args.contains(&"--die-with-parent".to_string()));
    }

    #[test]
    fn termination_never_signals_a_cached_foreign_init_pid() {
        let mut foreign = Command::new("/usr/bin/sleep").arg("30").spawn().unwrap();
        let child = Command::new("/usr/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let mut spawned = SpawnedSandbox {
            child,
            child_pid: Some(foreign.id()),
        };
        spawned.terminate();
        assert!(spawned.child.try_wait().unwrap().is_some());
        spawned.terminate();
        let foreign_status = foreign.try_wait().unwrap();
        let _ = foreign.kill();
        let _ = foreign.wait();
        assert!(
            foreign_status.is_none(),
            "saved numeric PID must not be signaled"
        );
    }

    #[test]
    fn reaped_supervisor_never_authorizes_a_later_group_signal() {
        let mut foreign = Command::new("/usr/bin/sleep").arg("30").spawn().unwrap();
        let mut child = Command::new("/usr/bin/true")
            .process_group(0)
            .spawn()
            .unwrap();
        child.wait().unwrap();
        let mut spawned = SpawnedSandbox {
            child,
            child_pid: Some(foreign.id()),
        };
        spawned.terminate();
        let foreign_status = foreign.try_wait().unwrap();
        let _ = foreign.kill();
        let _ = foreign.wait();
        assert!(foreign_status.is_none());
    }

    #[test]
    fn togaf_readonly_binds() {
        // TOGAF: --ro-bind /work/company /company + System-Binaries
        let config = BwrapConfig::for_agent("test");
        let args = config.to_args();
        assert!(args.contains(&"--ro-bind".to_string()));
        // Firmendaten
        assert!(args.contains(&"/work/company".to_string()));
        assert!(args.contains(&"/company".to_string()));
        // System-Binaries (noetig fuer agent-runtime Execution)
        assert!(args.contains(&"/usr".to_string()));
        assert!(args.contains(&"/lib".to_string()));
        assert!(args.contains(&"/lib64".to_string()));
        // DNS
        assert!(args.contains(&"/etc/resolv.conf".to_string()));
    }

    #[test]
    fn togaf_writable_binds() {
        // TOGAF: --bind /ram/agents/{name} /home/{name}
        let config = BwrapConfig::for_agent("test");
        let args = config.to_args();
        assert!(args.contains(&"--bind".to_string()));
        assert!(args.contains(&"/ram/agents/test".to_string()));
        assert!(args.contains(&"/home/test".to_string()));
    }

    #[test]
    fn chunk_workspace_replaces_only_the_writable_workspace_bind() {
        let config = BwrapConfig::for_agent("test")
            .for_workbench()
            .with_workbench_roots(Path::new("/ram/agents/test"))
            .with_workbench_workspace(Path::new("/sentinel-fs/AGENT-01/workspaces"));
        let workspaces: Vec<_> = config
            .writable_binds
            .iter()
            .filter(|(_, guest)| guest == "/workspace")
            .collect();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].0, "/sentinel-fs/AGENT-01/workspaces");
        assert!(config.writable_binds.contains(&(
            "/ram/agents/test/artifacts".to_owned(),
            "/artifacts".to_owned(),
        )));
        assert!(config.readonly_overlay_binds.contains(&(
            "/ram/agents/test/inputs".to_owned(),
            "/workspace/.inputs".to_owned(),
        )));
    }

    #[test]
    fn workbench_roots_stay_inside_the_agent_backing_directory() {
        let config = BwrapConfig::for_agent("test")
            .for_workbench()
            .with_workbench_roots(Path::new("/ram/agents/test"));
        assert!(config.writable_binds.contains(&(
            "/ram/agents/test/workspaces".to_string(),
            "/workspace".to_string(),
        )));
        assert!(!config
            .readonly_binds
            .iter()
            .any(|(_, guest)| guest == "/company" || guest == "/etc/resolv.conf"));
        assert!(!config
            .writable_binds
            .iter()
            .any(|(_, guest)| guest.starts_with("/home/")));
        assert!(config.clear_environment);
        assert!(config.writable_binds.contains(&(
            "/ram/agents/test/artifacts".to_string(),
            "/artifacts".to_string(),
        )));
        assert!(config.readonly_overlay_binds.contains(&(
            "/ram/agents/test/inputs".to_string(),
            "/workspace/.inputs".to_string(),
        )));
        assert!(config.require_all_binds);
        assert_eq!(
            WORKBENCH_ENVIRONMENT,
            [
                ("HOME", "/workspace"),
                ("LANG", "C.UTF-8"),
                ("LC_ALL", "C.UTF-8"),
                ("PATH", "/usr/bin:/bin"),
            ]
        );
        let args = config.to_args();
        let workspace = args
            .windows(3)
            .position(|args| {
                args[0] == "--bind"
                    && args[1] == "/ram/agents/test/workspaces"
                    && args[2] == "/workspace"
            })
            .unwrap();
        let inputs = args
            .windows(3)
            .position(|args| {
                args[0] == "--ro-bind"
                    && args[1] == "/ram/agents/test/inputs"
                    && args[2] == "/workspace/.inputs"
            })
            .unwrap();
        assert!(
            workspace < inputs,
            "read-only input overlay must be mounted last"
        );
    }

    #[test]
    fn command_controller_is_separate_from_employee_workspace_and_receipts() {
        let config = BwrapConfig::for_agent("test")
            .for_workbench()
            .with_workbench_roots(Path::new("/ram/agents/test"))
            .with_command_boundary(
                Path::new("/sys/fs/cgroup/sentinel/test/commands"),
                64 * 1024 * 1024,
            );
        assert_eq!(
            config.command_boundary,
            Some(("/run/sentinel-command-cgroups".into(), 64 * 1024 * 1024))
        );
        assert!(config.writable_binds.contains(&(
            "/sys/fs/cgroup/sentinel/test/commands".into(),
            "/run/sentinel-command-cgroups".into(),
        )));
        assert!(config.writable_binds.contains(&(
            "/sys/fs/cgroup/sentinel/test/runtime/cgroup.procs".into(),
            "/run/sentinel-runtime-cgroup.procs".into(),
        )));
        assert!(!config
            .writable_binds
            .iter()
            .any(|(_, guest)| guest == "/sys/fs/cgroup"));
        assert!(config.clear_environment);
    }

    #[test]
    fn startup_barrier_source_cannot_be_overwritten_by_protocol_descriptors() {
        let source = File::open("/dev/null").unwrap();
        let identity = source.metadata().unwrap();
        let highest = source.as_raw_fd() + 3;
        let pinned = reserve_above_protocol_fds(source.into(), highest).unwrap();
        assert!(pinned.as_raw_fd() > highest);
        let pinned = File::from(pinned);
        assert_eq!(pinned.metadata().unwrap().dev(), identity.dev());
        assert_eq!(pinned.metadata().unwrap().ino(), identity.ino());
        assert!(nix::fcntl::fcntl(&pinned, nix::fcntl::FcntlArg::F_GETFD)
            .map(|flags| nix::fcntl::FdFlag::from_bits_retain(flags)
                .contains(nix::fcntl::FdFlag::FD_CLOEXEC))
            .unwrap());
    }

    #[test]
    fn private_command_proc_requires_a_real_mount_not_an_ordinary_directory() {
        let directory = tempfile::tempdir().unwrap();
        let root = File::open(directory.path()).unwrap();
        let namespace = File::open("/proc/self/ns/pid").unwrap();
        assert!(verify_private_command_proc(&root, &namespace).is_err());
        std::fs::create_dir_all(directory.path().join(COMMAND_PROC.trim_start_matches('/')))
            .unwrap();
        let error = verify_private_command_proc(&root, &namespace).unwrap_err();
        assert!(error.to_string().contains("not procfs"));
    }

    #[test]
    fn private_proc_setup_cleanup_stops_descendants_before_reaping_its_leader() {
        use std::io::BufRead;
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 30 & echo $!; wait"])
            .process_group(0)
            .env_clear()
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let leader = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap());
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let descendant = line.trim().parse::<u32>().unwrap();
        let mut helper = OwnedProcSetup(Some(child));
        assert!(!helper.finish().unwrap().success());
        assert!(helper.0.is_none());
        assert_eq!(
            nix::sys::wait::waitpid(leader, Some(nix::sys::wait::WaitPidFlag::WNOHANG)),
            Err(nix::errno::Errno::ECHILD)
        );
        if let Ok(identity) = std::fs::read_to_string(format!("/proc/{descendant}/stat")) {
            let state = identity
                .rsplit_once(") ")
                .unwrap()
                .1
                .split_whitespace()
                .next()
                .unwrap();
            assert!(matches!(state, "Z" | "X"), "setup descendant remains live");
        }
    }

    #[test]
    fn private_proc_setup_observation_retains_the_leader_until_group_cleanup() {
        use nix::sys::wait::{waitid, Id, WaitPidFlag, WaitStatus};
        let child = Command::new("/bin/true").process_group(0).spawn().unwrap();
        let pid = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap());
        let mut helper = OwnedProcSetup(Some(child));
        assert_eq!(
            waitid(Id::Pid(pid), WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT).unwrap(),
            WaitStatus::Exited(pid, 0)
        );
        assert!(helper.finish().unwrap().success());
        assert!(helper.0.is_none());
    }

    #[test]
    fn private_proc_setup_inspection_failure_still_reaps_the_owned_leader() {
        use nix::sys::wait::{waitid, Id, WaitPidFlag, WaitStatus};
        let child = Command::new("/bin/true").process_group(0).spawn().unwrap();
        let pid = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap());
        let mut helper = OwnedProcSetup(Some(child));
        assert_eq!(
            waitid(Id::Pid(pid), WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT).unwrap(),
            WaitStatus::Exited(pid, 0)
        );
        let error = helper
            .finish_with_inspection(|_, _| anyhow::bail!("injected inspection failure"))
            .unwrap_err();
        assert!(error.to_string().contains("injected inspection failure"));
        assert!(helper.0.is_none());
        assert_eq!(
            nix::sys::wait::waitpid(pid, Some(WaitPidFlag::WNOHANG)),
            Err(nix::errno::Errno::ECHILD)
        );
        assert!(setup_group_has_live_members(
            pid.as_raw(),
            Instant::now() - Duration::from_millis(1)
        )
        .is_err());
    }

    #[test]
    #[ignore = "Requires root, writable sentinel cgroups and built immutable wrapper/runtime paths in SENTINEL_TEST_NAMESPACE_WRAPPER/SENTINEL_TEST_AGENT_RUNTIME"]
    fn kernel_workbench_bootstrap_and_replacement_preserve_private_namespaces() {
        use crate::cgroups::{self, CgroupLimits};
        use std::io::Write;
        let wrapper = std::path::PathBuf::from(
            std::env::var_os("SENTINEL_TEST_NAMESPACE_WRAPPER")
                .expect("Set SENTINEL_TEST_NAMESPACE_WRAPPER"),
        );
        let runtime = std::path::PathBuf::from(
            std::env::var_os("SENTINEL_TEST_AGENT_RUNTIME")
                .expect("Set SENTINEL_TEST_AGENT_RUNTIME"),
        );
        let name = format!("bootstrap-test-{}", uuid::Uuid::new_v4());
        assert!(!Path::new(&cgroups::cgroup_path(&name)).exists());
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = cgroups::remove_cgroup(&self.0);
            }
        }
        let _cleanup = Cleanup(name.clone());
        let root = tempfile::tempdir().unwrap();
        for entry in ["workspaces", "inputs", "artifacts"] {
            std::fs::create_dir(root.path().join(entry)).unwrap();
        }
        // This covers real namespace/broker bootstrap and full-tree replacement,
        // not company authority, invocation effects or FUSE acceptance.
        for _ in 0..2 {
            cgroups::create_cgroup(&name, &CgroupLimits::default()).unwrap();
            let namespace = cgroups::prepare_workbench_namespace(&name, &wrapper).unwrap();
            let commands = cgroups::prepare_workbench_cgroup(&name).unwrap();
            let membership = cgroups::open_workbench_runtime_membership(&name).unwrap();
            let mut config = BwrapConfig::for_agent(&name)
                .for_workbench()
                .with_workbench_roots(root.path())
                .with_command_boundary(&commands, 64 * 1024 * 1024)
                .with_workbench_namespace(namespace, membership);
            config.readonly_binds.push((
                wrapper.to_string_lossy().into_owned(),
                "/landlock-wrapper".into(),
            ));
            config.readonly_binds.push((
                runtime.to_string_lossy().into_owned(),
                "/usr/bin/agent-runtime".into(),
            ));
            let command = vec![
                "/landlock-wrapper".into(),
                "--attest-v1".into(),
                uuid::Uuid::new_v4().to_string(),
                crate::landlock::LANDLOCK_RULESET_ABI.to_string(),
                name.clone(),
                "--".into(),
                "/usr/bin/agent-runtime".into(),
            ];
            let mut sandbox = config.spawn(&command).unwrap();
            let init = sandbox.child_pid.unwrap();
            assert_ne!(
                std::fs::metadata(format!("/proc/{init}/ns/cgroup"))
                    .unwrap()
                    .ino(),
                std::fs::metadata("/proc/self/ns/cgroup").unwrap().ino()
            );
            sandbox.child.stdin.as_mut().unwrap().write_all(
                b"{\"kind\":\"health\",\"schema_version\":1,\"request_id\":\"bootstrap-probe\"}\n"
            ).unwrap();
            drop(sandbox.child.stdin.take());
            let deadline = Instant::now() + Duration::from_secs(10);
            let status = loop {
                if let Some(status) = sandbox.child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= deadline {
                    sandbox.terminate();
                    panic!("compiled workbench bootstrap did not finish");
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            assert!(
                status.success(),
                "compiled workbench bootstrap failed: {status}"
            );
            let mut output = String::new();
            sandbox
                .child
                .stdout
                .take()
                .unwrap()
                .take(65536)
                .read_to_string(&mut output)
                .unwrap();
            assert!(
                output
                    .lines()
                    .any(|line| serde_json::from_str::<serde_json::Value>(line)
                        .is_ok_and(|frame| frame["kind"] == "health" && frame["healthy"] == true)),
                "compiled runtime did not answer the health frame"
            );
            drop(config);
            cgroups::remove_cgroup(&name).unwrap();
            assert!(!Path::new(&cgroups::cgroup_path(&name)).exists());
        }
    }

    #[test]
    fn agent_default_is_full_cage() {
        // #75: agents make no network calls; the default is a full network cage
        // (own netns, loopback only) — NO --share-net.
        let config = BwrapConfig::for_agent("test");
        assert!(!config.share_net, "#75: agent default must be full cage");
        let args = config.to_args();
        assert!(
            !args.contains(&"--share-net".to_string()),
            "agents must not get --share-net, args: {args:?}"
        );
        assert!(args.contains(&"--unshare-all".to_string()));
    }

    #[test]
    fn workbench_preserves_pinned_cgroup_namespace_and_all_other_cages() {
        // Descriptor validity is enforced by setns/write at spawn. This test
        // checks argv only; it does not claim kernel namespace enforcement.
        let config = BwrapConfig::for_agent("test")
            .for_workbench()
            .with_workbench_namespace(
                File::open("/dev/null").unwrap(),
                File::open("/dev/null").unwrap(),
            );
        let args = config.to_args();
        for required in [
            "--unshare-user",
            "--unshare-ipc",
            "--unshare-pid",
            "--unshare-uts",
            "--unshare-net",
            "--die-with-parent",
        ] {
            assert!(args.iter().any(|arg| arg == required));
        }
        for forbidden in ["--unshare-all", "--unshare-cgroup", "--share-net"] {
            assert!(!args.iter().any(|arg| arg == forbidden));
        }
        assert!(!config
            .writable_binds
            .iter()
            .any(|(_, guest)| guest == "/sys/fs/cgroup"));
        assert!(args.windows(2).any(|pair| pair == ["--dir", COMMAND_PROC]));
        assert!(!config
            .readonly_binds
            .iter()
            .chain(&config.writable_binds)
            .any(|(_, guest)| guest == COMMAND_PROC));
    }

    #[test]
    fn parse_child_pid_extracts_pid() {
        assert_eq!(parse_child_pid(r#"{"child-pid": 12345}"#), Some(12345));
        assert_eq!(
            parse_child_pid(r#"{"child-pid": 7, "cgroup": "x"}"#),
            Some(7)
        );
    }

    #[test]
    fn parse_child_pid_handles_garbage() {
        assert_eq!(parse_child_pid(""), None);
        assert_eq!(parse_child_pid("not json"), None);
        assert_eq!(parse_child_pid(r#"{"no-pid": 1}"#), None);
    }

    #[test]
    fn parse_child_pid_bytes_accumulates() {
        // bwrap's first write() is incomplete JSON -> None, so the loop keeps reading.
        assert_eq!(parse_child_pid_bytes(b"{\n    \"child-pid\": 12345"), None);
        // A complete object parses, even before the trailing newline.
        assert_eq!(
            parse_child_pid_bytes(b"{\n    \"child-pid\": 12345\n}"),
            Some(12345)
        );
        // Full bwrap-style payload with the trailing newline.
        let full = b"{\n    \"child-pid\": 7,\n    \"net-namespace\": 123\n}\n";
        assert_eq!(parse_child_pid_bytes(full), Some(7));
        // Not-yet-valid UTF-8 -> None (keep accumulating).
        assert_eq!(parse_child_pid_bytes(&[0xff, 0xfe]), None);
    }

    // Replays bwrap 0.9.0's eight-write info-JSON sequence across a real pipe
    // with gaps, so the read loop must accumulate across poll/read iterations.
    #[test]
    fn read_child_pid_reassembles_chunked_writes() {
        use std::io::Write;
        use std::os::fd::{IntoRawFd, OwnedFd};

        let (reader, mut writer) = std::io::pipe().expect("pipe");
        let handle = std::thread::spawn(move || {
            let chunks: [&[u8]; 8] = [
                b"{\n    \"child-pid\": 12345",
                b",\n    \"cgroup-namespace\": 4026534166",
                b",\n    \"ipc-namespace\": 4026534164",
                b",\n    \"mnt-namespace\": 4026534162",
                b",\n    \"net-namespace\": 4026534167",
                b",\n    \"pid-namespace\": 4026534165",
                b",\n    \"uts-namespace\": 4026534163",
                b"\n}\n",
            ];
            for chunk in chunks {
                let _ = writer.write_all(chunk);
                let _ = writer.flush();
                std::thread::sleep(Duration::from_millis(2));
            }
            // writer dropped -> EOF
        });

        let read_fd = OwnedFd::from(reader).into_raw_fd();
        let pid = read_child_pid_from_info_fd(read_fd, 5000);
        handle.join().unwrap();
        assert_eq!(pid, Some(12345));
    }

    #[test]
    fn read_child_pid_eof_before_complete_returns_none() {
        use std::io::Write;
        use std::os::fd::{IntoRawFd, OwnedFd};

        let (reader, mut writer) = std::io::pipe().expect("pipe");
        let handle = std::thread::spawn(move || {
            // Only the first (incomplete) chunk, then close -> EOF before complete.
            let _ = writer.write_all(b"{\n    \"child-pid\": 999");
            // writer dropped -> EOF
        });

        let read_fd = OwnedFd::from(reader).into_raw_fd();
        let pid = read_child_pid_from_info_fd(read_fd, 2000);
        handle.join().unwrap();
        assert_eq!(pid, None);
    }

    #[test]
    fn with_shared_net_builder() {
        let config = BwrapConfig::for_agent("test").with_shared_net();
        assert!(config.share_net);
        let args = config.to_args();
        assert!(args.contains(&"--share-net".to_string()));
    }

    #[test]
    fn togaf_proc_mount_default() {
        // TOGAF: --proc /proc
        let config = BwrapConfig::for_agent("test");
        assert_eq!(config.proc_mount, Some("/proc".to_string()));
        let args = config.to_args();
        let idx = args
            .iter()
            .position(|a| a == "--proc")
            .expect("--proc missing");
        assert_eq!(args[idx + 1], "/proc");
    }

    #[test]
    fn togaf_dev_mount_default() {
        // TOGAF: --dev /dev
        let config = BwrapConfig::for_agent("test");
        assert_eq!(config.dev_mount, Some("/dev".to_string()));
        let args = config.to_args();
        let idx = args
            .iter()
            .position(|a| a == "--dev")
            .expect("--dev missing");
        assert_eq!(args[idx + 1], "/dev");
    }

    #[test]
    fn togaf_hostname() {
        let config = BwrapConfig::for_agent("thomas");
        assert_eq!(config.hostname, "sentinel-thomas");
        let args = config.to_args();
        let idx = args
            .iter()
            .position(|a| a == "--hostname")
            .expect("--hostname missing");
        assert_eq!(args[idx + 1], "sentinel-thomas");
    }

    #[test]
    fn hostname_sanitizes_display_names_for_bwrap() {
        let config = BwrapConfig::for_agent(
            "Victoria Lehmann (intern \"Vicky\", akzeptiert beide Varianten)",
        );
        assert!(config.hostname.starts_with("sentinel-victoria-lehmann"));
        assert!(config.hostname.len() <= 63);
        assert!(!config.hostname.ends_with('-'));
        assert!(config
            .hostname
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'));
    }

    #[test]
    fn with_fs_mount_replaces_agent_home() {
        let config =
            BwrapConfig::for_agent("thomas").with_fs_mount("/sentinel-fs", "AGENT-01", "thomas");
        let args = config.to_args();
        // Old /ram/agents/ path must be gone
        assert!(
            !args.contains(&"/ram/agents/thomas".to_string()),
            "Old ram path should be replaced"
        );
        // New sentinel-fs path must be present
        assert!(
            args.contains(&"/sentinel-fs/AGENT-01".to_string()),
            "sentinel-fs path missing, args: {:?}",
            args
        );
        assert!(
            args.contains(&"/home/thomas".to_string()),
            "guest /home/thomas missing"
        );
    }

    #[test]
    fn togaf_tmpfs() {
        let config = BwrapConfig::for_agent("test");
        let args = config.to_args();
        let idx = args
            .iter()
            .position(|a| a == "--tmpfs")
            .expect("--tmpfs missing");
        assert_eq!(args[idx + 1], "/tmp");
    }
}
