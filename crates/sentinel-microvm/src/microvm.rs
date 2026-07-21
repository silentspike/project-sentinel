//! [`MicrovmNanoRuntime`]: erfuellt den `NanoRuntime`-Vertrag (#408) ueber Firecracker-microVMs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use sentinel_common::nano_runtime::{
    ensure_handle_instance, ensure_handle_runtime, NanoExecRequest, NanoExecResult, NanoHandle,
    NanoHealth, NanoHealthState, NanoIsolationPolicy, NanoIsolationReport, NanoRuntime,
    NanoRuntimeResources, NanoSnapshot, NanoSnapshotSemantics, NanoStopResult, NanoWorkloadSpec,
    RUNTIME_MICROVM,
};
use serde::{Deserialize, Serialize};

use crate::firecracker::{self, FirecrackerProcess};

/// Konfiguration der microVM-Runtime (Binary, Gast-Kernel/rootfs, Ressourcen, Arbeitsverzeichnis).
#[derive(Debug, Clone)]
pub struct MicrovmConfig {
    pub firecracker_bin: String,
    pub kernel_image_path: String,
    pub rootfs_path: String,
    pub rootfs_read_only: bool,
    /// Guest PID 1 that consumes `sentinel.workload_spec_hex` and executes the
    /// requested command. The kernel is never allowed to fall back to a
    /// generic rootfs init for a Nano workload.
    pub guest_init_path: String,
    /// Basisverzeichnis fuer API-Sockets und Snapshot-Dateien.
    pub work_dir: String,
    pub vcpu_count: u32,
    pub mem_size_mib: u32,
    pub boot_args: String,
}

impl MicrovmConfig {
    /// Liest die Konfiguration aus Environment-Variablen mit konservativen Defaults
    /// (Minimal-Setup unter `/opt/sentinel/microvm/`).
    pub fn from_env() -> Self {
        let env =
            |key: &str, default: &str| std::env::var(key).unwrap_or_else(|_| default.to_string());
        Self {
            firecracker_bin: env("SENTINEL_FIRECRACKER_BIN", "firecracker"),
            kernel_image_path: env("SENTINEL_MICROVM_KERNEL", "/opt/sentinel/microvm/vmlinux"),
            rootfs_path: env(
                "SENTINEL_MICROVM_ROOTFS",
                "/opt/sentinel/microvm/rootfs.ext4",
            ),
            rootfs_read_only: true,
            guest_init_path: env(
                "SENTINEL_MICROVM_GUEST_INIT",
                "/opt/sentinel/bin/sentinel-nano-init",
            ),
            work_dir: env("SENTINEL_MICROVM_WORKDIR", "/opt/sentinel/microvm/run"),
            vcpu_count: 1,
            mem_size_mib: 128,
            boot_args: env(
                "SENTINEL_MICROVM_BOOT_ARGS",
                "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda",
            ),
        }
    }
}

impl Default for MicrovmConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

/// STABILE Snapshot-Metadaten (KEIN volatiler Guest-RAM — der liegt in den referenzierten
/// Firecracker mem/state-Dateien). Deterministisch je `workload_id` => `restore(snapshot(x))`
/// ist payload-stabil (Conformance-Vertrag).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MicrovmSnapshotPayload {
    workload: NanoWorkloadSpec,
    vcpu_count: u32,
    mem_size_mib: u32,
    kernel_image_path: String,
    rootfs_path: String,
    snapshot_path: String,
    mem_file_path: String,
    semantics_note: String,
}

struct MicrovmWorkloadState {
    instance_id: uuid::Uuid,
    workload: NanoWorkloadSpec,
}

/// microVM-Runtime: jeder Workload laeuft als eigene Firecracker-microVM (KVM).
pub struct MicrovmNanoRuntime {
    config: MicrovmConfig,
    processes: HashMap<String, FirecrackerProcess>,
    workloads: HashMap<String, MicrovmWorkloadState>,
}

impl MicrovmNanoRuntime {
    /// Erstellt die Runtime mit Environment-/Default-Konfiguration.
    pub fn detect() -> Self {
        Self::with_config(MicrovmConfig::from_env())
    }

    pub fn with_config(config: MicrovmConfig) -> Self {
        Self {
            config,
            processes: HashMap::new(),
            workloads: HashMap::new(),
        }
    }

    /// True wenn KVM nutzbar ist.
    pub fn kvm_available(&self) -> bool {
        firecracker::kvm_available()
    }

