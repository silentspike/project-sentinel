#![cfg(feature = "wasm")]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use sentinel_common::nano_runtime::{
    ensure_handle_instance, ensure_handle_runtime, NanoExecRequest, NanoExecResult, NanoHandle,
    NanoHealth, NanoHealthState, NanoIsolationPolicy, NanoIsolationReport, NanoRuntime,
    NanoRuntimeControlAction, NanoRuntimeControlResult, NanoRuntimeResources, NanoSnapshot,
    NanoSnapshotSemantics, NanoStopResult, NanoWorkloadSpec, RUNTIME_WASM_WASMTIME,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    AgentSnapshot, ExecutionContext, PluginConfig, SandboxConfig, ToolDefinition, ToolRuntime,
    ToolType,
};

const WASM_RUNTIME_SNAPSHOT_VERSION: u16 = 2;

fn legacy_wasm_runtime_snapshot_version() -> u16 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WasmBoundExecution {
    /// SHA-256 of the input whose completed result is bound below. Legacy
    /// output-only snapshots have no input digest.
    #[serde(default)]
    input_sha256: Option<String>,
    output: String,
    success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WasmRuntimeSnapshot {
    #[serde(default = "legacy_wasm_runtime_snapshot_version")]
    schema_version: u16,
    workload: NanoWorkloadSpec,
    tool_name: String,
    wasm_path: String,
    /// A completed result is observational runtime state, not a command to
    /// execute during restore.
    #[serde(default)]
    bound_execution: Option<WasmBoundExecution>,
    /// Legacy v1 fields are decode-only. Restore may bind their completed
    /// result, but it must never replay `last_input`.
    #[serde(default)]
    last_input: Option<String>,
    #[serde(default)]
    last_output: Option<String>,
    semantics_note: String,
}

#[derive(Debug, Clone)]
struct WasmWorkloadState {
    instance_id: uuid::Uuid,
    workload: NanoWorkloadSpec,
    tool_name: String,
    wasm_path: PathBuf,
    bound_execution: Option<WasmBoundExecution>,
}

pub struct WasmtimeNanoRuntime {
    runtime: ToolRuntime,
    workloads: HashMap<String, WasmWorkloadState>,
    suspended: HashSet<String>,
}

impl WasmtimeNanoRuntime {
    pub fn new() -> Self {
        Self {
            runtime: ToolRuntime::new(),
            workloads: HashMap::new(),
            suspended: HashSet::new(),
        }
    }

    fn state_from_workload(workload: &NanoWorkloadSpec) -> Result<WasmWorkloadState> {
        let wasm_path = workload
            .metadata
            .get("wasm_path")
            .ok_or_else(|| anyhow!("wasm-wasmtime workload requires metadata.wasm_path"))?;
        let tool_name = workload
            .metadata
            .get("tool_name")
            .cloned()
            .unwrap_or_else(|| workload.workload_id.clone());

        let wasm_path = PathBuf::from(wasm_path).canonicalize().with_context(|| {
            format!("canonicalize WASM component for '{}'", workload.workload_id)
        })?;

        Ok(WasmWorkloadState {
            instance_id: uuid::Uuid::new_v4(),
            workload: workload.clone(),
            tool_name,
            wasm_path,
            bound_execution: None,
        })
    }

    fn ensure_snapshot_identity(
        snapshot_workload_id: &str,
        workload: &NanoWorkloadSpec,
    ) -> Result<()> {
        if workload.workload_id != snapshot_workload_id {
            return Err(anyhow!(
                "WASM snapshot workload '{}' does not match envelope '{}'",
                workload.workload_id,
                snapshot_workload_id
            ));
        }
        Ok(())
    }

    fn load_state(&mut self, state: &WasmWorkloadState) -> Result<()> {
        if let Some(existing) = self.runtime.get_tool(&state.tool_name) {
            let existing_path = existing
                .wasm_path
                .as_deref()
                .map(PathBuf::from)
                .and_then(|path| path.canonicalize().ok());
            if existing.tool_type != ToolType::Wasm
                || existing_path.as_deref() != Some(state.wasm_path.as_path())
            {
                return Err(anyhow!(
                    "WASM tool '{}' is already bound to a different component",
                    state.tool_name
                ));
            }
        }
        if !self.runtime.plugin_host().is_loaded(&state.wasm_path) {
            self.runtime.plugin_host_mut().load(PluginConfig {
                wasm_path: state.wasm_path.clone(),
                ..PluginConfig::default()
            })?;
        }
        if self.runtime.get_tool(&state.tool_name).is_none() {
            self.runtime.register_tool(ToolDefinition {
                name: state.tool_name.clone(),
                description: "NanoRuntime WASM workload".to_string(),
                wasm_path: Some(state.wasm_path.to_string_lossy().to_string()),
                tool_type: ToolType::Wasm,
                required_capabilities: vec![state.tool_name.clone()],
            })?;
        }
        Ok(())
    }

    fn release_unreferenced_resources(&mut self, state: &WasmWorkloadState) {
        if !self
            .workloads
            .values()
            .any(|other| other.tool_name == state.tool_name)
        {
            self.runtime.unregister_tool(&state.tool_name);
        }
        if !self
            .workloads
            .values()
            .any(|other| other.wasm_path == state.wasm_path)
        {
            self.runtime.plugin_host_mut().unload(&state.wasm_path);
        }
    }

    fn execution_context(state: &WasmWorkloadState) -> ExecutionContext {
        ExecutionContext {
            agent_id: state.workload.agent_name.clone(),
            agent_capabilities: vec![state.tool_name.clone()],
            sandbox: SandboxConfig::restrictive(),
            correlation_id: format!("nano-runtime-{}", state.workload.workload_id),
            tick: 0,
            agent_snapshot: Some(AgentSnapshot {
                agent_id: state
                    .workload
                    .agent_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| state.workload.workload_id.clone()),
                name: state.workload.agent_name.clone(),
                role: state.workload.role.clone(),
                room_id: state.workload.room_id.clone(),
                ..AgentSnapshot::default()
            }),
            rooms: Some(HashMap::new()),
        }
    }

    fn snapshot_payload(state: &WasmWorkloadState) -> Result<serde_json::Value> {
        let payload = WasmRuntimeSnapshot {
            schema_version: WASM_RUNTIME_SNAPSHOT_VERSION,
            workload: state.workload.clone(),
            tool_name: state.tool_name.clone(),
            wasm_path: state.wasm_path.to_string_lossy().to_string(),
            bound_execution: state.bound_execution.clone(),
            last_input: None,
            last_output: None,
            semantics_note:
                "wasm-wasmtime snapshot is declarative workload state plus an already-bound result; restore never re-executes stored input and external-effect retries require a durable idempotency receipt"
                    .to_string(),
        };
        Ok(serde_json::to_value(payload)?)
    }

    fn input_sha256(input: &str) -> String {
        format!("{:x}", Sha256::digest(input.as_bytes()))
    }

    fn bound_execution_from_snapshot(
        payload: &WasmRuntimeSnapshot,
    ) -> Result<Option<WasmBoundExecution>> {
        if let Some(bound) = &payload.bound_execution {
            return Ok(Some(bound.clone()));
        }
        match (&payload.last_input, &payload.last_output) {
            (Some(input), Some(output)) => Ok(Some(WasmBoundExecution {
                input_sha256: Some(Self::input_sha256(input)),
                output: output.clone(),
                success: true,
            })),
            (None, Some(output)) => Ok(Some(WasmBoundExecution {
                input_sha256: None,
                output: output.clone(),
                success: true,
            })),
            (Some(_), None) => Err(anyhow!(
                "legacy WASM snapshot contains an input without a bound result; replay during restore is forbidden"
            )),
            (None, None) => Ok(None),
        }
    }
}

