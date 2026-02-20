//! Controlplane-Kernel Typen: Observations, Incidents, Actions, Policies.

use serde::{Deserialize, Serialize};

/// Snapshot einer Beobachtungsrunde.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub tick: u64,
    pub timestamp_ms: u64,
    pub agents: Vec<AgentObservation>,
}

/// Beobachtungsdaten eines einzelnen Agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentObservation {
    pub agent_id: u16,
    pub hunger: f32,
    pub energy: f32,
    pub stress: f32,
    pub bladder: f32,
    pub social_need: f32,
    pub caffeine: f32,
    pub room_id: String,
    pub in_transit: bool,
    pub valence: f32,
    pub arousal: f32,
}

/// Erkannter Vorfall.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incident {
    pub id: String,
    pub tick: u64,
    pub timestamp_ms: u64,
    pub incident_type: IncidentType,
    pub severity: Severity,
    pub agent_id: Option<u16>,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IncidentType {
    HungerCritical,
    EnergyDepleted,
    StressCritical,
    BladderCritical,
    AgentStuck,
    HighStressCluster,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

/// Aktion die der Controlplane ausfuehrt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlAction {
    pub id: String,
    pub incident_id: String,
    pub action_type: ControlActionType,
    pub agent_id: Option<u16>,
    pub ttl_ticks: u64,
    pub rollback_condition: String,
    pub status: ActionStatus,
    pub created_tick: u64,
    pub verify_after_tick: u64,
    pub verify_outcome: Option<VerifyOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlActionType {
    /// Nur loggen, keine Zustandsaenderung.
    LogOnly { message: String },
    /// DomainEvent in Limbo schreiben.
    EmitEvent {
        event_type: String,
        description: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionStatus {
    Pending,
    Executed,
    Verified,
    RolledBack,
    Expired,
}

/// Ergebnis der Verify-Phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyOutcome {
    pub tick: u64,
    pub success: bool,
    pub reason: String,
}

/// Runtime-Zustand des Controlplane-Kernels.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeState {
    pub last_cycle_tick: u64,
    pub total_cycles: u64,
    pub total_incidents: u64,
    pub total_actions: u64,
}
