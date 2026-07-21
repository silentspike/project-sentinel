use std::collections::BTreeMap;

use sentinel_common::nano_runtime::conformance::{
    assert_nano_runtime_conformance, assert_nano_runtime_stop_isolation,
};
use sentinel_common::nano_runtime::{NanoRuntime, NanoWorkloadSpec, RUNTIME_MICROVM};
use sentinel_common::AgentId;
use sentinel_microvm::MicrovmNanoRuntime;

fn workload_spec() -> NanoWorkloadSpec {
    NanoWorkloadSpec {
        workload_id: "microvm-conformance".to_string(),
        runtime_key: Some(RUNTIME_MICROVM.to_string()),
        agent_id: Some(AgentId(1)),
        agent_name: "Conformance microVM".to_string(),
        role: "Tester".to_string(),
        room_id: "empfang".to_string(),
        shift_set: 1,
        command: vec!["/opt/sentinel/bin/agent-runtime".to_string()],
        capabilities: Vec::new(),
        metadata: BTreeMap::new(),
        ecs_snapshot: None,
    }
}

/// Voller Conformance-Vertrag (#408) gegen eine ECHTE Firecracker-microVM.
/// Benoetigt KVM + Gast-Kernel/rootfs unter /opt/sentinel/microvm/ (Deploy-VM) — daher `#[ignore]`.
/// Ausfuehren: `cargo test -p sentinel-microvm -- --ignored` auf der Deploy-VM.
#[test]
#[ignore = "benoetigt KVM + Gast-Kernel/rootfs (Deploy-VM)"]
fn microvm_runtime_satisfies_nano_runtime_conformance() {
    let mut runtime = MicrovmNanoRuntime::detect();
    assert_nano_runtime_conformance(&mut runtime, workload_spec());
}

#[test]
#[ignore = "benoetigt KVM + Gast-Kernel/rootfs (Deploy-VM)"]
fn microvm_stop_is_idempotent_and_workload_scoped() {
    let mut runtime = MicrovmNanoRuntime::detect();
    let workload_a = workload_spec();
    let workload_b = NanoWorkloadSpec {
        workload_id: "microvm-stop-b".to_string(),
        agent_id: Some(AgentId(2)),
        agent_name: "Conformance microVM B".to_string(),
        ..workload_a.clone()
    };
    assert_nano_runtime_stop_isolation(&mut runtime, workload_a, workload_b);
}

/// KVM-freier Pfad (#417 AC-4): ohne `/dev/kvm` muss `spawn` mit einem sauberen, KVM-benennenden
/// Fehler scheitern (kein Panic, kein haengender Prozess). Laeuft ueberall (z.B. Build-Server .155).
/// Auf KVM-faehigen Maschinen wird der echte Pfad durch den `#[ignore]`-Conformance-Test abgedeckt.
#[test]
fn spawn_without_kvm_fails_cleanly() {
    let mut runtime = MicrovmNanoRuntime::detect();
    if runtime.kvm_available() {
        eprintln!(
            "KVM vorhanden -> KVM-frei-Test uebersprungen (Conformance via `-- --ignored` ausfuehren)"
        );
        return;
    }
    let err = runtime
        .spawn(workload_spec())
        .expect_err("ohne KVM muss spawn fehlschlagen");
    let msg = err.to_string();
    assert!(
        msg.contains("KVM"),
        "Fehlermeldung muss KVM benennen, war: {msg}"
    );
}
