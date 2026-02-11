//! Agent Runtime Orchestrator fuer Teammate-basierte Agent-Sessions.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use sentinel_common::components::{AgentIdentity, ShiftInfo};
use sentinel_common::{AgentId, Tick};

/// Status eines laufenden Agenten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Orchestriert Agent-Lifecycle: Spawn, Despawn, Schichtwechsel, Health-Checks.
pub struct RuntimeOrchestrator {
    agents: HashMap<AgentId, AgentHandle>,
    max_agents: usize,
}

impl RuntimeOrchestrator {
    pub fn new(max_agents: usize) -> Self {
        Self {
            agents: HashMap::new(),
            max_agents,
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
