//! Bubblewrap sandbox configuration.

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
            share_net: true,
            die_with_parent: true,
        }
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
    fn no_network() {
        let mut config = BwrapConfig::for_agent("test");
        config.share_net = false;
        let args = config.to_args();
        assert!(!args.contains(&"--share-net".to_string()));
        assert!(args.contains(&"--unshare-all".to_string()));
    }
}
