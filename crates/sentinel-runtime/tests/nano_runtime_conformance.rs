use std::collections::BTreeMap;

use sentinel_common::nano_runtime::conformance::{
    assert_nano_runtime_conformance, assert_nano_runtime_stop_isolation,
};
use sentinel_common::nano_runtime::{NanoWorkloadSpec, RUNTIME_ECS_NATIVE};
use sentinel_common::AgentId;
use sentinel_runtime::EcsNativeRuntime;

#[test]
fn ecs_native_runtime_satisfies_nano_runtime_conformance() {
    let mut runtime = EcsNativeRuntime::new(8);
    let workload = NanoWorkloadSpec {
        workload_id: "ecs-native-conformance".to_string(),
        runtime_key: Some(RUNTIME_ECS_NATIVE.to_string()),
        agent_id: Some(AgentId(1)),
        agent_name: "Conformance Agent".to_string(),
        role: "Tester".to_string(),
        room_id: "empfang".to_string(),
        shift_set: 1,
        command: Vec::new(),
        capabilities: Vec::new(),
        metadata: BTreeMap::new(),
        ecs_snapshot: None,
    };

    assert_nano_runtime_conformance(&mut runtime, workload);
}

#[test]
fn ecs_native_stop_is_idempotent_and_workload_scoped() {
    let mut runtime = EcsNativeRuntime::new(8);
    let workload_a = NanoWorkloadSpec {
        workload_id: "ecs-stop-a".to_string(),
        runtime_key: Some(RUNTIME_ECS_NATIVE.to_string()),
        agent_id: Some(AgentId(1)),
        agent_name: "Stop Agent A".to_string(),
        role: "Tester".to_string(),
        room_id: "empfang".to_string(),
        shift_set: 1,
        command: Vec::new(),
        capabilities: Vec::new(),
        metadata: BTreeMap::new(),
        ecs_snapshot: None,
    };
    let workload_b = NanoWorkloadSpec {
        workload_id: "ecs-stop-b".to_string(),
        runtime_key: Some(RUNTIME_ECS_NATIVE.to_string()),
        agent_id: Some(AgentId(2)),
        agent_name: "Stop Agent B".to_string(),
        ..workload_a.clone()
    };
    assert_nano_runtime_stop_isolation(&mut runtime, workload_a, workload_b);
}
