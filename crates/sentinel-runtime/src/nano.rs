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

trait SnapshotSemanticsLabel {
    fn semantics_string(&self) -> String;
}

impl SnapshotSemanticsLabel for NanoSnapshot {
    fn semantics_string(&self) -> String {
        format!("{:?}", self.semantics)
    }
}
