//! Agent Runtime Orchestrator fuer Teammate-basierte Agent-Sessions.
//!
//! Emittiert Lifecycle-Events in sentinel-limbo (AC-2) und
//! unterstuetzt Snapshot-basiertes Resume nach Neustart (AC-4).

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use sentinel_common::components::{AgentIdentity, ShiftInfo};
use sentinel_common::events::{DomainEvent, DomainEventPayload};
use sentinel_common::{AgentId, Tick};
use sentinel_limbo::EventStore;
use serde::{Deserialize, Serialize};

/// Status eines laufenden Agenten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    Active,
    Sleeping,
    Suspended,
    Errored,
}

/// Handle fuer einen einzelnen Agenten in der Runtime.
pub struct AgentHandle {
    pub identity: AgentIdentity,
    pub shift: ShiftInfo,
    pub status: AgentStatus,
    pub last_activity_tick: Tick,
}

/// Serialisierbarer Snapshot eines AgentHandle (fuer Persistence).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentSnapshot {
    identity: AgentIdentity,
    shift: ShiftInfo,
    status: AgentStatus,
    last_activity_tick: u64,
}

impl From<&AgentHandle> for AgentSnapshot {
    fn from(h: &AgentHandle) -> Self {
        Self {
            identity: h.identity.clone(),
            shift: h.shift.clone(),
            status: h.status,
            last_activity_tick: h.last_activity_tick.0,
        }
    }
}

impl From<AgentSnapshot> for AgentHandle {
    fn from(s: AgentSnapshot) -> Self {
        Self {
            identity: s.identity,
            shift: s.shift,
            status: s.status,
            last_activity_tick: Tick(s.last_activity_tick),
        }
    }
}

/// Serialisierbarer Snapshot des gesamten RuntimeOrchestrator State.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeSnapshot {
    agents: Vec<AgentSnapshot>,
    max_agents: usize,
}

/// Orchestriert Agent-Lifecycle: Spawn, Despawn, Schichtwechsel, Health-Checks.
/// Optional mit Event-Store fuer event-sourced Lifecycle-Events und Snapshot-Persistence.
pub struct RuntimeOrchestrator {
    agents: HashMap<AgentId, AgentHandle>,
    max_agents: usize,
    event_store: Option<Arc<EventStore>>,
    current_tick: u64,
}

const RUNTIME_AGGREGATE: &str = "runtime";
const RUNTIME_SNAPSHOT_TYPE: &str = "runtime_state";

impl RuntimeOrchestrator {
    pub fn new(max_agents: usize) -> Self {
        Self {
            agents: HashMap::new(),
            max_agents,
            event_store: None,
            current_tick: 0,
        }
    }

    /// Attaches an EventStore for lifecycle event emission and snapshot persistence.
    pub fn with_event_store(mut self, store: Arc<EventStore>) -> Self {
        self.event_store = Some(store);
        self
    }

    /// Updates the current simulation tick (used for event timestamps).
    pub fn set_tick(&mut self, tick: u64) {
        self.current_tick = tick;
    }

    /// Spawnt einen neuen Agenten. Fehler bei Duplikat oder max erreicht.
    pub fn spawn_agent(&mut self, identity: AgentIdentity, shift: ShiftInfo) -> Result<()> {
        if self.agents.contains_key(&identity.agent_id) {
            return Err(anyhow!("Agent {} already exists", identity.agent_id));
        }

        if self.agents.len() >= self.max_agents {
            return Err(anyhow!(
                "Max agent limit reached ({}/{})",
                self.agents.len(),
                self.max_agents
            ));
        }

        let agent_id = identity.agent_id;
        let shift_set = shift.shift_set;
        let name = identity.name.clone();
        let role = identity.role.clone();

        let handle = AgentHandle {
            identity,
            shift,
            status: AgentStatus::Active,
            last_activity_tick: Tick(0),
        };

        self.agents.insert(agent_id, handle);
        tracing::info!(
            agent_id = %agent_id,
            "Agent spawned"
        );

        // Emit lifecycle event (AC-2)
        let payload = DomainEventPayload::AgentSpawned {
            agent_id,
            name,
            role,
            shift_set,
        };
        self.emit_event(
            payload.event_type_str(),
            &format!("AGENT-{:02}", agent_id.0),
            &payload.to_json(),
            &format!("spawn-{}", agent_id.0),
        );

        Ok(())
    }