impl Default for WasmtimeNanoRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl NanoRuntime for WasmtimeNanoRuntime {
    fn runtime_key(&self) -> &'static str {
        RUNTIME_WASM_WASMTIME
    }

    fn spawn(&mut self, workload: NanoWorkloadSpec) -> Result<NanoHandle> {
        if self.workloads.contains_key(&workload.workload_id) {
            return Err(anyhow!(
                "wasm workload '{}' is already active",
                workload.workload_id
            ));
        }
        let state = Self::state_from_workload(&workload)?;
        let instance_id = state.instance_id;
        if let Err(error) = self.load_state(&state) {
            self.release_unreferenced_resources(&state);
            return Err(error);
        }
        self.workloads.insert(workload.workload_id.clone(), state);
        Ok(NanoHandle {
            instance_id,
            runtime_key: self.runtime_key().to_string(),
            workload_id: workload.workload_id,
            agent_id: workload.agent_id,
            pid: None,
        })
    }

    fn stop(&mut self, handle: &NanoHandle) -> Result<NanoStopResult> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        let Some(state) = self.workloads.get(&handle.workload_id) else {
            return Ok(NanoStopResult::new(
                self.runtime_key(),
                &handle.workload_id,
                false,
            ));
        };
        ensure_handle_instance(handle, state.instance_id)?;
        let state = self
            .workloads
            .remove(&handle.workload_id)
            .expect("workload checked above");
        self.suspended.remove(&handle.workload_id);
        self.release_unreferenced_resources(&state);
        Ok(NanoStopResult::new(
            self.runtime_key(),
            &handle.workload_id,
            true,
        ))
    }

    fn resources(&self, handle: &NanoHandle) -> Result<NanoRuntimeResources> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        let state = self
            .workloads
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("unknown WASM workload '{}'", handle.workload_id))?;
        ensure_handle_instance(handle, state.instance_id)?;
        Ok(NanoRuntimeResources {
            instance_id: Some(state.instance_id),
            ..NanoRuntimeResources::default()
        })
    }

    fn exec(&mut self, handle: &NanoHandle, request: NanoExecRequest) -> Result<NanoExecResult> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        if self.suspended.contains(&handle.workload_id) {
            return Err(anyhow!(
                "WASM workload '{}' is suspended",
                handle.workload_id
            ));
        }
        let state = self
            .workloads
            .get(&handle.workload_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown wasm workload '{}'", handle.workload_id))?;
        ensure_handle_instance(handle, state.instance_id)?;
        let input = if request.input.is_empty() {
            request.operation
        } else {
            request.input
        };
        let ctx = Self::execution_context(&state);
        let result = self.runtime.execute(&state.tool_name, &input, &ctx)?;

        if let Some(stored) = self.workloads.get_mut(&handle.workload_id) {
            stored.bound_execution = Some(WasmBoundExecution {
                input_sha256: Some(Self::input_sha256(&input)),
                output: result.output.clone(),
                success: result.success,
            });
        }

        Ok(NanoExecResult {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            success: result.success,
            output: result.output,
        })
    }

    fn snapshot(&mut self, handle: &NanoHandle) -> Result<NanoSnapshot> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        let state = self
            .workloads
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("unknown wasm workload '{}'", handle.workload_id))?;
        ensure_handle_instance(handle, state.instance_id)?;
        Ok(NanoSnapshot {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            agent_id: handle.agent_id,
            semantics: NanoSnapshotSemantics::WasmBoundState,
            payload: Self::snapshot_payload(state)?,
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
        if !matches!(
            snapshot.semantics,
            NanoSnapshotSemantics::WasmBoundState | NanoSnapshotSemantics::WasmReexecute
        ) {
            return Err(anyhow!(
                "cannot restore {:?} snapshot into {} runtime",
                snapshot.semantics,
                self.runtime_key()
            ));
        }
        let payload: WasmRuntimeSnapshot = serde_json::from_value(snapshot.payload)?;
        if payload.schema_version == 0 || payload.schema_version > WASM_RUNTIME_SNAPSHOT_VERSION {
            return Err(anyhow!(
                "unsupported WASM runtime snapshot schema {}",
                payload.schema_version
            ));
        }
        let bound_execution = Self::bound_execution_from_snapshot(&payload)?;
        let mut metadata = payload.workload.metadata.clone();
        metadata.insert("wasm_path".to_string(), payload.wasm_path.clone());
        metadata.insert("tool_name".to_string(), payload.tool_name.clone());
        let workload = NanoWorkloadSpec {
            metadata,
            ..payload.workload
        };
        Self::ensure_snapshot_identity(&snapshot.workload_id, &workload)?;
        let mut state = Self::state_from_workload(&workload)?;
        if let Err(error) = self.load_state(&state) {
            self.release_unreferenced_resources(&state);
            return Err(error);
        }
        state.bound_execution = bound_execution;

        let instance_id = state.instance_id;
        self.suspended.remove(&snapshot.workload_id);
        if let Some(previous) = self.workloads.insert(snapshot.workload_id.clone(), state) {
            self.release_unreferenced_resources(&previous);
        }
        Ok(NanoHandle {
            instance_id,
            runtime_key: self.runtime_key().to_string(),
            workload_id: snapshot.workload_id,
            agent_id: snapshot.agent_id,
            pid: None,
        })
    }

    fn health(&mut self, handle: &NanoHandle) -> Result<NanoHealth> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        let Some(state) = self.workloads.get(&handle.workload_id) else {
            return Ok(NanoHealth {
                runtime_key: self.runtime_key().to_string(),
                workload_id: handle.workload_id.clone(),
                state: NanoHealthState::Stopped,
                detail: "Wasm workload stopped".to_string(),
            });
        };
        ensure_handle_instance(handle, state.instance_id)?;
        let loaded = self.runtime.plugin_host().is_loaded(&state.wasm_path);
        Ok(NanoHealth {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            state: if self.suspended.contains(&handle.workload_id) {
                NanoHealthState::Degraded
            } else if loaded {
                NanoHealthState::Healthy
            } else {
                NanoHealthState::Unavailable
            },
            detail: "Wasmtime component cached; execution uses a fresh Store per call".to_string(),
        })
    }

    fn isolate(
        &mut self,
        handle: &NanoHandle,
        policy: NanoIsolationPolicy,
    ) -> Result<NanoIsolationReport> {
        self.resources(handle)?;
        Ok(NanoIsolationReport {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            applied: true,
            detail: format!(
                "WASI capability isolation and fuel limit {:?}",
                policy.wasm_fuel
            ),
        })
    }

    fn control(
        &mut self,
        handle: &NanoHandle,
        action: NanoRuntimeControlAction,
    ) -> Result<NanoRuntimeControlResult> {
        self.resources(handle)?;
        let applied = match action {
            NanoRuntimeControlAction::Suspend => self.suspended.insert(handle.workload_id.clone()),
            NanoRuntimeControlAction::Resume => self.suspended.remove(&handle.workload_id),
        };
        Ok(NanoRuntimeControlResult::new(
            self.runtime_key(),
            &handle.workload_id,
            action,
            applied,
            0,
        ))
    }
}

