//! Shared Nano-Container runtime contract.
//!
//! The contract intentionally mirrors a CRI-style interface without selecting a
//! default runtime. Each workload carries an explicit runtime key, or the
//! caller supplies an explicit fallback policy at registry construction.

use std::collections::{BTreeMap, HashMap};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AgentId, EcsSnapshot};

pub const RUNTIME_ECS_NATIVE: &str = "ecs-native";
pub const RUNTIME_WASM_WASMTIME: &str = "wasm-wasmtime";
pub const RUNTIME_BWRAP_LANDLOCK: &str = "bwrap-landlock";
pub const RUNTIME_MICROVM: &str = "microvm";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NanoWorkloadSpec {
    pub workload_id: String,
    #[serde(default)]
    pub runtime_key: Option<String>,
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    pub agent_name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub room_id: String,
    #[serde(default = "default_shift_set")]
    pub shift_set: u8,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub ecs_snapshot: Option<EcsSnapshot>,
}

impl NanoWorkloadSpec {
    pub fn runtime_key_or<'a>(&'a self, fallback: Option<&'a str>) -> Result<&'a str> {
        if let Some(key) = self.runtime_key.as_deref().filter(|key| !key.is_empty()) {
            return Ok(key);
        }
        fallback.ok_or_else(|| {
            anyhow!(
                "workload '{}' has no explicit runtime_key and registry has no explicit fallback",
                self.workload_id
            )
        })
    }
}

fn default_shift_set() -> u8 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NanoHandle {
    /// Unique identity for this concrete workload incarnation. Runtime keys and
    /// workload ids are routing metadata; neither is sufficient to prevent a
    /// stale or rewritten handle from addressing a newer workload instance.
    #[serde(default)]
    pub instance_id: Uuid,
    pub runtime_key: String,
    pub workload_id: String,
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    #[serde(default)]
    pub pid: Option<u32>,
}

/// Runtime-owned resources that the production orchestrator may observe without
/// taking ownership away from the selected adapter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NanoRuntimeResources {
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub child_pid: Option<u32>,
    #[serde(default)]
    pub cgroup_created: bool,
    #[serde(default)]
    pub io_available: bool,
    #[serde(default)]
    pub landlock_applied: bool,
    #[serde(default)]
    pub network_isolated: bool,
}

impl NanoHandle {
    pub fn new(
        runtime_key: &str,
        workload_id: String,
        agent_id: Option<AgentId>,
        pid: Option<u32>,
    ) -> Self {
        Self {
            instance_id: Uuid::new_v4(),
            runtime_key: runtime_key.to_string(),
            workload_id,
            agent_id,
            pid,
        }
    }
}