    /// Entfernt einen Agenten. Fehler wenn nicht gefunden.
    pub fn despawn_agent(&mut self, agent_id: AgentId) -> Result<()> {
        self.agents
            .remove(&agent_id)
            .ok_or_else(|| anyhow!("Agent {} not found", agent_id))?;

        tracing::info!(
            agent_id = %agent_id,
            "Agent despawned"
        );

        // Emit lifecycle event (AC-2)
        let payload = DomainEventPayload::AgentDespawned {
            agent_id,
            reason: "explicit_despawn".to_string(),
        };
        self.emit_event(
            payload.event_type_str(),
            &format!("AGENT-{:02}", agent_id.0),
            &payload.to_json(),
            &format!("despawn-{}", agent_id.0),
        );

        Ok(())
    }

    /// Schichtwechsel: Entfernt alle Agenten deren shift_set != new_shift_set UND != 0 (Sonder).
    /// Gibt die entfernten AgentIds zurueck.
    pub fn shift_transition(&mut self, new_shift_set: u8) -> Vec<AgentId> {
        let to_remove: Vec<AgentId> = self
            .agents
            .iter()
            .filter(|(_, handle)| {
                // Behalte: Sonder-Schicht (0) ODER neue Schicht
                handle.shift.shift_set != 0 && handle.shift.shift_set != new_shift_set
            })
            .map(|(id, _)| *id)
            .collect();

        for agent_id in &to_remove {
            self.agents.remove(agent_id);
            tracing::info!(
                agent_id = %agent_id,
                "Agent removed during shift transition"
            );
        }

        tracing::info!(
            new_shift_set,
            removed_count = to_remove.len(),
            "Shift transition completed"
        );

        // Emit lifecycle event (AC-2)
        let payload = DomainEventPayload::ShiftTransitionCompleted {
            new_shift_set,
            removed_count: to_remove.len() as u32,
            removed_agents: to_remove.clone(),
        };
        self.emit_event(
            payload.event_type_str(),
            RUNTIME_AGGREGATE,
            &payload.to_json(),
            &format!("shift-{}-tick-{}", new_shift_set, self.current_tick),
        );

        to_remove
    }

