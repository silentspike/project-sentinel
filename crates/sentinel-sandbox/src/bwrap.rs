//! Bubblewrap sandbox configuration.

use std::path::Path;
use std::process::{Child, Command};

use anyhow::{Context, Result};
use tracing::{info, warn};

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
    /// - Shared Network fuer Cortex Gateway API-Zugang
    ///
    /// Landlock (Defense-in-Depth) schraenkt Zugriff innerhalb des Namespace weiter ein.
    pub fn for_agent(name: &str) -> Self {
        Self {
            hostname: format!("sentinel-{name}"),
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
            // TOGAF: --share-net (Agent braucht Cortex Gateway API-Zugang)
            share_net: true,
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

    /// Returns a config with shared network (fallback when netns is not available).
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
    /// Returns the Child handle. Caller is responsible for managing the child
    /// (e.g. adding to cgroup, forgetting handle for --die-with-parent).
    pub fn spawn(&self, command: &[String]) -> Result<Child> {
        let config = self.with_existing_host_binds();
        let mut args = config.to_args();
        args.extend(command.iter().cloned());

        info!(
            "Spawning bwrap: {} args, command: {:?}",
            args.len(),
            command
        );

        Command::new("bwrap")
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .context("Failed to spawn bwrap process")
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
    fn togaf_shared_net_default() {
        // TOGAF: --share-net (Agent braucht Cortex Gateway API-Zugang)
        let config = BwrapConfig::for_agent("test");
        assert!(config.share_net, "TOGAF: Default should be --share-net");
        let args = config.to_args();
        assert!(args.contains(&"--share-net".to_string()));
        assert!(args.contains(&"--unshare-all".to_string()));
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
