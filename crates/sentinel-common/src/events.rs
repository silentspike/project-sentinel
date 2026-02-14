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
    /// Periodischer Tick-Snapshot-Marker
    TickSnapshot { tick: u64, agent_count: u32 },
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
            Self::TickSnapshot { .. } => "tick_snapshot",
        }
    }
}