    /// Gibt alle Agenten zurueck die Errored oder Suspended sind.
    pub fn check_health(&self) -> Vec<(AgentId, AgentStatus)> {
        self.agents
            .iter()
            .filter_map(|(id, handle)| {
                if handle.status == AgentStatus::Errored || handle.status == AgentStatus::Suspended
                {
                    Some((*id, handle.status))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Mutable Zugriff auf einen AgentHandle.
    pub fn get_agent_mut(&mut self, agent_id: AgentId) -> Option<&mut AgentHandle> {
        self.agents.get_mut(&agent_id)
    }

    /// Anzahl aktiver Agenten.
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Saves the full runtime state as a snapshot to the event store (AC-4).
    pub fn save_state(&self) -> Result<()> {
        let store = self
            .event_store
            .as_ref()
            .ok_or_else(|| anyhow!("No event store attached"))?;

        let snapshot = RuntimeSnapshot {
            agents: self.agents.values().map(AgentSnapshot::from).collect(),
            max_agents: self.max_agents,
        };

        let json = serde_json::to_string(&snapshot)?;

        store.save_snapshot(RUNTIME_AGGREGATE, RUNTIME_SNAPSHOT_TYPE, &json, 0)?;

        tracing::info!(
            agent_count = self.agents.len(),
            "Runtime state snapshot saved"
        );

        Ok(())
    }

    /// Restores a RuntimeOrchestrator from the latest snapshot in the event store (AC-4).
    pub fn restore(store: Arc<EventStore>, max_agents: usize) -> Result<Self> {
        let snapshot_row = store
            .get_latest_snapshot(RUNTIME_AGGREGATE)?
            .ok_or_else(|| anyhow!("No runtime snapshot found"))?;

        let snapshot: RuntimeSnapshot = serde_json::from_str(&snapshot_row.payload)?;

        let mut agents = HashMap::new();
        for agent_snap in snapshot.agents {
            let agent_id = agent_snap.identity.agent_id;
            agents.insert(agent_id, AgentHandle::from(agent_snap));
        }

        tracing::info!(
            agent_count = agents.len(),
            "Runtime state restored from snapshot"
        );

        Ok(Self {
            agents,
            max_agents: if max_agents > 0 {
                max_agents
            } else {
                snapshot.max_agents
            },
            event_store: Some(store),
            current_tick: 0,
        })
    }

    /// Emits a lifecycle event to the event store (best-effort, logs on failure).
    fn emit_event(&self, event_type: &str, aggregate_id: &str, payload: &str, op_suffix: &str) {
        let store = match &self.event_store {
            Some(s) => s,
            None => return,
        };

        let op_id = format!("runtime-{}-{}", op_suffix, self.current_tick);
        let event = DomainEvent::new(event_type, aggregate_id, payload, &op_id, self.current_tick)
            .with_operation_id(&op_id);

        let topic = format!("sentinel/runtime/events/{}", aggregate_id);
        if let Err(e) = store.append_with_outbox(&event, &topic) {
            tracing::warn!(
                error = %e,
                event_type,
                "Failed to emit runtime lifecycle event"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_limbo::EventStore;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn create_identity(id: u16, name: &str, role: &str) -> AgentIdentity {
        AgentIdentity {
            agent_id: AgentId(id),
            name: name.to_string(),
            role: role.to_string(),
        }
    }

    fn create_shift(shift_set: u8, start: u8, end: u8) -> ShiftInfo {
        ShiftInfo {
            shift_set,
            shift_start_hour: start,
            shift_end_hour: end,
            is_on_duty: true,
        }
    }

    fn temp_event_store() -> (TempDir, Arc<EventStore>) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test_runtime.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();
        (dir, Arc::new(store))
    }

    #[test]
    fn spawn_despawn() {
        let mut orchestrator = RuntimeOrchestrator::new(10);

        let identity = create_identity(1, "Thomas", "CEO");
        let shift = create_shift(1, 6, 14);

        orchestrator.spawn_agent(identity, shift).unwrap();
        assert_eq!(orchestrator.agent_count(), 1);

        orchestrator.despawn_agent(AgentId(1)).unwrap();
        assert_eq!(orchestrator.agent_count(), 0);
    }

    #[test]
    fn max_agents_limit() {
        let mut orchestrator = RuntimeOrchestrator::new(2);

        let identity1 = create_identity(1, "Thomas", "CEO");
        let shift1 = create_shift(1, 6, 14);
        orchestrator.spawn_agent(identity1, shift1).unwrap();

        let identity2 = create_identity(2, "Lisa", "Designer");
        let shift2 = create_shift(1, 6, 14);
        orchestrator.spawn_agent(identity2, shift2).unwrap();

        // Dritter Agent sollte fehlschlagen
        let identity3 = create_identity(3, "Andreas", "Developer");
        let shift3 = create_shift(1, 6, 14);
        let result = orchestrator.spawn_agent(identity3, shift3);

        assert!(result.is_err());
        assert_eq!(orchestrator.agent_count(), 2);
    }

    #[test]
    fn shift_transition() {
        let mut orchestrator = RuntimeOrchestrator::new(10);

        // Set 1 Agent (Frueh-Schicht)
        let identity1 = create_identity(1, "Thomas", "CEO");
        let shift1 = create_shift(1, 6, 14);
        orchestrator.spawn_agent(identity1, shift1).unwrap();

        // Set 2 Agent (Mittel-Schicht)
        let identity2 = create_identity(16, "Michael", "CEO");
        let shift2 = create_shift(2, 14, 22);
        orchestrator.spawn_agent(identity2, shift2).unwrap();

        assert_eq!(orchestrator.agent_count(), 2);

        // Transition zu Set 2
        let removed = orchestrator.shift_transition(2);

        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0], AgentId(1));
        assert_eq!(orchestrator.agent_count(), 1);

        // Nur Set 2 Agent sollte uebrig sein
        assert!(orchestrator.get_agent_mut(AgentId(16)).is_some());
        assert!(orchestrator.get_agent_mut(AgentId(1)).is_none());
    }

    #[test]
    fn sonder_shift_preserved() {
        let mut orchestrator = RuntimeOrchestrator::new(10);

        // Sonder-Agent (Set 0)
        let identity_sonder = create_identity(46, "Betriebsrat", "Sonder");
        let shift_sonder = create_shift(0, 0, 23);
        orchestrator
            .spawn_agent(identity_sonder, shift_sonder)
            .unwrap();

        // Set 1 Agent
        let identity1 = create_identity(1, "Thomas", "CEO");
        let shift1 = create_shift(1, 6, 14);
        orchestrator.spawn_agent(identity1, shift1).unwrap();

        assert_eq!(orchestrator.agent_count(), 2);

        // Transition zu Set 2
        let removed = orchestrator.shift_transition(2);

        // Nur Set 1 Agent sollte entfernt werden, Sonder bleibt
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0], AgentId(1));
        assert_eq!(orchestrator.agent_count(), 1);

        // Sonder-Agent sollte noch da sein
        assert!(orchestrator.get_agent_mut(AgentId(46)).is_some());
    }

    #[test]
    fn check_health() {
        let mut orchestrator = RuntimeOrchestrator::new(10);

        let identity1 = create_identity(1, "Thomas", "CEO");
        let shift1 = create_shift(1, 6, 14);
        orchestrator.spawn_agent(identity1, shift1).unwrap();

        let identity2 = create_identity(2, "Lisa", "Designer");
        let shift2 = create_shift(1, 6, 14);
        orchestrator.spawn_agent(identity2, shift2).unwrap();

        // Setze einen auf Errored
        if let Some(handle) = orchestrator.get_agent_mut(AgentId(1)) {
            handle.status = AgentStatus::Errored;
        }

        let unhealthy = orchestrator.check_health();
        assert_eq!(unhealthy.len(), 1);
        assert_eq!(unhealthy[0].0, AgentId(1));
        assert_eq!(unhealthy[0].1, AgentStatus::Errored);
    }

    #[test]
    fn active_agents_list() {
        let mut orchestrator = RuntimeOrchestrator::new(10);

        let identity1 = create_identity(1, "Thomas", "CEO");
        let shift1 = create_shift(1, 6, 14);
        orchestrator.spawn_agent(identity1, shift1).unwrap();

        let identity2 = create_identity(2, "Lisa", "Designer");
        let shift2 = create_shift(1, 6, 14);
        orchestrator.spawn_agent(identity2, shift2).unwrap();

        let identity3 = create_identity(3, "Andreas", "Developer");
        let shift3 = create_shift(1, 6, 14);
        orchestrator.spawn_agent(identity3, shift3).unwrap();

        assert_eq!(orchestrator.agent_count(), 3);

        orchestrator.despawn_agent(AgentId(2)).unwrap();
        assert_eq!(orchestrator.agent_count(), 2);

        // Agent 1 und 3 sollten noch da sein
        assert!(orchestrator.get_agent_mut(AgentId(1)).is_some());
        assert!(orchestrator.get_agent_mut(AgentId(3)).is_some());
        assert!(orchestrator.get_agent_mut(AgentId(2)).is_none());
    }

    #[test]
    fn event_emission_on_spawn_despawn() {
        let (_dir, store) = temp_event_store();
        let mut orch = RuntimeOrchestrator::new(10).with_event_store(store.clone());
        orch.set_tick(42);

        orch.spawn_agent(create_identity(1, "Thomas", "CEO"), create_shift(1, 6, 14))
            .unwrap();

        // Verify spawn event in store
        let events = store.get_events_by_aggregate("AGENT-01", 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "agent_spawned");
        assert_eq!(events[0].tick, 42);

        // Despawn
        orch.despawn_agent(AgentId(1)).unwrap();

        let events = store.get_events_by_aggregate("AGENT-01", 10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].event_type, "agent_despawned");
    }

    #[test]
    fn event_emission_on_shift_transition() {
        let (_dir, store) = temp_event_store();
        let mut orch = RuntimeOrchestrator::new(10).with_event_store(store.clone());
        orch.set_tick(100);

        orch.spawn_agent(create_identity(1, "Thomas", "CEO"), create_shift(1, 6, 14))
            .unwrap();
        orch.spawn_agent(
            create_identity(16, "Michael", "CEO"),
            create_shift(2, 14, 22),
        )
        .unwrap();

        let _removed = orch.shift_transition(2);

        // Shift transition event on "runtime" aggregate
        let events = store.get_events_by_aggregate("runtime", 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "shift_transition_completed");
        assert_eq!(events[0].tick, 100);
    }

    #[test]
    fn snapshot_save_restore() {
        let (_dir, store) = temp_event_store();
        let mut orch = RuntimeOrchestrator::new(15).with_event_store(store.clone());
        orch.set_tick(50);

        // Spawn 5 agents
        for i in 1..=5 {
            orch.spawn_agent(
                create_identity(i, &format!("Agent-{}", i), "Worker"),
                create_shift(1, 6, 14),
            )
            .unwrap();
        }

        // Set one to Errored
        if let Some(h) = orch.get_agent_mut(AgentId(3)) {
            h.status = AgentStatus::Errored;
        }

        // Save snapshot
        orch.save_state().unwrap();

        // Restore into new orchestrator
        let restored = RuntimeOrchestrator::restore(store, 15).unwrap();

        assert_eq!(restored.agent_count(), 5);

        // Verify specific agent state survived
        // Note: get_agent_mut requires &mut, so we create a mutable binding
        let mut restored = restored;
        let agent3 = restored.get_agent_mut(AgentId(3)).unwrap();
        assert_eq!(agent3.status, AgentStatus::Errored);
        assert_eq!(agent3.identity.name, "Agent-3");
        assert_eq!(agent3.shift.shift_set, 1);
    }

    #[test]
    fn no_event_without_store() {
        // Without event store, operations should still work (no panic)
        let mut orch = RuntimeOrchestrator::new(10);

        orch.spawn_agent(create_identity(1, "Thomas", "CEO"), create_shift(1, 6, 14))
            .unwrap();
        orch.despawn_agent(AgentId(1)).unwrap();
        let _ = orch.shift_transition(2);
        // No panic = success
    }

    #[test]
    fn save_state_requires_store() {
        let orch = RuntimeOrchestrator::new(10);
        let result = orch.save_state();
        assert!(result.is_err());
    }
}
