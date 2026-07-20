use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use sentinel_common::nano_runtime::{
    ensure_handle_runtime, NanoExecRequest, NanoExecResult, NanoHandle, NanoHealth,
    NanoHealthState, NanoIsolationPolicy, NanoIsolationReport, NanoRuntime, NanoSnapshot,
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
    workload: NanoWorkloadSpec,
    command: Vec<String>,
    /// `ArtifactPlane` object ids pinning the chunks of this workload's last home
    /// snapshot (released on re-snapshot/teardown to avoid chunk leaks, N1').
    owned_object_ids: Vec<u64>,
}

/// Default home content-addressed store location (chunks live under `/ram`).
const DEFAULT_HOME_CAS_DIR: &str = "/ram/agents/.sentinel-home-cas";

pub struct BwrapNanoRuntime {
    enforcer: SandboxEnforcer,
    /// Directory holding the home-content `ArtifactPlane`, opened lazily so the
    /// constructor stays infallible and does no I/O (daemon/registry callers that
    /// never snapshot are unaffected).
    cas_dir: PathBuf,
    workloads: HashMap<String, BwrapWorkloadState>,
    handles: HashMap<String, SandboxHandle>,
    processes: HashMap<String, AgentProcess>,
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
            workloads: HashMap::new(),
            handles: HashMap::new(),
            processes: HashMap::new(),
        }
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

    fn home_dir(agent_name: &str) -> PathBuf {
        PathBuf::from(format!("/ram/agents/{agent_name}"))
    }

    fn write_marker(agent_name: &str, workload_id: &str) -> Result<()> {
        let home = Self::home_dir(agent_name);
        std::fs::create_dir_all(&home)?;
        std::fs::write(home.join(".nano-runtime"), workload_id.as_bytes())?;
        Ok(())
    }

    fn remove_marker(agent_name: &str, workload_id: &str) -> Result<bool> {
        let marker = Self::home_dir(agent_name).join(".nano-runtime");
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
        let mut stopped = false;
        if let Some(mut process) = self.processes.remove(workload_id) {
            stopped = true;
            process.terminate();
        }
        if let Some(handle) = self.handles.get(workload_id).cloned() {
            stopped = true;
            self.enforcer.teardown_agent(&handle)?;
            self.handles.remove(workload_id);
        }
        if let Some(state) = self.workloads.get(workload_id) {
            if !state.owned_object_ids.is_empty() {
                let plane = self.open_plane()?;
                home_manifest::release_manifest(&plane, &state.owned_object_ids)?;
            }
            stopped |= Self::remove_marker(&state.workload.agent_name, workload_id)?;
        }
        stopped |= self.workloads.remove(workload_id).is_some();
        Ok(stopped)
    }

    fn spawn_state(&mut self, state: BwrapWorkloadState) -> Result<NanoHandle> {
        let workload = state.workload.clone();
        let agent_name = workload.agent_name.clone();
        Self::write_marker(&agent_name, &workload.workload_id)?;

        let mut handle = self
            .enforcer
            .setup_agent(&agent_name, &CgroupLimits::default())
            .with_context(|| format!("bwrap setup_agent failed for {agent_name}"))?;
        let proc = self
            .enforcer
            .start_agent_process(&agent_name, Some(&workload.workload_id), &state.command)
            .with_context(|| format!("bwrap start_agent_process failed for {agent_name}"))?;
        let pid = proc.pid;
        handle.bwrap_pid = Some(pid);

        self.processes.insert(workload.workload_id.clone(), proc);
        self.handles.insert(workload.workload_id.clone(), handle);
        self.workloads.insert(workload.workload_id.clone(), state);

        Ok(NanoHandle {
            runtime_key: RUNTIME_BWRAP_LANDLOCK.to_string(),
            workload_id: workload.workload_id,
            agent_id: workload.agent_id,
            pid: Some(pid),
        })
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
            .cloned()
            .collect();
        ids.sort();
        ids.dedup();
        for id in ids {
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
        let state = BwrapWorkloadState {
            command: Self::command_for(&workload),
            workload,
            owned_object_ids: Vec::new(),
        };
        self.spawn_state(state)
    }

    fn stop(&mut self, handle: &NanoHandle) -> Result<NanoStopResult> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        Ok(NanoStopResult::new(
            self.runtime_key(),
            &handle.workload_id,
            self.teardown_workload(&handle.workload_id)?,
        ))
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
        // Clone the bits we need so `self` can be re-borrowed mutably below.
        let (workload, command, prev_owned) = {
            let state = self
                .workloads
                .get(&handle.workload_id)
                .ok_or_else(|| anyhow!("unknown bwrap workload '{}'", handle.workload_id))?;
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
        let home = Self::home_dir(&workload.agent_name);
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
        self.teardown_workload(&snapshot.workload_id)?;

        // Rehydrate the agent home from the manifest (metadata-aware, V24 path
        // safety) instead of writing raw bytes back.
        let home = Self::home_dir(&payload.workload.agent_name);
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
            workload: payload.workload,
            command: payload.command,
            owned_object_ids: Vec::new(),
        })
    }

    fn health(&mut self, handle: &NanoHandle) -> Result<NanoHealth> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        let state = if let Some(process) = self.processes.get_mut(&handle.workload_id) {
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
                io_available: false,
                bwrap_pid: Some(pid),
                landlock_applied: false,
                network_isolated: false,
            },
        );
        runtime.workloads.insert(
            workload_id.clone(),
            BwrapWorkloadState {
                workload,
                command: Vec::new(),
                owned_object_ids,
            },
        );
        NanoHandle {
            runtime_key: RUNTIME_BWRAP_LANDLOCK.to_string(),
            workload_id,
            agent_id: None,
            pid: Some(pid),
        }
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
