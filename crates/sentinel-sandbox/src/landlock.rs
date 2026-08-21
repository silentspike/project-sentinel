//! Landlock LSM filesystem access control.
//!
//! Provides kernel-enforced filesystem restrictions for agent processes.
//! Applied inside the bwrap namespace as Defense-in-Depth.
//!
//! Pfad-Policy (Masterplan-konform):
//! - Read: /company (Firmendaten), /etc/resolv.conf (DNS), Runtime-Libs
//! - Write: /home/{name} (Agent-Home), /tmp (Temp)
//! - Execute: nur explizit freigegebene Binaries

use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing::{info, warn};

/// ABI whose filesystem rights are requested by the current Sentinel ruleset.
pub const LANDLOCK_RULESET_ABI: u8 = 4;

/// Kernel result returned after the irreversible `restrict_self` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandlockEnforcement {
    FullyEnforced { abi: u8 },
    PartiallyEnforced,
    NotEnforced,
}

fn compatibility_level(require_full_enforcement: bool) -> landlock::CompatLevel {
    if require_full_enforcement {
        landlock::CompatLevel::HardRequirement
    } else {
        landlock::CompatLevel::BestEffort
    }
}

/// Returns the measured ABI only when the irreversible ruleset result exactly
/// satisfies the workbench contract.
pub fn workbench_fully_enforced_abi(
    enforcement: LandlockEnforcement,
    expected_abi: u8,
) -> Option<u8> {
    match enforcement {
        LandlockEnforcement::FullyEnforced { abi } if abi == expected_abi => Some(abi),
        _ => None,
    }
}

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
    fn default_exec_paths() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/usr/bin/agent-runtime"),
            // M0 web-authoring profile: syntax validation only; arguments are
            // separately constrained by the digest-bound command policy.
            PathBuf::from("/usr/bin/node"),
            PathBuf::from("/breakout-helper"),
            // Dynamically linked ELF binaries also need their loader executable.
            PathBuf::from("/lib64/ld-linux-x86-64.so.2"),
            PathBuf::from("/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"),
        ]
    }

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
            read_paths: vec![
                PathBuf::from("/company"),
                PathBuf::from("/etc/resolv.conf"),
                PathBuf::from("/usr"),
                PathBuf::from("/lib"),
                PathBuf::from("/lib64"),
                // bwrap creates a private PID namespace; this read grant is
                // required for bounded process-group resource accounting and
                // cannot expose host process metadata.
                PathBuf::from("/proc"),
            ],
            write_paths: vec![
                PathBuf::from(format!("/home/{name}")),
                PathBuf::from("/workspace"),
                PathBuf::from("/artifacts"),
                PathBuf::from("/tmp"),
                // Command stdio uses Stdio::null after Landlock is active.
                PathBuf::from("/dev/null"),
            ],
            exec_paths: Self::default_exec_paths(),
        }
    }

    /// Allows the concrete workload entrypoint in addition to the static runtime binaries.
    pub fn with_entrypoint_exec(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if !self.exec_paths.iter().any(|existing| existing == &path) {
            self.exec_paths.push(path);
        }
        self
    }

    /// Applies the Landlock ruleset to the current process (irreversible).
    ///
    /// Must be called in the bwrap child process BEFORE exec'ing the agent.
    /// Returns true if fully or partially enforced, false if not enforced.
    pub fn apply(&self) -> Result<bool> {
        Ok(matches!(
            self.apply_status()?,
            LandlockEnforcement::FullyEnforced { .. } | LandlockEnforcement::PartiallyEnforced
        ))
    }

    /// Applies the ruleset and returns the actual irreversible enforcement
    /// result. Workbench callers must require `FullyEnforced`; the general
    /// agent path may retain its existing best-effort policy.
    pub fn apply_status(&self) -> Result<LandlockEnforcement> {
        self.apply_status_with_compatibility(false)
    }

    /// Applies the ruleset only when every requested Landlock feature is
    /// supported. Workbench callers use this fail-closed path because a
    /// partially enforced sandbox cannot satisfy the execution contract.
    pub fn apply_required_status(&self) -> Result<LandlockEnforcement> {
        self.apply_status_with_compatibility(true)
    }

    fn apply_status_with_compatibility(
        &self,
        require_full_enforcement: bool,
    ) -> Result<LandlockEnforcement> {
        use landlock::*;

        let abi = ABI::V4;

        // Handle all FS access types — restricts everything not explicitly allowed
        let all_access = AccessFs::from_all(abi);
        let read_access = AccessFs::ReadFile | AccessFs::ReadDir;
        let exec_access = read_access | AccessFs::Execute;
        let mut write_access = all_access;
        write_access.remove(AccessFs::Execute);

        let mut ruleset = Ruleset::default()
            .set_compatibility(compatibility_level(require_full_enforcement))
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
                    .add_rule(PathBeneath::new(PathFd::new(path)?, write_access))
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
                Ok(LandlockEnforcement::FullyEnforced {
                    abi: LANDLOCK_RULESET_ABI,
                })
            }
            RulesetStatus::PartiallyEnforced => {
                warn!("Landlock partially enforced (kernel ABI may be older)");
                Ok(LandlockEnforcement::PartiallyEnforced)
            }
            RulesetStatus::NotEnforced => {
                warn!("Landlock not enforced");
                Ok(LandlockEnforcement::NotEnforced)
            }
        }
    }
}

