//! Bubblewrap sandbox configuration.

use std::fs::File;
use std::io::Read;
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};

use anyhow::{Context, Result};
use tracing::{info, warn};

/// File descriptor bwrap writes its sandbox info JSON to (`--info-fd`).
const INFO_FD: RawFd = 3;

/// Bounded wait for bwrap to report the sandboxed child PID, so a stuck bwrap
/// cannot block the daemon's spawn path indefinitely (bwrap sets up in <10ms).
const INFO_FD_TIMEOUT_MS: libc::c_int = 5000;

/// Result of spawning a bwrap sandbox.
///
/// `child` is the bwrap **supervisor** process (host/root netns by design,
/// used for cgroup membership and SIGTERM). `child_pid` is the **sandboxed**
/// process inside the agent's network namespace, reported by bwrap via
/// `--info-fd`; this is the PID to verify isolation against (#75). `None` if
/// bwrap did not report it (then netns verification is skipped, but the bwrap
/// exit code remains the primary fail-closed signal).
#[derive(Debug)]
pub struct SpawnedSandbox {
    pub child: Child,
    pub child_pid: Option<u32>,
}

/// Bubblewrap sandbox configuration fuer einen einzelnen Agenten.
#[derive(Debug, Clone)]
pub struct BwrapConfig {
    pub hostname: String,
    pub readonly_binds: Vec<(String, String)>, // (host, guest)
    pub writable_binds: Vec<(String, String)>, // (host, guest)
    pub tmpfs: Vec<String>,
    pub share_net: bool,
    pub die_with_parent: bool,
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
            tmpfs: vec!["/tmp".to_string()],
            // #75 full cage: agents make NO network calls (agent-runtime has no
            // network code); the daemon proxies all LLM traffic to the Cortex
            // Gateway on the host. No --share-net -> own netns, loopback only.
            share_net: false,
            die_with_parent: true,
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
    /// the sandboxed child PID (from bwrap `--info-fd`). The caller manages the
    /// child (cgroup membership uses the supervisor PID; netns isolation must
    /// be verified against `child_pid`, since the supervisor stays in the root
    /// netns by design — #75).
    pub fn spawn(&self, command: &[String]) -> Result<SpawnedSandbox> {
        let config = self.with_existing_host_binds();
        let mut args = config.to_args();
        // bwrap writes `{"child-pid": N, ...}` to --info-fd once the sandbox is
        // set up. Options must precede the command.
        args.push("--info-fd".to_string());
        args.push(INFO_FD.to_string());
        args.extend(command.iter().cloned());

        info!(
            "Spawning bwrap: {} args, command: {:?}",
            args.len(),
            command
        );

        // Pipe for bwrap's --info-fd. Both ends CLOEXEC; the write end is
        // re-published at INFO_FD in the child via pre_exec (clearing CLOEXEC
        // on that descriptor only).
        let mut fds: [libc::c_int; 2] = [0; 2];
        // SAFETY: `fds` is a valid 2-element array that pipe2 fills.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("Failed to create bwrap --info-fd pipe");
        }
        let (read_fd, write_fd) = (fds[0], fds[1]);

        let mut cmd = Command::new("bwrap");
        cmd.args(&args).stdin(std::process::Stdio::piped());
        // SAFETY: the closure runs in the forked child before exec and only
        // calls async-signal-safe fcntl/dup2 on a captured raw fd.
        unsafe {
            cmd.pre_exec(move || {
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
                Ok(())
            });
        }

        let spawn_result = cmd.spawn();
        // Parent never writes to the info pipe.
        // SAFETY: write_fd is a valid fd owned by this function until here.
        unsafe { libc::close(write_fd) };

        let child = match spawn_result {
            Ok(child) => child,
            Err(e) => {
                // SAFETY: read_fd is still open and owned here.
                unsafe { libc::close(read_fd) };
                return Err(e).context("Failed to spawn bwrap process");
            }
        };

        // Takes ownership of read_fd and closes it.
        let child_pid = read_child_pid_from_info_fd(read_fd, INFO_FD_TIMEOUT_MS);
        if child_pid.is_none() {
            warn!(
                "bwrap did not report a sandboxed child PID via --info-fd; \
                 netns isolation verification will be skipped for this agent"
            );
        }

        Ok(SpawnedSandbox { child, child_pid })
    }

    fn with_existing_host_binds(&self) -> Self {
        let mut config = self.clone();
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
        config
    }

    /// Generiert bwrap CLI-Argumente.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = vec!["--unshare-all".to_string()];

        if self.share_net {
            args.push("--share-net".to_string());
        }

        if self.die_with_parent {
            args.push("--die-with-parent".to_string());
        }

        args.push("--hostname".to_string());
        args.push(self.hostname.clone());

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

/// Reads the sandboxed child PID from bwrap's `--info-fd` pipe.
///
/// Polls (bounded by `timeout_ms`) for bwrap to write its one-shot info JSON,
/// then does a single read (the JSON is one small `write()`, well under
/// `PIPE_BUF`). Takes ownership of `read_fd` and closes it. Returns `None` on
/// timeout, read error, or if bwrap exited without reporting (fail-closed: the
/// caller treats a missing PID by skipping verification, while the bwrap exit
/// code stays the primary signal).
fn read_child_pid_from_info_fd(read_fd: RawFd, timeout_ms: libc::c_int) -> Option<u32> {
    let mut pfd = libc::pollfd {
        fd: read_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: single valid pollfd; poll does not retain the pointer.
    let prc = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    if prc <= 0 {
        // SAFETY: read_fd is still open and owned here.
        unsafe { libc::close(read_fd) };
        return None;
    }

    // SAFETY: read_fd is a valid open fd; File takes ownership and closes it.
    let mut file = unsafe { File::from_raw_fd(read_fd) };
    let mut buf = [0u8; 1024];
    let n = file.read(&mut buf).ok()?;
    if n == 0 {
        return None;
    }
    let text = std::str::from_utf8(&buf[..n]).ok()?;
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

    #[test]
    fn bwrap_command_structure() {
        let config = BwrapConfig::for_agent("test");
        let args = config.to_args();
        assert!(args.contains(&"--unshare-all".to_string()));
        assert!(args.contains(&"--die-with-parent".to_string()));
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
