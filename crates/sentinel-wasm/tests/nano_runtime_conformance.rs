#![cfg(feature = "wasm")]

use sentinel_common::nano_runtime::conformance::assert_nano_runtime_conformance;
use sentinel_common::nano_runtime::{NanoWorkloadSpec, RUNTIME_WASM_WASMTIME};
use sentinel_common::AgentId;
use sentinel_wasm::{wasm_conformance_metadata, WasmtimeNanoRuntime};

fn echo_fixture() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/echo-plugin.wasm")
}

#[test]
fn wasmtime_runtime_satisfies_nano_runtime_conformance() {
    let mut runtime = WasmtimeNanoRuntime::new();
    let workload = NanoWorkloadSpec {
        workload_id: "wasm-conformance".to_string(),
        runtime_key: Some(RUNTIME_WASM_WASMTIME.to_string()),
        agent_id: Some(AgentId(2)),
        agent_name: "WASM Conformance Agent".to_string(),
        role: "Tool Tester".to_string(),
        room_id: "empfang".to_string(),
        shift_set: 1,
        command: Vec::new(),
        capabilities: vec!["echo".to_string()],
        metadata: wasm_conformance_metadata(echo_fixture(), "echo"),
        ecs_snapshot: None,
    };

    assert_nano_runtime_conformance(&mut runtime, workload);
}
