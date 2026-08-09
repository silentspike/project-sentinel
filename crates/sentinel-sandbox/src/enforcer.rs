//! SandboxEnforcer — zentrale Facade fuer Landlock + cgroups + bwrap.
//!
//! Orchestriert alle drei Isolationsmechanismen:
//! - Landlock LSM (Kernel-Level Filesystem-Restriktion)
//! - cgroups v2 (CPU, Memory, PID Limits)
//! - bwrap (Namespace-Isolation: PID, Mount, UTS)
//!
//! Lifecycle:
//! 1. `detect()` — prueft verfuegbare Kernel-Features, setzt OOM-Score
//! 2. `setup_agent()` — erstellt cgroup + Agent-Home
//! 3. `start_agent_process()` — startet bwrap (spaeter: mit Landlock im Child)
//! 4. `teardown_agent()` — beendet bwrap-Reste + entfernt cgroup

use std::io::{BufRead, BufReader, Write};
#[cfg(test)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};

use anyhow::{bail, Context, Result};
use tracing::{debug, info, warn};

use crate::bwrap::{terminate_sandbox_process, BwrapConfig, SpawnedSandbox};
use crate::cgroups::{self, CgroupLimits, PsiMetrics};
use crate::landlock;

const PROTOCOL_LINE_LIMIT_BYTES: usize = 1024 * 1024;
const PROTOCOL_QUEUE_DEPTH: usize = 64;

enum ProtocolFrame {
    Line(String),
    Rejected,
}

pub(crate) struct ProtocolDrain {
    pub(crate) lines: Vec<String>,
    pub(crate) disconnected: bool,
}

/// Handle fuer einen laufenden Agent-Prozess in bwrap.
///
/// Haelt den Child-Handle am Leben (bwrap hat --die-with-parent,
/// stirbt also wenn der Daemon stirbt). stdin ist piped fuer
/// stream-json Kommunikation mit dem Agent.
pub struct AgentProcess {
    /// PID des bwrap-Supervisor-Prozesses (bleibt by-design im Root-netns;
    /// genutzt fuer cgroup-Membership und SIGTERM).
    pub pid: u32,
    /// PID des sandboxed `agent-runtime` im Agent-netns (aus bwrap `--info-fd`).
    /// `None`, falls bwrap ihn nicht meldete -> netns-Verifikation entfaellt;
    /// das bwrap-Exit bleibt das primaere fail-closed-Signal (#75).
    pub child_pid: Option<u32>,
    /// Child handle — NICHT droppen solange Agent laufen soll.
    child: Child,
    protocol_stdin: Option<ChildStdin>,
    protocol_stdout: Option<Receiver<ProtocolFrame>>,
    protocol_reader: Option<std::thread::JoinHandle<()>>,
}

impl AgentProcess {
    #[cfg(test)]
    pub(crate) fn launch_fixture() -> Result<Self> {
        let mut command = std::process::Command::new("/usr/bin/sleep");
        command
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // SAFETY: setpgid is async-signal-safe and gives the fixture the same
        // complete-tree termination boundary as production bwrap processes.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().context("start sandbox lifecycle fixture")?;
        let pid = child.id();
        Ok(Self {
            pid,
            child_pid: None,
            protocol_stdin: child.stdin.take(),
            protocol_stdout: None,
            protocol_reader: None,
            child,
        })
    }

