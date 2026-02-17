//! Bubblewrap sandbox configuration.

use std::process::{Child, Command};

use anyhow::{Context, Result};
use tracing::info;

/// Bubblewrap sandbox configuration fuer einen einzelnen Agenten.
#[derive(Debug, Clone)]
pub struct BwrapConfig {
    pub hostname: String,
    pub readonly_binds: Vec<(String, String)>, // (host, guest)
    pub writable_binds: Vec<(String, String)>, // (host, guest)
    pub tmpfs: Vec<String>,
    pub share_net: bool,
    pub die_with_parent: bool,
}

impl BwrapConfig {
    /// Standard-Sandbox-Config fuer einen Agenten.
    pub fn for_agent(name: &str) -> Self {
        Self {
            hostname: format!("sentinel-{name}"),
            readonly_binds: vec![("/work/company".to_string(), "/company".to_string())],
            writable_binds: vec![(format!("/ram/agents/{name}"), format!("/home/{name}"))],
            tmpfs: vec!["/tmp".to_string()],
            share_net: false,
            die_with_parent: true,
        }
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
        let mut args = self.to_args();
        args.extend(command.iter().cloned());

        info!(
            "Spawning bwrap: {} args, command: {:?}",
            args.len(),
            command
        );

        Command::new("bwrap")
            .args(&args)
            .spawn()
            .context("Failed to spawn bwrap process")
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
    }

    #[test]
    fn readonly_binds() {
        let config = BwrapConfig::for_agent("test");
        let args = config.to_args();
        assert!(args.contains(&"--ro-bind".to_string()));
        assert!(args.contains(&"/work/company".to_string()));
        assert!(args.contains(&"/company".to_string()));
    }

    #[test]
    fn writable_binds() {
        let config = BwrapConfig::for_agent("test");
        let args = config.to_args();
        assert!(args.contains(&"--bind".to_string()));
        assert!(args.contains(&"/ram/agents/test".to_string()));
        assert!(args.contains(&"/home/test".to_string()));
    }

    #[test]
    fn default_network_isolated() {
        let config = BwrapConfig::for_agent("test");
        assert!(!config.share_net, "Default should be network-isolated");
        let args = config.to_args();
        assert!(!args.contains(&"--share-net".to_string()));
        assert!(args.contains(&"--unshare-all".to_string()));
    }

    #[test]
    fn with_shared_net_fallback() {
        let config = BwrapConfig::for_agent("test").with_shared_net();
        assert!(config.share_net);
        let args = config.to_args();
        assert!(args.contains(&"--share-net".to_string()));
    }
}
