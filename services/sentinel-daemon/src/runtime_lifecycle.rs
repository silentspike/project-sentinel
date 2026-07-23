//! Exclusive ownership boundary for NanoRuntime adapters.
//!
//! Production lifecycle callers operate through `RuntimeAdapterOwner`; the raw
//! registry and concrete adapters are private to this module. This makes direct
//! PID/cgroup or adapter mutations outside the owner structurally unavailable.

use anyhow::Result;
use sentinel_common::nano_runtime::{
    NanoHandle, NanoRecoveryResult, NanoRuntimeControlAction, NanoRuntimeControlResult,
    NanoRuntimeRegistry, NanoRuntimeResources, NanoSnapshot, NanoStopResult, NanoWorkloadSpec,
    RUNTIME_BWRAP_LANDLOCK,
};
use sentinel_microvm::MicrovmNanoRuntime;
use sentinel_runtime::EcsNativeRuntime;
use sentinel_sandbox::BwrapNanoRuntime;
#[cfg(feature = "wasm")]
use sentinel_wasm::WasmtimeNanoRuntime;

pub(crate) struct RuntimeAdapterOwner {
    registry: NanoRuntimeRegistry,
}

impl RuntimeAdapterOwner {
    pub(crate) fn production(max_agents: usize, fs_mount: Option<&str>) -> Result<Self> {
        let mut registry = NanoRuntimeRegistry::new(Some(RUNTIME_BWRAP_LANDLOCK.to_string()));
        registry.register(EcsNativeRuntime::external_lifecycle(max_agents))?;
        let mut bwrap = BwrapNanoRuntime::detect();
        if let Some(fs_mount) = fs_mount {
            bwrap.set_fs_mount(fs_mount);
        }
        bwrap.set_cas_manifest_enabled(
            sentinel_common::feature_flags::RuntimeFlags::global().bwrap_cas_world_snapshot_enabled,
        );
        registry.register(bwrap)?;
        #[cfg(feature = "wasm")]
        registry.register(WasmtimeNanoRuntime::new())?;
        registry.register(MicrovmNanoRuntime::detect())?;
        Ok(Self { registry })
    }

    pub(crate) fn keys(&self) -> Vec<String> {
        self.registry.keys()
    }

    pub(crate) fn select_key(&self, workload: &NanoWorkloadSpec) -> Result<String> {
        self.registry.select_key(workload)
    }

    pub(crate) fn spawn(
        &mut self,
        runtime_key: &str,
        workload: NanoWorkloadSpec,
    ) -> Result<NanoHandle> {
        self.registry.get_mut(runtime_key)?.spawn(workload)
    }

    pub(crate) fn resources(&mut self, handle: &NanoHandle) -> Result<NanoRuntimeResources> {
        self.registry.resources(handle)
    }

    pub(crate) fn stop(&mut self, handle: &NanoHandle) -> Result<NanoStopResult> {
        self.registry.stop(handle)
    }

    pub(crate) fn control(
        &mut self,
        handle: &NanoHandle,
        action: NanoRuntimeControlAction,
    ) -> Result<NanoRuntimeControlResult> {
        self.registry.control(handle, action)
    }

    pub(crate) fn snapshot(&mut self, handle: &NanoHandle) -> Result<NanoSnapshot> {
        self.registry.snapshot(handle)
    }

    pub(crate) fn restore(&mut self, snapshot: NanoSnapshot) -> Result<NanoHandle> {
        self.registry.restore(snapshot)
    }

    pub(crate) fn reconcile_abandoned(
        &mut self,
        workload: &NanoWorkloadSpec,
    ) -> Result<NanoRecoveryResult> {
        self.registry.reconcile_abandoned(workload)
    }

    #[cfg(test)]
    pub(crate) fn health(
        &mut self,
        handle: &NanoHandle,
    ) -> Result<sentinel_common::nano_runtime::NanoHealth> {
        self.registry.health(handle)
    }

    #[cfg(test)]
    pub(crate) fn from_registry(registry: NanoRuntimeRegistry) -> Self {
        Self { registry }
    }
}