    #[cfg(test)]
    pub(crate) fn launch_protocol_fixture(lines: &[&str]) -> Result<Self> {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                "IFS= read -r frame; if [ \"$#\" -gt 0 ]; then printf '%s\\n' \"$@\"; fi; sleep 5",
                "fixture",
            ])
            .args(lines)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        // SAFETY: see launch_fixture.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().context("start sandbox protocol fixture")?;
        let pid = child.id();
        let protocol_stdin = child.stdin.take();
        let (protocol_stdout, protocol_reader) = protocol_reader_parts(child.stdout.take());
        Ok(Self {
            pid,
            child_pid: None,
            protocol_stdin,
            protocol_stdout,
            protocol_reader,
            child,
        })
    }

    #[cfg(test)]
    pub(crate) fn launch_protocol_eof_fixture() -> Result<Self> {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", "IFS= read -r frame; exit 0"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        // SAFETY: see launch_fixture.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .context("start sandbox protocol EOF fixture")?;
        let pid = child.id();
        let protocol_stdin = child.stdin.take();
        let (protocol_stdout, protocol_reader) = protocol_reader_parts(child.stdout.take());
        Ok(Self {
            pid,
            child_pid: None,
            protocol_stdin,
            protocol_stdout,
            protocol_reader,
            child,
        })
    }

    #[cfg(test)]
    pub(crate) fn launch_raw_protocol_fixture(script: &str) -> Result<Self> {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", script])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        // SAFETY: see launch_fixture.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .context("start raw sandbox protocol fixture")?;
        let pid = child.id();
        let protocol_stdin = child.stdin.take();
        let (protocol_stdout, protocol_reader) = protocol_reader_parts(child.stdout.take());
        Ok(Self {
            pid,
            child_pid: None,
            protocol_stdin,
            protocol_stdout,
            protocol_reader,
            child,
        })
    }

    #[cfg(test)]
    pub(crate) fn launch_recording_protocol_fixture(
        lines: &[&str],
        record_path: &Path,
        descendant_pid_path: &Path,
    ) -> Result<Self> {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                "record=$1; descendant_file=$2; shift 2; sleep 30 & descendant=$!; printf '%s\\n' \"$descendant\" > \"$descendant_file\"; emitted=0; while IFS= read -r frame; do printf '%s\\n' \"$frame\" >> \"$record\"; if [ \"$emitted\" -eq 0 ]; then if [ \"$#\" -gt 0 ]; then printf '%s\\n' \"$@\"; fi; emitted=1; fi; done; wait \"$descendant\"",
                "fixture",
            ])
            .arg(record_path)
            .arg(descendant_pid_path)
            .args(lines)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        // SAFETY: see launch_fixture.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .context("start recording sandbox protocol fixture")?;
        let pid = child.id();
        let protocol_stdin = child.stdin.take();
        let (protocol_stdout, protocol_reader) = protocol_reader_parts(child.stdout.take());
        Ok(Self {
            pid,
            child_pid: None,
            protocol_stdin,
            protocol_stdout,
            protocol_reader,
            child,
        })
    }

    /// Nimmt den stdin-Handle fuer stream-json Kommunikation (einmalig).
    pub fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.protocol_stdin.take()
    }

    /// Sends one bounded JSONL protocol frame to the sandboxed child.
    pub fn send_protocol_line(&mut self, line: &str) -> Result<()> {
        if line.len() > PROTOCOL_LINE_LIMIT_BYTES {
            anyhow::bail!("sandbox protocol input exceeded its configured bound");
        }
        if line
            .as_bytes()
            .iter()
            .any(|byte| matches!(byte, b'\n' | b'\r'))
        {
            anyhow::bail!("sandbox protocol input must contain exactly one JSONL record");
        }
        let stdin = self
            .protocol_stdin
            .as_mut()
            .context("sandbox protocol stdin is unavailable")?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .context("write sandbox protocol frame")
    }

    pub(crate) fn protocol_channel_available(&self) -> bool {
        self.protocol_stdin.is_some() && self.protocol_stdout.is_some()
    }

    /// Drains complete JSONL frames already emitted by the child. Reading is
    /// performed by a dedicated thread so registry polling never blocks.
    pub(crate) fn drain_protocol_lines(&mut self) -> Result<ProtocolDrain> {
        let receiver = self
            .protocol_stdout
            .as_ref()
            .context("sandbox protocol stdout is unavailable")?;
        let mut lines = Vec::new();
        let mut disconnected = false;
        loop {
            match receiver.try_recv() {
                Ok(ProtocolFrame::Line(line)) => lines.push(line),
                Ok(ProtocolFrame::Rejected) => {
                    anyhow::bail!("sandbox protocol stdout emitted an invalid or oversized frame")
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        Ok(ProtocolDrain {
            lines,
            disconnected,
        })
    }

    /// Prueft ob der Prozess noch laeuft.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Terminates and reaps the child process owned by this handle.
    pub fn terminate(&mut self) {
        self.terminate_process_group();
        self.join_protocol_reader();
    }

    /// Terminates and reaps the child, surfacing incomplete cleanup so the
    /// NanoRuntime can retain ownership and retry instead of forgetting a live
    /// sandbox process.
    pub fn terminate_checked(&mut self) -> Result<()> {
        self.terminate();
        match self
            .child
            .try_wait()
            .context("query sandbox supervisor after termination")?
        {
            Some(_) => {}
            None => bail!(
                "sandbox supervisor {} remained alive after termination",
                self.pid
            ),
        }
        if self.child_pid.is_some_and(pid_exists) {
            bail!("sandboxed child remained alive after termination");
        }
        Ok(())
    }

    pub(crate) fn terminate_process_group(&mut self) {
        self.protocol_stdin.take();
        self.protocol_stdout.take();
        if let Ok(group) = i32::try_from(self.pid) {
            // Every AgentProcess is launched as its own process-group leader.
            // Signal the group even if the tracked leader has already exited,
            // because a descendant may still own stdout or other resources.
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
        terminate_sandbox_process(&mut self.child, self.child_pid);
    }

    pub(crate) fn join_protocol_reader(&mut self) {
        if let Some(reader) = self.protocol_reader.take() {
            let _ = reader.join();
        }
    }
}

impl From<SpawnedSandbox> for AgentProcess {
    fn from(spawned: SpawnedSandbox) -> Self {
        let mut child = spawned.child;
        let pid = child.id();
        let protocol_stdin = child.stdin.take();
        let (protocol_stdout, protocol_reader) = protocol_reader_parts(child.stdout.take());
        Self {
            pid,
            child_pid: spawned.child_pid,
            protocol_stdin,
            protocol_stdout,
            protocol_reader,
            child,
        }
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        // The runtime's normal teardown joins the reader after cgroup cleanup.
        // Drop still closes pipes and kills/reaps the owned process group, but
        // does not risk blocking forever on a descendant that escaped that
        // group while remaining in the production cgroup.
        self.terminate_process_group();
    }
}

fn pid_exists(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

fn wait_for_pid_exit(pid: u32) {
    for _ in 0..20 {
        if !pid_exists(pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn cleanup_cgroup_after_process_exit(name: &str) -> Result<()> {
    cleanup_cgroup_after_process_exit_with(
        name,
        cgroups::list_pids_in_cgroup,
        cgroups::kill_cgroup_processes,
        cgroups::remove_cgroup,
    )
}

fn cleanup_cgroup_after_process_exit_with<List, Kill, Remove>(
    name: &str,
    list_pids: List,
    kill_pids: Kill,
    remove: Remove,
) -> Result<()>
where
    List: Fn(&str) -> Result<Vec<u32>>,
    Kill: Fn(&str) -> Result<usize>,
    Remove: Fn(&str) -> Result<()>,
{
    match list_pids(name) {
        Ok(pids) if pids.is_empty() => remove(name),
        Ok(pids) => {
            debug!(
                cgroup = %name,
                pid_count = pids.len(),
                "cgroup vor Entfernen noch belegt, beende Mitglieder"
            );
            kill_pids(name)?;
            remove(name)
        }
        Err(error) => {
            warn!(
                cgroup = %name,
                error = %error,
                "cgroup-Mitglieder konnten vor Remove nicht gelesen werden"
            );
            remove(name)
        }
    }
}

impl std::fmt::Debug for AgentProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentProcess")
            .field("pid", &self.pid)
            .finish()
    }
}

/// Warnings about degraded sandbox capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxWarning {
    /// Landlock LSM not available on this kernel.
    LandlockNotAvailable,
    /// A cgroup controller is not delegated to the user.
    CgroupNotDelegated(String),
    /// bwrap can't create user namespaces (AppArmor blocks it).
    BwrapUsernsDenied,
    /// IO controller not delegated — io.max limits cannot be enforced.
    IoNotDelegated,
    /// Failed to set OOM score for ECS core process.
    OomScoreFailed(String),
}

/// Result of verifying that an agent runs in its own network namespace (#75).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationStatus {
    /// Agent netns differs from the daemon's — full cage in effect.
    Isolated,
    /// Agent shares the daemon's netns (same inode) — NOT caged. Act on this.
    NotIsolated,
    /// Namespace inode could not be read (transient). MUST NOT be treated as a
    /// cage breach — the bwrap exit code is the primary fail-closed signal.
    ProbeError,
}

/// Handle returned by setup_agent() — tracks what was created.
#[derive(Debug, Clone)]
pub struct SandboxHandle {
    pub agent_name: String,
    pub cgroup_created: bool,
    /// Captured at setup so eBPF deregistration remains possible after the
    /// adapter has removed the cgroup directory.
    pub cgroup_id: Option<u64>,
    pub io_available: bool,
    pub bwrap_pid: Option<u32>,
    pub landlock_applied: bool,
    /// Whether the post-spawn netns verification confirmed isolation (#75).
    pub network_isolated: bool,
}

/// Central sandbox enforcement facade.
///
/// Bundles Landlock + cgroups v2 + bwrap into a single interface.
/// Created via `detect()` which probes kernel capabilities.
pub struct SandboxEnforcer {
    /// Detected Landlock ABI version (None = not available).
    landlock_abi: Option<u8>,
    /// cgroup v2 root for sentinel agents.
    cgroup_root: PathBuf,
    /// Whether cgroup root is writable by current user.
    cgroup_available: bool,
    /// Whether bwrap with user namespaces works.
    bwrap_available: bool,
    /// Whether OOM score has been set for ECS core.
    oom_set: AtomicBool,
    /// Optional sentinel-fs FUSE mount path.
    /// When set, bwrap binds `{fs_mount}/{name}` instead of `/ram/agents/{name}`.
    fs_mount: Option<String>,
}

impl std::fmt::Debug for SandboxEnforcer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxEnforcer")
            .field("landlock_abi", &self.landlock_abi)
            .field("cgroup_root", &self.cgroup_root)
            .field("cgroup_available", &self.cgroup_available)
            .field("bwrap_available", &self.bwrap_available)
            .finish()
    }
}

impl SandboxEnforcer {
    fn cgroup_root_writable(cgroup_root: &Path) -> bool {
        let probe = cgroup_root.join(format!(".sentinel-write-check-{}", std::process::id()));
        match std::fs::create_dir(&probe) {
            Ok(()) => {
                let _ = std::fs::remove_dir(&probe);
                true
            }
            Err(_) => false,
        }
    }

    /// Detects available kernel sandbox features.
    ///
    /// Probes:
    /// - Landlock ABI version
    /// - cgroup v2 root writability
    /// - IO controller delegation
    /// - bwrap user namespace support
    /// - Sets OOM score -1000 for ECS core process (immortal)
    pub fn detect() -> (Self, Vec<SandboxWarning>) {
        let mut warnings = Vec::new();

        // 1. Landlock detection
        let landlock_abi = landlock::detect_abi();
        if let Some(abi) = landlock_abi {
            info!("Landlock ABI v{abi} detected");
        } else {
            warnings.push(SandboxWarning::LandlockNotAvailable);
        }

        // 2. cgroup root + controller delegation
        let cgroup_root = PathBuf::from("/sys/fs/cgroup/sentinel");
        let cgroup_available = if cgroup_root.exists() {
            if Self::cgroup_root_writable(&cgroup_root) {
                true
            } else {
                warnings.push(SandboxWarning::CgroupNotDelegated(format!(
                    "{} exists but is not writable by the current user",
                    cgroup_root.display()
                )));
                false
            }
        } else {
            match std::fs::create_dir_all(&cgroup_root) {
                Ok(_) => {
                    info!("Created cgroup root: {}", cgroup_root.display());
                    true
                }
                Err(e) => {
                    warnings.push(SandboxWarning::CgroupNotDelegated(format!(
                        "Cannot create {}: {e}",
                        cgroup_root.display()
                    )));
                    false
                }
            }
        };

        // 2b. Delegate controllers (cpu, memory, pids, io) from root cgroup to sentinel
        // This enables cpu.max, memory.max, etc. in agent child cgroups.
        if cgroup_available {
            // First enable controllers at /sys/fs/cgroup level (root → sentinel)
            cgroups::delegate_controllers("/sys/fs/cgroup");
            // Then enable at sentinel level (sentinel → agent children)
            cgroups::delegate_controllers("/sys/fs/cgroup/sentinel");
        }

        // 3. IO controller check — verify IO is now available in sentinel subtree
        let sentinel_has_io =
            cgroup_available && cgroups::io_controller_enabled("/sys/fs/cgroup/sentinel");
        if !sentinel_has_io {
            warnings.push(SandboxWarning::IoNotDelegated);
        }

        // 4. bwrap userns check
        let bwrap_available = BwrapConfig::test_userns();
        if !bwrap_available {
            warnings.push(SandboxWarning::BwrapUsernsDenied);
        } else {
            info!("bwrap user namespace support confirmed");
        }

        // 5. OOM score for ECS core (-1000 = immortal)
        let oom_set = match cgroups::set_oom_score(std::process::id(), -1000) {
            Ok(_) => {
                info!("ECS core OOM score set to -1000 (immortal)");
                AtomicBool::new(true)
            }
            Err(e) => {
                warnings.push(SandboxWarning::OomScoreFailed(e.to_string()));
                AtomicBool::new(false)
            }
        };

        // #75: no CAP_NET_ADMIN / bridge/veth detection — agents are full-caged
        // by bwrap --unshare-all (needs user namespaces, checked above). The
        // daemon verifies isolation post-spawn on the sandboxed child PID.

        let enforcer = Self {
            landlock_abi,
            cgroup_root,
            cgroup_available,
            bwrap_available,
            oom_set,
            fs_mount: None,
        };

        (enforcer, warnings)
    }

    /// Sets the sentinel-fs FUSE mount path.
    ///
    /// When set, `start_agent_process()` binds `{fs_mount}/{host_agent_dir}` instead
    /// of the default `/ram/agents/{name}` as the agent's writable home.
    pub fn set_fs_mount(&mut self, path: String) {
        self.fs_mount = Some(path);
    }

    /// Creates sandbox resources for an agent (cgroup + home directory).
    ///
    /// Does NOT start a process. Call `start_agent_process()` to launch bwrap.
    /// Called by RuntimeOrchestrator::spawn_agent().
    pub fn setup_agent(&self, name: &str, limits: &CgroupLimits) -> Result<SandboxHandle> {
        let mut handle = SandboxHandle {
            agent_name: name.to_string(),
            cgroup_created: false,
            cgroup_id: None,
            io_available: false,
            bwrap_pid: None,
            landlock_applied: false,
            network_isolated: false,
        };

        // 1. Create cgroup with resource limits
        if self.cgroup_available {
            let setup = cgroups::create_cgroup(name, limits)
                .with_context(|| format!("Failed to create cgroup for agent {name}"))?;
            handle.cgroup_created = true;
            handle.cgroup_id = cgroups::cgroup_id(name);
            handle.io_available = setup.io_available;
        } else {
            warn!("Skipping cgroup creation for {name} (cgroup root not available)");
        }

        // 2. Create agent home directory (sentinel-fs Integrationspunkt)
        let home = format!("/ram/agents/{name}");
        if let Err(e) = std::fs::create_dir_all(&home) {
            warn!("Failed to create agent home {home}: {e}");
            // Non-fatal: might be on a system without /ram/agents
        }

        Ok(handle)
    }

    /// Starts a bwrap process for the agent.
    ///
    /// The bwrap process runs in isolated namespaces with Landlock FS restrictions.
    /// If Landlock is available, a wrapper binary is injected between bwrap and the
    /// agent command that applies irreversible Landlock rules before exec.
    /// Returns an [`AgentProcess`] with PID and Child handle.
    /// The Child's stdin is piped for stream-json communication.
    pub fn start_agent_process(
        &self,
        name: &str,
        fs_host_agent_dir: Option<&str>,
        command: &[String],
    ) -> Result<AgentProcess> {
        if !self.bwrap_available {
            anyhow::bail!("bwrap not available — cannot start agent process");
        }

        let mut config = BwrapConfig::for_agent(name);

        // sentinel-fs FUSE mount: replace /ram/agents/ with FUSE mount path
        if let Some(ref fs_mount) = self.fs_mount {
            config = config.with_fs_mount(fs_mount, fs_host_agent_dir.unwrap_or(name), name);
        }

        // #75: full cage is unconditional — BwrapConfig::for_agent already sets
        // share_net=false (no --share-net). The daemon verifies isolation
        // post-spawn on the sandboxed child PID.

        // Wrap command with Landlock enforcement if available
        let wrapped_command = if self.landlock_abi.is_some() {
            // Bind the wrapper binary into the namespace
            let wrapper_path = landlock_wrapper_path();
            if wrapper_path.exists() {
                config.readonly_binds.push((
                    wrapper_path.to_string_lossy().into_owned(),
                    "/landlock-wrapper".to_string(),
                ));
                let mut cmd = vec![
                    "/landlock-wrapper".to_string(),
                    name.to_string(),
                    "--".to_string(),
                ];
                cmd.extend_from_slice(command);
                info!("Landlock wrapper injected for agent {name}");
                cmd
            } else {
                warn!(
                    "landlock-wrapper binary not found at {}, skipping Landlock",
                    wrapper_path.display()
                );
                command.to_vec()
            }
        } else {
            command.to_vec()
        };

        let process = AgentProcess::from(config.spawn(&wrapped_command)?);
        let pid = process.pid;
        let child_pid = process.child_pid;

        // Add bwrap process to agent's cgroup (supervisor PID — children inherit
        // the cgroup; this is correct for cgroups, unlike netns which needs the
        // sandboxed child PID).
        if self.cgroup_available {
            if let Err(e) = cgroups::add_pid_to_cgroup(name, pid) {
                warn!("Failed to add bwrap PID {pid} to cgroup {name}: {e}");
            }
        }

        debug!(
            name,
            pid, child_pid, "bwrap process started, returning AgentProcess handle"
        );

        Ok(process)
    }

    /// Verifies that the sandboxed agent process runs in its own network
    /// namespace (#75 full cage), comparing `/proc/<child_pid>/ns/net` to the
    /// daemon's `/proc/self/ns/net`.
    ///
    /// `child_pid` MUST be the sandboxed `agent-runtime` PID (from bwrap
    /// `--info-fd`), NOT the bwrap supervisor PID — the supervisor stays in the
    /// root netns by design, so verifying it would falsely report every agent
    /// as un-caged. A transient read failure returns [`IsolationStatus::ProbeError`]
    /// and MUST NOT be treated as a cage breach; the bwrap exit code is the
    /// primary fail-closed signal.
    pub fn verify_agent_netns_isolation(&self, child_pid: u32) -> IsolationStatus {
        let daemon_ns = read_netns_inode("/proc/self/ns/net");
        let agent_ns = read_netns_inode(&format!("/proc/{child_pid}/ns/net"));
        classify_isolation(daemon_ns, agent_ns)
    }

    /// Tears down sandbox resources for an agent.
    ///
    /// Kills the bwrap process (if running) and removes the cgroup. The agent's
    /// network namespace is anonymous (bwrap --unshare-all) and is torn down by
    /// the kernel when the sandboxed process exits — no explicit netns cleanup
    /// (#75).
    /// Called by RuntimeOrchestrator::despawn_agent().
    pub fn teardown_agent(&self, handle: &SandboxHandle) -> Result<()> {
        // Ask bwrap to exit first; remaining cgroup members are handled below.
        if let Some(pid) = handle.bwrap_pid {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            wait_for_pid_exit(pid);
        }

        if handle.cgroup_created {
            cleanup_cgroup_after_process_exit(&handle.agent_name)
                .with_context(|| format!("remove sandbox cgroup for {}", handle.agent_name))?;
        }

        Ok(())
    }

    /// Reconcile setup that failed before a [`SandboxHandle`] could be
    /// returned. This is intentionally limited to the deterministic cgroup
    /// path; workload home ownership remains guarded by the NanoRuntime marker.
    pub fn recover_partial_agent_setup(&self, agent_name: &str) -> Result<()> {
        if std::path::Path::new(&cgroups::cgroup_path(agent_name)).exists() {
            cleanup_cgroup_after_process_exit(agent_name)
                .with_context(|| format!("recover partial sandbox setup for {agent_name}"))?;
        }
        Ok(())
    }

    /// Reads PSI metrics for an agent's cgroup.
    ///
    /// Used for Zenoh publish -> Bio-Engine pipeline.
    /// resource: "cpu" or "memory"
    pub fn read_agent_psi(&self, name: &str, resource: &str) -> Result<PsiMetrics> {
        cgroups::read_psi_from_cgroup(name, resource)
    }

    /// Whether Landlock is available on this system.
    pub fn has_landlock(&self) -> bool {
        self.landlock_abi.is_some()
    }

    /// Detected Landlock ABI version.
    pub fn landlock_abi(&self) -> Option<u8> {
        self.landlock_abi
    }

    /// Whether cgroup v2 is available for agent isolation.
    pub fn has_cgroups(&self) -> bool {
        self.cgroup_available
    }

    /// Whether bwrap with user namespaces is available.
    pub fn has_bwrap(&self) -> bool {
        self.bwrap_available
    }

    /// Whether OOM score was set for the ECS core process.
    pub fn oom_score_set(&self) -> bool {
        self.oom_set.load(Ordering::Relaxed)
    }
}

fn protocol_reader_parts(
    stdout: Option<std::process::ChildStdout>,
) -> (
    Option<Receiver<ProtocolFrame>>,
    Option<std::thread::JoinHandle<()>>,
) {
    match stdout {
        Some(stdout) => {
            let (receiver, reader) = protocol_line_receiver(stdout);
            (Some(receiver), Some(reader))
        }
        None => (None, None),
    }
}

fn protocol_line_receiver(
    stdout: std::process::ChildStdout,
) -> (Receiver<ProtocolFrame>, std::thread::JoinHandle<()>) {
    let (sender, receiver) = mpsc::sync_channel(PROTOCOL_QUEUE_DEPTH);
    let reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut bytes = Vec::new();
            let mut rejected = false;
            let mut reached_eof = false;
            loop {
                let available = match reader.fill_buf() {
                    Ok(available) => available,
                    Err(_) => {
                        rejected = true;
                        reached_eof = true;
                        &[]
                    }
                };
                if available.is_empty() {
                    reached_eof = true;
                    break;
                }
                let newline = available.iter().position(|byte| *byte == b'\n');
                let consumed = newline.map_or(available.len(), |index| index + 1);
                let payload = if newline.is_some() {
                    &available[..consumed - 1]
                } else {
                    &available[..consumed]
                };
                if bytes.len().saturating_add(payload.len()) > PROTOCOL_LINE_LIMIT_BYTES {
                    rejected = true;
                } else if !rejected {
                    bytes.extend_from_slice(payload);
                }
                reader.consume(consumed);
                if newline.is_some() {
                    break;
                }
            }
            if reached_eof && bytes.is_empty() && !rejected {
                break;
            }
            let frame = if rejected {
                ProtocolFrame::Rejected
            } else {
                match String::from_utf8(bytes) {
                    Ok(line) => ProtocolFrame::Line(line),
                    Err(_) => ProtocolFrame::Rejected,
                }
            };
            if sender.send(frame).is_err() || reached_eof {
                break;
            }
        }
    });
    (receiver, reader)
}

