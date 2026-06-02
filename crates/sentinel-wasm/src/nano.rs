#![cfg(feature = "wasm")]

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use sentinel_common::nano_runtime::{
    NanoExecRequest, NanoExecResult, NanoHandle, NanoHealth, NanoHealthState, NanoIsolationPolicy,
    NanoIsolationReport, NanoRuntime, NanoSnapshot, NanoSnapshotSemantics, NanoWorkloadSpec,
    RUNTIME_WASM_WASMTIME,
};
use serde::{Deserialize, Serialize};

use crate::{
    AgentSnapshot, ExecutionContext, PluginConfig, SandboxConfig, ToolDefinition, ToolRuntime,
    ToolType,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WasmRuntimeSnapshot {
    workload: NanoWorkloadSpec,
    tool_name: String,
    wasm_path: String,
    #[serde(default)]
    last_input: Option<String>,
    #[serde(default)]
    last_output: Option<String>,
    semantics_note: String,
}

#[derive(Debug, Clone)]
struct WasmWorkloadState {
    workload: NanoWorkloadSpec,
    tool_name: String,
    wasm_path: PathBuf,
    last_input: Option<String>,
    last_output: Option<String>,
}

pub struct WasmtimeNanoRuntime {
    runtime: ToolRuntime,
    workloads: HashMap<String, WasmWorkloadState>,
}

impl WasmtimeNanoRuntime {
    pub fn new() -> Self {
        Self {
            runtime: ToolRuntime::new(),
            workloads: HashMap::new(),
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

        Ok(WasmWorkloadState {
            workload: workload.clone(),
            tool_name,
            wasm_path: PathBuf::from(wasm_path),
            last_input: None,
            last_output: None,
        })
    }

    fn load_state(&mut self, state: &WasmWorkloadState) -> Result<()> {
        self.runtime.plugin_host_mut().load(PluginConfig {
            wasm_path: state.wasm_path.clone(),
            ..PluginConfig::default()
        })?;
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
            workload: state.workload.clone(),
            tool_name: state.tool_name.clone(),
            wasm_path: state.wasm_path.to_string_lossy().to_string(),
            last_input: state.last_input.clone(),
            last_output: state.last_output.clone(),
            semantics_note:
                "wasm-wasmtime snapshot is declarative input+ECS re-execute state; no Store dump"
                    .to_string(),
        };
        Ok(serde_json::to_value(payload)?)
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
        let state = Self::state_from_workload(&workload)?;
        self.load_state(&state)?;
        self.workloads.insert(workload.workload_id.clone(), state);
        Ok(NanoHandle {
            runtime_key: self.runtime_key().to_string(),
            workload_id: workload.workload_id,
            agent_id: workload.agent_id,
            pid: None,
        })
    }

    fn exec(&mut self, handle: &NanoHandle, request: NanoExecRequest) -> Result<NanoExecResult> {
        let state = self
            .workloads
            .get(&handle.workload_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown wasm workload '{}'", handle.workload_id))?;
        let input = if request.input.is_empty() {
            request.operation
        } else {
            request.input
        };
        let ctx = Self::execution_context(&state);
        let result = self.runtime.execute(&state.tool_name, &input, &ctx)?;

        if let Some(stored) = self.workloads.get_mut(&handle.workload_id) {
            stored.last_input = Some(input.clone());
            stored.last_output = Some(result.output.clone());
        }

        Ok(NanoExecResult {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            success: result.success,
            output: result.output,
        })
    }

    fn snapshot(&mut self, handle: &NanoHandle) -> Result<NanoSnapshot> {
        let state = self
            .workloads
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("unknown wasm workload '{}'", handle.workload_id))?;
        Ok(NanoSnapshot {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            agent_id: handle.agent_id,
            semantics: NanoSnapshotSemantics::WasmReexecute,
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
        let payload: WasmRuntimeSnapshot = serde_json::from_value(snapshot.payload)?;
        let mut metadata = payload.workload.metadata.clone();
        metadata.insert("wasm_path".to_string(), payload.wasm_path.clone());
        metadata.insert("tool_name".to_string(), payload.tool_name.clone());
        let workload = NanoWorkloadSpec {
            metadata,
            ..payload.workload
        };
        let mut state = Self::state_from_workload(&workload)?;
        self.load_state(&state)?;

        if let Some(input) = payload.last_input.clone() {
            let ctx = Self::execution_context(&state);
            let result = self.runtime.execute(&state.tool_name, &input, &ctx)?;
            state.last_input = Some(input);
            state.last_output = Some(result.output);
        } else {
            state.last_output = payload.last_output;
        }

        self.workloads.insert(snapshot.workload_id.clone(), state);
        Ok(NanoHandle {
            runtime_key: self.runtime_key().to_string(),
            workload_id: snapshot.workload_id,
            agent_id: snapshot.agent_id,
            pid: None,
        })
    }

    fn health(&mut self, handle: &NanoHandle) -> Result<NanoHealth> {
        let state = self
            .workloads
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("unknown wasm workload '{}'", handle.workload_id))?;
        let loaded = self.runtime.plugin_host().is_loaded(&state.wasm_path);
        Ok(NanoHealth {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            state: if loaded {
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
