use std::collections::BTreeMap;

use sentinel_common::nano_runtime::conformance::assert_nano_runtime_conformance;
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
