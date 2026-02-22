//! Domain-Event Typen fuer den Event Store.
//!
//! Jede mutierende Aktion erzeugt ein DomainEvent mit UUIDv4 event_id.
//! Events werden append-only in Limbo (SQLite) persistiert.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AgentId, EventType};

/// Domain-Event mit Saga-ready Kettenfeldern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainEvent {
    /// Eindeutige Event-ID (UUIDv4)
    pub event_id: String,
    /// Event-Typ, z.B. "agent_action_received", "transit_started"
    pub event_type: String,
    /// Aggregate-ID, z.B. "AGENT-01" oder "buero-dev-1"
    pub aggregate_id: String,
    /// JSON-serialisierter Payload
    pub payload: String,
    /// Gruppiert zusammengehoerige Events (gleiche Correlation = gleicher Vorgang)
    pub correlation_id: String,
    /// Was hat dieses Event ausgeloest (causation chain)
    pub causation_id: Option<String>,
    /// Idempotenz-Key (gleiche operation_id = gleicher Vorgang, kein Duplikat)
    pub operation_id: String,
    /// Simulations-Tick
    pub tick: u64,
    /// Wall-Clock Millisekunden
    pub timestamp_ms: u64,
    /// Schema-Version fuer Forward-Compatibility
    pub schema_version: u32,
    /// Kompensations-Typ fuer Saga-Pattern (default: "none")
    pub compensation_type: String,
}

impl DomainEvent {
    /// Erstellt ein neues DomainEvent mit generierten IDs.
    pub fn new(
        event_type: &str,
        aggregate_id: &str,
        payload: &str,
        correlation_id: &str,
        tick: u64,
    ) -> Self {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            event_id: Uuid::new_v4().to_string(),
            event_type: event_type.to_string(),
            aggregate_id: aggregate_id.to_string(),
            payload: payload.to_string(),
            correlation_id: correlation_id.to_string(),
            causation_id: None,
            operation_id: Uuid::new_v4().to_string(),
            tick,
            timestamp_ms: now_ms,
            schema_version: 1,
            compensation_type: "none".to_string(),
        }
    }

    /// Setzt die causation_id (Event-Kette).
    pub fn with_causation(mut self, causation_id: &str) -> Self {
        self.causation_id = Some(causation_id.to_string());
        self
    }

    /// Setzt eine explizite correlation_id (Vorgangs-Gruppierung).
    pub fn with_correlation(mut self, correlation_id: &str) -> Self {
        self.correlation_id = correlation_id.to_string();
        self
    }

    /// Setzt eine explizite operation_id (Idempotenz).
    pub fn with_operation_id(mut self, operation_id: &str) -> Self {
        self.operation_id = operation_id.to_string();
        self
    }

    /// Setzt den Kompensations-Typ (Saga-Pattern).
    pub fn with_compensation_type(mut self, compensation_type: &str) -> Self {
        self.compensation_type = compensation_type.to_string();
        self
    }
}

/// Event-Payloads die in Limbo persistiert werden.
///
/// Nicht JEDE Bio-Tick-Berechnung wird ein Event. Nur explizite Aktionen:
/// Agent-Aktionen (vom LLM), Chaos-Events, Transit-Events, Bio-Aktionen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DomainEventPayload {
    /// Agent hat eine Aktion vom LLM erhalten
    AgentActionReceived {
        agent_id: AgentId,
        action_type: String,
        target_room: Option<String>,
        content: Option<String>,
    },
    /// Agent startet Raumwechsel
    TransitStarted {
        agent_id: AgentId,
        from_room: String,
        to_room: String,
        duration_ms: u32,
    },
    /// Agent kommt im Zielraum an
    TransitCompleted { agent_id: AgentId, room_id: String },
    /// Chaos-Event ausgeloest
    ChaosTriggered {
        event_type: EventType,
        target_room: Option<String>,
        description: String,
    },
    /// Bio-Aktion (essen, trinken, Toilette)
    BioActionPerformed { agent_id: AgentId, action: String },
    /// Periodischer Bio-State Snapshot pro Agent (alle N Ticks)
    BioStateUpdated {
        agent_id: AgentId,
        hunger: f32,
        energy: f32,
        stress: f32,
        bladder: f32,
        social_need: f32,
        caffeine_mg: f32,
        room_id: String,
        mood: String,
    },
    /// Periodischer Raum-Physik Snapshot (Temperatur, CO2, Laerm)
    RoomPhysicsUpdated {
        room_id: String,
        temperature: f32,
        co2_ppm: f32,
        noise_db: f32,
        occupant_count: u32,
    },
    /// Periodischer Tick-Snapshot-Marker
    TickSnapshot { tick: u64, agent_count: u32 },
    /// Agent wurde in der Runtime gespawnt
    AgentSpawned {
        agent_id: AgentId,
        name: String,
        role: String,
        shift_set: u8,
        room_id: String,
    },
    /// Agent wurde aus der Runtime entfernt
    AgentDespawned { agent_id: AgentId, reason: String },
    /// Schichtwechsel abgeschlossen
    ShiftTransitionCompleted {
        new_shift_set: u8,
        removed_count: u32,
        removed_agents: Vec<AgentId>,
    },
    /// Agent-Status hat sich geaendert
    AgentStatusChanged {
        agent_id: AgentId,
        old_status: String,
        new_status: String,
    },
    /// Nightrun-Konsolidierung gestartet
    NightRunStarted {
        run_id: String,
        trigger_shift_set: u8,
        agents_queued: u32,
    },
    /// Nightrun-Konsolidierung abgeschlossen
    NightRunCompleted {
        run_id: String,
        trigger_shift_set: u8,
        agents_consolidated: u32,
        agents_failed: u32,
        agents_skipped: u32,
        total_episodes: u32,
        duration_ms: u64,
        /// Final hash of the deterministic event chain (for replay verification).
        #[serde(default)]
        hash_chain: Option<String>,
    },
    /// Einzelner Agent konsolidiert
    AgentConsolidated {
        run_id: String,
        agent_name: String,
        episodes_processed: u32,
        episodes_consolidated: u32,
        duration_ms: u64,
    },
    /// Agent-Konsolidierung fehlgeschlagen
    AgentConsolidationFailed {
        run_id: String,
        agent_name: String,
        error: String,
    },
}

impl DomainEventPayload {
    /// Serialisiert den Payload zu JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Gibt den Event-Typ-String zurueck.
    pub fn event_type_str(&self) -> &'static str {
        match self {
            Self::AgentActionReceived { .. } => "agent_action_received",
            Self::TransitStarted { .. } => "transit_started",
            Self::TransitCompleted { .. } => "transit_completed",
            Self::ChaosTriggered { .. } => "chaos_triggered",
            Self::BioActionPerformed { .. } => "bio_action_performed",
            Self::BioStateUpdated { .. } => "bio_state_updated",
            Self::RoomPhysicsUpdated { .. } => "room_physics_updated",
            Self::TickSnapshot { .. } => "tick_snapshot",
            Self::AgentSpawned { .. } => "agent_spawned",
            Self::AgentDespawned { .. } => "agent_despawned",
            Self::ShiftTransitionCompleted { .. } => "shift_transition_completed",
            Self::AgentStatusChanged { .. } => "agent_status_changed",
            Self::NightRunStarted { .. } => "nightrun_started",
            Self::NightRunCompleted { .. } => "nightrun_completed",
            Self::AgentConsolidated { .. } => "agent_consolidated",
            Self::AgentConsolidationFailed { .. } => "agent_consolidation_failed",
        }
    }
}
