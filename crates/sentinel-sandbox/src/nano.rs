use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use sentinel_common::nano_runtime::{
    ensure_handle_instance, ensure_handle_runtime, NanoExecRequest, NanoExecResult, NanoHandle,
    NanoHealth, NanoHealthState, NanoIsolationPolicy, NanoIsolationReport, NanoRuntime,
    NanoRuntimeControlAction, NanoRuntimeControlResult, NanoRuntimeResources, NanoSnapshot,
    NanoSnapshotSemantics, NanoStopResult, NanoWorkloadSpec, RUNTIME_BWRAP_LANDLOCK,
};
use sentinel_fs::artifact::ArtifactPlane;
use sentinel_fs::home_manifest::{self, HomeManifest, RestorePolicy};
use serde::{Deserialize, Serialize};

use crate::{cgroups, AgentProcess, CgroupLimits, SandboxEnforcer, SandboxHandle};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BwrapSnapshotPayload {
    workload: NanoWorkloadSpec,
    command: Vec<String>,
    home_manifest: HomeManifest,
    cgroup_created: bool,
    io_available: bool,
    bwrap_available: bool,
    landlock_available: bool,
    semantics_note: String,
}

#[derive(Debug, Clone)]
struct BwrapWorkloadState {
    instance_id: uuid::Uuid,
    workload: NanoWorkloadSpec,
    command: Vec<String>,
    /// `ArtifactPlane` object ids pinning the chunks of this workload's last home
    /// snapshot (released on re-snapshot/teardown to avoid chunk leaks, N1').
    owned_object_ids: Vec<u64>,
    suspended: bool,
}

struct BwrapSpawnTransaction {
    state: BwrapWorkloadState,
    marker_written: bool,
    setup_started: bool,
    handle: Option<SandboxHandle>,
    process: Option<AgentProcess>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BwrapSpawnStage {
    MarkerWritten,
    SetupComplete,
    ProcessStarted,
}

/// Default home content-addressed store location (chunks live under `/ram`).
const DEFAULT_HOME_CAS_DIR: &str = "/ram/agents/.sentinel-home-cas";
const DEFAULT_AGENT_HOME_ROOT: &str = "/ram/agents";

pub struct BwrapNanoRuntime {
    enforcer: SandboxEnforcer,
    /// Directory holding the home-content `ArtifactPlane`, opened lazily so the
    /// constructor stays infallible and does no I/O (daemon/registry callers that
    /// never snapshot are unaffected).
    cas_dir: PathBuf,
    agent_home_root: PathBuf,
    workloads: HashMap<String, BwrapWorkloadState>,
    handles: HashMap<String, SandboxHandle>,
    processes: HashMap<String, AgentProcess>,
    pending_spawns: HashMap<String, BwrapSpawnTransaction>,
}

impl BwrapNanoRuntime {
    pub fn detect() -> Self {
        Self::with_cas_dir(DEFAULT_HOME_CAS_DIR)
    }

    /// Construct with an explicit home-content CAS directory (used in tests).
    pub fn with_cas_dir(cas_dir: impl Into<PathBuf>) -> Self {
        let (enforcer, _warnings) = SandboxEnforcer::detect();
        Self {
            enforcer,
            cas_dir: cas_dir.into(),
            agent_home_root: PathBuf::from(DEFAULT_AGENT_HOME_ROOT),
            workloads: HashMap::new(),
            handles: HashMap::new(),
            processes: HashMap::new(),
            pending_spawns: HashMap::new(),
        }
    }

    /// Keep daemon FUSE routing identical when bwrap lifecycle ownership moves
    /// behind the NanoRuntime adapter.
    pub fn set_fs_mount(&mut self, mount: impl Into<String>) {
        self.enforcer.set_fs_mount(mount.into());
    }

    #[cfg(test)]
    fn with_test_dirs(cas_dir: impl Into<PathBuf>, agent_home_root: impl Into<PathBuf>) -> Self {
        let mut runtime = Self::with_cas_dir(cas_dir);
        runtime.agent_home_root = agent_home_root.into();
        runtime
    }

    /// Open (or create) the home-content `ArtifactPlane`. Called only on the
    /// snapshot/restore paths, so the constructor stays I/O-free.
    fn open_plane(&self) -> Result<ArtifactPlane> {
        std::fs::create_dir_all(&self.cas_dir)
            .with_context(|| format!("create home CAS dir {}", self.cas_dir.display()))?;
        ArtifactPlane::open(self.cas_dir.join("home.redb"))
    }

    fn command_for(workload: &NanoWorkloadSpec) -> Vec<String> {
        if workload.command.is_empty() {
            vec!["/usr/bin/sleep".to_string(), "30".to_string()]
        } else {
            workload.command.clone()
        }
    }

    fn home_dir(&self, agent_name: &str) -> PathBuf {
        self.agent_home_root.join(agent_name)
    }