    fn api_sock_path(&self, workload_id: &str) -> PathBuf {
        Path::new(&self.config.work_dir).join(format!("{}.sock", sanitize(workload_id)))
    }

    fn snapshot_dir(&self, workload_id: &str) -> PathBuf {
        Path::new(&self.config.work_dir)
            .join("snapshots")
            .join(sanitize(workload_id))
    }

    fn vsock_uds_path(&self, workload_id: &str) -> PathBuf {
        Path::new(&self.config.work_dir).join(format!("{}.vsock", sanitize(workload_id)))
    }

    fn workload_boot_args(&self, workload: &NanoWorkloadSpec) -> Result<String> {
        if workload.command.is_empty() {
            return Err(anyhow!(
                "microVM workload '{}' requires a guest command",
                workload.workload_id
            ));
        }
        if !self.config.guest_init_path.starts_with('/')
            || self.config.guest_init_path.chars().any(char::is_whitespace)
        {
            return Err(anyhow!(
                "microVM guest init must be an absolute path without whitespace"
            ));
        }
        let encoded = serde_json::to_vec(workload)?
            .into_iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let boot_args = format!(
            "{} init={} sentinel.nano_contract=kernel-cmdline-v1 sentinel.workload_spec_hex={encoded}",
            self.config.boot_args, self.config.guest_init_path
        );
        if boot_args.len() > 4096 {
            return Err(anyhow!(
                "microVM workload '{}' exceeds the 4096-byte guest boot contract",
                workload.workload_id
            ));
        }
        Ok(boot_args)
    }

    /// Boots a fresh microVM and passes the complete workload identity and
    /// command to the guest launcher through the versioned kernel-cmdline
    /// contract. A generic rootfs boot without an executable workload is
    /// rejected.
    fn boot_fresh(&self, workload: &NanoWorkloadSpec) -> Result<FirecrackerProcess> {
        firecracker::ensure_kvm_available()?;
        let cfg = &self.config;
        if !Path::new(&cfg.kernel_image_path).exists() {
            bail!("Gast-Kernel-Image fehlt: {}", cfg.kernel_image_path);
        }
        if !Path::new(&cfg.rootfs_path).exists() {
            bail!("rootfs-Image fehlt: {}", cfg.rootfs_path);
        }
        std::fs::create_dir_all(&cfg.work_dir)?;

        let sock = self.api_sock_path(&workload.workload_id);
        let proc = FirecrackerProcess::launch(&cfg.firecracker_bin, &sock)?;
        firecracker::configure_machine(&sock, cfg.vcpu_count, cfg.mem_size_mib)?;
        let boot_args = self.workload_boot_args(workload)?;
        firecracker::configure_boot_source(&sock, &cfg.kernel_image_path, &boot_args)?;
        firecracker::configure_rootfs(&sock, "rootfs", &cfg.rootfs_path, cfg.rootfs_read_only)?;
        // vsock-Geraet fuer optionale Host<->Gast-Steuerung (guest_cid 3); stale Listener entfernen.
        let vsock_uds = self.vsock_uds_path(&workload.workload_id);
        let _ = std::fs::remove_file(&vsock_uds);
        firecracker::configure_vsock(&sock, 3, &vsock_uds.to_string_lossy())?;
        firecracker::instance_start(&sock)?;
        Ok(proc)
    }

    fn remove_runtime_file(path: &Path) -> Result<bool> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn teardown_workload(&mut self, workload_id: &str) -> Result<bool> {
        let stopped = self.processes.contains_key(workload_id)
            || self.workloads.contains_key(workload_id)
            || self.api_sock_path(workload_id).exists()
            || self.vsock_uds_path(workload_id).exists();
        if let Some(proc) = self.processes.get_mut(workload_id) {
            proc.terminate()?;
        }
        Self::remove_runtime_file(&self.api_sock_path(workload_id))?;
        Self::remove_runtime_file(&self.vsock_uds_path(workload_id))?;
        self.processes.remove(workload_id);
        self.workloads.remove(workload_id);
        Ok(stopped)
    }

    fn ensure_runtime_paths_available(&self, workload_id: &str) -> Result<()> {
        let runtime_name = sanitize(workload_id);
        if let Some(existing) = self
            .workloads
            .keys()
            .find(|existing| existing.as_str() != workload_id && sanitize(existing) == runtime_name)
        {
            return Err(anyhow!(
                "microVM workload '{}' collides with active workload '{}' after path sanitization",
                workload_id,
                existing
            ));
        }
        Ok(())
    }