/// Returns the expected path for the landlock-wrapper binary.
///
/// Checks (in order): next to current executable, /opt/sentinel/bin/, /usr/local/bin/.
fn landlock_wrapper_path() -> PathBuf {
    // 1. Same directory as current executable
    if let Ok(exe) = std::env::current_exe() {
        let candidate = exe
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("landlock-wrapper");
        if candidate.exists() {
            return candidate;
        }
    }

    // 2. Standard deployment path
    let deploy = PathBuf::from("/opt/sentinel/bin/landlock-wrapper");
    if deploy.exists() {
        return deploy;
    }

    // 3. System path (fallback)
    PathBuf::from("/usr/local/bin/landlock-wrapper")
}

/// Reads the network-namespace inode behind `/proc/<pid>/ns/net`.
///
/// The symlink target has the form `net:[INODE]`; the inode uniquely
/// identifies the namespace. Returns `None` if the link cannot be read or
/// parsed (transient race / process already gone).
fn read_netns_inode(ns_path: &str) -> Option<u64> {
    let target = std::fs::read_link(ns_path).ok()?;
    parse_ns_inode(&target.to_string_lossy())
}

/// Parses the inode out of a `net:[INODE]` namespace link target.
fn parse_ns_inode(link: &str) -> Option<u64> {
    let start = link.find('[')? + 1;
    let end = link.find(']')?;
    link.get(start..end)?.parse().ok()
}

