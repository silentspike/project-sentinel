use std::collections::HashMap;

use anyhow::{anyhow, Result};
use bevy_ecs::prelude::World;
use sentinel_common::components::{AgentIdentity, ShiftInfo};
use sentinel_common::nano_runtime::{
    NanoExecRequest, NanoExecResult, NanoHandle, NanoHealth, NanoHealthState, NanoIsolationPolicy,
    NanoIsolationReport, NanoRuntime, NanoSnapshot, NanoSnapshotSemantics, NanoWorkloadSpec,
    RUNTIME_ECS_NATIVE,
};
use sentinel_common::AgentId;

use crate::{AgentStatus, RuntimeOrchestrator};

pub struct EcsNativeRuntime {
    orchestrator: RuntimeOrchestrator,
    world: World,
    handles: HashMap<String, AgentId>,
    max_agents: usize,
}

impl EcsNativeRuntime {
    pub fn new(max_agents: usize) -> Self {
        let (world, _schedule) = sentinel_ecs::create_simulation_world();
        Self {
            orchestrator: RuntimeOrchestrator::new(max_agents),
            world,
            handles: HashMap::new(),
            max_agents,
        }
    }

    fn shift_info(workload: &NanoWorkloadSpec) -> ShiftInfo {
        let (start, end) = match workload.shift_set {
            1 => (6, 14),
            2 => (14, 22),
            3 => (22, 6),
            0 => (0, 0),
            _ => (6, 14),
        };
        ShiftInfo {
            shift_set: workload.shift_set,
            shift_start_hour: start,
            shift_end_hour: end,
            is_on_duty: true,
        }
    }

    fn health_state(status: AgentStatus) -> NanoHealthState {
        match status {
            AgentStatus::Active | AgentStatus::Sleeping => NanoHealthState::Healthy,
            AgentStatus::Suspended => NanoHealthState::Degraded,
            AgentStatus::Errored => NanoHealthState::Unavailable,
        }
    }

    fn rebuild_orchestrator_from_ecs_snapshot(
        &mut self,
        snapshot: &sentinel_common::EcsSnapshot,
    ) -> Result<()> {
        self.orchestrator = RuntimeOrchestrator::new(self.max_agents);
        self.handles.clear();

        for (id, identity) in &snapshot.identities {
            let agent_id = AgentId(*id);
            let shift = snapshot
                .shift_infos
                .iter()
                .find(|(shift_id, _)| shift_id == id)
                .map(|(_, shift)| shift.clone())
                .unwrap_or(ShiftInfo {
                    shift_set: 1,
                    shift_start_hour: 6,
                    shift_end_hour: 14,
                    is_on_duty: true,
                });
            let room_id = snapshot
                .positions
                .iter()
                .find(|(pos_id, _)| pos_id == id)
                .map(|(_, pos)| pos.room_id.as_str())
                .unwrap_or("empfang");
            self.orchestrator
                .spawn_agent(identity.clone(), shift, room_id)?;
            self.handles
                .insert(format!("ecs-native-{}", agent_id.0), agent_id);
        }

        Ok(())
    }
}

impl Default for EcsNativeRuntime {
    fn default() -> Self {
        Self::new(64)
    }
}

