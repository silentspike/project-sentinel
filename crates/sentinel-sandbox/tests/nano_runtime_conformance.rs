use std::collections::BTreeMap;

use sentinel_common::nano_runtime::conformance::assert_nano_runtime_conformance;
use sentinel_common::nano_runtime::{NanoWorkloadSpec, RUNTIME_BWRAP_LANDLOCK};
use sentinel_common::AgentId;
use sentinel_sandbox::BwrapNanoRuntime;

#[test]
#[ignore = "requires bwrap user namespaces and cgroup-capable host"]
fn bwrap_runtime_satisfies_nano_runtime_conformance() {
    let mut runtime = BwrapNanoRuntime::detect();
    let workload = NanoWorkloadSpec {
        workload_id: "bwrap-conformance".to_string(),
        runtime_key: Some(RUNTIME_BWRAP_LANDLOCK.to_string()),
        agent_id: Some(AgentId(3)),
        agent_name: "bwrap-conformance-agent".to_string(),
        role: "Sandbox Tester".to_string(),
        room_id: "empfang".to_string(),
        shift_set: 1,
        command: vec!["/usr/bin/sleep".to_string(), "30".to_string()],
        capabilities: Vec::new(),
        metadata: BTreeMap::new(),
        ecs_snapshot: None,
    };

    assert_nano_runtime_conformance(&mut runtime, workload);
}