pub fn wasm_conformance_metadata(wasm_path: PathBuf, tool_name: &str) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "wasm_path".to_string(),
        wasm_path.to_string_lossy().to_string(),
    );
    metadata.insert("tool_name".to_string(), tool_name.to_string());
    metadata
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/echo-plugin.wasm")
    }

    fn fs_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fs-plugin.wasm")
    }

    fn workload(workload_id: &str) -> NanoWorkloadSpec {
        NanoWorkloadSpec {
            workload_id: workload_id.to_string(),
            runtime_key: Some(RUNTIME_WASM_WASMTIME.to_string()),
            agent_id: None,
            agent_name: workload_id.to_string(),
            role: "Tester".to_string(),
            room_id: "empfang".to_string(),
            shift_set: 1,
            command: Vec::new(),
            capabilities: vec!["echo".to_string()],
            metadata: wasm_conformance_metadata(echo_fixture(), "echo"),
            ecs_snapshot: None,
        }
    }

    fn workload_with(workload_id: &str, wasm_path: PathBuf, tool_name: &str) -> NanoWorkloadSpec {
        let mut workload = workload(workload_id);
        workload.metadata = wasm_conformance_metadata(wasm_path, tool_name);
        workload.capabilities = vec![tool_name.to_string()];
        workload
    }

    #[test]
    fn stop_releases_tool_and_component_after_last_workload() {
        let mut runtime = WasmtimeNanoRuntime::new();
        let handle_a = runtime.spawn(workload("wasm-cleanup-a")).unwrap();
        let handle_b = runtime.spawn(workload("wasm-cleanup-b")).unwrap();

        assert_eq!(runtime.runtime.tool_count(), 1);
        assert_eq!(runtime.runtime.plugin_host().cached_count(), 1);
        runtime.stop(&handle_a).unwrap();
        assert_eq!(runtime.runtime.tool_count(), 1);
        assert_eq!(runtime.runtime.plugin_host().cached_count(), 1);

        runtime.stop(&handle_b).unwrap();
        assert_eq!(runtime.runtime.tool_count(), 0);
        assert_eq!(runtime.runtime.plugin_host().cached_count(), 0);
    }

    #[test]
    fn duplicate_workload_id_is_rejected_without_replacing_owner() {
        let mut runtime = WasmtimeNanoRuntime::new();
        let handle = runtime.spawn(workload("wasm-duplicate")).unwrap();
        assert!(runtime.spawn(workload("wasm-duplicate")).is_err());
        assert_eq!(
            runtime.stop(&handle).unwrap().outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
    }

    #[test]
    fn tool_name_collision_with_different_component_fails_closed() {
        let mut runtime = WasmtimeNanoRuntime::new();
        let owner = runtime
            .spawn(workload_with("wasm-owner", echo_fixture(), "shared-tool"))
            .unwrap();

        assert!(runtime
            .spawn(workload_with("wasm-alias", fs_fixture(), "shared-tool"))
            .is_err());
        assert_eq!(runtime.runtime.tool_count(), 1);
        assert_eq!(runtime.runtime.plugin_host().cached_count(), 1);
        assert_eq!(
            runtime.stop(&owner).unwrap().outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
        assert_eq!(runtime.runtime.tool_count(), 0);
        assert_eq!(runtime.runtime.plugin_host().cached_count(), 0);
    }

    #[test]
    fn stop_releases_distinct_tools_sharing_one_component() {
        let mut runtime = WasmtimeNanoRuntime::new();
        let handle_a = runtime
            .spawn(workload_with("wasm-tool-a", echo_fixture(), "echo-a"))
            .unwrap();
        let handle_b = runtime
            .spawn(workload_with("wasm-tool-b", echo_fixture(), "echo-b"))
            .unwrap();

        assert_eq!(runtime.runtime.tool_count(), 2);
        assert_eq!(runtime.runtime.plugin_host().cached_count(), 1);
        runtime.stop(&handle_a).unwrap();
        assert_eq!(runtime.runtime.tool_count(), 1);
        assert_eq!(runtime.runtime.plugin_host().cached_count(), 1);
        runtime.stop(&handle_b).unwrap();
        assert_eq!(runtime.runtime.tool_count(), 0);
        assert_eq!(runtime.runtime.plugin_host().cached_count(), 0);
    }

    #[test]
    fn stop_unloads_component_after_source_file_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        let copied = temp.path().join("echo-plugin.wasm");
        std::fs::copy(echo_fixture(), &copied).unwrap();
        let mut runtime = WasmtimeNanoRuntime::new();
        let handle = runtime
            .spawn(workload_with("wasm-removed-source", copied.clone(), "echo"))
            .unwrap();
        std::fs::remove_file(copied).unwrap();

        runtime.stop(&handle).unwrap();
        assert_eq!(runtime.runtime.tool_count(), 0);
        assert_eq!(runtime.runtime.plugin_host().cached_count(), 0);
    }

    #[test]
    fn restore_rejects_mismatched_workload_identity_without_loading_component() {
        let runtime = WasmtimeNanoRuntime::new();
        let workload = workload("payload-workload");

        assert!(
            WasmtimeNanoRuntime::ensure_snapshot_identity("envelope-workload", &workload).is_err()
        );
        assert_eq!(runtime.runtime.tool_count(), 0);
        assert_eq!(runtime.runtime.plugin_host().cached_count(), 0);
    }

    #[test]
    fn snapshot_binds_completed_result_without_storing_a_replay_command() {
        let mut runtime = WasmtimeNanoRuntime::new();
        let handle = runtime.spawn(workload("wasm-bound-result")).unwrap();
        let result = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "echo".to_string(),
                    input: "completed input".to_string(),
                },
            )
            .unwrap();
        let snapshot = runtime.snapshot(&handle).unwrap();

        assert_eq!(snapshot.semantics, NanoSnapshotSemantics::WasmBoundState);
        assert_eq!(
            snapshot.payload["schema_version"],
            WASM_RUNTIME_SNAPSHOT_VERSION
        );
        assert_eq!(snapshot.payload["bound_execution"]["output"], result.output);
        assert_eq!(snapshot.payload["last_input"], serde_json::Value::Null);
        assert_eq!(snapshot.payload["last_output"], serde_json::Value::Null);

        let restored = runtime.restore(snapshot.clone()).unwrap();
        assert_ne!(restored.instance_id, handle.instance_id);
        assert_eq!(
            runtime.snapshot(&restored).unwrap().payload,
            snapshot.payload
        );
    }

    #[test]
    fn restore_never_replays_legacy_effectful_last_input() {
        let temp = tempfile::Builder::new()
            .prefix("sentinel-wasm-restore-effect-")
            .tempdir_in("/tmp")
            .unwrap();
        let relative_dir = temp.path().strip_prefix("/tmp").unwrap().to_string_lossy();
        let effect_path = temp.path().join("restore-replay.txt");
        let effect_input = format!("write {relative_dir}/restore-replay.txt replayed");
        let effect_workload = workload_with("wasm-legacy-effect", fs_fixture(), "effectful-fs");
        let payload = serde_json::json!({
            "workload": effect_workload,
            "tool_name": "effectful-fs",
            "wasm_path": fs_fixture().canonicalize().unwrap().to_string_lossy(),
            "last_input": effect_input,
            "last_output": "legacy bound receipt",
            "semantics_note": "legacy re-execute schema"
        });
        let snapshot = NanoSnapshot {
            runtime_key: RUNTIME_WASM_WASMTIME.to_string(),
            workload_id: "wasm-legacy-effect".to_string(),
            agent_id: None,
            semantics: NanoSnapshotSemantics::WasmReexecute,
            payload,
        };

        let mut runtime = WasmtimeNanoRuntime::new();
        let restored = runtime.restore(snapshot).unwrap();

        assert!(
            !effect_path.exists(),
            "restore must not replay an effect-bearing legacy last_input"
        );
        let rebound = runtime.snapshot(&restored).unwrap();
        assert_eq!(rebound.semantics, NanoSnapshotSemantics::WasmBoundState);
        assert_eq!(
            rebound.payload["bound_execution"]["output"],
            "legacy bound receipt"
        );
    }

    #[test]
    fn restore_rejects_legacy_effect_without_bound_result() {
        let temp = tempfile::Builder::new()
            .prefix("sentinel-wasm-restore-unbound-effect-")
            .tempdir_in("/tmp")
            .unwrap();
        let relative_dir = temp.path().strip_prefix("/tmp").unwrap().to_string_lossy();
        let effect_path = temp.path().join("restore-replay.txt");
        let effect_input = format!("write {relative_dir}/restore-replay.txt replayed");
        let effect_workload = workload_with("wasm-unbound-effect", fs_fixture(), "effectful-fs");
        let payload = serde_json::json!({
            "workload": effect_workload,
            "tool_name": "effectful-fs",
            "wasm_path": fs_fixture().canonicalize().unwrap().to_string_lossy(),
            "last_input": effect_input,
            "semantics_note": "legacy re-execute schema"
        });
        let snapshot = NanoSnapshot {
            runtime_key: RUNTIME_WASM_WASMTIME.to_string(),
            workload_id: "wasm-unbound-effect".to_string(),
            agent_id: None,
            semantics: NanoSnapshotSemantics::WasmReexecute,
            payload,
        };

        let mut runtime = WasmtimeNanoRuntime::new();
        let error = runtime.restore(snapshot).unwrap_err();

        assert!(
            error.to_string().contains("bound result"),
            "unreceipted effect must fail closed: {error}"
        );
        assert!(
            !effect_path.exists(),
            "rejected restore must not execute the effect-bearing input"
        );
        assert_eq!(runtime.runtime.tool_count(), 0);
        assert_eq!(runtime.runtime.plugin_host().cached_count(), 0);
    }

    #[test]
    fn suspended_wasm_workload_rejects_execution_until_resumed() {
        let mut runtime = WasmtimeNanoRuntime::new();
        let handle = runtime.spawn(workload("wasm-control")).unwrap();
        runtime
            .control(&handle, NanoRuntimeControlAction::Suspend)
            .unwrap();
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Degraded
        );
        assert!(runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "echo".to_string(),
                    input: "blocked".to_string(),
                },
            )
            .is_err());
        runtime
            .control(&handle, NanoRuntimeControlAction::Resume)
            .unwrap();
        assert_eq!(
            runtime.health(&handle).unwrap().state,
            NanoHealthState::Healthy
        );
    }
}
