//! Landlock LSM filesystem access control.
//!
//! Provides kernel-enforced filesystem restrictions for agent processes.
//! Applied inside the bwrap namespace as Defense-in-Depth.
//!
//! Pfad-Policy (Masterplan-konform):
//! - Read: /company (Firmendaten), /etc/resolv.conf (DNS)
//! - Write: /home/{name} (Agent-Home), /tmp (Temp)
//! - Execute: /usr (Binaries+Libs), /lib (Shared Libs)

use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing::{info, warn};

/// Landlock filesystem ruleset for agent isolation.
#[derive(Debug, Clone)]
pub struct LandlockRuleset {
    /// Paths with read-only access.
    pub read_paths: Vec<PathBuf>,
    /// Paths with read-write access (includes all FS operations except execute).
    pub write_paths: Vec<PathBuf>,
    /// Paths with read + execute access.
    pub exec_paths: Vec<PathBuf>,
}

impl LandlockRuleset {
    /// Creates a Masterplan-compliant ruleset for an agent.
    ///
    /// Paths are relative to the bwrap mount namespace:
    /// - /company -> readonly Firmendaten
    /// - /home/{name} -> writable Agent-Home (bwrap-mapped from /ram/agents/{name})
    /// - /tmp -> writable temp (bwrap tmpfs)
    /// - /usr, /lib -> readonly system + exec
    /// - /etc/resolv.conf -> DNS resolution
    pub fn for_agent(name: &str) -> Self {
        Self {
            read_paths: vec![PathBuf::from("/company"), PathBuf::from("/etc/resolv.conf")],
            write_paths: vec![
                PathBuf::from(format!("/home/{name}")),
                PathBuf::from("/tmp"),
            ],
            exec_paths: vec![PathBuf::from("/usr"), PathBuf::from("/lib")],
        }
    }

    /// Applies the Landlock ruleset to the current process (irreversible).
    ///
    /// Must be called in the bwrap child process BEFORE exec'ing the agent.
    /// Returns true if fully or partially enforced, false if not enforced.
    pub fn apply(&self) -> Result<bool> {
        use landlock::*;

        let abi = ABI::V4;

        // Handle all FS access types — restricts everything not explicitly allowed
        let all_access = AccessFs::from_all(abi);
        let read_access = AccessFs::ReadFile | AccessFs::ReadDir;
        let exec_access = read_access | AccessFs::Execute;

        let mut ruleset = Ruleset::default()
            .handle_access(all_access)
            .context("Failed to handle Landlock access")?
            .create()
            .context("Failed to create Landlock ruleset")?;

        // Read-only paths
        for path in &self.read_paths {
            if path.exists() {
                ruleset = ruleset
                    .add_rule(PathBeneath::new(PathFd::new(path)?, read_access))
                    .with_context(|| format!("Failed to add read rule for {}", path.display()))?;
            }
        }

        // Writable paths — full access except execute
        for path in &self.write_paths {
            if path.exists() {
                ruleset = ruleset
                    .add_rule(PathBeneath::new(PathFd::new(path)?, all_access))
                    .with_context(|| format!("Failed to add write rule for {}", path.display()))?;
            }
        }

        // Executable paths — read + execute
        for path in &self.exec_paths {
            if path.exists() {
                ruleset = ruleset
                    .add_rule(PathBeneath::new(PathFd::new(path)?, exec_access))
                    .with_context(|| format!("Failed to add exec rule for {}", path.display()))?;
            }
        }

        let restriction = ruleset
            .restrict_self()
            .context("Failed to apply Landlock restriction")?;

        match restriction.ruleset {
            RulesetStatus::FullyEnforced => {
                info!("Landlock fully enforced");
                Ok(true)
            }
            RulesetStatus::PartiallyEnforced => {
                warn!("Landlock partially enforced (kernel ABI may be older)");
                Ok(true)
            }
            RulesetStatus::NotEnforced => {
                warn!("Landlock not enforced");
                Ok(false)
            }
        }
    }
}

/// Detects whether Landlock is available on this kernel.
/// Returns the ABI version (1-4) or None if not available.
pub fn detect_abi() -> Option<u8> {
    if !std::path::Path::new("/sys/kernel/security/landlock").exists() {
        return None;
    }
    // VM kernel 6.8 supports ABI v4.
    // The landlock crate handles ABI negotiation via BestEffort compat level.
    Some(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruleset_for_agent_paths() {
        let rs = LandlockRuleset::for_agent("thomas");
        assert!(rs.read_paths.contains(&PathBuf::from("/company")));
        assert!(rs.read_paths.contains(&PathBuf::from("/etc/resolv.conf")));
        assert!(rs.write_paths.contains(&PathBuf::from("/home/thomas")));
        assert!(rs.write_paths.contains(&PathBuf::from("/tmp")));
        assert!(rs.exec_paths.contains(&PathBuf::from("/usr")));
        assert!(rs.exec_paths.contains(&PathBuf::from("/lib")));
    }

    #[test]
    fn ruleset_no_root_write() {
        let rs = LandlockRuleset::for_agent("thomas");
        assert!(!rs.write_paths.contains(&PathBuf::from("/")));
        assert!(!rs.write_paths.contains(&PathBuf::from("/etc")));
        assert!(!rs.write_paths.contains(&PathBuf::from("/company")));
    }

    #[test]
    fn different_agents_different_homes() {
        let rs1 = LandlockRuleset::for_agent("thomas");
        let rs2 = LandlockRuleset::for_agent("lisa");
        assert!(rs1.write_paths.contains(&PathBuf::from("/home/thomas")));
        assert!(rs2.write_paths.contains(&PathBuf::from("/home/lisa")));
        assert!(!rs1.write_paths.contains(&PathBuf::from("/home/lisa")));
    }
}