    fn ensure_snapshot_identity(
        snapshot_workload_id: &str,
        workload: &NanoWorkloadSpec,
    ) -> Result<()> {
        if workload.workload_id != snapshot_workload_id {
            return Err(anyhow!(
                "microVM snapshot workload '{}' does not match envelope '{}'",
                workload.workload_id,
                snapshot_workload_id
            ));
        }
        Ok(())
    }
}

impl Default for MicrovmNanoRuntime {
    fn default() -> Self {
        Self::detect()
    }
}

impl Drop for MicrovmNanoRuntime {
    fn drop(&mut self) {
        let mut ids: Vec<String> = self
            .processes
            .keys()
            .chain(self.workloads.keys())
            .cloned()
            .collect();
        ids.sort();
        ids.dedup();
        for id in ids {
            let _ = self.teardown_workload(&id);
        }
    }
}

impl NanoRuntime for MicrovmNanoRuntime {
    fn runtime_key(&self) -> &'static str {
        RUNTIME_MICROVM
    }

    fn spawn(&mut self, workload: NanoWorkloadSpec) -> Result<NanoHandle> {
        if workload.workload_id.is_empty() {
            return Err(anyhow!("microVM workload requires workload_id"));
        }
        if self.workloads.contains_key(&workload.workload_id)
            || self.processes.contains_key(&workload.workload_id)
        {
            return Err(anyhow!(
                "microVM workload '{}' is already active",
                workload.workload_id
            ));
        }
        self.ensure_runtime_paths_available(&workload.workload_id)?;
        let proc = self.boot_fresh(&workload)?;
        let pid = proc.pid();
        let instance_id = uuid::Uuid::new_v4();
        self.processes.insert(workload.workload_id.clone(), proc);
        self.workloads.insert(
            workload.workload_id.clone(),
            MicrovmWorkloadState {
                instance_id,
                workload: workload.clone(),
            },
        );
        Ok(NanoHandle {
            instance_id,
            runtime_key: RUNTIME_MICROVM.to_string(),
            workload_id: workload.workload_id,
            agent_id: workload.agent_id,
            pid: Some(pid),
        })
    }

    fn stop(&mut self, handle: &NanoHandle) -> Result<NanoStopResult> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        if let Some(state) = self.workloads.get(&handle.workload_id) {
            ensure_handle_instance(handle, state.instance_id)?;
        }
        Ok(NanoStopResult::new(
            self.runtime_key(),
            &handle.workload_id,
            self.teardown_workload(&handle.workload_id)?,
        ))
    }

    fn resources(&self, handle: &NanoHandle) -> Result<NanoRuntimeResources> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        let state = self
            .workloads
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("unknown microVM workload '{}'", handle.workload_id))?;
        ensure_handle_instance(handle, state.instance_id)?;
        let process = self
            .processes
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("missing microVM process '{}'", handle.workload_id))?;
        Ok(NanoRuntimeResources {
            instance_id: Some(state.instance_id),
            pid: Some(process.pid()),
            ..NanoRuntimeResources::default()
        })
    }

    fn exec(&mut self, handle: &NanoHandle, request: NanoExecRequest) -> Result<NanoExecResult> {
        self.resources(handle)?;
        let output = match request.operation.as_str() {
            "health" => format!("{:?}", self.health(handle)?.state),
            "state" => {
                let sock = self.api_sock_path(&handle.workload_id);
                firecracker::instance_state(&sock).unwrap_or_else(|_| "unknown".to_string())
            }
            other => bail!("microVM exec operation '{other}' wird nicht unterstuetzt"),
        };
        Ok(NanoExecResult {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            success: true,
            output,
        })
    }

    fn snapshot(&mut self, handle: &NanoHandle) -> Result<NanoSnapshot> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        let workload = self
            .workloads
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("unknown microVM workload '{}'", handle.workload_id))?;
        ensure_handle_instance(handle, workload.instance_id)?;
        let workload = workload.workload.clone();

        let sock = self.api_sock_path(&handle.workload_id);
        let dir = self.snapshot_dir(&handle.workload_id);
        std::fs::create_dir_all(&dir)?;
        let snap_path = dir.join("snapshot.state");
        let mem_path = dir.join("snapshot.mem");
        let snap_str = snap_path.to_string_lossy().into_owned();
        let mem_str = mem_path.to_string_lossy().into_owned();

        // Firecracker verlangt: Pause -> Snapshot -> Resume. Resume best-effort, damit ein
        // Snapshot-Fehler die VM nicht pausiert zuruecklaesst.
        firecracker::pause(&sock)?;
        let created = firecracker::create_snapshot(&sock, &snap_str, &mem_str);
        let _ = firecracker::resume(&sock);
        created?;

        let payload = MicrovmSnapshotPayload {
            workload,
            vcpu_count: self.config.vcpu_count,
            mem_size_mib: self.config.mem_size_mib,
            kernel_image_path: self.config.kernel_image_path.clone(),
            rootfs_path: self.config.rootfs_path.clone(),
            snapshot_path: snap_str,
            mem_file_path: mem_str,
            semantics_note: "microVM snapshot: payload = stabile Metadaten + deterministische \
                 Firecracker mem/state-Pfade je workload_id; der Guest-RAM liegt in diesen Dateien"
                .to_string(),
        };

        Ok(NanoSnapshot {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            agent_id: handle.agent_id,
            semantics: NanoSnapshotSemantics::MicrovmMemory,
            payload: serde_json::to_value(payload)?,
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
        if snapshot.semantics != NanoSnapshotSemantics::MicrovmMemory {
            return Err(anyhow!(
                "microVM restore requires MicrovmMemory snapshot, got {:?}",
                snapshot.semantics
            ));
        }
        let payload: MicrovmSnapshotPayload = serde_json::from_value(snapshot.payload)?;
        Self::ensure_snapshot_identity(&snapshot.workload_id, &payload.workload)?;
        firecracker::ensure_kvm_available()?;

        self.ensure_runtime_paths_available(&snapshot.workload_id)?;
        self.teardown_workload(&snapshot.workload_id)?;
        std::fs::create_dir_all(&self.config.work_dir)?;

        let sock = self.api_sock_path(&snapshot.workload_id);
        let proc = FirecrackerProcess::launch(&self.config.firecracker_bin, &sock)?;
        // Snapshot traegt die vsock-Config; Firecracker legt den Host-Listener beim Load neu an,
        // daher den stale Listener vorab entfernen.
        let _ = std::fs::remove_file(self.vsock_uds_path(&snapshot.workload_id));
        firecracker::load_snapshot(&sock, &payload.snapshot_path, &payload.mem_file_path, true)?;
        let pid = proc.pid();
        let instance_id = uuid::Uuid::new_v4();

        self.processes.insert(snapshot.workload_id.clone(), proc);
        self.workloads.insert(
            snapshot.workload_id.clone(),
            MicrovmWorkloadState {
                instance_id,
                workload: payload.workload,
            },
        );

        Ok(NanoHandle {
            instance_id,
            runtime_key: self.runtime_key().to_string(),
            workload_id: snapshot.workload_id,
            agent_id: snapshot.agent_id,
            pid: Some(pid),
        })
    }

    fn health(&mut self, handle: &NanoHandle) -> Result<NanoHealth> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        if let Some(state) = self.workloads.get(&handle.workload_id) {
            ensure_handle_instance(handle, state.instance_id)?;
        }
        let sock = self.api_sock_path(&handle.workload_id);
        let running = self
            .processes
            .get_mut(&handle.workload_id)
            .map(|proc| proc.is_running())
            .unwrap_or(false);

        let (state, detail) = if !running {
            (
                NanoHealthState::Stopped,
                "Firecracker-Prozess beendet".to_string(),
            )
        } else {
            match firecracker::instance_state(&sock) {
                Ok(s) if s.eq_ignore_ascii_case("Running") => {
                    (NanoHealthState::Healthy, format!("microVM state={s}"))
                }
                Ok(s) => (NanoHealthState::Degraded, format!("microVM state={s}")),
                Err(e) => (
                    NanoHealthState::Degraded,
                    format!("Prozess laeuft, API nicht abfragbar: {e}"),
                ),
            }
        };

        Ok(NanoHealth {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            state,
            detail,
        })
    }

    fn isolate(
        &mut self,
        handle: &NanoHandle,
        _policy: NanoIsolationPolicy,
    ) -> Result<NanoIsolationReport> {
        ensure_handle_runtime(handle, self.runtime_key())?;
        if let Some(state) = self.workloads.get(&handle.workload_id) {
            ensure_handle_instance(handle, state.instance_id)?;
        }
        let applied = self.processes.contains_key(&handle.workload_id);
        Ok(NanoIsolationReport {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            applied,
            detail:
                "microVM = KVM-hardware-virtualisierte Isolation (eigener Gast-Kernel, vCPU, Speicher)"
                    .to_string(),
        })
    }
}