pub const NANO_STOP_RESULT_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NanoStopOutcome {
    Stopped,
    AlreadyStopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NanoStopResult {
    pub version: u16,
    pub runtime_key: String,
    pub workload_id: String,
    pub outcome: NanoStopOutcome,
}

impl NanoStopResult {
    pub fn new(runtime_key: &str, workload_id: &str, stopped: bool) -> Self {
        Self {
            version: NANO_STOP_RESULT_VERSION,
            runtime_key: runtime_key.to_string(),
            workload_id: workload_id.to_string(),
            outcome: if stopped {
                NanoStopOutcome::Stopped
            } else {
                NanoStopOutcome::AlreadyStopped
            },
        }
    }
}

pub fn ensure_handle_runtime(handle: &NanoHandle, expected_runtime_key: &str) -> Result<()> {
    if handle.runtime_key != expected_runtime_key {
        return Err(anyhow!(
            "NanoHandle for runtime '{}' cannot be used with runtime '{}'",
            handle.runtime_key,
            expected_runtime_key
        ));
    }
    Ok(())
}

pub fn ensure_handle_instance(handle: &NanoHandle, expected_instance_id: Uuid) -> Result<()> {
    if handle.instance_id != expected_instance_id {
        return Err(anyhow!(
            "NanoHandle instance '{}' does not own active workload '{}'",
            handle.instance_id,
            handle.workload_id
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NanoExecRequest {
    pub operation: String,
    #[serde(default)]
    pub input: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NanoExecResult {
    pub runtime_key: String,
    pub workload_id: String,
    pub success: bool,
    pub output: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NanoSnapshotSemantics {
    EcsWorld,
    WasmReexecute,
    BwrapConfigFs,
    RuntimeMetadata,
    /// microVM-Snapshot (#417): die NanoSnapshot.payload traegt STABILE Metadaten (Config +
    /// deterministische Pfade zu Firecracker mem/state-Dateien je workload_id), NICHT die volatilen
    /// Guest-RAM-Bytes. Der echte Speicher-Snapshot liegt in den referenzierten Firecracker-Dateien.
    MicrovmMemory,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NanoSnapshot {
    pub runtime_key: String,
    pub workload_id: String,
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    pub semantics: NanoSnapshotSemantics,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NanoHealthState {
    Healthy,
    Degraded,
    Stopped,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NanoHealth {
    pub runtime_key: String,
    pub workload_id: String,
    pub state: NanoHealthState,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NanoIsolationPolicy {
    #[serde(default)]
    pub filesystem: bool,
    #[serde(default)]
    pub cgroups: bool,
    #[serde(default)]
    pub landlock: bool,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub wasm_fuel: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NanoIsolationReport {
    pub runtime_key: String,
    pub workload_id: String,
    pub applied: bool,
    pub detail: String,
}

pub trait NanoRuntime: Send {
    fn runtime_key(&self) -> &'static str;
    fn spawn(&mut self, workload: NanoWorkloadSpec) -> Result<NanoHandle>;
    fn stop(&mut self, handle: &NanoHandle) -> Result<NanoStopResult>;
    fn exec(&mut self, handle: &NanoHandle, request: NanoExecRequest) -> Result<NanoExecResult>;
    fn snapshot(&mut self, handle: &NanoHandle) -> Result<NanoSnapshot>;
    fn restore(&mut self, snapshot: NanoSnapshot) -> Result<NanoHandle>;
    fn health(&mut self, handle: &NanoHandle) -> Result<NanoHealth>;
    fn isolate(
        &mut self,
        handle: &NanoHandle,
        policy: NanoIsolationPolicy,
    ) -> Result<NanoIsolationReport>;

    /// Return process/isolation resources for monitoring side effects. The
    /// adapter remains their sole lifecycle owner.
    fn resources(&self, handle: &NanoHandle) -> Result<NanoRuntimeResources> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        Ok(NanoRuntimeResources {
            pid: handle.pid,
            ..NanoRuntimeResources::default()
        })
    }

    fn migrate(&mut self, target: &mut dyn NanoRuntime, handle: &NanoHandle) -> Result<NanoHandle>
    where
        Self: Sized,
    {
        let snapshot = self.snapshot(handle)?;
        target.restore(snapshot)
    }
}

pub struct NanoRuntimeRegistry {
    runtimes: HashMap<String, Box<dyn NanoRuntime>>,
    explicit_fallback_key: Option<String>,
}

impl NanoRuntimeRegistry {
    pub fn new(explicit_fallback_key: Option<String>) -> Self {
        Self {
            runtimes: HashMap::new(),
            explicit_fallback_key,
        }
    }

    pub fn register<R>(&mut self, runtime: R) -> Result<()>
    where
        R: NanoRuntime + 'static,
    {
        let key = runtime.runtime_key().to_string();
        if self.runtimes.contains_key(&key) {
            return Err(anyhow!("NanoRuntime '{key}' already registered"));
        }
        self.runtimes.insert(key, Box::new(runtime));
        Ok(())
    }

    pub fn contains(&self, key: &str) -> bool {
        self.runtimes.contains_key(key)
    }

    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.runtimes.keys().cloned().collect();
        keys.sort();
        keys
    }

    pub fn select_key(&self, workload: &NanoWorkloadSpec) -> Result<String> {
        let key = workload.runtime_key_or(self.explicit_fallback_key.as_deref())?;
        if !self.runtimes.contains_key(key) {
            return Err(anyhow!("NanoRuntime '{key}' is not registered"));
        }
        Ok(key.to_string())
    }

    pub fn get_mut(&mut self, key: &str) -> Result<&mut (dyn NanoRuntime + '_)> {
        if let Some(runtime) = self.runtimes.get_mut(key) {
            Ok(runtime.as_mut())
        } else {
            Err(anyhow!("NanoRuntime '{key}' is not registered"))
        }
    }

    pub fn stop(&mut self, handle: &NanoHandle) -> Result<NanoStopResult> {
        self.get_mut(&handle.runtime_key)?.stop(handle)
    }

    pub fn resources(&mut self, handle: &NanoHandle) -> Result<NanoRuntimeResources> {
        self.get_mut(&handle.runtime_key)?.resources(handle)
    }
}

#[cfg(any(test, feature = "test-util"))]
pub mod conformance {
    use super::*;

    pub fn assert_nano_runtime_stop_isolation<R>(
        runtime: &mut R,
        workload_a: NanoWorkloadSpec,
        workload_b: NanoWorkloadSpec,
    ) where
        R: NanoRuntime,
    {
        assert_ne!(workload_a.workload_id, workload_b.workload_id);
        let duplicate_a = workload_a.clone();
        let handle_a = runtime.spawn(workload_a).expect("spawn workload A");
        assert!(
            runtime.spawn(duplicate_a).is_err(),
            "adapter must reject an active duplicate workload id"
        );
        let handle_b = runtime.spawn(workload_b).expect("spawn workload B");
        assert_ne!(handle_a.instance_id, handle_b.instance_id);

        assert_eq!(
            runtime.health(&handle_a).expect("health workload A").state,
            NanoHealthState::Healthy,
            "workload A must be live before stop is exercised"
        );
        assert_eq!(
            runtime.health(&handle_b).expect("health workload B").state,
            NanoHealthState::Healthy,
            "workload B must be live before stop is exercised"
        );

        let wrong_runtime = NanoHandle {
            runtime_key: "wrong-runtime".to_string(),
            ..handle_a.clone()
        };
        assert!(
            runtime.stop(&wrong_runtime).is_err(),
            "adapter must reject a handle owned by another runtime"
        );

        let stale_for_b = NanoHandle {
            instance_id: handle_a.instance_id,
            ..handle_b.clone()
        };
        assert!(
            runtime.stop(&stale_for_b).is_err(),
            "adapter must reject a stale instance identity for an active workload"
        );

        let stopped = runtime.stop(&handle_a).expect("stop workload A");
        assert_eq!(stopped.version, NANO_STOP_RESULT_VERSION);
        assert_eq!(stopped.outcome, NanoStopOutcome::Stopped);
        assert_eq!(
            runtime.health(&handle_a).expect("health workload A").state,
            NanoHealthState::Stopped
        );
        assert!(matches!(
            runtime.health(&handle_b).expect("health workload B").state,
            NanoHealthState::Healthy | NanoHealthState::Degraded
        ));

        let replay = runtime.stop(&handle_a).expect("replay stop workload A");
        assert_eq!(replay.outcome, NanoStopOutcome::AlreadyStopped);
        assert_eq!(
            runtime.stop(&handle_b).expect("stop workload B").outcome,
            NanoStopOutcome::Stopped
        );
    }

    pub fn assert_nano_runtime_conformance<R>(runtime: &mut R, workload: NanoWorkloadSpec)
    where
        R: NanoRuntime,
    {
        let handle = runtime
            .spawn(workload.clone())
            .expect("nano runtime spawn must succeed");
        assert_eq!(handle.runtime_key, runtime.runtime_key());
        assert_eq!(handle.workload_id, workload.workload_id);

        let health = runtime
            .health(&handle)
            .expect("nano runtime health must succeed");
        assert_eq!(health.runtime_key, runtime.runtime_key());
        assert!(
            matches!(
                health.state,
                NanoHealthState::Healthy | NanoHealthState::Degraded
            ),
            "runtime must be live enough for conformance, got {:?}",
            health.state
        );

        let exec = runtime
            .exec(
                &handle,
                NanoExecRequest {
                    operation: "health".to_string(),
                    input: "conformance".to_string(),
                },
            )
            .expect("nano runtime exec must succeed");
        assert!(exec.success, "exec result must be successful");

        let before = runtime
            .snapshot(&handle)
            .expect("nano runtime snapshot must succeed");
        assert_eq!(before.runtime_key, runtime.runtime_key());
        assert_eq!(before.workload_id, workload.workload_id);

        let restored = runtime
            .restore(before.clone())
            .expect("nano runtime restore must succeed");
        assert_eq!(restored.runtime_key, runtime.runtime_key());
        assert_eq!(restored.workload_id, workload.workload_id);
        assert_ne!(
            restored.instance_id, handle.instance_id,
            "restore must create a new workload incarnation"
        );
        assert!(
            runtime.stop(&handle).is_err(),
            "a pre-restore handle must not stop the restored incarnation"
        );

        let after = runtime
            .snapshot(&restored)
            .expect("nano runtime snapshot after restore must succeed");
        assert_eq!(after.runtime_key, before.runtime_key);
        assert_eq!(after.workload_id, before.workload_id);
        assert_eq!(after.agent_id, before.agent_id);
        assert_eq!(after.semantics, before.semantics);
        assert_eq!(
            after.payload, before.payload,
            "restore(snapshot(x)) must preserve the documented snapshot payload"
        );

        let isolation = runtime
            .isolate(&restored, NanoIsolationPolicy::default())
            .expect("nano runtime isolate must succeed");
        assert_eq!(isolation.runtime_key, runtime.runtime_key());
        assert_eq!(isolation.workload_id, workload.workload_id);

        let stopped = runtime
            .stop(&restored)
            .expect("nano runtime stop must succeed");
        assert_eq!(stopped.version, NANO_STOP_RESULT_VERSION);
        assert_eq!(stopped.outcome, NanoStopOutcome::Stopped);
        assert_eq!(stopped.runtime_key, runtime.runtime_key());
        assert_eq!(stopped.workload_id, workload.workload_id);

        let replay = runtime
            .stop(&restored)
            .expect("replayed nano runtime stop must succeed");
        assert_eq!(replay.outcome, NanoStopOutcome::AlreadyStopped);

        let health = runtime
            .health(&restored)
            .expect("health after stop must be readable");
        assert_eq!(health.state, NanoHealthState::Stopped);
    }

    #[cfg(test)]
    struct DummyRuntime {
        key: &'static str,
        snapshot: Option<NanoSnapshot>,
        active: HashMap<String, Uuid>,
    }

    #[cfg(test)]
    impl Default for DummyRuntime {
        fn default() -> Self {
            Self::named("dummy-runtime")
        }
    }

    #[cfg(test)]
    impl DummyRuntime {
        fn named(key: &'static str) -> Self {
            Self {
                key,
                snapshot: None,
                active: HashMap::new(),
            }
        }
    }

    #[cfg(test)]
    impl NanoRuntime for DummyRuntime {
        fn runtime_key(&self) -> &'static str {
            self.key
        }

        fn spawn(&mut self, workload: NanoWorkloadSpec) -> Result<NanoHandle> {
            if self.active.contains_key(&workload.workload_id) {
                return Err(anyhow!(
                    "dummy workload '{}' is already active",
                    workload.workload_id
                ));
            }
            let handle = NanoHandle::new(
                self.runtime_key(),
                workload.workload_id,
                workload.agent_id,
                None,
            );
            self.active
                .insert(handle.workload_id.clone(), handle.instance_id);
            Ok(handle)
        }

        fn stop(&mut self, handle: &NanoHandle) -> Result<NanoStopResult> {
            ensure_handle_runtime(handle, self.runtime_key())?;
            if let Some(instance_id) = self.active.get(&handle.workload_id) {
                ensure_handle_instance(handle, *instance_id)?;
            }
            Ok(NanoStopResult::new(
                self.runtime_key(),
                &handle.workload_id,
                self.active.remove(&handle.workload_id).is_some(),
            ))
        }

        fn exec(
            &mut self,
            handle: &NanoHandle,
            _request: NanoExecRequest,
        ) -> Result<NanoExecResult> {
            Ok(NanoExecResult {
                runtime_key: self.runtime_key().to_string(),
                workload_id: handle.workload_id.clone(),
                success: true,
                output: "ok".to_string(),
            })
        }

        fn snapshot(&mut self, handle: &NanoHandle) -> Result<NanoSnapshot> {
            Ok(self.snapshot.clone().unwrap_or_else(|| NanoSnapshot {
                runtime_key: self.runtime_key().to_string(),
                workload_id: handle.workload_id.clone(),
                agent_id: handle.agent_id,
                semantics: NanoSnapshotSemantics::RuntimeMetadata,
                payload: serde_json::json!({"stable": true}),
            }))
        }

        fn restore(&mut self, snapshot: NanoSnapshot) -> Result<NanoHandle> {
            self.snapshot = Some(snapshot.clone());
            let handle = NanoHandle::new(
                self.runtime_key(),
                snapshot.workload_id,
                snapshot.agent_id,
                None,
            );
            self.active
                .insert(handle.workload_id.clone(), handle.instance_id);
            Ok(handle)
        }

        fn health(&mut self, handle: &NanoHandle) -> Result<NanoHealth> {
            ensure_handle_runtime(handle, self.runtime_key())?;
            Ok(NanoHealth {
                runtime_key: self.runtime_key().to_string(),
                workload_id: handle.workload_id.clone(),
                state: if self.active.contains_key(&handle.workload_id) {
                    NanoHealthState::Healthy
                } else {
                    NanoHealthState::Stopped
                },
                detail: "dummy".to_string(),
            })
        }

        fn isolate(
            &mut self,
            handle: &NanoHandle,
            _policy: NanoIsolationPolicy,
        ) -> Result<NanoIsolationReport> {
            Ok(NanoIsolationReport {
                runtime_key: self.runtime_key().to_string(),
                workload_id: handle.workload_id.clone(),
                applied: true,
                detail: "dummy".to_string(),
            })
        }
    }

    #[test]
    fn registry_requires_explicit_runtime_or_fallback() {
        let mut registry = NanoRuntimeRegistry::new(None);
        registry.register(DummyRuntime::default()).unwrap();

        let workload = NanoWorkloadSpec {
            workload_id: "w1".to_string(),
            runtime_key: None,
            agent_id: None,
            agent_name: "Agent".to_string(),
            role: "Tester".to_string(),
            room_id: "empfang".to_string(),
            shift_set: 1,
            command: Vec::new(),
            capabilities: Vec::new(),
            metadata: BTreeMap::new(),
            ecs_snapshot: None,
        };

        assert!(registry.select_key(&workload).is_err());
        let registry = NanoRuntimeRegistry::new(Some("dummy-runtime".to_string()));
        assert!(registry.select_key(&workload).is_err());
    }

    #[test]
    fn conformance_harness_checks_roundtrip() {
        let mut runtime = DummyRuntime::default();
        let workload = NanoWorkloadSpec {
            workload_id: "w1".to_string(),
            runtime_key: Some("dummy-runtime".to_string()),
            agent_id: None,
            agent_name: "Agent".to_string(),
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
    fn registry_dispatches_idempotent_stop_and_rejects_wrong_runtime() {
        let mut registry = NanoRuntimeRegistry::new(None);
        registry.register(DummyRuntime::default()).unwrap();
        registry
            .register(DummyRuntime::named("other-runtime"))
            .unwrap();
        let workload = NanoWorkloadSpec {
            workload_id: "registry-stop".to_string(),
            runtime_key: Some("dummy-runtime".to_string()),
            agent_id: None,
            agent_name: "Agent".to_string(),
            role: "Tester".to_string(),
            room_id: "empfang".to_string(),
            shift_set: 1,
            command: Vec::new(),
            capabilities: Vec::new(),
            metadata: BTreeMap::new(),
            ecs_snapshot: None,
        };
        let key = registry.select_key(&workload).unwrap();
        let handle = registry
            .get_mut(&key)
            .unwrap()
            .spawn(workload.clone())
            .unwrap();
        let other_handle = registry
            .get_mut("other-runtime")
            .unwrap()
            .spawn(NanoWorkloadSpec {
                runtime_key: Some("other-runtime".to_string()),
                ..workload
            })
            .unwrap();

        let forged = NanoHandle {
            runtime_key: "other-runtime".to_string(),
            ..handle.clone()
        };
        assert!(registry.stop(&forged).is_err());
        assert_eq!(
            registry
                .get_mut("other-runtime")
                .unwrap()
                .health(&other_handle)
                .unwrap()
                .state,
            NanoHealthState::Healthy,
            "rewriting a handle runtime key must not stop the other adapter's same-id workload"
        );

        let first = registry.stop(&handle).unwrap();
        assert_eq!(first.version, NANO_STOP_RESULT_VERSION);
        assert_eq!(first.outcome, NanoStopOutcome::Stopped);
        let wire = serde_json::to_value(&first).unwrap();
        assert_eq!(wire["version"], NANO_STOP_RESULT_VERSION);
        assert_eq!(wire["outcome"], "stopped");
        let replay = registry.stop(&handle).unwrap();
        assert_eq!(replay.outcome, NanoStopOutcome::AlreadyStopped);
        assert_eq!(
            registry.stop(&other_handle).unwrap().outcome,
            NanoStopOutcome::Stopped
        );
    }

    #[test]
    fn legacy_handle_decodes_to_fail_closed_nil_instance() {
        let handle: NanoHandle = serde_json::from_value(serde_json::json!({
            "runtime_key": "dummy-runtime",
            "workload_id": "legacy-handle",
            "agent_id": null,
            "pid": null
        }))
        .unwrap();
        assert!(handle.instance_id.is_nil());

        let mut runtime = DummyRuntime::default();
        let active = runtime
            .spawn(NanoWorkloadSpec {
                workload_id: handle.workload_id.clone(),
                runtime_key: Some("dummy-runtime".to_string()),
                agent_id: None,
                agent_name: "Legacy".to_string(),
                role: "Tester".to_string(),
                room_id: "empfang".to_string(),
                shift_set: 1,
                command: Vec::new(),
                capabilities: Vec::new(),
                metadata: BTreeMap::new(),
                ecs_snapshot: None,
            })
            .unwrap();
        assert!(runtime.stop(&handle).is_err());
        assert_eq!(
            runtime.stop(&active).unwrap().outcome,
            NanoStopOutcome::Stopped
        );
    }
}
