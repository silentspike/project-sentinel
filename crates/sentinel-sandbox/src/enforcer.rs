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
//! 4. `teardown_agent()` — killt bwrap + entfernt cgroup

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::bwrap::BwrapConfig;
use crate::cgroups::{self, CgroupLimits, PsiMetrics};
use crate::landlock;

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

/// Handle returned by setup_agent() — tracks what was created.
#[derive(Debug, Clone)]
pub struct SandboxHandle {
    pub agent_name: String,
    pub cgroup_created: bool,
    pub io_available: bool,
    pub bwrap_pid: Option<u32>,
    pub landlock_applied: bool,
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

        // 2. cgroup root
        let cgroup_root = PathBuf::from("/sys/fs/cgroup/sentinel");
        let cgroup_available = if cgroup_root.exists() {
            true
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

        // 3. IO controller check
        let io_delegated = std::fs::read_to_string("/sys/fs/cgroup/cgroup.subtree_control")
            .map(|s| s.contains("io"))
            .unwrap_or(false);
        if !io_delegated {
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

        let enforcer = Self {
            landlock_abi,
            cgroup_root,
            cgroup_available,
            bwrap_available,
            oom_set,
        };

        (enforcer, warnings)
    }

    /// Creates sandbox resources for an agent (cgroup + home directory).
    ///
    /// Does NOT start a process. Call `start_agent_process()` to launch bwrap.
    /// Called by RuntimeOrchestrator::spawn_agent().
    pub fn setup_agent(&self, name: &str, limits: &CgroupLimits) -> Result<SandboxHandle> {
        let mut handle = SandboxHandle {
            agent_name: name.to_string(),
            cgroup_created: false,
            io_available: false,
            bwrap_pid: None,
            landlock_applied: false,
        };

        // 1. Create cgroup with resource limits
        if self.cgroup_available {
            let setup = cgroups::create_cgroup(name, limits)
                .with_context(|| format!("Failed to create cgroup for agent {name}"))?;
            handle.cgroup_created = true;
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
    /// The bwrap process runs in isolated namespaces.
    /// Landlock is applied inside the bwrap child (not yet implemented — TODO).
    /// Returns the bwrap PID on success.
    pub fn start_agent_process(
        &self,
        name: &str,
        command: &[String],
    ) -> Result<u32> {
        if !self.bwrap_available {
            anyhow::bail!("bwrap not available — cannot start agent process");
        }

        let config = BwrapConfig::for_agent(name);
        let child = config.spawn(command)?;
        let pid = child.id();

        // Add bwrap process to agent's cgroup
        if self.cgroup_available {
            if let Err(e) = cgroups::add_pid_to_cgroup(name, pid) {
                warn!("Failed to add bwrap PID {pid} to cgroup {name}: {e}");
            }
        }

        // Release child handle — bwrap has --die-with-parent
        std::mem::forget(child);

        Ok(pid)
    }

    /// Tears down sandbox resources for an agent.
    ///
    /// Kills the bwrap process (if running) and removes the cgroup.
    /// Called by RuntimeOrchestrator::despawn_agent().
    pub fn teardown_agent(&self, handle: &SandboxHandle) -> Result<()> {
        // Kill bwrap process if running
        if let Some(pid) = handle.bwrap_pid {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
        }

        // Remove cgroup (may fail if processes still in it)
        if handle.cgroup_created {
            if let Err(e) = cgroups::remove_cgroup(&handle.agent_name) {
                warn!(
                    "Failed to remove cgroup for {}: {e}",
                    handle.agent_name
                );
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_warning_variants() {
        // Verify all warning variants exist and are distinct
        let warnings = vec![
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
            io_available: false,
            bwrap_pid: None,
            landlock_applied: false,
        };
        assert_eq!(handle.agent_name, "test");
        assert!(!handle.cgroup_created);
        assert!(handle.bwrap_pid.is_none());
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