impl NanoRuntime for EcsNativeRuntime {
    fn runtime_key(&self) -> &'static str {
        RUNTIME_ECS_NATIVE
    }

    fn spawn(&mut self, workload: NanoWorkloadSpec) -> Result<NanoHandle> {
        let agent_id = workload
            .agent_id
            .ok_or_else(|| anyhow!("ecs-native workload requires agent_id"))?;
        let identity = AgentIdentity {
            agent_id,
            name: workload.agent_name.clone(),
            role: workload.role.clone(),
        };
        let shift = Self::shift_info(&workload);
        let room_id = if workload.room_id.is_empty() {
            "empfang"
        } else {
            workload.room_id.as_str()
        };

        self.orchestrator
            .spawn_agent(identity, shift, room_id)
            .map_err(|error| anyhow!("ecs-native orchestrator spawn failed: {error}"))?;
        sentinel_ecs::spawn_agent(
            &mut self.world,
            agent_id,
            &workload.agent_name,
            &workload.role,
            workload.shift_set,
            room_id,
        );
        self.handles.insert(workload.workload_id.clone(), agent_id);

        Ok(NanoHandle {
            runtime_key: self.runtime_key().to_string(),
            workload_id: workload.workload_id,
            agent_id: Some(agent_id),
            pid: None,
        })
    }

    fn exec(&mut self, handle: &NanoHandle, request: NanoExecRequest) -> Result<NanoExecResult> {
        let health = self.health(handle)?;
        let output = match request.operation.as_str() {
            "health" => format!("{:?}", health.state),
            "snapshot" => self.snapshot(handle)?.semantics_string(),
            other => {
                return Err(anyhow!(
                    "ecs-native exec operation '{other}' is not supported"
                ))
            }
        };

        Ok(NanoExecResult {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            success: true,
            output,
        })
    }

    fn snapshot(&mut self, handle: &NanoHandle) -> Result<NanoSnapshot> {
        if let Some(agent_id) = handle.agent_id {
            if !self.orchestrator.agents().contains_key(&agent_id) {
                return Err(anyhow!("ecs-native handle references unknown {agent_id}"));
            }
        }
        let ecs_snapshot = sentinel_ecs::snapshot_ecs_state(&mut self.world);
        Ok(NanoSnapshot {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            agent_id: handle.agent_id,
            semantics: NanoSnapshotSemantics::EcsWorld,
            payload: serde_json::to_value(ecs_snapshot)?,
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
        if snapshot.semantics != NanoSnapshotSemantics::EcsWorld {
            return Err(anyhow!(
                "ecs-native restore requires EcsWorld snapshot, got {:?}",
                snapshot.semantics
            ));
        }

        let ecs_snapshot: sentinel_common::EcsSnapshot = serde_json::from_value(snapshot.payload)?;
        sentinel_ecs::restore_ecs_state(&mut self.world, &ecs_snapshot);
        self.rebuild_orchestrator_from_ecs_snapshot(&ecs_snapshot)?;

        Ok(NanoHandle {
            runtime_key: self.runtime_key().to_string(),
            workload_id: snapshot.workload_id,
            agent_id: snapshot.agent_id,
            pid: None,
        })
    }

    fn health(&mut self, handle: &NanoHandle) -> Result<NanoHealth> {
        let agent_id = handle
            .agent_id
            .ok_or_else(|| anyhow!("ecs-native health requires agent_id"))?;
        let state = self
            .orchestrator
            .agents()
            .get(&agent_id)
            .map(|handle| Self::health_state(handle.status))
            .ok_or_else(|| anyhow!("ecs-native handle references stopped {agent_id}"))?;

        Ok(NanoHealth {
            runtime_key: self.runtime_key().to_string(),
            workload_id: handle.workload_id.clone(),
            state,
            detail: "logical ECS-native lifecycle state".to_string(),
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
            detail: "logical ECS isolation only; no process boundary".to_string(),
        })
    }
}

/// Ergebnis einer lokalen Live-Migration (#413).
#[derive(Debug, Clone)]
pub struct MigrationOutcome {
    pub runtime_key: String,
    pub agent_count: u32,
    pub from_handle: String,
    pub to_handle: String,
}

/// Migriert eine ECS-native Instanz lokal (#413): seedet eine Quell-Instanz aus dem Welt-Snapshot,
/// uebergibt sie via `NanoRuntime::migrate` (Trait #408: snapshot(source) -> restore(target)) auf
/// eine frische Ziel-Instanz und beendet die Quelle (drop). Kontrakt-treu; die Live-Daemon-Welt
/// bleibt unangetastet — migriert wird die aus dem Snapshot abgeleitete Instanz (LOKAL).
pub fn migrate_ecs_native_instance(
    ecs_snapshot: &sentinel_common::EcsSnapshot,
    max_agents: usize,
    workload_id: &str,
) -> Result<MigrationOutcome> {
    let agent_count = ecs_snapshot.identities.len() as u32;
    // 1. Quell-Instanz aus dem Welt-Snapshot seeden.
    let mut source = EcsNativeRuntime::new(max_agents);
    let seed = NanoSnapshot {
        runtime_key: RUNTIME_ECS_NATIVE.to_string(),
        workload_id: workload_id.to_string(),
        agent_id: None,
        semantics: NanoSnapshotSemantics::EcsWorld,
        payload: serde_json::to_value(ecs_snapshot)?,
    };
    let source_handle = source.restore(seed)?;
    // 2. Frische Ziel-Instanz; Handoff via Trait-migrate (snapshot(source) -> restore(target)).
    let mut target = EcsNativeRuntime::new(max_agents);
    let target_handle = source.migrate(&mut target, &source_handle)?;
    let outcome = MigrationOutcome {
        runtime_key: RUNTIME_ECS_NATIVE.to_string(),
        agent_count,
        from_handle: source_handle.workload_id.clone(),
        to_handle: target_handle.workload_id.clone(),
    };
    // 3. Quelle sauber beenden.
    drop(source);
    drop(target);
    Ok(outcome)
}

trait SnapshotSemanticsLabel {
    fn semantics_string(&self) -> String;
}

impl SnapshotSemanticsLabel for NanoSnapshot {
    fn semantics_string(&self) -> String {
        format!("{:?}", self.semantics)
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use sentinel_common::nano_runtime::{NanoWorkloadSpec, RUNTIME_ECS_NATIVE};
    use sentinel_common::AgentId;
    use std::collections::BTreeMap;

    fn workload_spec(id: u16, workload_id: &str) -> NanoWorkloadSpec {
        NanoWorkloadSpec {
            workload_id: workload_id.to_string(),
            runtime_key: Some(RUNTIME_ECS_NATIVE.to_string()),
            agent_id: Some(AgentId(id)),
            agent_name: format!("Agent {id}"),
            role: "Tester".to_string(),
            room_id: "empfang".to_string(),
            shift_set: 1,
            command: Vec::new(),
            capabilities: Vec::new(),
            metadata: BTreeMap::new(),
            ecs_snapshot: None,
        }
    }

    /// Spawnt `n` Agents in einer frischen Instanz und liefert den Welt-Snapshot als EcsSnapshot.
    fn seeded_ecs_snapshot(n: u16) -> sentinel_common::EcsSnapshot {
        let mut rt = EcsNativeRuntime::new(64);
        let mut last = None;
        for id in 1..=n {
            last = Some(
                rt.spawn(workload_spec(id, &format!("ecs-native-{id}")))
                    .expect("spawn"),
            );
        }
        let handle = last.expect("mindestens ein Agent");
        let snap = rt.snapshot(&handle).expect("snapshot");
        serde_json::from_value(snap.payload).expect("EcsSnapshot deser")
    }

    #[test]
    fn migrate_ecs_native_instance_preserves_agent_count_and_identity() {
        // #413 AC-1/AC-2: lokale Migration einer ECS-native-Instanz liefert ein konsistentes Outcome.
        let ecs_snapshot = seeded_ecs_snapshot(2);
        let outcome = migrate_ecs_native_instance(&ecs_snapshot, 64, "ecs-world-migrate-test")
            .expect("migrate");

        assert_eq!(outcome.runtime_key, RUNTIME_ECS_NATIVE);
        assert_eq!(
            outcome.agent_count, 2,
            "agent_count muss der Anzahl der Identities entsprechen"
        );
        // migrate (Trait-Default snapshot->restore) bewahrt die Workload-Identitaet.
        assert_eq!(outcome.from_handle, "ecs-world-migrate-test");
        assert_eq!(outcome.to_handle, "ecs-world-migrate-test");
    }

    #[test]
    fn migration_roundtrip_preserves_all_identities() {
        // #413 AC-2: Zustand nach Migration identisch (Roundtrip-Invariante) — der
        // snapshot(source)->restore(target)-Handoff darf keine Identity verlieren.
        let original = seeded_ecs_snapshot(3);
        let mut original_ids: Vec<u16> = original.identities.iter().map(|(id, _)| *id).collect();
        original_ids.sort_unstable();
        assert_eq!(original_ids, vec![1, 2, 3]);

        // Migrationspfad nachstellen: source seeden -> auf target migrieren -> target re-snapshotten.
        let mut source = EcsNativeRuntime::new(64);
        let seed = NanoSnapshot {
            runtime_key: RUNTIME_ECS_NATIVE.to_string(),
            workload_id: "roundtrip-wl".to_string(),
            agent_id: None,
            semantics: NanoSnapshotSemantics::EcsWorld,
            payload: serde_json::to_value(&original).expect("seed ser"),
        };
        let source_handle = source.restore(seed).expect("restore source");
        let mut target = EcsNativeRuntime::new(64);
        let target_handle = source
            .migrate(&mut target, &source_handle)
            .expect("migrate handoff");

        let migrated_snap = target.snapshot(&target_handle).expect("snapshot target");
        let migrated: sentinel_common::EcsSnapshot =
            serde_json::from_value(migrated_snap.payload).expect("migrated deser");
        let mut migrated_ids: Vec<u16> = migrated.identities.iter().map(|(id, _)| *id).collect();
        migrated_ids.sort_unstable();

        assert_eq!(
            migrated_ids, original_ids,
            "Migration muss alle Agent-Identities erhalten"
        );
    }
}
