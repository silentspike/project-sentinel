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
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::bwrap::BwrapConfig;
use crate::cgroups::{self, CgroupLimits, PsiMetrics};
use crate::landlock;
use crate::netns::{self, NetworkNsConfig};

/// Handle fuer einen laufenden Agent-Prozess in bwrap.
///
/// Haelt den Child-Handle am Leben (bwrap hat --die-with-parent,
/// stirbt also wenn der Daemon stirbt). stdin ist piped fuer
/// stream-json Kommunikation mit dem Agent.
pub struct AgentProcess {
    /// PID des bwrap-Prozesses.
    pub pid: u32,
    /// Child handle — NICHT droppen solange Agent laufen soll.
    child: Child,
}

impl AgentProcess {
    /// Nimmt den stdin-Handle fuer stream-json Kommunikation (einmalig).
    pub fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.child.stdin.take()
    }

    /// Prueft ob der Prozess noch laeuft.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        // Reap the child process to prevent zombies.
        // try_wait() is non-blocking — if the child is still running,
        // --die-with-parent will handle cleanup when the daemon exits.
        match self.child.try_wait() {
            Ok(Some(_status)) => {} // Already exited, reaped by try_wait
            Ok(None) => {
                // Still running — let --die-with-parent handle it.
                // We intentionally do NOT kill the child here, because
                // the daemon might be shutting down gracefully.
            }
            Err(_) => {} // Error checking status, nothing we can do
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
    /// Network namespace isolation not available (no CAP_NET_ADMIN).
    NetnsNotAvailable,
}

/// Handle returned by setup_agent() — tracks what was created.
#[derive(Debug, Clone)]
pub struct SandboxHandle {
    pub agent_name: String,
    pub cgroup_created: bool,
    pub io_available: bool,
    pub bwrap_pid: Option<u32>,
    pub landlock_applied: bool,
    /// Whether this agent has network namespace isolation active.
    pub network_isolated: bool,
    /// Network namespace config (set after setup_network()).
    pub netns_config: Option<NetworkNsConfig>,
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
    /// Whether network namespace isolation is available (CAP_NET_ADMIN).
    netns_available: bool,
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
            .field("netns_available", &self.netns_available)
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

        // 2. cgroup root + controller delegation
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

        // 6. Network namespace support (CAP_NET_ADMIN + ip + nft)
        let netns_available = netns::detect_netns_support();
        if !netns_available {
            warnings.push(SandboxWarning::NetnsNotAvailable);
        }

        let enforcer = Self {
            landlock_abi,
            cgroup_root,
            cgroup_available,
            bwrap_available,
            netns_available,
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
            io_available: false,
            bwrap_pid: None,
            landlock_applied: false,
            network_isolated: false,
            netns_config: None,
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

        // TOGAF default: share_net=true (Cortex Gateway API-Zugang)
        // Wenn netns verfuegbar: isoliertes Netzwerk via veth-Pair (spaeter)
        if self.netns_available {
            config.share_net = false;
        }

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

        let child = config.spawn(&wrapped_command)?;
        let pid = child.id();

        // Add bwrap process to agent's cgroup
        if self.cgroup_available {
            if let Err(e) = cgroups::add_pid_to_cgroup(name, pid) {
                warn!("Failed to add bwrap PID {pid} to cgroup {name}: {e}");
            }
        }

        debug!(
            name,
            pid, "bwrap process started, returning AgentProcess handle"
        );

        Ok(AgentProcess { pid, child })
    }

    /// Sets up network namespace isolation for a running agent.
    ///
    /// Creates bridge (idempotent), veth pair, and loads nftables rules.
    /// Must be called AFTER start_agent_process() (needs PID).
    ///
    /// Returns true if network isolation was applied, false if skipped.
    pub fn setup_network(
        &self,
        handle: &mut SandboxHandle,
        pid: u32,
        agent_index: u8,
    ) -> Result<bool> {
        if !self.netns_available {
            return Ok(false);
        }

        let config = NetworkNsConfig::for_agent(&handle.agent_name, agent_index);

        // Ensure bridge exists
        netns::setup_bridge(&config).context("Failed to setup sentinel bridge")?;

        // Setup veth + nftables inside agent NS
        netns::setup_netns(pid, &config).with_context(|| {
            format!(
                "Failed to setup network namespace for agent {}",
                handle.agent_name
            )
        })?;

        handle.network_isolated = true;
        handle.netns_config = Some(config);
        Ok(true)
    }

    /// Tears down sandbox resources for an agent.
    ///
    /// Kills the bwrap process (if running), removes network namespace
    /// resources, and removes the cgroup.
    /// Called by RuntimeOrchestrator::despawn_agent().
    pub fn teardown_agent(&self, handle: &SandboxHandle) -> Result<()> {
        // Kill bwrap process if running
        if let Some(pid) = handle.bwrap_pid {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
        }

        // Remove network namespace resources BEFORE cgroup cleanup
        if handle.network_isolated {
            if let Some(ref config) = handle.netns_config {
                if let Err(e) = netns::teardown_netns(config) {
                    warn!("Failed to teardown netns for {}: {e}", handle.agent_name);
                }
            }
        }

        // Remove cgroup (may fail if processes still in it)
        if handle.cgroup_created {
            if let Err(e) = cgroups::remove_cgroup(&handle.agent_name) {
                warn!("Failed to remove cgroup for {}: {e}", handle.agent_name);
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

    /// Whether network namespace isolation is available.
    pub fn has_netns(&self) -> bool {
        self.netns_available
    }

    /// Whether OOM score was set for the ECS core process.
    pub fn oom_score_set(&self) -> bool {
        self.oom_set.load(Ordering::Relaxed)
    }
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
            SandboxWarning::NetnsNotAvailable,
        ];
        assert_eq!(warnings.len(), 6);
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
            network_isolated: false,
            netns_config: None,
        };
        assert_eq!(handle.agent_name, "test");
        assert!(!handle.cgroup_created);
        assert!(handle.bwrap_pid.is_none());
        assert!(!handle.network_isolated);
        assert!(handle.netns_config.is_none());
    }

    #[test]
    fn sandbox_handle_with_netns() {
        let config = NetworkNsConfig::for_agent("test", 5);
        let handle = SandboxHandle {
            agent_name: "test".into(),
            cgroup_created: false,
            io_available: false,
            bwrap_pid: Some(12345),
            landlock_applied: false,
            network_isolated: true,
            netns_config: Some(config),
        };
        assert!(handle.network_isolated);
        assert!(handle.netns_config.is_some());
        assert_eq!(handle.netns_config.unwrap().agent_ip(), "10.42.0.7");
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
