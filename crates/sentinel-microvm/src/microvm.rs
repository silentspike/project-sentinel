//! [`MicrovmNanoRuntime`]: erfuellt den `NanoRuntime`-Vertrag (#408) ueber Firecracker-microVMs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use sentinel_common::nano_runtime::{
    NanoExecRequest, NanoExecResult, NanoHandle, NanoHealth, NanoHealthState, NanoIsolationPolicy,
    NanoIsolationReport, NanoRuntime, NanoSnapshot, NanoSnapshotSemantics, NanoWorkloadSpec,
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

    /// Bootet eine frische microVM fuer `workload_id` (KVM + Kernel/rootfs vorausgesetzt).
    fn boot_fresh(&self, workload_id: &str) -> Result<FirecrackerProcess> {
        firecracker::ensure_kvm_available()?;
        let cfg = &self.config;
        if !Path::new(&cfg.kernel_image_path).exists() {
            bail!("Gast-Kernel-Image fehlt: {}", cfg.kernel_image_path);
        }
        if !Path::new(&cfg.rootfs_path).exists() {
            bail!("rootfs-Image fehlt: {}", cfg.rootfs_path);
        }
        std::fs::create_dir_all(&cfg.work_dir)?;

        let sock = self.api_sock_path(workload_id);
        let proc = FirecrackerProcess::launch(&cfg.firecracker_bin, &sock)?;
        firecracker::configure_machine(&sock, cfg.vcpu_count, cfg.mem_size_mib)?;
        firecracker::configure_boot_source(&sock, &cfg.kernel_image_path, &cfg.boot_args)?;
        firecracker::configure_rootfs(&sock, "rootfs", &cfg.rootfs_path, cfg.rootfs_read_only)?;
        // vsock-Geraet fuer optionale Host<->Gast-Steuerung (guest_cid 3); stale Listener entfernen.
        let vsock_uds = self.vsock_uds_path(workload_id);
        let _ = std::fs::remove_file(&vsock_uds);
        firecracker::configure_vsock(&sock, 3, &vsock_uds.to_string_lossy())?;
        firecracker::instance_start(&sock)?;
        Ok(proc)
    }

    fn teardown_workload(&mut self, workload_id: &str) {
        if let Some(mut proc) = self.processes.remove(workload_id) {
            proc.terminate();
        }
        let _ = std::fs::remove_file(self.vsock_uds_path(workload_id));
        self.workloads.remove(workload_id);
    }
}

impl Default for MicrovmNanoRuntime {
    fn default() -> Self {
        Self::detect()
    }
}

impl Drop for MicrovmNanoRuntime {
    fn drop(&mut self) {
        let ids: Vec<String> = self.processes.keys().cloned().collect();
        for id in ids {
            self.teardown_workload(&id);
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
        let proc = self.boot_fresh(&workload.workload_id)?;
        let pid = proc.pid();
        self.processes.insert(workload.workload_id.clone(), proc);
        self.workloads.insert(
            workload.workload_id.clone(),
            MicrovmWorkloadState {
                workload: workload.clone(),
            },
        );
        Ok(NanoHandle {
            runtime_key: RUNTIME_MICROVM.to_string(),
            workload_id: workload.workload_id,
            agent_id: workload.agent_id,
            pid: Some(pid),
        })
    }

    fn exec(&mut self, handle: &NanoHandle, request: NanoExecRequest) -> Result<NanoExecResult> {
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
        let workload = self
            .workloads
            .get(&handle.workload_id)
            .ok_or_else(|| anyhow!("unknown microVM workload '{}'", handle.workload_id))?
            .workload
            .clone();

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
        firecracker::ensure_kvm_available()?;

        self.teardown_workload(&snapshot.workload_id);
        std::fs::create_dir_all(&self.config.work_dir)?;

        let sock = self.api_sock_path(&snapshot.workload_id);
        let proc = FirecrackerProcess::launch(&self.config.firecracker_bin, &sock)?;
        // Snapshot traegt die vsock-Config; Firecracker legt den Host-Listener beim Load neu an,
        // daher den stale Listener vorab entfernen.
        let _ = std::fs::remove_file(self.vsock_uds_path(&snapshot.workload_id));
        firecracker::load_snapshot(&sock, &payload.snapshot_path, &payload.mem_file_path, true)?;
        let pid = proc.pid();

        self.processes.insert(snapshot.workload_id.clone(), proc);
        self.workloads.insert(
            snapshot.workload_id.clone(),
            MicrovmWorkloadState {
                workload: payload.workload,
            },
        );

        Ok(NanoHandle {
            runtime_key: self.runtime_key().to_string(),
            workload_id: snapshot.workload_id,
            agent_id: snapshot.agent_id,
            pid: Some(pid),
        })
    }

    fn health(&mut self, handle: &NanoHandle) -> Result<NanoHealth> {
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

    #[test]
    fn sanitize_makes_paths_safe() {
        assert_eq!(sanitize("ecs-world-migrate-123"), "ecs-world-migrate-123");
        assert_eq!(sanitize("agent/01:weird"), "agent_01_weird");
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
    fn runtime_key_is_microvm() {
        let rt = MicrovmNanoRuntime::detect();
        assert_eq!(rt.runtime_key(), RUNTIME_MICROVM);
    }
}
