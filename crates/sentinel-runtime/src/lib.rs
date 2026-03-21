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

impl AgentStatus {
    /// Prueft ob ein Statusuebergang gueltig ist (State-Machine).
    ///
    /// Gueltige Transitionen:
    /// - Active -> Suspended, Sleeping, Errored
    /// - Suspended -> Active, Errored
    /// - Sleeping -> Active, Errored
    /// - Errored -> Active
    pub fn can_transition_to(self, target: AgentStatus) -> bool {
        if self == target {
            return false;
        }
        matches!(
            (self, target),
            (AgentStatus::Active, AgentStatus::Suspended)
                | (AgentStatus::Active, AgentStatus::Sleeping)
                | (AgentStatus::Active, AgentStatus::Errored)
                | (AgentStatus::Suspended, AgentStatus::Active)
                | (AgentStatus::Suspended, AgentStatus::Errored)
                | (AgentStatus::Sleeping, AgentStatus::Active)
                | (AgentStatus::Sleeping, AgentStatus::Errored)
                | (AgentStatus::Errored, AgentStatus::Active)
        )
    }

    /// Gibt den Status als String zurueck (fuer Event-Payload).
    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::Active => "active",
            AgentStatus::Sleeping => "sleeping",
            AgentStatus::Suspended => "suspended",
            AgentStatus::Errored => "errored",
        }
    }
}

/// Integration-Hook fuer externe Systeme (ECS, Cortex).
///
/// Implementierer werden bei Lifecycle-Events synchron benachrichtigt.
/// Dies ist der primaere Integrationspunkt fuer sentinel-ecs:
/// - ECS World kann Entities spawnen/despawnen wenn Runtime-Lifecycle-Events feuern
/// - Cortex-Gateway empfaengt Events zusaetzlich asynchron via Limbo-Outbox -> Zenoh
pub trait RuntimeEventSink: Send + Sync {
    /// Aufgerufen nach erfolgreichem Agent-Spawn.
    fn on_agent_spawned(&self, agent_id: AgentId, identity: &AgentIdentity, shift: &ShiftInfo);
    /// Aufgerufen nach Agent-Despawn.
    fn on_agent_despawned(&self, agent_id: AgentId);
    /// Aufgerufen bei Statuswechsel (pause, resume, error, recover).
    fn on_agent_status_changed(&self, agent_id: AgentId, old: AgentStatus, new: AgentStatus);
    /// Aufgerufen nach Schichtwechsel mit Liste der entfernten Agents.
    fn on_shift_transition(&self, new_shift_set: u8, removed: &[AgentId]);
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
    event_sink: Option<Arc<dyn RuntimeEventSink>>,
    current_tick: u64,
    event_seq: u64,
}

const RUNTIME_AGGREGATE: &str = "runtime";
const RUNTIME_SNAPSHOT_TYPE: &str = "runtime_state";

impl RuntimeOrchestrator {
    pub fn new(max_agents: usize) -> Self {
        Self {
            agents: HashMap::new(),
            max_agents,
            event_store: None,
            event_sink: None,
            current_tick: 0,
            event_seq: 0,
        }
    }

    /// Attaches an EventStore for lifecycle event emission and snapshot persistence.
    pub fn with_event_store(mut self, store: Arc<EventStore>) -> Self {
        self.event_store = Some(store);
        self
    }

    /// Attaches a RuntimeEventSink for ECS/Cortex integration.
    pub fn with_event_sink(mut self, sink: Arc<dyn RuntimeEventSink>) -> Self {
        self.event_sink = Some(sink);
        self
    }

    /// Updates the current simulation tick (used for event timestamps).
    pub fn set_tick(&mut self, tick: u64) {
        self.current_tick = tick;
    }

    /// Spawnt einen neuen Agenten. Fehler bei Duplikat oder max erreicht.
    pub fn spawn_agent(
        &mut self,
        identity: AgentIdentity,
        shift: ShiftInfo,
        room_id: &str,
    ) -> Result<()> {
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
            name: name.clone(),
            role: role.clone(),
            shift_set,
            room_id: room_id.to_string(),
        };
        self.emit_event(
            payload.event_type_str(),
            &format!("AGENT-{:02}", agent_id.0),
            &payload.to_json(),
            &format!("spawn-{}", agent_id.0),
        );

