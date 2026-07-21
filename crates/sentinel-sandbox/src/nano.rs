use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use sentinel_common::nano_runtime::{
    ensure_handle_runtime, NanoExecRequest, NanoExecResult, NanoHandle, NanoHealth,
    NanoHealthState, NanoIsolationPolicy, NanoIsolationReport, NanoRuntime, NanoRuntimeResources,
    NanoSnapshot, NanoSnapshotSemantics, NanoStopResult, NanoWorkloadSpec, RUNTIME_BWRAP_LANDLOCK,
};
use sentinel_fs::artifact::ArtifactPlane;
use sentinel_fs::home_manifest::{self, HomeManifest, RestorePolicy};
use serde::{Deserialize, Serialize};

use crate::{cgroups, AgentProcess, CgroupLimits, IsolationStatus, SandboxEnforcer, SandboxHandle};

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
    /// Production workbench processes own a dedicated kernel cgroup. Protocol
    /// fixtures deliberately do not and therefore skip only this kernel write.
    enforce_cgroup_limits: bool,
    /// `ArtifactPlane` object ids pinning the chunks of this workload's last home
    /// snapshot (released on re-snapshot/teardown to avoid chunk leaks, N1').
    owned_object_ids: Vec<u64>,
}

#[derive(Debug)]
struct WorkbenchExchange {
    invocation_id: String,
    deadline_unix_ms: u64,
    cancel_requested_at_ms: Option<u64>,
    messages: Vec<serde_json::Value>,
    retained_bytes: usize,
    result_seen: bool,
}

/// Default home content-addressed store location (chunks live under `/ram`).
const DEFAULT_HOME_CAS_DIR: &str = "/ram/agents/.sentinel-home-cas";
const MAX_WORKBENCH_FRAME_BYTES: usize = 1024 * 1024;
const MAX_WORKBENCH_OUTPUT_BYTES: usize = 256 * 1024;
const WORKBENCH_CANCEL_GRACE_MS: u64 = 1_000;
const WORKBENCH_RECOVERY_GRACE_MS: u64 = 5_000;

fn workbench_cgroup_limits() -> CgroupLimits {
    CgroupLimits {
        memory_bytes: 128 * 1024 * 1024,
        process_count: 16,
        ..CgroupLimits::default()
    }
}

