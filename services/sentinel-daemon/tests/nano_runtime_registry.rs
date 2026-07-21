use std::collections::BTreeMap;

use sentinel_common::nano_runtime::{
    NanoRuntimeRegistry, NanoWorkloadSpec, RUNTIME_BWRAP_LANDLOCK, RUNTIME_ECS_NATIVE,
};
use sentinel_common::AgentId;
use sentinel_runtime::EcsNativeRuntime;
use sentinel_sandbox::BwrapNanoRuntime;

#[cfg(feature = "wasm")]
use sentinel_common::nano_runtime::RUNTIME_WASM_WASMTIME;
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

fn rust_function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .expect("function signature must exist");
    let function = &source[start..];
    let open = function.find('{').expect("function body must start");
    let mut depth = 0usize;
    for (offset, byte) in function.as_bytes()[open..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &function[..open + offset + 1];
                }
            }
            _ => {}
        }
    }
    panic!("function body must end")
}

fn rust_source_region<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source.find(start).expect("source region must start");
    let tail = &source[start..];
    let end = tail.find(end).expect("source region must end");
    &tail[..end]
}

#[test]
fn productive_lifecycle_call_sites_remain_registry_owned() {
    // Compile this against the exact daemon source so a future callsite cannot
    // silently restore direct process/cgroup ownership next to the registry.
    let source = include_str!("../src/orchestrator.rs");
    let production_registry = rust_function_body(source, "fn production(");
    assert!(production_registry.contains("EcsNativeRuntime::external_lifecycle"));
    assert!(production_registry.contains("registry.register(bwrap)"));
    assert!(production_registry.contains("WasmtimeNanoRuntime::new"));
    assert!(production_registry.contains("MicrovmNanoRuntime::detect"));

    for signature in [
        "fn teardown_agent_full(",
        "fn teardown_runtime_for_world_restore(",
        "fn stop_all_nano_runtimes_with_retries(",
    ] {
        let body = rust_function_body(source, signature);
        assert!(
            body.contains("stop_agent_runtime_layer("),
            "{signature} must stop through the owning adapter"
        );
    }

    let stop_layer = rust_function_body(source, "fn stop_agent_runtime_layer(");
    assert!(stop_layer.contains("nano_runtimes.stop(agent_id)"));
    assert!(stop_layer.contains("captured_cgroup_id"));
    assert!(stop_layer.contains("ebpf_collector.unregister_agent(cgroup_id)"));
    let registry_stop = rust_function_body(source, "fn stop(&mut self, agent_id: AgentId)");
    assert!(registry_stop.contains("self.registry.stop(&handle)"));
    assert!(registry_stop.contains("self.handles.remove(&agent_id)"));

    assert!(source.contains("runtime_orch.shift_removal_candidates"));
    assert!(source.contains("runtime_orch.commit_shift_transition"));
    assert!(source.contains("shutdown blocked by NanoRuntime cleanup failure"));
    assert!(source.contains("runtime switch failed: {error}"));
    assert!(source.contains("fn apply_agent_runtime_control("));
    assert!(source.contains("fn reapply_persisted_runtime_suspension("));
    assert!(
        !source.contains("fn suspend_agent_cgroup_processes("),
        "adapter-specific suspension must not escape the NanoRuntime owner"
    );
    assert!(
        source.matches("apply_agent_runtime_control(").count() >= 4,
        "operator pause/resume, control-plane suspend, and restart re-suspend must share the ownership barrier"
    );

    let reconcile = rust_source_region(
        source,
        "fn run_runtime_reconcile(",
        "fn resolve_platform_analysis_target(",
    );
    assert!(reconcile.contains("runtime_resources_are_healthy("));
    assert!(reconcile.contains("runtime_agent_is_healthy(snapshot)"));
    assert!(reconcile.contains("nano_runtimes.observe(AgentId(agent_id))"));
    assert!(reconcile.contains("record_nano_runtime_snapshot("));
}