        // Notify integration sink (ECS/Cortex)
        if let Some(sink) = &self.event_sink {
            let handle = self.agents.get(&agent_id).unwrap();
            sink.on_agent_spawned(agent_id, &handle.identity, &handle.shift);
        }

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

        // Notify integration sink
        if let Some(sink) = &self.event_sink {
            sink.on_agent_despawned(agent_id);
        }

        Ok(())
    }

    /// Pausiert einen Agenten (Active -> Suspended). Fehler bei ungueltigem Uebergang.
    pub fn pause_agent(&mut self, agent_id: AgentId) -> Result<()> {
        let handle = self
            .agents
            .get_mut(&agent_id)
            .ok_or_else(|| anyhow!("Agent {} not found", agent_id))?;

        let old_status = handle.status;
        if !old_status.can_transition_to(AgentStatus::Suspended) {
            return Err(anyhow!(
                "Cannot pause agent {} in state {:?}",
                agent_id,
                old_status
            ));
        }

        handle.status = AgentStatus::Suspended;
        tracing::info!(agent_id = %agent_id, "Agent paused (suspended)");

        // Emit status change event (AC-2)
        let payload = DomainEventPayload::AgentStatusChanged {
            agent_id,
            old_status: old_status.as_str().to_string(),
            new_status: AgentStatus::Suspended.as_str().to_string(),
        };
        self.emit_event(
            payload.event_type_str(),
            &format!("AGENT-{:02}", agent_id.0),
            &payload.to_json(),
            &format!("pause-{}", agent_id.0),
        );

        // Notify integration sink
        if let Some(sink) = &self.event_sink {
            sink.on_agent_status_changed(agent_id, old_status, AgentStatus::Suspended);
        }

        Ok(())
    }

    /// Reaktiviert einen pausierten/schlafenden Agenten (Suspended/Sleeping -> Active).
    pub fn resume_agent(&mut self, agent_id: AgentId) -> Result<()> {
        let handle = self
            .agents
            .get_mut(&agent_id)
            .ok_or_else(|| anyhow!("Agent {} not found", agent_id))?;

        let old_status = handle.status;
        if !old_status.can_transition_to(AgentStatus::Active) {
            return Err(anyhow!(
                "Cannot resume agent {} in state {:?}",
                agent_id,
                old_status
            ));
        }

        handle.status = AgentStatus::Active;
        tracing::info!(agent_id = %agent_id, old_status = ?old_status, "Agent resumed");

        // Emit status change event (AC-2)
        let payload = DomainEventPayload::AgentStatusChanged {
            agent_id,
            old_status: old_status.as_str().to_string(),
            new_status: AgentStatus::Active.as_str().to_string(),
        };
        self.emit_event(
            payload.event_type_str(),
            &format!("AGENT-{:02}", agent_id.0),
            &payload.to_json(),
            &format!("resume-{}", agent_id.0),
        );

        // Notify integration sink
        if let Some(sink) = &self.event_sink {
            sink.on_agent_status_changed(agent_id, old_status, AgentStatus::Active);
        }

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

        // Notify integration sink
        if let Some(sink) = &self.event_sink {
            sink.on_shift_transition(new_shift_set, &to_remove);
        }

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

    /// Read-only Zugriff auf alle Agent-Handles (fuer Resource Manager).
    pub fn agents(&self) -> &std::collections::HashMap<AgentId, AgentHandle> {
        &self.agents
    }

    /// Emits AgentDespawned events for all active agents during graceful shutdown.
    ///
    /// This ensures the Projection Worker can decrement occupant_count for each
    /// agent's current room. Without these events, daemon restarts cause
    /// monotonically growing occupant counts (no matching -1 for the +1 at spawn).
    pub fn despawn_all_for_shutdown(&mut self) -> usize {
        let agent_ids: Vec<AgentId> = self.agents.keys().copied().collect();
        let count = agent_ids.len();
        for agent_id in agent_ids {
            let payload = DomainEventPayload::AgentDespawned {
                agent_id,
                reason: "daemon_shutdown".to_string(),
            };
            self.emit_event(
                payload.event_type_str(),
                &format!("AGENT-{:02}", agent_id.0),
                &payload.to_json(),
                &format!("shutdown-despawn-{}", agent_id.0),
            );
        }
        // Clear the agent map — they're all despawned now
        self.agents.clear();
        count
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

        let last_event_id = store.max_event_rowid().unwrap_or(0);
        store.save_snapshot(
            RUNTIME_AGGREGATE,
            RUNTIME_SNAPSHOT_TYPE,
            &json,
            last_event_id,
        )?;

        tracing::info!(
            agent_count = self.agents.len(),
            last_event_id,
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
            event_sink: None,
            current_tick: 0,
            event_seq: 0,
        })
    }

    /// Emits a lifecycle event to the event store (best-effort, logs on failure).
    fn emit_event(&mut self, event_type: &str, aggregate_id: &str, payload: &str, op_suffix: &str) {
        let store = match &self.event_store {
            Some(s) => s,
            None => return,
        };

        self.event_seq += 1;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let op_id = format!(
            "runtime-{}-{}-{}-{}",
            op_suffix, self.current_tick, self.event_seq, ts
        );
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

        orchestrator
            .spawn_agent(identity, shift, "empfang")
            .unwrap();
        assert_eq!(orchestrator.agent_count(), 1);

        orchestrator.despawn_agent(AgentId(1)).unwrap();
        assert_eq!(orchestrator.agent_count(), 0);
    }

    #[test]
    fn max_agents_limit() {
        let mut orchestrator = RuntimeOrchestrator::new(2);

        let identity1 = create_identity(1, "Thomas", "CEO");
        let shift1 = create_shift(1, 6, 14);
        orchestrator
            .spawn_agent(identity1, shift1, "empfang")
            .unwrap();

        let identity2 = create_identity(2, "Lisa", "Designer");
        let shift2 = create_shift(1, 6, 14);
        orchestrator
            .spawn_agent(identity2, shift2, "empfang")
            .unwrap();

        // Dritter Agent sollte fehlschlagen
        let identity3 = create_identity(3, "Andreas", "Developer");
        let shift3 = create_shift(1, 6, 14);
        let result = orchestrator.spawn_agent(identity3, shift3, "empfang");

        assert!(result.is_err());
        assert_eq!(orchestrator.agent_count(), 2);
    }

    #[test]
    fn shift_transition() {
        let mut orchestrator = RuntimeOrchestrator::new(10);

        // Set 1 Agent (Frueh-Schicht)
        let identity1 = create_identity(1, "Thomas", "CEO");
        let shift1 = create_shift(1, 6, 14);
        orchestrator
            .spawn_agent(identity1, shift1, "empfang")
            .unwrap();

        // Set 2 Agent (Mittel-Schicht)
        let identity2 = create_identity(16, "Michael", "CEO");
        let shift2 = create_shift(2, 14, 22);
        orchestrator
            .spawn_agent(identity2, shift2, "empfang")
            .unwrap();

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
            .spawn_agent(identity_sonder, shift_sonder, "empfang")
            .unwrap();

        // Set 1 Agent
        let identity1 = create_identity(1, "Thomas", "CEO");
        let shift1 = create_shift(1, 6, 14);
        orchestrator
            .spawn_agent(identity1, shift1, "empfang")
            .unwrap();

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
        orchestrator
            .spawn_agent(identity1, shift1, "empfang")
            .unwrap();

        let identity2 = create_identity(2, "Lisa", "Designer");
        let shift2 = create_shift(1, 6, 14);
        orchestrator
            .spawn_agent(identity2, shift2, "empfang")
            .unwrap();

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
        orchestrator
            .spawn_agent(identity1, shift1, "empfang")
            .unwrap();

        let identity2 = create_identity(2, "Lisa", "Designer");
        let shift2 = create_shift(1, 6, 14);
        orchestrator
            .spawn_agent(identity2, shift2, "empfang")
            .unwrap();

        let identity3 = create_identity(3, "Andreas", "Developer");
        let shift3 = create_shift(1, 6, 14);
        orchestrator
            .spawn_agent(identity3, shift3, "empfang")
            .unwrap();

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

        orch.spawn_agent(
            create_identity(1, "Thomas", "CEO"),
            create_shift(1, 6, 14),
            "empfang",
        )
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

        orch.spawn_agent(
            create_identity(1, "Thomas", "CEO"),
            create_shift(1, 6, 14),
            "empfang",
        )
        .unwrap();
        orch.spawn_agent(
            create_identity(16, "Michael", "CEO"),
            create_shift(2, 14, 22),
            "empfang",
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
                "empfang",
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

        orch.spawn_agent(
            create_identity(1, "Thomas", "CEO"),
            create_shift(1, 6, 14),
            "empfang",
        )
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

    // ── State Machine Tests ──────────────────────────

    #[test]
    fn state_machine_valid_transitions() {
        // Active kann zu Suspended, Sleeping, Errored
        assert!(AgentStatus::Active.can_transition_to(AgentStatus::Suspended));
        assert!(AgentStatus::Active.can_transition_to(AgentStatus::Sleeping));
        assert!(AgentStatus::Active.can_transition_to(AgentStatus::Errored));
        // Suspended kann zu Active, Errored
        assert!(AgentStatus::Suspended.can_transition_to(AgentStatus::Active));
        assert!(AgentStatus::Suspended.can_transition_to(AgentStatus::Errored));
        // Sleeping kann zu Active, Errored
        assert!(AgentStatus::Sleeping.can_transition_to(AgentStatus::Active));
        assert!(AgentStatus::Sleeping.can_transition_to(AgentStatus::Errored));
        // Errored kann zu Active (Recover)
        assert!(AgentStatus::Errored.can_transition_to(AgentStatus::Active));
    }

    #[test]
    fn state_machine_invalid_transitions() {
        // Gleicher Status
        assert!(!AgentStatus::Active.can_transition_to(AgentStatus::Active));
        // Suspended kann nicht zu Sleeping
        assert!(!AgentStatus::Suspended.can_transition_to(AgentStatus::Sleeping));
        // Sleeping kann nicht zu Suspended
        assert!(!AgentStatus::Sleeping.can_transition_to(AgentStatus::Suspended));
        // Errored kann nicht zu Suspended/Sleeping
        assert!(!AgentStatus::Errored.can_transition_to(AgentStatus::Suspended));
        assert!(!AgentStatus::Errored.can_transition_to(AgentStatus::Sleeping));
    }

    // ── Pause/Resume Tests ──────────────────────────

    #[test]
    fn pause_resume_lifecycle() {
        let mut orch = RuntimeOrchestrator::new(10);
        orch.spawn_agent(
            create_identity(1, "Thomas", "CEO"),
            create_shift(1, 6, 14),
            "empfang",
        )
        .unwrap();

        // Active -> Suspended (pause)
        orch.pause_agent(AgentId(1)).unwrap();
        assert_eq!(
            orch.get_agent_mut(AgentId(1)).unwrap().status,
            AgentStatus::Suspended
        );

        // Suspended -> Active (resume)
        orch.resume_agent(AgentId(1)).unwrap();
        assert_eq!(
            orch.get_agent_mut(AgentId(1)).unwrap().status,
            AgentStatus::Active
        );
    }

    #[test]
    fn pause_already_suspended_fails() {
        let mut orch = RuntimeOrchestrator::new(10);
        orch.spawn_agent(
            create_identity(1, "Thomas", "CEO"),
            create_shift(1, 6, 14),
            "empfang",
        )
        .unwrap();
        orch.pause_agent(AgentId(1)).unwrap();

        // Suspended -> Suspended: ungueltig
        let result = orch.pause_agent(AgentId(1));
        assert!(result.is_err());
    }

    #[test]
    fn resume_already_active_fails() {
        let mut orch = RuntimeOrchestrator::new(10);
        orch.spawn_agent(
            create_identity(1, "Thomas", "CEO"),
            create_shift(1, 6, 14),
            "empfang",
        )
        .unwrap();

        // Active -> Active via resume: ungueltig
        let result = orch.resume_agent(AgentId(1));
        assert!(result.is_err());
    }

    #[test]
    fn resume_from_sleeping() {
        let mut orch = RuntimeOrchestrator::new(10);
        orch.spawn_agent(
            create_identity(1, "Thomas", "CEO"),
            create_shift(1, 6, 14),
            "empfang",
        )
        .unwrap();

        // Manuell auf Sleeping setzen (Sleep-Cycle)
        orch.get_agent_mut(AgentId(1)).unwrap().status = AgentStatus::Sleeping;

        // Sleeping -> Active (resume)
        orch.resume_agent(AgentId(1)).unwrap();
        assert_eq!(
            orch.get_agent_mut(AgentId(1)).unwrap().status,
            AgentStatus::Active
        );
    }

    #[test]
    fn resume_from_errored() {
        let mut orch = RuntimeOrchestrator::new(10);
        orch.spawn_agent(
            create_identity(1, "Thomas", "CEO"),
            create_shift(1, 6, 14),
            "empfang",
        )
        .unwrap();

        // Manuell auf Errored setzen
        orch.get_agent_mut(AgentId(1)).unwrap().status = AgentStatus::Errored;

        // Errored -> Active (recover via resume)
        orch.resume_agent(AgentId(1)).unwrap();
        assert_eq!(
            orch.get_agent_mut(AgentId(1)).unwrap().status,
            AgentStatus::Active
        );
    }

    #[test]
    fn pause_resume_events_sourced() {
        let (_dir, store) = temp_event_store();
        let mut orch = RuntimeOrchestrator::new(10).with_event_store(store.clone());
        orch.set_tick(1);

        orch.spawn_agent(
            create_identity(1, "Thomas", "CEO"),
            create_shift(1, 6, 14),
            "empfang",
        )
        .unwrap();

        // Pause
        orch.pause_agent(AgentId(1)).unwrap();
        let events = store.get_events_by_aggregate("AGENT-01", 10).unwrap();
        assert_eq!(events.len(), 2); // spawn + status_changed
        assert_eq!(events[1].event_type, "agent_status_changed");

        // Resume
        orch.resume_agent(AgentId(1)).unwrap();
        let events = store.get_events_by_aggregate("AGENT-01", 10).unwrap();
        assert_eq!(events.len(), 3); // spawn + pause + resume
        assert_eq!(events[2].event_type, "agent_status_changed");
    }

    // ── EventSink Integration Tests ──────────────────

    #[test]
    fn event_sink_receives_lifecycle_events() {
        use std::sync::Mutex;

        #[derive(Default)]
        struct TestSink {
            spawned: Mutex<Vec<AgentId>>,
            despawned: Mutex<Vec<AgentId>>,
            status_changes: Mutex<Vec<(AgentId, AgentStatus, AgentStatus)>>,
            transitions: Mutex<Vec<(u8, Vec<AgentId>)>>,
        }

        impl RuntimeEventSink for TestSink {
            fn on_agent_spawned(
                &self,
                agent_id: AgentId,
                _identity: &AgentIdentity,
                _shift: &ShiftInfo,
            ) {
                self.spawned.lock().unwrap().push(agent_id);
            }
            fn on_agent_despawned(&self, agent_id: AgentId) {
                self.despawned.lock().unwrap().push(agent_id);
            }
            fn on_agent_status_changed(
                &self,
                agent_id: AgentId,
                old: AgentStatus,
                new: AgentStatus,
            ) {
                self.status_changes
                    .lock()
                    .unwrap()
                    .push((agent_id, old, new));
            }
            fn on_shift_transition(&self, new_shift_set: u8, removed: &[AgentId]) {
                self.transitions
                    .lock()
                    .unwrap()
                    .push((new_shift_set, removed.to_vec()));
            }
        }

        let sink = Arc::new(TestSink::default());
        let mut orch = RuntimeOrchestrator::new(10).with_event_sink(sink.clone());

        // Spawn
        orch.spawn_agent(
            create_identity(1, "Thomas", "CEO"),
            create_shift(1, 6, 14),
            "empfang",
        )
        .unwrap();
        assert_eq!(sink.spawned.lock().unwrap().len(), 1);

        // Pause
        orch.pause_agent(AgentId(1)).unwrap();
        assert_eq!(sink.status_changes.lock().unwrap().len(), 1);
        let (id, old, new) = sink.status_changes.lock().unwrap()[0];
        assert_eq!(id, AgentId(1));
        assert_eq!(old, AgentStatus::Active);
        assert_eq!(new, AgentStatus::Suspended);

        // Resume
        orch.resume_agent(AgentId(1)).unwrap();
        assert_eq!(sink.status_changes.lock().unwrap().len(), 2);

        // Shift transition
        orch.spawn_agent(
            create_identity(16, "Michael", "CEO"),
            create_shift(2, 14, 22),
            "empfang",
        )
        .unwrap();
        let _ = orch.shift_transition(2);
        assert_eq!(sink.transitions.lock().unwrap().len(), 1);

        // Despawn
        orch.despawn_agent(AgentId(16)).unwrap();
        assert_eq!(sink.despawned.lock().unwrap().len(), 1);
    }

    #[test]
    fn pause_resume_snapshot_roundtrip() {
        let (_dir, store) = temp_event_store();
        let mut orch = RuntimeOrchestrator::new(10).with_event_store(store.clone());
        orch.set_tick(1);

        orch.spawn_agent(
            create_identity(1, "Thomas", "CEO"),
            create_shift(1, 6, 14),
            "empfang",
        )
        .unwrap();
        orch.spawn_agent(
            create_identity(2, "Lisa", "Designer"),
            create_shift(1, 6, 14),
            "empfang",
        )
        .unwrap();

        // Pause Agent 1
        orch.pause_agent(AgentId(1)).unwrap();

        // Save + Restore
        orch.save_state().unwrap();
        let mut restored = RuntimeOrchestrator::restore(store, 10).unwrap();

        // Suspended status muss persistiert sein
        assert_eq!(
            restored.get_agent_mut(AgentId(1)).unwrap().status,
            AgentStatus::Suspended
        );
        assert_eq!(
            restored.get_agent_mut(AgentId(2)).unwrap().status,
            AgentStatus::Active
        );
    }

    /// Footprint-Messung (manuell ausfuehrbar via --ignored).
    #[test]
    #[ignore]
    fn footprint_measurement() {
        println!(
            "sizeof(AgentHandle):            {} bytes",
            std::mem::size_of::<AgentHandle>()
        );
        println!(
            "sizeof(AgentStatus):            {} bytes",
            std::mem::size_of::<AgentStatus>()
        );
        println!(
            "sizeof(AgentIdentity):          {} bytes",
            std::mem::size_of::<sentinel_common::components::AgentIdentity>()
        );
        println!(
            "sizeof(ShiftInfo):              {} bytes",
            std::mem::size_of::<sentinel_common::components::ShiftInfo>()
        );
        println!(
            "sizeof(RuntimeOrchestrator):    {} bytes",
            std::mem::size_of::<RuntimeOrchestrator>()
        );
        println!(
            "sizeof(AgentSnapshot):          {} bytes",
            std::mem::size_of::<AgentSnapshot>()
        );
        println!(
            "sizeof(RuntimeSnapshot):        {} bytes",
            std::mem::size_of::<RuntimeSnapshot>()
        );

        // Measure RSS with 50 agents
        let rss_before = get_rss_kb();
        let (_dir, store) = temp_event_store();
        let mut orch = RuntimeOrchestrator::new(100).with_event_store(store);
        orch.set_tick(1);
        for id in 1..=50u16 {
            orch.spawn_agent(
                create_identity(id, &format!("Agent-{id}"), "Worker"),
                create_shift(1, 6, 14),
                "empfang",
            )
            .unwrap();
        }
        let rss_after = get_rss_kb();

        println!("RSS before 50 agents:           {} KB", rss_before);
        println!("RSS after 50 agents:            {} KB", rss_after);
        println!(
            "RSS delta (50 agents):          {} KB",
            rss_after.saturating_sub(rss_before)
        );
        println!("Threads:                        {}", get_thread_count());
    }

    fn get_rss_kb() -> u64 {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        for line in status.lines() {
            if line.starts_with("VmRSS:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                return parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
        }
        0
    }

    fn get_thread_count() -> u64 {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        for line in status.lines() {
            if line.starts_with("Threads:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                return parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
        }
        0
    }
}