/// Detects whether Landlock is available on this kernel.
/// Returns the ABI version (1-4) or None if not available.
///
/// Detection order:
/// 1. `/sys/kernel/security/landlock` (securityfs, kernel < 6.11)
/// 2. `/sys/kernel/security/lsm` contains "landlock" (kernel 6.11+ removed securityfs dir)
pub fn detect_abi() -> Option<u8> {
    // Kernel < 6.11: securityfs exposes landlock directory.
    if std::path::Path::new("/sys/kernel/security/landlock").exists() {
        return Some(4);
    }

    // Kernel 6.11+: securityfs directory removed, check LSM list instead.
    if let Ok(lsm) = std::fs::read_to_string("/sys/kernel/security/lsm") {
        if lsm.contains("landlock") {
            return Some(4);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruleset_for_agent_paths() {
        let rs = LandlockRuleset::for_agent("thomas");
        assert!(rs.read_paths.contains(&PathBuf::from("/company")));
        assert!(rs.read_paths.contains(&PathBuf::from("/etc/resolv.conf")));
        assert!(rs.read_paths.contains(&PathBuf::from("/usr")));
        assert!(rs.read_paths.contains(&PathBuf::from("/proc")));
        assert!(rs.read_paths.contains(&PathBuf::from("/lib")));
        assert!(rs.read_paths.contains(&PathBuf::from("/lib64")));
        assert!(rs.write_paths.contains(&PathBuf::from("/home/thomas")));
        assert!(rs.write_paths.contains(&PathBuf::from("/workspace")));
        assert!(rs.write_paths.contains(&PathBuf::from("/artifacts")));
        assert!(rs.write_paths.contains(&PathBuf::from("/tmp")));
        assert!(rs.write_paths.contains(&PathBuf::from("/dev/null")));
        assert!(rs
            .exec_paths
            .contains(&PathBuf::from("/usr/bin/agent-runtime")));
        assert!(rs.exec_paths.contains(&PathBuf::from("/usr/bin/node")));
        assert!(rs.exec_paths.contains(&PathBuf::from("/breakout-helper")));
        assert!(rs
            .exec_paths
            .contains(&PathBuf::from("/lib64/ld-linux-x86-64.so.2")));
        assert!(!rs.exec_paths.contains(&PathBuf::from("/usr")));
    }

    #[test]
    fn ruleset_no_root_write() {
        let rs = LandlockRuleset::for_agent("thomas");
        assert!(!rs.write_paths.contains(&PathBuf::from("/")));
        assert!(!rs.write_paths.contains(&PathBuf::from("/etc")));
        assert!(!rs.write_paths.contains(&PathBuf::from("/company")));
    }

    #[test]
    fn ruleset_allows_explicit_entrypoint_once() {
        let rs = LandlockRuleset::for_agent("thomas")
            .with_entrypoint_exec("/usr/bin/sleep")
            .with_entrypoint_exec("/usr/bin/sleep");

        let occurrences = rs
            .exec_paths
            .iter()
            .filter(|path| *path == &PathBuf::from("/usr/bin/sleep"))
            .count();
        assert_eq!(occurrences, 1);
    }

    #[test]
    fn different_agents_different_homes() {
        let rs1 = LandlockRuleset::for_agent("thomas");
        let rs2 = LandlockRuleset::for_agent("lisa");
        assert!(rs1.write_paths.contains(&PathBuf::from("/home/thomas")));
        assert!(rs2.write_paths.contains(&PathBuf::from("/home/lisa")));
        assert!(!rs1.write_paths.contains(&PathBuf::from("/home/lisa")));
    }

    #[test]
    fn workbench_rejects_partial_missing_and_mismatched_enforcement() {
        assert_eq!(
            workbench_fully_enforced_abi(
                LandlockEnforcement::FullyEnforced {
                    abi: LANDLOCK_RULESET_ABI,
                },
                LANDLOCK_RULESET_ABI,
            ),
            Some(LANDLOCK_RULESET_ABI)
        );
        assert_eq!(
            workbench_fully_enforced_abi(
                LandlockEnforcement::FullyEnforced {
                    abi: LANDLOCK_RULESET_ABI,
                },
                LANDLOCK_RULESET_ABI - 1,
            ),
            None
        );
        assert_eq!(
            workbench_fully_enforced_abi(
                LandlockEnforcement::PartiallyEnforced,
                LANDLOCK_RULESET_ABI,
            ),
            None
        );
        assert_eq!(
            workbench_fully_enforced_abi(LandlockEnforcement::NotEnforced, LANDLOCK_RULESET_ABI,),
            None
        );
    }

    #[test]
    fn workbench_and_general_paths_select_distinct_compatibility_contracts() {
        use landlock::CompatLevel;

        assert_eq!(compatibility_level(false), CompatLevel::BestEffort);
        assert_eq!(compatibility_level(true), CompatLevel::HardRequirement);
    }
}