    fn write_marker(&self, agent_name: &str, workload_id: &str) -> Result<()> {
        let home = self.home_dir(agent_name);
        std::fs::create_dir_all(&home)?;
        let marker = home.join(".nano-runtime");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(mut file) => {
                if let Err(error) = (|| -> std::io::Result<()> {
                    file.write_all(workload_id.as_bytes())?;
                    file.sync_all()
                })() {
                    let _ = std::fs::remove_file(&marker);
                    return Err(error.into());
                }
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let recorded = std::fs::read_to_string(&marker)?;
                if recorded == workload_id {
                    Ok(())
                } else {
                    Err(anyhow!(
                        "bwrap marker for '{agent_name}' belongs to workload '{recorded}', not '{workload_id}'"
                    ))
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    fn remove_marker(&self, agent_name: &str, workload_id: &str) -> Result<bool> {
        let marker = self.home_dir(agent_name).join(".nano-runtime");
        let recorded = match std::fs::read_to_string(&marker) {
            Ok(recorded) => recorded,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if recorded != workload_id {
            return Err(anyhow!(
                "bwrap marker for '{agent_name}' belongs to workload '{recorded}', not '{workload_id}'"
            ));
        }
        std::fs::remove_file(marker)?;
        Ok(true)
    }

    fn teardown_workload(&mut self, workload_id: &str) -> Result<bool> {
        let stopped = self.processes.contains_key(workload_id)
            || self.handles.contains_key(workload_id)
            || self.workloads.contains_key(workload_id);
        if let Some(process) = self.processes.get_mut(workload_id) {
            process.terminate_checked()?;
        }
        if let Some(handle) = self.handles.get(workload_id).cloned() {
            self.enforcer.teardown_agent(&handle)?;
        }
        if let Some((agent_name, owned_object_ids)) = self.workloads.get(workload_id).map(|state| {
            (
                state.workload.agent_name.clone(),
                state.owned_object_ids.clone(),
            )
        }) {
            if !owned_object_ids.is_empty() {
                let plane = self.open_plane()?;
                home_manifest::release_manifest(&plane, &owned_object_ids)?;
            }
            self.remove_marker(&agent_name, workload_id)?;
        }
        self.processes.remove(workload_id);
        self.handles.remove(workload_id);
        self.workloads.remove(workload_id);
        Ok(stopped)
    }

    fn rollback_pending_spawn(&mut self, workload_id: &str) -> Result<bool> {
        if !self.pending_spawns.contains_key(workload_id) {
            return Ok(false);
        }
        if let Some(process) = self
            .pending_spawns
            .get_mut(workload_id)
            .and_then(|transaction| transaction.process.as_mut())
        {
            process
                .terminate_checked()
                .with_context(|| format!("rollback process for {workload_id}"))?;
        }
        let handle = self
            .pending_spawns
            .get(workload_id)
            .and_then(|transaction| transaction.handle.clone());
        let (setup_started, marker_written, agent_name, marker_workload_id) = {
            let transaction = self
                .pending_spawns
                .get(workload_id)
                .expect("pending spawn checked above");
            (
                transaction.setup_started,
                transaction.marker_written,
                transaction.state.workload.agent_name.clone(),
                transaction.state.workload.workload_id.clone(),
            )
        };
        if let Some(handle) = handle.as_ref() {
            self.enforcer
                .teardown_agent(handle)
                .with_context(|| format!("rollback sandbox for {workload_id}"))?;
        } else if setup_started {
            self.enforcer
                .recover_partial_agent_setup(&agent_name)
                .with_context(|| format!("rollback partial setup for {workload_id}"))?;
        }
        if marker_written {
            self.remove_marker(&agent_name, &marker_workload_id)
                .with_context(|| format!("rollback marker for {workload_id}"))?;
        }
        self.pending_spawns.remove(workload_id);
        Ok(true)
    }

    fn reconcile_durable_spawn_marker(&self, agent_name: &str, workload_id: &str) -> Result<bool> {
        let marker = self.home_dir(agent_name).join(".nano-runtime");
        let recorded = match std::fs::read_to_string(&marker) {
            Ok(recorded) => recorded,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if recorded != workload_id {
            return Err(anyhow!(
                "bwrap agent home '{agent_name}' has durable ownership by workload '{recorded}'"
            ));
        }
        self.enforcer
            .recover_partial_agent_setup(agent_name)
            .with_context(|| format!("recover durable bwrap spawn for {workload_id}"))?;
        self.remove_marker(agent_name, workload_id)?;
        Ok(true)
    }

    fn ensure_workload_available(&self, workload: &NanoWorkloadSpec) -> Result<()> {
        if self.workloads.contains_key(&workload.workload_id)
            || self.handles.contains_key(&workload.workload_id)
            || self.processes.contains_key(&workload.workload_id)
            || self.pending_spawns.contains_key(&workload.workload_id)
        {
            return Err(anyhow!(
                "bwrap workload '{}' is already active",
                workload.workload_id
            ));
        }
        if let Some(existing) = self
            .workloads
            .values()
            .find(|state| state.workload.agent_name == workload.agent_name)
        {
            return Err(anyhow!(
                "bwrap agent home '{}' is already owned by workload '{}'",
                workload.agent_name,
                existing.workload.workload_id
            ));
        }
        if let Some(existing) = self
            .pending_spawns
            .values()
            .find(|transaction| transaction.state.workload.agent_name == workload.agent_name)
        {
            return Err(anyhow!(
                "bwrap agent home '{}' has pending ownership by workload '{}'",
                workload.agent_name,
                existing.state.workload.workload_id
            ));
        }
        Ok(())
    }

    fn ensure_restore_target_available(
        &self,
        snapshot_workload_id: &str,
        workload: &NanoWorkloadSpec,
    ) -> Result<()> {
        if workload.workload_id != snapshot_workload_id {
            return Err(anyhow!(
                "bwrap snapshot workload '{}' does not match envelope '{}'",
                workload.workload_id,
                snapshot_workload_id
            ));
        }
        if let Some(existing) = self.workloads.values().find(|state| {
            state.workload.workload_id != snapshot_workload_id
                && state.workload.agent_name == workload.agent_name
        }) {
            return Err(anyhow!(
                "bwrap agent home '{}' is already owned by workload '{}'",
                workload.agent_name,
                existing.workload.workload_id
            ));
        }
        if let Some(existing) = self.pending_spawns.values().find(|transaction| {
            transaction.state.workload.workload_id != snapshot_workload_id
                && transaction.state.workload.agent_name == workload.agent_name
        }) {
            return Err(anyhow!(
                "bwrap agent home '{}' has pending ownership by workload '{}'",
                workload.agent_name,
                existing.state.workload.workload_id
            ));
        }
        Ok(())
    }

    fn workload_pids(&self, workload_id: &str) -> Result<Vec<u32>> {
        let handle = self
            .handles
            .get(workload_id)
            .ok_or_else(|| anyhow!("missing bwrap sandbox handle '{workload_id}'"))?;
        let mut pids = if handle.cgroup_created {
            cgroups::list_pids_in_cgroup(&handle.agent_name)
                .with_context(|| format!("list bwrap cgroup members for {}", handle.agent_name))?
        } else {
            Vec::new()
        };
        if let Some(process) = self.processes.get(workload_id) {
            pids.push(process.pid);
            if let Some(child_pid) = process.child_pid {
                pids.push(child_pid);
            }
        }
        pids.sort_unstable();
        pids.dedup();
        if pids.is_empty() {
            return Err(anyhow!(
                "bwrap workload '{workload_id}' has no live execution unit"
            ));
        }
        Ok(pids)
    }

    fn signal_workload(&self, workload_id: &str, signal: i32) -> Result<usize> {
        let pids = self.workload_pids(workload_id)?;
        for pid in &pids {
            // SAFETY: libc::kill does not dereference memory. The PID belongs to
            // this exact adapter-owned workload and the signal is fixed by the
            // caller to SIGSTOP or SIGCONT.
            let result = unsafe { libc::kill(*pid as i32, signal) };
            if result != 0 && std::path::Path::new(&format!("/proc/{pid}")).exists() {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("signal {signal} to bwrap PID {pid}"));
            }
        }
        let expect_stopped = signal == libc::SIGSTOP;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let states = pids
                .iter()
                .filter_map(|pid| {
                    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
                    let state = status.lines().find_map(|line| {
                        line.strip_prefix("State:")
                            .and_then(|value| value.trim().chars().next())
                    })?;
                    (state != 'Z').then_some((*pid, state))
                })
                .collect::<Vec<_>>();
            let confirmed = if expect_stopped {
                !states.is_empty() && states.iter().all(|(_, state)| *state == 'T')
            } else {
                !states.is_empty() && states.iter().all(|(_, state)| *state != 'T')
            };
            if confirmed {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return Err(anyhow!(
                    "bwrap workload '{workload_id}' did not confirm signal {signal}; states={states:?}"
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        Ok(pids.len())
    }

    fn spawn_state_with<Start, Checkpoint>(
        &mut self,
        state: BwrapWorkloadState,
        start_process: Start,
        mut checkpoint: Checkpoint,
    ) -> Result<NanoHandle>
    where
        Start: FnOnce(&SandboxEnforcer, &str, &str, &[String]) -> Result<AgentProcess>,
        Checkpoint: FnMut(BwrapSpawnStage) -> Result<()>,
    {
        let workload = state.workload.clone();
        let instance_id = state.instance_id;
        let agent_name = workload.agent_name.clone();
        let workload_id = workload.workload_id.clone();
        self.pending_spawns.insert(
            workload_id.clone(),
            BwrapSpawnTransaction {
                state,
                marker_written: false,
                setup_started: false,
                handle: None,
                process: None,
            },
        );

        let attempted = (|| -> Result<u32> {
            // Claim marker cleanup ownership before attempting the write. If
            // write/fsync fails and the immediate unlink also fails, rollback
            // must still probe and retain this exact transaction for retry.
            self.pending_spawns
                .get_mut(&workload_id)
                .expect("pending spawn inserted")
                .marker_written = true;
            self.write_marker(&agent_name, &workload_id)?;
            checkpoint(BwrapSpawnStage::MarkerWritten)?;

            self.pending_spawns
                .get_mut(&workload_id)
                .expect("pending spawn inserted")
                .setup_started = true;
            let handle = self
                .enforcer
                .setup_agent(&agent_name, &CgroupLimits::default())
                .with_context(|| format!("bwrap setup_agent failed for {agent_name}"))?;
            self.pending_spawns
                .get_mut(&workload_id)
                .expect("pending spawn inserted")
                .handle = Some(handle);
            checkpoint(BwrapSpawnStage::SetupComplete)?;

            let command = &self
                .pending_spawns
                .get(&workload_id)
                .expect("pending spawn inserted")
                .state
                .command;
            let process = start_process(&self.enforcer, &agent_name, &workload_id, command)
                .with_context(|| format!("bwrap start_agent_process failed for {agent_name}"))?;
            let pid = process.pid;
            let transaction = self
                .pending_spawns
                .get_mut(&workload_id)
                .expect("pending spawn inserted");
            transaction
                .handle
                .as_mut()
                .expect("setup completed before process start")
                .bwrap_pid = Some(pid);
            transaction.process = Some(process);
            checkpoint(BwrapSpawnStage::ProcessStarted)?;
            Ok(pid)
        })();

        let pid = match attempted {
            Ok(pid) => pid,
            Err(error) => {
                let rollback_error = self.rollback_pending_spawn(&workload_id).err();
                return Err(match rollback_error {
                    Some(rollback_error) => anyhow!(
                        "bwrap spawn transaction failed: {error}; rollback retained for retry: {rollback_error}"
                    ),
                    None => error,
                });
            }
        };

        let transaction = self
            .pending_spawns
            .remove(&workload_id)
            .expect("successful spawn has pending transaction");
        self.processes.insert(
            workload_id.clone(),
            transaction.process.expect("process started before commit"),
        );
        self.handles.insert(
            workload_id.clone(),
            transaction.handle.expect("setup completed before commit"),
        );
        self.workloads
            .insert(workload_id.clone(), transaction.state);

        Ok(NanoHandle {
            instance_id,
            runtime_key: RUNTIME_BWRAP_LANDLOCK.to_string(),
            workload_id,
            agent_id: workload.agent_id,
            pid: Some(pid),
        })
    }

    fn spawn_state(&mut self, state: BwrapWorkloadState) -> Result<NanoHandle> {
        self.spawn_state_with(
            state,
            |enforcer, agent_name, workload_id, command| {
                enforcer.start_agent_process(agent_name, Some(workload_id), command)
            },
            |_| Ok(()),
        )
    }
}

impl Default for BwrapNanoRuntime {
    fn default() -> Self {
        Self::detect()
    }
}

impl Drop for BwrapNanoRuntime {
    fn drop(&mut self) {
        let mut ids: Vec<String> = self
            .processes
            .keys()
            .chain(self.handles.keys())
            .chain(self.workloads.keys())
            .chain(self.pending_spawns.keys())
            .cloned()
            .collect();
        ids.sort();
        ids.dedup();
        for id in ids {
            let _ = self.rollback_pending_spawn(&id);
            let _ = self.teardown_workload(&id);
        }
    }
}

impl NanoRuntime for BwrapNanoRuntime {
    fn runtime_key(&self) -> &'static str {
        RUNTIME_BWRAP_LANDLOCK
    }

    fn spawn(&mut self, workload: NanoWorkloadSpec) -> Result<NanoHandle> {
        if workload.agent_name.is_empty() {
            return Err(anyhow!("bwrap workload requires agent_name"));
        }
        if self.pending_spawns.contains_key(&workload.workload_id) {
            self.rollback_pending_spawn(&workload.workload_id)
                .with_context(|| {
                    format!(
                        "recover previous bwrap spawn for '{}'",
                        workload.workload_id
                    )
                })?;
        }
        self.ensure_workload_available(&workload)?;
        self.reconcile_durable_spawn_marker(&workload.agent_name, &workload.workload_id)?;
        let state = BwrapWorkloadState {
            instance_id: uuid::Uuid::new_v4(),
            command: Self::command_for(&workload),
            workload,
            owned_object_ids: Vec::new(),
            suspended: false,
        };
        self.spawn_state(state)
    }

    fn stop(&mut self, handle: &NanoHandle) -> Result<NanoStopResult> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        if let Some(state) = self.workloads.get(&handle.workload_id) {
            ensure_handle_instance(handle, state.instance_id)?;
        }
        Ok(NanoStopResult::new(
            self.runtime_key(),
            &handle.workload_id,
            self.teardown_workload(&handle.workload_id)?,
        ))
    }

    fn resources(&self, handle: &NanoHandle) -> Result<NanoRuntimeResources> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        let state = self
            .workloads
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("missing bwrap workload '{}'", handle.workload_id))?;
        ensure_handle_instance(handle, state.instance_id)?;
        let sandbox = self
            .handles
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("missing bwrap sandbox handle '{}'", handle.workload_id))?;
        let process = self
            .processes
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("missing bwrap process '{}'", handle.workload_id))?;
        Ok(NanoRuntimeResources {
            instance_id: Some(state.instance_id),
            pid: Some(process.pid),
            child_pid: process.child_pid,
            cgroup_created: sandbox.cgroup_created,
            cgroup_id: sandbox.cgroup_id,
            io_available: sandbox.io_available,
            landlock_applied: sandbox.landlock_applied,
            network_isolated: sandbox.network_isolated,
        })
    }

    fn exec(&mut self, handle: &NanoHandle, request: NanoExecRequest) -> Result<NanoExecResult> {
        let health = self.health(handle)?;
        let output = match request.operation.as_str() {
            "health" => format!("{:?}", health.state),
            other => return Err(anyhow!("bwrap exec operation '{other}' is not supported")),
        };
        Ok(NanoExecResult {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            success: true,
            output,
        })
    }

    fn snapshot(&mut self, handle: &NanoHandle) -> Result<NanoSnapshot> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        // Clone the bits we need so `self` can be re-borrowed mutably below.
        let (workload, command, prev_owned) = {
            let state = self
                .workloads
                .get(&handle.workload_id)
                .ok_or_else(|| anyhow!("unknown bwrap workload '{}'", handle.workload_id))?;
            ensure_handle_instance(handle, state.instance_id)?;
            (
                state.workload.clone(),
                state.command.clone(),
                state.owned_object_ids.clone(),
            )
        };
        let (cgroup_created, io_available) = {
            let sandbox_handle = self
                .handles
                .get(&handle.workload_id)
                .ok_or_else(|| anyhow!("missing bwrap sandbox handle '{}'", handle.workload_id))?;
            (sandbox_handle.cgroup_created, sandbox_handle.io_available)
        };

        // Walk the agent home into a metadata-aware CAS manifest (no file bytes).
        let home = self.home_dir(&workload.agent_name);
        let plane = self.open_plane()?;
        // Release the previous snapshot's pinned objects before re-walking.
        home_manifest::release_manifest(&plane, &prev_owned)?;
        let walked = home_manifest::walk_home(&home, &plane)?;
        if let Some(state) = self.workloads.get_mut(&handle.workload_id) {
            state.owned_object_ids = walked.owned_object_ids;
        }

        let payload = BwrapSnapshotPayload {
            workload,
            command,
            home_manifest: walked.manifest,
            cgroup_created,
            io_available,
            bwrap_available: self.enforcer.has_bwrap(),
            landlock_available: self.enforcer.has_landlock(),
            semantics_note: "bwrap snapshot is a metadata-aware CAS manifest of the agent-home filesystem; no process RAM or CRIU checkpoint".to_string(),
        };

        Ok(NanoSnapshot {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            agent_id: handle.agent_id,
            semantics: NanoSnapshotSemantics::BwrapConfigFs,
            payload: serde_json::to_value(payload)?,
        })
    }

