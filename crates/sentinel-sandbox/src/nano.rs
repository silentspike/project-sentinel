use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use sentinel_common::nano_runtime::{
    NanoExecRequest, NanoExecResult, NanoHandle, NanoHealth, NanoHealthState, NanoIsolationPolicy,
    NanoIsolationReport, NanoRuntime, NanoSnapshot, NanoSnapshotSemantics, NanoWorkloadSpec,
    RUNTIME_BWRAP_LANDLOCK,
};
use serde::{Deserialize, Serialize};

use crate::{cgroups, AgentProcess, CgroupLimits, SandboxEnforcer, SandboxHandle};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BwrapSnapshotPayload {
    workload: NanoWorkloadSpec,
    command: Vec<String>,
    home_files: BTreeMap<String, Vec<u8>>,
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
}

pub struct BwrapNanoRuntime {
    enforcer: SandboxEnforcer,
    workloads: HashMap<String, BwrapWorkloadState>,
    handles: HashMap<String, SandboxHandle>,
    processes: HashMap<String, AgentProcess>,
}

impl BwrapNanoRuntime {
    pub fn detect() -> Self {
        let (enforcer, _warnings) = SandboxEnforcer::detect();
        Self {
            enforcer,
            workloads: HashMap::new(),
            handles: HashMap::new(),
            processes: HashMap::new(),
        }
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

    fn collect_home_files(agent_name: &str) -> Result<BTreeMap<String, Vec<u8>>> {
        let home = Self::home_dir(agent_name);
        let mut files = BTreeMap::new();
        if !home.exists() {
            return Ok(files);
        }

        let mut stack = vec![home.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)
                .with_context(|| format!("read agent home dir {}", dir.display()))?
            {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.is_file() {
                    let rel = path
                        .strip_prefix(&home)
                        .unwrap_or(path.as_path())
                        .to_string_lossy()
                        .to_string();
                    files.insert(rel, std::fs::read(&path)?);
                }
            }
        }
        Ok(files)
    }

    fn restore_home_files(agent_name: &str, files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
        let home = Self::home_dir(agent_name);
        if home.exists() {
            std::fs::remove_dir_all(&home)
                .with_context(|| format!("reset agent home dir {}", home.display()))?;
        }
        std::fs::create_dir_all(&home)?;
        for (rel, bytes) in files {
            let path = home.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, bytes)?;
        }
        Ok(())
    }

    fn write_marker(agent_name: &str, workload_id: &str) -> Result<()> {
        let home = Self::home_dir(agent_name);
        std::fs::create_dir_all(&home)?;
        std::fs::write(home.join(".nano-runtime"), workload_id.as_bytes())?;
        Ok(())
    }

    fn teardown_workload(&mut self, workload_id: &str) {
        if let Some(mut process) = self.processes.remove(workload_id) {
            process.terminate();
        }
        if let Some(handle) = self.handles.remove(workload_id) {
            let _ = self.enforcer.teardown_agent(&handle);
        }
        self.workloads.remove(workload_id);
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
        let ids: Vec<String> = self.handles.keys().cloned().collect();
        for id in ids {
            self.teardown_workload(&id);
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
        };
        self.spawn_state(state)
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
        let state = self
            .workloads
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("unknown bwrap workload '{}'", handle.workload_id))?;
        let sandbox_handle = self
            .handles
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("missing bwrap sandbox handle '{}'", handle.workload_id))?;
        let home_files = Self::collect_home_files(&state.workload.agent_name)?;
        let payload = BwrapSnapshotPayload {
            workload: state.workload.clone(),
            command: state.command.clone(),
            home_files,
            cgroup_created: sandbox_handle.cgroup_created,
            io_available: sandbox_handle.io_available,
            bwrap_available: self.enforcer.has_bwrap(),
            landlock_available: self.enforcer.has_landlock(),
            semantics_note: "bwrap snapshot is config+agent-home filesystem state; no process RAM or CRIU checkpoint".to_string(),
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
        self.teardown_workload(&snapshot.workload_id);
        Self::restore_home_files(&payload.workload.agent_name, &payload.home_files)?;
        self.spawn_state(BwrapWorkloadState {
            workload: payload.workload,
            command: payload.command,
        })
    }

    fn health(&mut self, handle: &NanoHandle) -> Result<NanoHealth> {
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

#[allow(dead_code)]
fn ensure_relative(path: &Path) -> Result<()> {
    if path.is_absolute() {
        return Err(anyhow!("agent-home snapshot path must be relative"));
    }
    Ok(())
}