pub struct BwrapNanoRuntime {
    enforcer: SandboxEnforcer,
    /// Directory holding the home-content `ArtifactPlane`, opened lazily so the
    /// constructor stays infallible and does no I/O (daemon/registry callers that
    /// never snapshot are unaffected).
    cas_dir: PathBuf,
    workloads: HashMap<String, BwrapWorkloadState>,
    handles: HashMap<String, SandboxHandle>,
    processes: HashMap<String, AgentProcess>,
    exchanges: HashMap<String, WorkbenchExchange>,
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
            exchanges: HashMap::new(),
        }
    }

    /// Keep daemon FUSE routing identical when bwrap lifecycle ownership moves
    /// behind the NanoRuntime adapter.
    pub fn set_fs_mount(&mut self, mount: impl Into<String>) {
        self.enforcer.set_fs_mount(mount.into());
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
        self.exchanges.remove(workload_id);
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

        let is_workbench =
            state.command.first().map(String::as_str) == Some("/usr/bin/agent-runtime");
        let limits = if is_workbench {
            workbench_cgroup_limits()
        } else {
            CgroupLimits::default()
        };
        let mut handle = match self.enforcer.setup_agent(&agent_name, &limits) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = Self::remove_marker(&agent_name, &workload.workload_id);
                return Err(error)
                    .with_context(|| format!("bwrap setup_agent failed for {agent_name}"));
            }
        };
        let mut proc = match self.enforcer.start_agent_process(
            &agent_name,
            Some(&workload.workload_id),
            &state.command,
        ) {
            Ok(proc) => proc,
            Err(error) => {
                let teardown_error = self.enforcer.teardown_agent(&handle).err();
                let marker_error = Self::remove_marker(&agent_name, &workload.workload_id).err();
                return Err(anyhow!(
                    "bwrap start_agent_process failed for {agent_name}: {error}; \
                     cgroup teardown: {}; marker cleanup: {}",
                    teardown_error
                        .as_ref()
                        .map_or_else(|| "ok".to_string(), ToString::to_string),
                    marker_error
                        .as_ref()
                        .map_or_else(|| "ok".to_string(), ToString::to_string)
                ));
            }
        };
        let pid = proc.pid;
        handle.bwrap_pid = Some(pid);
        handle.landlock_applied = proc.landlock_applied;

        if is_workbench {
            let attestation_error = if !handle.cgroup_created {
                Some("dedicated cgroup was not created")
            } else if !handle.landlock_applied {
                Some("Landlock was not applied")
            } else {
                match proc.child_pid {
                    Some(child_pid) => {
                        match self.enforcer.verify_agent_netns_isolation(child_pid) {
                            IsolationStatus::Isolated => {
                                handle.network_isolated = true;
                                None
                            }
                            IsolationStatus::NotIsolated => {
                                Some("sandboxed process shares the daemon network namespace")
                            }
                            IsolationStatus::ProbeError => {
                                Some("network namespace isolation could not be attested")
                            }
                        }
                    }
                    None => Some("sandboxed child PID was not reported"),
                }
            };

            if let Some(reason) = attestation_error {
                proc.terminate();
                let teardown_error = self.enforcer.teardown_agent(&handle).err();
                let marker_error = Self::remove_marker(&agent_name, &workload.workload_id).err();
                return Err(anyhow!(
                    "workbench isolation attestation failed for {agent_name}: {reason}; \
                     cgroup teardown: {}; marker cleanup: {}",
                    teardown_error
                        .as_ref()
                        .map_or_else(|| "ok".to_string(), ToString::to_string),
                    marker_error
                        .as_ref()
                        .map_or_else(|| "ok".to_string(), ToString::to_string)
                ));
            }
        }

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

    fn start_workbench_exchange(
        &mut self,
        handle: &NanoHandle,
        input: &str,
    ) -> Result<NanoExecResult> {
        if input.len() > MAX_WORKBENCH_FRAME_BYTES {
            return Err(anyhow!("workbench protocol frame exceeds the input limit"));
        }
        if self.exchanges.contains_key(&handle.workload_id) {
            return Err(anyhow!("workbench exchange already active for workload"));
        }
        let frame: serde_json::Value =
            serde_json::from_str(input).context("parse workbench start frame")?;
        if frame.get("kind").and_then(|value| value.as_str()) != Some("execute") {
            return Err(anyhow!("workbench start requires an execute frame"));
        }
        let request = frame
            .get("request")
            .and_then(|value| value.as_object())
            .context("workbench execute frame lacks request")?;
        let invocation_id = request
            .get("invocation_id")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .context("workbench request lacks invocation_id")?
            .to_string();
        let deadline_unix_ms = request
            .get("deadline_unix_ms")
            .and_then(|value| value.as_u64())
            .context("workbench request lacks deadline_unix_ms")?;
        if deadline_unix_ms <= unix_time_ms() {
            return Err(anyhow!("workbench request deadline has expired"));
        }
        self.processes
            .get_mut(&handle.workload_id)
            .context("missing bwrap process for workbench exchange")?
            .send_protocol_line(input)?;
        self.exchanges.insert(
            handle.workload_id.clone(),
            WorkbenchExchange {
                invocation_id: invocation_id.clone(),
                deadline_unix_ms,
                cancel_requested_at_ms: None,
                messages: Vec::new(),
                retained_bytes: 0,
                result_seen: false,
            },
        );
        workbench_exec_result(handle, true, &invocation_id, "accepted", Vec::new())
    }

    fn start_workbench_recovery(
        &mut self,
        handle: &NanoHandle,
        input: &str,
    ) -> Result<NanoExecResult> {
        if input.len() > MAX_WORKBENCH_FRAME_BYTES {
            return Err(anyhow!("workbench recovery frame exceeds the input limit"));
        }
        if self.exchanges.contains_key(&handle.workload_id) {
            return Err(anyhow!("workbench exchange already active for workload"));
        }
        let frame: serde_json::Value =
            serde_json::from_str(input).context("parse workbench recovery frame")?;
        if frame.get("kind").and_then(|value| value.as_str()) != Some("recover") {
            return Err(anyhow!("workbench recovery requires a recover frame"));
        }
        let invocation_id = frame
            .get("invocation_id")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .context("workbench recovery frame lacks invocation_id")?
            .to_string();
        let input_digest = frame
            .get("input_digest")
            .and_then(|value| value.as_str())
            .filter(|value| value.len() == 64)
            .context("workbench recovery frame lacks input_digest")?;
        if !input_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(anyhow!("workbench recovery digest is invalid"));
        }
        self.processes
            .get_mut(&handle.workload_id)
            .context("missing bwrap process for workbench recovery")?
            .send_protocol_line(input)?;
        self.exchanges.insert(
            handle.workload_id.clone(),
            WorkbenchExchange {
                invocation_id: invocation_id.clone(),
                deadline_unix_ms: unix_time_ms().saturating_add(WORKBENCH_RECOVERY_GRACE_MS),
                cancel_requested_at_ms: None,
                messages: Vec::new(),
                retained_bytes: 0,
                result_seen: false,
            },
        );
        workbench_exec_result(handle, true, &invocation_id, "accepted", Vec::new())
    }

    fn cancel_workbench_exchange(
        &mut self,
        handle: &NanoHandle,
        input: &str,
    ) -> Result<NanoExecResult> {
        if input.len() > MAX_WORKBENCH_FRAME_BYTES {
            return Err(anyhow!("workbench cancel frame exceeds the input limit"));
        }
        let frame: serde_json::Value =
            serde_json::from_str(input).context("parse workbench cancel frame")?;
        if frame.get("kind").and_then(|value| value.as_str()) != Some("cancel") {
            return Err(anyhow!("workbench cancel requires a cancel frame"));
        }
        let invocation_id = frame
            .get("invocation_id")
            .and_then(|value| value.as_str())
            .context("workbench cancel frame lacks invocation_id")?;
        let exchange = self
            .exchanges
            .get_mut(&handle.workload_id)
            .context("no active workbench exchange")?;
        if exchange.invocation_id != invocation_id {
            return Err(anyhow!(
                "workbench cancel invocation does not match active exchange"
            ));
        }
        self.processes
            .get_mut(&handle.workload_id)
            .context("missing bwrap process for workbench cancellation")?
            .send_protocol_line(input)?;
        exchange
            .cancel_requested_at_ms
            .get_or_insert(unix_time_ms());
        workbench_exec_result(handle, true, invocation_id, "cancelling", Vec::new())
    }

    fn poll_workbench_exchange(
        &mut self,
        handle: &NanoHandle,
        invocation_id: &str,
    ) -> Result<NanoExecResult> {
        let lines = match self
            .processes
            .get_mut(&handle.workload_id)
            .context("missing bwrap process for workbench poll")?
            .drain_protocol_lines()
        {
            Ok(lines) => lines,
            Err(error) => {
                let _ = self.teardown_workload(&handle.workload_id);
                return Err(error.context("workbench protocol channel failed closed"));
            }
        };
        let now_ms = unix_time_ms();
        let mut terminal = false;
        let mut kill_for_bound = false;
        {
            let exchange = self
                .exchanges
                .get_mut(&handle.workload_id)
                .context("no active workbench exchange")?;
            if exchange.invocation_id != invocation_id {
                return Err(anyhow!(
                    "workbench poll invocation does not match active exchange"
                ));
            }
            for line in lines {
                exchange.retained_bytes = exchange.retained_bytes.saturating_add(line.len());
                if exchange.retained_bytes > MAX_WORKBENCH_OUTPUT_BYTES {
                    kill_for_bound = true;
                    break;
                }
                let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
                    kill_for_bound = true;
                    break;
                };
                if message
                    .get("schema_version")
                    .and_then(|value| value.as_u64())
                    != Some(1)
                {
                    kill_for_bound = true;
                    break;
                }
                let message_invocation = message
                    .get("invocation_id")
                    .and_then(|value| value.as_str());
                if message_invocation != Some(exchange.invocation_id.as_str()) {
                    kill_for_bound = true;
                    break;
                }
                match message.get("kind").and_then(|value| value.as_str()) {
                    Some("result") => {
                        if exchange.result_seen {
                            kill_for_bound = true;
                            break;
                        }
                        exchange.result_seen = true;
                    }
                    Some("progress")
                        if message.get("stage").and_then(|value| value.as_str())
                            == Some("completed") =>
                    {
                        if !exchange.result_seen {
                            kill_for_bound = true;
                            break;
                        }
                        terminal = true;
                    }
                    Some("error") => terminal = true,
                    Some("progress") | Some("cancelled") => {}
                    _ => {
                        kill_for_bound = true;
                        break;
                    }
                }
                exchange.messages.push(message);
            }
            if !terminal {
                if let Some(cancelled_at) = exchange.cancel_requested_at_ms {
                    kill_for_bound =
                        now_ms.saturating_sub(cancelled_at) >= WORKBENCH_CANCEL_GRACE_MS;
                } else if now_ms >= exchange.deadline_unix_ms {
                    exchange.cancel_requested_at_ms = Some(now_ms);
                }
            }
        }
        if kill_for_bound {
            let _ = self.teardown_workload(&handle.workload_id);
            return Err(anyhow!(
                "workbench exchange violated its protocol, output, or cancellation bound"
            ));
        }
        let exchange = self
            .exchanges
            .get_mut(&handle.workload_id)
            .context("workbench exchange disappeared during poll")?;
        if now_ms >= exchange.deadline_unix_ms && exchange.cancel_requested_at_ms == Some(now_ms) {
            let cancel = serde_json::json!({
                "kind": "cancel",
                "schema_version": 1,
                "invocation_id": exchange.invocation_id,
                "reason": "deadline_expired"
            });
            self.processes
                .get_mut(&handle.workload_id)
                .context("missing bwrap process for deadline cancellation")?
                .send_protocol_line(&serde_json::to_string(&cancel)?)?;
        }
        let messages = std::mem::take(&mut exchange.messages);
        let state = if terminal { "completed" } else { "pending" };
        let result = workbench_exec_result(handle, true, invocation_id, state, messages)?;
        if terminal {
            self.exchanges.remove(&handle.workload_id);
        }
        Ok(result)
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn workbench_exec_result(
    handle: &NanoHandle,
    success: bool,
    invocation_id: &str,
    state: &str,
    messages: Vec<serde_json::Value>,
) -> Result<NanoExecResult> {
    Ok(NanoExecResult {
        runtime_key: handle.runtime_key.clone(),
        workload_id: handle.workload_id.clone(),
        success,
        output: serde_json::to_string(&serde_json::json!({
            "schema_version": 1,
            "invocation_id": invocation_id,
            "state": state,
            "messages": messages,
        }))?,
    })
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
            enforce_cgroup_limits: true,
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

    fn resources(&self, handle: &NanoHandle) -> Result<NanoRuntimeResources> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        let sandbox = self
            .handles
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("missing bwrap sandbox handle '{}'", handle.workload_id))?;
        let process = self
            .processes
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("missing bwrap process '{}'", handle.workload_id))?;
        Ok(NanoRuntimeResources {
            pid: Some(process.pid),
            child_pid: process.child_pid,
            cgroup_created: sandbox.cgroup_created,
            io_available: sandbox.io_available,
            landlock_applied: sandbox.landlock_applied,
            network_isolated: sandbox.network_isolated,
        })
    }

    fn exec(&mut self, handle: &NanoHandle, request: NanoExecRequest) -> Result<NanoExecResult> {
        match request.operation.as_str() {
            "health" => {
                let health = self.health(handle)?;
                Ok(NanoExecResult {
                    runtime_key: self.runtime_key().to_string(),
                    workload_id: handle.workload_id.clone(),
                    success: true,
                    output: format!("{:?}", health.state),
                })
            }
            "workbench_start" => {
                if self.health(handle)?.state != NanoHealthState::Healthy {
                    return Err(anyhow!("bwrap workload is not healthy"));
                }
                let state = self
                    .workloads
                    .get(&handle.workload_id)
                    .ok_or_else(|| anyhow!("unknown bwrap workload '{}'", handle.workload_id))?;
                if state.enforce_cgroup_limits {
                    cgroups::resize_cgroup(&state.workload.agent_name, &workbench_cgroup_limits())
                        .context("restore workbench cgroup ceiling before execution")?;
                }
                self.start_workbench_exchange(handle, &request.input)
            }
            "workbench_recover" => {
                if self.health(handle)?.state != NanoHealthState::Healthy {
                    return Err(anyhow!("bwrap workload is not healthy"));
                }
                self.start_workbench_recovery(handle, &request.input)
            }
            "workbench_poll" => self.poll_workbench_exchange(handle, &request.input),
            "workbench_cancel" => self.cancel_workbench_exchange(handle, &request.input),
            other => Err(anyhow!("bwrap exec operation '{other}' is not supported")),
        }
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
            enforce_cgroup_limits: true,
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
                enforce_cgroup_limits: false,
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

    fn insert_protocol_fixture(
        runtime: &mut BwrapNanoRuntime,
        workload_id: &str,
        lines: &[&str],
    ) -> NanoHandle {
        insert_protocol_process(
            runtime,
            workload_id,
            AgentProcess::launch_protocol_fixture(lines).unwrap(),
        )
    }

    fn insert_protocol_process(
        runtime: &mut BwrapNanoRuntime,
        workload_id: &str,
        process: AgentProcess,
    ) -> NanoHandle {
        let workload = fixture_workload(workload_id, "agent-protocol-fixture");
        let pid = process.pid;
        runtime.processes.insert(workload_id.to_string(), process);
        runtime.handles.insert(
            workload_id.to_string(),
            SandboxHandle {
                agent_name: workload.agent_name.clone(),
                cgroup_created: false,
                io_available: false,
                bwrap_pid: Some(pid),
                landlock_applied: false,
                network_isolated: true,
            },
        );
        runtime.workloads.insert(
            workload_id.to_string(),
            BwrapWorkloadState {
                workload,
                command: Vec::new(),
                enforce_cgroup_limits: false,
                owned_object_ids: Vec::new(),
            },
        );
        NanoHandle {
            runtime_key: RUNTIME_BWRAP_LANDLOCK.to_string(),
            workload_id: workload_id.to_string(),
            agent_id: None,
            pid: Some(pid),
        }
    }

    fn start_frame(invocation_id: &str, deadline_unix_ms: u64) -> String {
        serde_json::to_string(&serde_json::json!({
            "kind": "execute",
            "request": {
                "schema_version": 1,
                "invocation_id": invocation_id,
                "deadline_unix_ms": deadline_unix_ms
            }
        }))
        .unwrap()
    }

    fn recovery_frame(invocation_id: &str, input_digest: &str) -> String {
        serde_json::to_string(&serde_json::json!({
            "kind": "recover",
            "schema_version": 1,
            "invocation_id": invocation_id,
            "input_digest": input_digest,
        }))
        .unwrap()
    }

    fn poll_until_terminal(
        registry: &mut sentinel_common::nano_runtime::NanoRuntimeRegistry,
        handle: &NanoHandle,
        invocation_id: &str,
    ) -> NanoExecResult {
        for _ in 0..100 {
            let result = registry
                .exec(
                    handle,
                    NanoExecRequest {
                        operation: "workbench_poll".to_string(),
                        input: invocation_id.to_string(),
                    },
                )
                .unwrap();
            let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
            if output["state"] == "completed" {
                return result;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("protocol fixture did not reach a terminal state");
    }

    #[test]
    fn registry_exec_channel_returns_only_matching_bounded_terminal_exchange() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2901";
        let progress = format!(
            r#"{{"kind":"progress","schema_version":1,"invocation_id":"{invocation_id}","stage":"validated","elapsed_ms":0}}"#
        );
        let result = format!(
            r#"{{"kind":"result","schema_version":1,"invocation_id":"{invocation_id}","input_digest":"{}","outcome":"succeeded","resources":{{"duration_ms":1,"cpu_time_ms":0,"peak_memory_bytes":0,"peak_process_count":0,"bytes_read":0,"bytes_written":0,"artifact_bytes":0}},"artifacts":[],"output":{{}},"error":null}}"#,
            "a".repeat(64)
        );
        let completed = format!(
            r#"{{"kind":"progress","schema_version":1,"invocation_id":"{invocation_id}","stage":"completed","elapsed_ms":1}}"#
        );
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle = insert_protocol_fixture(
            &mut runtime,
            "protocol-success",
            &[&progress, &result, &completed],
        );
        let mut registry = sentinel_common::nano_runtime::NanoRuntimeRegistry::new(None);
        registry.register(runtime).unwrap();
        let accepted = registry
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();
        assert!(accepted.success);
        let terminal = poll_until_terminal(&mut registry, &handle, invocation_id);
        let output: serde_json::Value = serde_json::from_str(&terminal.output).unwrap();
        assert_eq!(output["invocation_id"], invocation_id);
        assert_eq!(output["state"], "completed");
        assert_eq!(output["messages"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn registry_recovery_channel_returns_digest_bound_receipt_without_reexecution() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2910";
        let input_digest = "a".repeat(64);
        let result = format!(
            r#"{{"kind":"result","schema_version":1,"invocation_id":"{invocation_id}","input_digest":"{input_digest}","outcome":"succeeded","resources":{{"duration_ms":1,"cpu_time_ms":0,"peak_memory_bytes":0,"peak_process_count":0,"bytes_read":0,"bytes_written":1,"artifact_bytes":0}},"artifacts":[],"output":{{}},"error":null}}"#
        );
        let completed = format!(
            r#"{{"kind":"progress","schema_version":1,"invocation_id":"{invocation_id}","stage":"completed","elapsed_ms":0}}"#
        );
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle =
            insert_protocol_fixture(&mut runtime, "protocol-recovery", &[&result, &completed]);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_recover".to_string(),
                    input: recovery_frame(invocation_id, &input_digest),
                },
            )
            .unwrap();
        let mut registry = sentinel_common::nano_runtime::NanoRuntimeRegistry::new(None);
        registry.register(runtime).unwrap();
        let terminal = poll_until_terminal(&mut registry, &handle, invocation_id);
        let output: serde_json::Value = serde_json::from_str(&terminal.output).unwrap();
        assert_eq!(output["state"], "completed");
        assert_eq!(output["messages"][0]["input_digest"], input_digest);
    }

    #[test]
    fn foreign_invocation_output_kills_the_selected_workload() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2902";
        let foreign = r#"{"kind":"error","schema_version":1,"invocation_id":"foreign","error":{"class":"protocol","code":"bad","safe_message":"bad","retryable":false}}"#;
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle = insert_protocol_fixture(&mut runtime, "protocol-foreign", &[foreign]);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();
        let mut rejected = false;
        for _ in 0..100 {
            match runtime.exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: invocation_id.to_string(),
                },
            ) {
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(error) => {
                    assert!(error.to_string().contains("violated"));
                    rejected = true;
                    break;
                }
            }
        }
        assert!(rejected);
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Stopped
        );
    }

    #[test]
    fn malformed_output_fails_closed_without_reflecting_private_content() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2906";
        let private_line = "not-json SECRET=must-not-be-reflected";
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle = insert_protocol_fixture(&mut runtime, "protocol-malformed", &[private_line]);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();
        let mut failure = None;
        for _ in 0..100 {
            match runtime.exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: invocation_id.to_string(),
                },
            ) {
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        let error = failure.expect("malformed output must fail closed");
        assert!(error.to_string().contains("violated"));
        assert!(!error.to_string().contains("SECRET"));
    }

    #[test]
    fn unacknowledged_cancel_is_bounded_and_kills_the_process_tree() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2903";
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle = insert_protocol_fixture(&mut runtime, "protocol-cancel", &[]);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();
        let cancel = serde_json::json!({
            "kind": "cancel",
            "schema_version": 1,
            "invocation_id": invocation_id,
            "reason": "operator_cancelled"
        });
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_cancel".to_string(),
                    input: serde_json::to_string(&cancel).unwrap(),
                },
            )
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(
            WORKBENCH_CANCEL_GRACE_MS + 20,
        ));
        let error = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: invocation_id.to_string(),
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("cancellation bound"));
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Stopped
        );
    }

    #[test]
    fn deadline_expiry_sends_cancel_then_kills_after_the_fixed_grace() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2904";
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle = insert_protocol_fixture(&mut runtime, "protocol-deadline", &[]);
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 20),
                },
            )
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let pending = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: invocation_id.to_string(),
                },
            )
            .unwrap();
        assert!(pending.output.contains("pending"));
        std::thread::sleep(std::time::Duration::from_millis(
            WORKBENCH_CANCEL_GRACE_MS + 20,
        ));
        assert!(runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: invocation_id.to_string(),
                },
            )
            .unwrap_err()
            .to_string()
            .contains("cancellation bound"));
    }

    #[test]
    fn protocol_eof_fails_closed_and_removes_the_workload() {
        let invocation_id = "018f3f32-4f01-7f2c-a6c1-f6f4a81b2905";
        let mut runtime = BwrapNanoRuntime::with_cas_dir(tempfile::tempdir().unwrap().path());
        let handle = insert_protocol_process(
            &mut runtime,
            "protocol-eof",
            AgentProcess::launch_protocol_eof_fixture().unwrap(),
        );
        runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_start".to_string(),
                    input: start_frame(invocation_id, unix_time_ms() + 10_000),
                },
            )
            .unwrap();
        let mut failed_closed = false;
        for _ in 0..100 {
            match runtime.exec(
                &handle,
                NanoExecRequest {
                    operation: "workbench_poll".to_string(),
                    input: invocation_id.to_string(),
                },
            ) {
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(error) => {
                    assert!(error.to_string().contains("failed closed"));
                    failed_closed = true;
                    break;
                }
            }
        }
        assert!(failed_closed);
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Stopped
        );
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