    fn restore(&mut self, snapshot: NanoSnapshot) -> Result<NanoHandle> {
        if snapshot.runtime_key != self.runtime_key() {
            return Err(anyhow!(
                "cannot restore {} snapshot into {} runtime",
                snapshot.runtime_key,
                self.runtime_key()
            ));
        }
        if snapshot.semantics != NanoSnapshotSemantics::BwrapConfigFs {
            return Err(anyhow!(
                "bwrap restore requires BwrapConfigFs snapshot, got {:?}",
                snapshot.semantics
            ));
        }
        let payload: BwrapSnapshotPayload = serde_json::from_value(snapshot.payload)?;
        self.rollback_pending_spawn(&snapshot.workload_id)?;
        self.ensure_restore_target_available(&snapshot.workload_id, &payload.workload)?;
        if !self.workloads.contains_key(&snapshot.workload_id)
            && !self.handles.contains_key(&snapshot.workload_id)
            && !self.processes.contains_key(&snapshot.workload_id)
        {
            self.reconcile_durable_spawn_marker(
                &payload.workload.agent_name,
                &snapshot.workload_id,
            )?;
        }
        self.teardown_workload(&snapshot.workload_id)?;

        // Rehydrate the agent home from the manifest (metadata-aware, V24 path
        // safety) instead of writing raw bytes back.
        let home = self.home_dir(&payload.workload.agent_name);
        if home.exists() {
            std::fs::remove_dir_all(&home)
                .with_context(|| format!("reset agent home dir {}", home.display()))?;
        }
        let plane = self.open_plane()?;
        home_manifest::rehydrate(
            &payload.home_manifest,
            &home,
            &plane,
            &RestorePolicy::default(),
        )?;

        self.spawn_state(BwrapWorkloadState {
            instance_id: uuid::Uuid::new_v4(),
            workload: payload.workload,
            command: payload.command,
            owned_object_ids: Vec::new(),
            suspended: false,
        })
    }