/// Classifies isolation from the daemon's and the agent's netns inodes.
///
/// Pure decision function (unit-tested): different inodes -> [`IsolationStatus::Isolated`];
/// equal inodes -> [`IsolationStatus::NotIsolated`] (agent shares the daemon netns);
/// any missing inode -> [`IsolationStatus::ProbeError`] (never a cage breach).
fn classify_isolation(daemon_ns: Option<u64>, agent_ns: Option<u64>) -> IsolationStatus {
    match (daemon_ns, agent_ns) {
        (Some(d), Some(a)) if d == a => IsolationStatus::NotIsolated,
        (Some(_), Some(_)) => IsolationStatus::Isolated,
        _ => IsolationStatus::ProbeError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_warning_variants() {
        // Verify all warning variants exist and are distinct
        let warnings = [
            SandboxWarning::LandlockNotAvailable,
            SandboxWarning::CgroupNotDelegated("io".into()),
            SandboxWarning::BwrapUsernsDenied,
            SandboxWarning::IoNotDelegated,
            SandboxWarning::OomScoreFailed("test".into()),
        ];
        assert_eq!(warnings.len(), 5);
        assert_ne!(warnings[0], warnings[1]);
    }

    #[test]
    fn sandbox_handle_defaults() {
        let handle = SandboxHandle {
            agent_name: "test".into(),
            cgroup_created: false,
            cgroup_id: None,
            io_available: false,
            bwrap_pid: None,
            landlock_applied: false,
            network_isolated: false,
        };
        assert_eq!(handle.agent_name, "test");
        assert!(!handle.cgroup_created);
        assert!(handle.bwrap_pid.is_none());
        assert!(!handle.network_isolated);
    }

    #[test]
    fn classify_isolation_three_states() {
        // #75: different inodes -> isolated; equal -> not isolated (cage breach);
        // missing inode -> probe error (must never terminate a healthy agent).
        assert_eq!(
            classify_isolation(Some(4026531840), Some(4026532500)),
            IsolationStatus::Isolated
        );
        assert_eq!(
            classify_isolation(Some(4026531840), Some(4026531840)),
            IsolationStatus::NotIsolated
        );
        assert_eq!(
            classify_isolation(None, Some(4026532500)),
            IsolationStatus::ProbeError
        );
        assert_eq!(
            classify_isolation(Some(4026531840), None),
            IsolationStatus::ProbeError
        );
        assert_eq!(classify_isolation(None, None), IsolationStatus::ProbeError);
    }

    #[test]
    fn parse_ns_inode_extracts_inode() {
        assert_eq!(parse_ns_inode("net:[4026531840]"), Some(4026531840));
        assert_eq!(parse_ns_inode("net:[1]"), Some(1));
        assert_eq!(parse_ns_inode("garbage"), None);
        assert_eq!(parse_ns_inode("net:[notnum]"), None);
        assert_eq!(parse_ns_inode("net:[]"), None);
    }

    #[test]
    fn teardown_cgroup_kills_members_before_remove() {
        let calls = std::cell::RefCell::new(Vec::new());

        cleanup_cgroup_after_process_exit_with(
            "agent",
            |_| {
                calls.borrow_mut().push("list");
                Ok(vec![42])
            },
            |_| {
                calls.borrow_mut().push("kill");
                Ok(1)
            },
            |_| {
                let killed = calls.borrow().contains(&"kill");
                assert!(killed, "occupied cgroup must be killed before remove");
                calls.borrow_mut().push("remove");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(calls.into_inner(), vec!["list", "kill", "remove"]);
    }

    #[test]
    fn teardown_cgroup_removes_empty_without_kill() {
        let calls = std::cell::RefCell::new(Vec::new());

        cleanup_cgroup_after_process_exit_with(
            "agent",
            |_| {
                calls.borrow_mut().push("list");
                Ok(Vec::new())
            },
            |_| {
                calls.borrow_mut().push("kill");
                Ok(0)
            },
            |_| {
                calls.borrow_mut().push("remove");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(calls.into_inner(), vec!["list", "remove"]);
    }

    #[test]
    #[ignore] // Needs real system capabilities (VM only)
    fn enforcer_detect() {
        let (enforcer, warnings) = SandboxEnforcer::detect();
        // On the VM, we expect landlock + cgroups + bwrap to be available
        println!("Landlock ABI: {:?}", enforcer.landlock_abi);
        println!("cgroup available: {}", enforcer.cgroup_available);
        println!("bwrap available: {}", enforcer.bwrap_available);
        println!("Warnings: {:?}", warnings);
    }
}