/// Macht eine `workload_id` dateisystem-sicher (fuer Socket-/Snapshot-Pfade).
fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::FileTypeExt;

    fn fixture_workload(workload_id: &str, agent_id: u16) -> NanoWorkloadSpec {
        NanoWorkloadSpec {
            workload_id: workload_id.to_string(),
            runtime_key: Some(RUNTIME_MICROVM.to_string()),
            agent_id: Some(sentinel_common::AgentId(agent_id)),
            agent_name: format!("Fixture {workload_id}"),
            role: "Tester".to_string(),
            room_id: "empfang".to_string(),
            shift_set: 1,
            command: vec!["/opt/sentinel/bin/agent-runtime".to_string()],
            capabilities: Vec::new(),
            metadata: Default::default(),
            ecs_snapshot: None,
        }
    }

    fn insert_fixture(runtime: &mut MicrovmNanoRuntime, workload: NanoWorkloadSpec) -> NanoHandle {
        let workload_id = workload.workload_id.clone();
        let api_sock = runtime.api_sock_path(&workload_id);
        let process = FirecrackerProcess::launch_fixture(&api_sock).unwrap();
        let pid = process.pid();
        let instance_id = uuid::Uuid::new_v4();
        std::fs::write(runtime.vsock_uds_path(&workload_id), b"fixture vsock").unwrap();
        runtime.workloads.insert(
            workload_id.clone(),
            MicrovmWorkloadState {
                instance_id,
                workload,
            },
        );
        runtime.processes.insert(workload_id.clone(), process);
        NanoHandle {
            instance_id,
            runtime_key: RUNTIME_MICROVM.to_string(),
            workload_id,
            agent_id: None,
            pid: Some(pid),
        }
    }

    #[test]
    fn sanitize_makes_paths_safe() {
        assert_eq!(sanitize("ecs-world-migrate-123"), "ecs-world-migrate-123");
        assert_eq!(sanitize("agent/01:weird"), "agent_01_weird");
    }

    #[test]
    fn path_alias_duplicate_is_rejected_without_touching_owner() {
        let temp = tempfile::tempdir().unwrap();
        let config = MicrovmConfig {
            work_dir: temp.path().to_string_lossy().into_owned(),
            ..MicrovmConfig::default()
        };
        let mut runtime = MicrovmNanoRuntime::with_config(config);
        let owner = insert_fixture(&mut runtime, fixture_workload("fixture/a", 1));

        assert!(runtime.ensure_runtime_paths_available("fixture_a").is_err());
        assert!(runtime.processes.contains_key(&owner.workload_id));
        assert!(runtime.workloads.contains_key(&owner.workload_id));
        assert_eq!(
            runtime.stop(&owner).unwrap().outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
    }

    #[test]
    fn restore_rejects_mismatched_workload_identity_before_kvm_or_mutation() {
        let payload = fixture_workload("payload-workload", 1);
        assert!(
            MicrovmNanoRuntime::ensure_snapshot_identity("envelope-workload", &payload).is_err()
        );
    }

    #[test]
    fn config_defaults_are_conservative() {
        let cfg = MicrovmConfig::from_env();
        // Wenn keine Env gesetzt ist, greifen die /opt/sentinel/microvm-Defaults.
        assert!(cfg.vcpu_count >= 1);
        assert!(cfg.mem_size_mib >= 64);
        assert!(!cfg.firecracker_bin.is_empty());
        assert!(cfg.boot_args.contains("console="));
    }

    #[test]
    fn boot_contract_carries_complete_workload_identity_and_command() {
        let runtime = MicrovmNanoRuntime::with_config(MicrovmConfig {
            boot_args: "console=ttyS0 root=/dev/vda".to_string(),
            ..MicrovmConfig::default()
        });
        let workload = fixture_workload("identity-contract", 17);
        let boot_args = runtime.workload_boot_args(&workload).unwrap();
        assert!(boot_args.contains("sentinel.nano_contract=kernel-cmdline-v1"));
        assert!(boot_args.contains("init=/opt/sentinel/bin/sentinel-nano-init"));
        let encoded = boot_args
            .split("sentinel.workload_spec_hex=")
            .nth(1)
            .expect("encoded workload contract");
        let bytes = encoded
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let text = std::str::from_utf8(pair).unwrap();
                u8::from_str_radix(text, 16).unwrap()
            })
            .collect::<Vec<_>>();
        let decoded: NanoWorkloadSpec = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.workload_id, workload.workload_id);
        assert_eq!(decoded.agent_id, workload.agent_id);
        assert_eq!(decoded.agent_name, workload.agent_name);
        assert_eq!(decoded.command, workload.command);

        let mut invalid = workload;
        invalid.command.clear();
        assert!(runtime.workload_boot_args(&invalid).is_err());
    }

    #[test]
    fn runtime_key_is_microvm() {
        let rt = MicrovmNanoRuntime::detect();
        assert_eq!(rt.runtime_key(), RUNTIME_MICROVM);
    }

    #[test]
    fn stop_fixture_reaps_process_removes_sockets_and_preserves_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let config = MicrovmConfig {
            work_dir: temp.path().to_string_lossy().into_owned(),
            ..MicrovmConfig::default()
        };
        let mut runtime = MicrovmNanoRuntime::with_config(config);
        let handle_a = insert_fixture(&mut runtime, fixture_workload("fixture-a", 1));
        let handle_b = insert_fixture(&mut runtime, fixture_workload("fixture-b", 2));
        assert!(
            std::fs::metadata(runtime.api_sock_path(&handle_a.workload_id))
                .unwrap()
                .file_type()
                .is_socket()
        );
        let stale_for_b = NanoHandle {
            instance_id: handle_a.instance_id,
            ..handle_b.clone()
        };
        assert!(runtime.stop(&stale_for_b).is_err());
        let snapshot_file = runtime
            .snapshot_dir(&handle_a.workload_id)
            .join("retained.state");
        std::fs::create_dir_all(snapshot_file.parent().unwrap()).unwrap();
        std::fs::write(&snapshot_file, b"retained snapshot").unwrap();

        let stopped = runtime.stop(&handle_a).unwrap();
        assert_eq!(
            stopped.outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
        assert!(!Path::new(&format!("/proc/{}", handle_a.pid.unwrap())).exists());
        assert!(!runtime.api_sock_path(&handle_a.workload_id).exists());
        assert!(!runtime.vsock_uds_path(&handle_a.workload_id).exists());
        assert!(snapshot_file.exists());
        assert_eq!(
            runtime.health(&handle_a).unwrap().state,
            NanoHealthState::Stopped
        );
        assert!(matches!(
            runtime.health(&handle_b).unwrap().state,
            NanoHealthState::Healthy | NanoHealthState::Degraded
        ));

        let replay = runtime.stop(&handle_a).unwrap();
        assert_eq!(
            replay.outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::AlreadyStopped
        );
        assert_eq!(
            runtime.stop(&handle_b).unwrap().outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
    }

    #[test]
    fn failed_socket_cleanup_retains_ownership_for_retry() {
        let temp = tempfile::tempdir().unwrap();
        let config = MicrovmConfig {
            work_dir: temp.path().to_string_lossy().into_owned(),
            ..MicrovmConfig::default()
        };
        let mut runtime = MicrovmNanoRuntime::with_config(config);
        let handle = insert_fixture(&mut runtime, fixture_workload("fixture-retry", 1));
        let vsock_path = runtime.vsock_uds_path(&handle.workload_id);
        std::fs::remove_file(&vsock_path).unwrap();
        std::fs::create_dir(&vsock_path).unwrap();

        assert!(runtime.stop(&handle).is_err());
        assert!(runtime.processes.contains_key(&handle.workload_id));
        assert!(runtime.workloads.contains_key(&handle.workload_id));
        assert!(!runtime.api_sock_path(&handle.workload_id).exists());
        assert!(std::fs::metadata(&vsock_path).unwrap().file_type().is_dir());

        std::fs::remove_dir(&vsock_path).unwrap();
        assert_eq!(
            runtime.stop(&handle).unwrap().outcome,
            sentinel_common::nano_runtime::NanoStopOutcome::Stopped
        );
        assert!(!runtime.processes.contains_key(&handle.workload_id));
        assert!(!runtime.workloads.contains_key(&handle.workload_id));
        assert!(!runtime.api_sock_path(&handle.workload_id).exists());
        assert!(!vsock_path.exists());
    }
}