    fn health(&mut self, handle: &NanoHandle) -> Result<NanoHealth> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        if let Some(state) = self.workloads.get(&handle.workload_id) {
            ensure_handle_instance(handle, state.instance_id)?;
        }
        let state = if self
            .workloads
            .get(&handle.workload_id)
            .is_some_and(|state| state.suspended)
        {
            NanoHealthState::Degraded
        } else if let Some(process) = self.processes.get_mut(&handle.workload_id) {
            if process.is_running() {
                NanoHealthState::Healthy
            } else {
                NanoHealthState::Stopped
            }
        } else if let Some(pid) = handle.pid {
            let cgroup_name = self
                .workloads
                .get(&handle.workload_id)
                .map(|state| state.workload.agent_name.as_str())
                .unwrap_or(handle.workload_id.as_str());
            let cgroup = cgroups::list_pids_in_cgroup(cgroup_name).unwrap_or_default();
            if cgroup.contains(&pid) {
                NanoHealthState::Degraded
            } else {
                NanoHealthState::Stopped
            }
        } else {
            NanoHealthState::Stopped
        };
        Ok(NanoHealth {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            state,
            detail: "bwrap process plus cgroup/Landlock sandbox state".to_string(),
        })
    }

    fn isolate(
        &mut self,
        handle: &NanoHandle,
        policy: NanoIsolationPolicy,
    ) -> Result<NanoIsolationReport> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        if let Some(state) = self.workloads.get(&handle.workload_id) {
            ensure_handle_instance(handle, state.instance_id)?;
        }
        let applied = self.handles.contains_key(&handle.workload_id);
        Ok(NanoIsolationReport {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            applied,
            detail: format!(
                "bwrap={} cgroups={} landlock={} network={}",
                self.enforcer.has_bwrap(),
                self.enforcer.has_cgroups() && policy.cgroups,
                self.enforcer.has_landlock() && policy.landlock,
                // #75: network isolation = full cage from bwrap --unshare-all.
                self.enforcer.has_bwrap() && policy.network
            ),
        })
    }

    fn control(
        &mut self,
        handle: &NanoHandle,
        action: NanoRuntimeControlAction,
    ) -> Result<NanoRuntimeControlResult> {
        self.resources(handle)?;
        let suspended = self
            .workloads
            .get(&handle.workload_id)
            .map(|state| state.suspended)
            .ok_or_else(|| anyhow!("unknown bwrap workload '{}'", handle.workload_id))?;
        let should_apply = match action {
            NanoRuntimeControlAction::Suspend => !suspended,
            NanoRuntimeControlAction::Resume => suspended,
        };
        let affected_units = if should_apply {
            let signal = match action {
                NanoRuntimeControlAction::Suspend => libc::SIGSTOP,
                NanoRuntimeControlAction::Resume => libc::SIGCONT,
            };
            let affected = self.signal_workload(&handle.workload_id, signal)?;
            if let Some(state) = self.workloads.get_mut(&handle.workload_id) {
                state.suspended = matches!(action, NanoRuntimeControlAction::Suspend);
            }
            affected
        } else {
            0
        };
        Ok(NanoRuntimeControlResult::new(
            self.runtime_key(),
            &handle.workload_id,
            action,
            should_apply,
            affected_units,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_workload(workload_id: &str, agent_name: &str) -> NanoWorkloadSpec {
        NanoWorkloadSpec {
            workload_id: workload_id.to_string(),
            runtime_key: Some(RUNTIME_BWRAP_LANDLOCK.to_string()),
            agent_id: None,
            agent_name: agent_name.to_string(),
            role: "Tester".to_string(),
            room_id: "empfang".to_string(),
            shift_set: 1,
            command: Vec::new(),
            capabilities: Vec::new(),
            metadata: Default::default(),
            ecs_snapshot: None,
        }
    }

    fn insert_fixture(
        runtime: &mut BwrapNanoRuntime,
        workload: NanoWorkloadSpec,
        owned_object_ids: Vec<u64>,
    ) -> NanoHandle {
        let workload_id = workload.workload_id.clone();
        let agent_name = workload.agent_name.clone();
        let process = AgentProcess::launch_fixture().unwrap();
        let pid = process.pid;
        runtime.processes.insert(workload_id.clone(), process);
        runtime.handles.insert(
            workload_id.clone(),
            SandboxHandle {
                agent_name,
                cgroup_created: false,
                cgroup_id: None,
                io_available: false,
                bwrap_pid: Some(pid),
                landlock_applied: false,
                network_isolated: false,
            },
        );
        runtime.workloads.insert(
            workload_id.clone(),
            BwrapWorkloadState {
                instance_id: uuid::Uuid::new_v4(),
                workload,
                command: Vec::new(),
                owned_object_ids,
                suspended: false,
            },
        );
        NanoHandle {
            instance_id: runtime.workloads[&workload_id].instance_id,
            runtime_key: RUNTIME_BWRAP_LANDLOCK.to_string(),
            workload_id,
            agent_id: None,
            pid: Some(pid),
        }
    }

    fn transactional_fixture_state(workload_id: &str, agent_name: &str) -> BwrapWorkloadState {
        let workload = fixture_workload(workload_id, agent_name);
        BwrapWorkloadState {
            instance_id: uuid::Uuid::new_v4(),
            command: vec!["/usr/bin/sleep".to_string(), "30".to_string()],
            workload,
            owned_object_ids: Vec::new(),
            suspended: false,
        }
    }

    #[test]
    fn spawn_failure_after_each_side_effect_rolls_back_transactionally() {
        for stage in [
            BwrapSpawnStage::MarkerWritten,
            BwrapSpawnStage::SetupComplete,
            BwrapSpawnStage::ProcessStarted,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let mut runtime = BwrapNanoRuntime::with_test_dirs(
                temp.path().join("cas"),
                temp.path().join("homes"),
            );
            let workload_id = format!("failure-{stage:?}");
            let agent_name = format!("failure-agent-{stage:?}-{}", std::process::id());
            let state = transactional_fixture_state(&workload_id, &agent_name);
            let started_pid = std::sync::Arc::new(std::sync::Mutex::new(None));
            let pid_observer = std::sync::Arc::clone(&started_pid);

            let error = runtime
                .spawn_state_with(
                    state,
                    move |_, _, _, _| {
                        let process = AgentProcess::launch_fixture()?;
                        *pid_observer.lock().unwrap() = Some(process.pid);
                        Ok(process)
                    },
                    |reached| {
                        if reached == stage {
                            Err(anyhow!("injected failure after {stage:?}"))
                        } else {
                            Ok(())
                        }
                    },
                )
                .unwrap_err();

            assert!(error.to_string().contains("injected failure"));
            assert!(!runtime.pending_spawns.contains_key(&workload_id));
            assert!(!runtime.workloads.contains_key(&workload_id));
            assert!(!runtime.handles.contains_key(&workload_id));
            assert!(!runtime.processes.contains_key(&workload_id));
            assert!(!runtime.home_dir(&agent_name).join(".nano-runtime").exists());
            let started_pid = *started_pid.lock().unwrap();
            if let Some(pid) = started_pid {
                assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
            }
        }
    }

    #[test]
    fn failed_spawn_rollback_retains_exact_transaction_for_retry() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime =
            BwrapNanoRuntime::with_test_dirs(temp.path().join("cas"), temp.path().join("homes"));
        let workload_id = "retry-spawn";
        let agent_name = format!("retry-spawn-agent-{}", std::process::id());
        let state = transactional_fixture_state(workload_id, &agent_name);
        let marker = runtime.home_dir(&agent_name).join(".nano-runtime");

        let error = runtime
            .spawn_state_with(
                state,
                |_, _, _, _| AgentProcess::launch_fixture(),
                |stage| {
                    if stage == BwrapSpawnStage::ProcessStarted {
                        std::fs::write(&marker, b"foreign-owner")?;
                        Err(anyhow!("injected post-process failure"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("rollback retained for retry"));
        assert!(runtime.pending_spawns.contains_key(workload_id));

        std::fs::write(&marker, workload_id.as_bytes()).unwrap();
        assert!(runtime.rollback_pending_spawn(workload_id).unwrap());
        assert!(!runtime.pending_spawns.contains_key(workload_id));
        assert!(!marker.exists());

        let handle = runtime
            .spawn_state_with(
                transactional_fixture_state(workload_id, &agent_name),
                |_, _, _, _| AgentProcess::launch_fixture(),
                |_| Ok(()),
            )
            .unwrap();
        assert_eq!(
            runtime.stop(&handle).unwrap().outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
    }

    #[test]
    fn durable_spawn_marker_recovery_is_exact_and_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let cas = temp.path().join("cas");
        let homes = temp.path().join("homes");
        let first = BwrapNanoRuntime::with_test_dirs(cas.clone(), homes.clone());
        first
            .write_marker("durable-agent", "durable-workload")
            .unwrap();
        drop(first);

        let recovered = BwrapNanoRuntime::with_test_dirs(cas.clone(), homes.clone());
        assert!(recovered
            .reconcile_durable_spawn_marker("durable-agent", "durable-workload")
            .unwrap());
        assert!(!homes.join("durable-agent/.nano-runtime").exists());

        recovered
            .write_marker("durable-agent", "foreign-workload")
            .unwrap();
        let error = recovered
            .reconcile_durable_spawn_marker("durable-agent", "durable-workload")
            .unwrap_err();
        assert!(error.to_string().contains("durable ownership"));
        assert_eq!(
            std::fs::read_to_string(homes.join("durable-agent/.nano-runtime")).unwrap(),
            "foreign-workload"
        );
    }

    #[test]
    fn bwrap_control_suspends_and_resumes_adapter_owned_process() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime =
            BwrapNanoRuntime::with_test_dirs(temp.path().join("cas"), temp.path().join("homes"));
        let handle = insert_fixture(
            &mut runtime,
            fixture_workload("control-fixture", "control-agent"),
            Vec::new(),
        );
        let pid = handle.pid.unwrap();

        let suspended = runtime
            .control(&handle, NanoRuntimeControlAction::Suspend)
            .unwrap();
        assert_eq!(suspended.affected_units, 1);
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        assert!(status.lines().any(|line| line.starts_with("State:\tT")));
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Degraded
        );

        runtime
            .control(&handle, NanoRuntimeControlAction::Resume)
            .unwrap();
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Healthy
        );
        runtime.stop(&handle).unwrap();
    }

    #[test]
    fn stop_fixture_reaps_process_releases_cas_and_preserves_other_workload() {
        let temp = tempfile::tempdir().unwrap();
        let cas_dir = temp.path().join("cas");
        let mut runtime = BwrapNanoRuntime::with_cas_dir(&cas_dir);
        let home_a = temp.path().join("home-a");
        let home_b = temp.path().join("home-b");
        std::fs::create_dir_all(&home_a).unwrap();
        std::fs::create_dir_all(&home_b).unwrap();
        std::fs::write(home_a.join("owned.txt"), b"workload a content").unwrap();
        std::fs::write(home_b.join("owned.txt"), b"workload b content").unwrap();
        let plane = runtime.open_plane().unwrap();
        let owned_a = home_manifest::walk_home(&home_a, &plane)
            .unwrap()
            .owned_object_ids;
        let owned_b = home_manifest::walk_home(&home_b, &plane)
            .unwrap()
            .owned_object_ids;
        assert!(!owned_a.is_empty());
        assert!(!owned_b.is_empty());
        drop(plane);

        let handle_a = insert_fixture(
            &mut runtime,
            fixture_workload("fixture-a", "agent-fixture-a"),
            owned_a.clone(),
        );
        let handle_b = insert_fixture(
            &mut runtime,
            fixture_workload("fixture-b", "agent-fixture-b"),
            owned_b.clone(),
        );

        let stale_for_b = NanoHandle {
            instance_id: handle_a.instance_id,
            ..handle_b.clone()
        };
        assert!(runtime.stop(&stale_for_b).is_err());

        let stopped = runtime.stop(&handle_a).unwrap();
        assert_eq!(
            stopped.outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
        assert!(!PathBuf::from(format!("/proc/{}", handle_a.pid.unwrap())).exists());
        assert_eq!(
            runtime.health(&handle_a).unwrap().state,
            NanoHealthState::Stopped
        );
        assert!(matches!(
            runtime.health(&handle_b).unwrap().state,
            NanoHealthState::Healthy | NanoHealthState::Degraded
        ));
        let plane = runtime.open_plane().unwrap();
        for object_id in owned_a {
            assert!(plane.get_object(object_id).unwrap().is_none());
        }
        for object_id in &owned_b {
            assert!(plane.get_object(*object_id).unwrap().is_some());
        }
        drop(plane);

        let replay = runtime.stop(&handle_a).unwrap();
        assert_eq!(
            replay.outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::AlreadyStopped
        );
        assert_eq!(
            runtime.stop(&handle_b).unwrap().outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
        let plane = runtime.open_plane().unwrap();
        for object_id in owned_b {
            assert!(plane.get_object(object_id).unwrap().is_none());
        }
    }

    #[test]
    fn failed_cleanup_retains_ownership_for_retry() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime = BwrapNanoRuntime::with_test_dirs(
            temp.path().join("cas"),
            temp.path().join("agent-homes"),
        );
        let source_home = temp.path().join("retry-home");
        std::fs::create_dir_all(&source_home).unwrap();
        std::fs::write(source_home.join("owned.txt"), b"retry-owned content").unwrap();
        let plane = runtime.open_plane().unwrap();
        let owned_object_ids = home_manifest::walk_home(&source_home, &plane)
            .unwrap()
            .owned_object_ids;
        assert!(!owned_object_ids.is_empty());
        drop(plane);
        let handle = insert_fixture(
            &mut runtime,
            fixture_workload("fixture-retry", "agent-fixture-retry"),
            owned_object_ids.clone(),
        );
        runtime
            .write_marker("agent-fixture-retry", "different-workload")
            .unwrap();

        assert!(runtime.stop(&handle).is_err());
        assert!(runtime.processes.contains_key(&handle.workload_id));
        assert!(runtime.handles.contains_key(&handle.workload_id));
        assert!(runtime.workloads.contains_key(&handle.workload_id));
        let plane = runtime.open_plane().unwrap();
        for object_id in &owned_object_ids {
            assert!(plane.get_object(*object_id).unwrap().is_none());
        }
        drop(plane);

        std::fs::write(
            runtime
                .home_dir("agent-fixture-retry")
                .join(".nano-runtime"),
            handle.workload_id.as_bytes(),
        )
        .unwrap();
        assert_eq!(
            runtime.stop(&handle).unwrap().outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
        assert!(!runtime.processes.contains_key(&handle.workload_id));
        assert!(!runtime.handles.contains_key(&handle.workload_id));
        assert!(!runtime.workloads.contains_key(&handle.workload_id));
    }

    #[test]
    fn duplicate_agent_home_is_rejected_without_touching_owner() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let owner = insert_fixture(
            &mut runtime,
            fixture_workload("fixture-owner", "shared-agent-home"),
            Vec::new(),
        );
        let alias = fixture_workload("fixture-alias", "shared-agent-home");

        assert!(runtime.ensure_workload_available(&alias).is_err());
        assert!(runtime
            .ensure_restore_target_available(&alias.workload_id, &alias)
            .is_err());
        assert!(runtime.processes.contains_key(&owner.workload_id));
        assert!(runtime.workloads.contains_key(&owner.workload_id));
        assert_eq!(
            runtime.stop(&owner).unwrap().outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
    }

    #[test]
    fn restore_rejects_mismatched_workload_identity_without_touching_owner() {
        let temp = tempfile::tempdir().unwrap();
        let mut runtime = BwrapNanoRuntime::with_cas_dir(temp.path().join("cas"));
        let owner = insert_fixture(
            &mut runtime,
            fixture_workload("fixture-owner", "fixture-owner-home"),
            Vec::new(),
        );
        let payload = fixture_workload("payload-workload", "payload-home");

        assert!(runtime
            .ensure_restore_target_available("envelope-workload", &payload)
            .is_err());
        assert!(runtime.processes.contains_key(&owner.workload_id));
        assert!(runtime.workloads.contains_key(&owner.workload_id));
        assert_eq!(
            runtime.stop(&owner).unwrap().outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
    }

    /// N5 + AC-1 at the adapter level: the bwrap snapshot representation is a
    /// metadata-aware CAS manifest (not file bytes), and it is deterministic — a
    /// re-walk of an identical home yields a serde-equal manifest, so the
    /// conformance harness's `after.payload == before.payload` holds. This
    /// exercises the rewired snapshot/restore data path without needing a real
    /// bwrap spawn (which is the `#[ignore]`d host conformance test).
    #[test]
    fn home_manifest_is_deterministic_and_block_ref_based() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = BwrapNanoRuntime::with_cas_dir(tmp.path().join("cas"));
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join("d")).unwrap();
        std::fs::write(home.join("d/f.txt"), b"deterministic agent-home content").unwrap();
        std::os::unix::fs::symlink("d/f.txt", home.join("link")).unwrap();

        let plane = rt.open_plane().unwrap();
        let m1 = home_manifest::walk_home(&home, &plane).unwrap().manifest;
        let m2 = home_manifest::walk_home(&home, &plane).unwrap().manifest;
        assert_eq!(
            serde_json::to_value(&m1).unwrap(),
            serde_json::to_value(&m2).unwrap(),
            "bwrap home manifest must be deterministic (N5 payload stability)"
        );

        // AC-1: the file entry carries BLAKE3-128 chunk refs, not bytes.
        let file = m1
            .entries
            .iter()
            .find(|e| e.rel_path_bytes == b"d/f.txt")
            .expect("file entry present");
        assert_eq!(file.kind, home_manifest::EntryKind::File);
        assert!(!file.content.is_empty());
        assert!(!file.content[0].chunk_refs.is_empty());

        // The snapshot payload embeds the manifest, never a raw byte map: the
        // type system guarantees this (BwrapSnapshotPayload.home_manifest), and
        // the serialized form shows the manifest field and no `home_files`.
        let workload: NanoWorkloadSpec = serde_json::from_value(serde_json::json!({
            "workload_id": "w-test",
            "agent_name": "agent-test",
        }))
        .unwrap();
        let payload = BwrapSnapshotPayload {
            workload,
            command: vec![],
            home_manifest: m1,
            cgroup_created: false,
            io_available: false,
            bwrap_available: false,
            landlock_available: false,
            semantics_note: String::new(),
        };
        let value = serde_json::to_value(&payload).unwrap();
        assert!(value.get("home_manifest").is_some());
        assert!(value.get("home_files").is_none(), "no raw byte map remains");
    }
}
