use std::collections::BTreeMap;

use sentinel_common::nano_runtime::{
    NanoRuntimeRegistry, NanoWorkloadSpec, RUNTIME_BWRAP_LANDLOCK, RUNTIME_ECS_NATIVE,
    RUNTIME_WASM_WASMTIME,
};
use sentinel_common::AgentId;
use sentinel_runtime::EcsNativeRuntime;
use sentinel_sandbox::BwrapNanoRuntime;

#[cfg(feature = "wasm")]
use sentinel_wasm::WasmtimeNanoRuntime;

fn workload(id: &str, runtime_key: Option<&str>, agent_id: u16) -> NanoWorkloadSpec {
    NanoWorkloadSpec {
        workload_id: id.to_string(),
        runtime_key: runtime_key.map(str::to_string),
        agent_id: Some(AgentId(agent_id)),
        agent_name: format!("registry-agent-{agent_id}"),
        role: "Registry Tester".to_string(),
        room_id: "empfang".to_string(),
        shift_set: 1,
        command: Vec::new(),
        capabilities: Vec::new(),
        metadata: BTreeMap::new(),
        ecs_snapshot: None,
    }
}

#[test]
fn runtime_registry_routes_explicit_workload_keys() {
    let mut registry = NanoRuntimeRegistry::new(Some(RUNTIME_ECS_NATIVE.to_string()));
    registry.register(EcsNativeRuntime::new(8)).unwrap();
    registry.register(BwrapNanoRuntime::detect()).unwrap();
    #[cfg(feature = "wasm")]
    registry.register(WasmtimeNanoRuntime::new()).unwrap();

    assert!(registry.contains(RUNTIME_ECS_NATIVE));
    assert!(registry.contains(RUNTIME_BWRAP_LANDLOCK));
    #[cfg(feature = "wasm")]
    assert!(registry.contains(RUNTIME_WASM_WASMTIME));

    assert_eq!(
        registry
            .select_key(&workload("workload-a", Some(RUNTIME_ECS_NATIVE), 1))
            .unwrap(),
        RUNTIME_ECS_NATIVE
    );
    #[cfg(feature = "wasm")]
    assert_eq!(
        registry
            .select_key(&workload("workload-b", Some(RUNTIME_WASM_WASMTIME), 2))
            .unwrap(),
        RUNTIME_WASM_WASMTIME
    );
    assert_eq!(
        registry
            .select_key(&workload("workload-c", Some(RUNTIME_BWRAP_LANDLOCK), 3))
            .unwrap(),
        RUNTIME_BWRAP_LANDLOCK
    );
    assert_eq!(
        registry.select_key(&workload("fallback", None, 4)).unwrap(),
        RUNTIME_ECS_NATIVE
    );
}
