//! Domain-Event Typen fuer den Event Store.
//!
//! Jede mutierende Aktion erzeugt ein DomainEvent mit UUIDv4 event_id.
//! Events werden append-only in Limbo (SQLite) persistiert.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent_config::HierarchyTier;
use crate::{AgentId, EventType, RoomStimulusType, TaskId};

/// Provenance of the cost attached to one provider pipeline response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    ProviderReported,
    UsagePriceTable,
    PricingUnknown,
    NonProviderZero,
}

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

    /// Sets the event schema version for additive wire/event migrations.
    pub fn with_schema_version(mut self, schema_version: u32) -> Self {
        self.schema_version = schema_version;
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
        /// #491 (TM-3): Herkunft der Aktion. `None`/`Some("external")` = vom LLM/Operator (echter
        /// Replay-Input), `Some("autonomy")` = vom deterministischen Autonomy-System abgeleitet.
        /// Letztere werden beim Replay NICHT erneut injiziert (sonst Doppel-Anwendung), da das
        /// Autonomy-System sie ohnehin wieder erzeugt. Ersetzt die fruehere content-Marker-Heuristik.
        #[serde(default)]
        source: Option<String>,
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
        /// #491 (TM-3): Dauer des Chaos in Ticks. Bisher implizit (TTL im System); explizit im
        /// Event, damit Operator-Chaos nach dem Anchor beim Replay exakt rekonstruiert werden kann.
        #[serde(default)]
        duration_ticks: u64,
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
        /// Valence-Wert aus Mood-System. Alte Events (vor Einfuehrung) haben Default 0.0.
        #[serde(default)]
        valence: f32,
        /// Arousal-Wert aus Mood-System. Alte Events (vor Einfuehrung) haben Default 0.0.
        #[serde(default)]
        arousal: f32,
    },
    /// Periodischer Raum-Physik Snapshot (Temperatur, CO2, Laerm)
    RoomPhysicsUpdated {
        room_id: String,
        temperature: f32,
        co2_ppm: f32,
        noise_db: f32,
        occupant_count: u32,
    },
    /// Manueller Raumreiz fuer direkte Physics-/Perception-Tests.
    RoomStimulusApplied {
        room_id: String,
        stimulus_type: RoomStimulusType,
        delta: f32,
        duration_ticks: u64,
        description: String,
    },
    /// Periodischer Tick-Snapshot-Marker
    TickSnapshot { tick: u64, agent_count: u32 },
    /// Agent wurde in der Runtime gespawnt
    AgentSpawned {
        agent_id: AgentId,
        name: String,
        role: String,
        shift_set: u8,
        /// Raum-ID bei Spawn. Alte Events (vor room_id-Einfuehrung) haben Default "".
        #[serde(default)]
        room_id: String,
    },
    /// Agent wurde aus der Runtime entfernt
    AgentDespawned { agent_id: AgentId, reason: String },
    /// Task/Auftrag erstellt (#438)
    TaskCreated {
        task_id: TaskId,
        title: String,
        assigned_to: AgentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_task: Option<TaskId>,
    },
    /// Task einem (anderen) Agent zugewiesen / delegiert (#438)
    TaskAssigned {
        task_id: TaskId,
        assigned_to: AgentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assigned_by: Option<AgentId>,
    },
    /// Task-Status geaendert (#438)
    TaskStatusChanged {
        task_id: TaskId,
        old_status: String,
        new_status: String,
    },
    /// Task abgeschlossen (#438)
    TaskCompleted {
        task_id: TaskId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
    },
    /// Task blockiert (#438)
    TaskBlocked { task_id: TaskId, reason: String },
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
        /// Episodes selected by the calibrated NMDA threshold.
        #[serde(default)]
        total_episodes_consolidated: u32,
        /// Aggregated NMDA selection rate for processed episodes.
        #[serde(default)]
        nmda_selection_rate: Option<f64>,
        /// Calibrated NMDA threshold used for the run.
        #[serde(default)]
        nmda_threshold: Option<f64>,
        /// Maximum episodes selected per agent during consolidation.
        #[serde(default)]
        nmda_max_consolidation_episodes: Option<u32>,
        /// Minimum NMDA score across all processed episodes, including rejects.
        #[serde(default)]
        nmda_score_min: Option<f64>,
        /// Average NMDA score across all processed episodes, including rejects.
        #[serde(default)]
        nmda_score_avg: Option<f64>,
        /// Maximum NMDA score across all processed episodes, including rejects.
        #[serde(default)]
        nmda_score_max: Option<f64>,
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
    /// Judge-Alert empfangen (drift, quality, fatigue, swap)
    JudgeAlertReceived {
        agent_id: AgentId,
        alert_type: String,
        severity: String,
        score: f64,
        details: String,
    },
    /// Zwei Agents begegnen sich im Flur waehrend Transit
    HallwayEncounterDetected {
        agent_a: AgentId,
        agent_b: AgentId,
        /// Aktuelle Encounter-Location. Alte Events nutzten das Feld `location`.
        #[serde(alias = "location")]
        room_id: String,
    },
    /// Geruchsereignis in einem Raum (Coffee, Food, etc.)
    SmellEventTriggered {
        room_id: String,
        smell_type: String,
        intensity: f32,
        duration_ticks: u64,
    },
    /// Platform-Controlplane Intervention (Self-Healing)
    PlatformIntervention {
        rule_name: String,
        target: String,
        action: String,
        description: String,
    },
    /// LLM-gestuetzte Platform-Analyse mit optionaler Suggested Action.
    PlatformAnalysis {
        trigger: String,
        severity: String,
        summary: String,
        recommendation: String,
        #[serde(default)]
        suggested_action: Option<String>,
        target: String,
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        unresolved_keys: Vec<String>,
        #[serde(default)]
        parameters: BTreeMap<String, serde_json::Value>,
    },
    /// Ressourcen-Profil eines Agents hat sich geaendert (cgroup Hot-Resize)
    ResourceProfileChanged {
        agent_id: AgentId,
        old_profile: String,
        new_profile: String,
    },
    /// Geblockter Execute-Versuch in der Sandbox wurde auditiert.
    SecurityExecBlocked {
        agent_name: String,
        scenario: String,
        attempted_path: String,
        exit_code: i32,
        stderr: String,
    },
    /// Voice of Gaia: Operator hat Gedanke eingepflanzt
    OperatorGaiaSent {
        target_agent_id: u16,
        thought: String,
    },
    /// Broadcast: Operator hat Durchsage gesendet
    OperatorBroadcastSent {
        message: String,
        broadcast_type: String,
    },
    /// DM: Operator/Gaia hat 1:1-Direktnachricht an einen Agent gesendet (#437)
    OperatorDmSent {
        target_agent_id: u16,
        sender_name: String,
        message: String,
    },
    /// Runtime Config-Apply abgeschlossen (#425): Audit-Trail fuer Live-Diff/Fresh-Load.
    ConfigApplied {
        /// "live" oder "fresh".
        mode: String,
        spawned: u32,
        updated: u32,
        despawned: u32,
        rooms_changed: u32,
        /// True wenn die Config erfolgreich in den config_dir persistiert wurde.
        persisted: bool,
    },
    /// Nano-Container Live-Migration abgeschlossen (#413): orchestrator-getriebener,
    /// lokaler Snapshot->Restore-Handoff einer NanoRuntime-Instanz (Time-Machine-Audit).
    MigrationCompleted {
        /// Runtime-Key der migrierten Instanz (z.B. "ecs-native").
        runtime_key: String,
        /// Workload-Identitaet der migrierten Instanz.
        workload_id: String,
        /// Anzahl Agents/Workloads in der migrierten Instanz.
        agent_count: u32,
        /// Workload-ID des Quell-Handles (vor der Migration).
        from_handle: String,
        /// Workload-ID des Ziel-Handles (frische Instanz nach Restore).
        to_handle: String,
        /// Dauer des snapshot->restore->terminate-Handoffs in Millisekunden.
        duration_ms: u64,
        /// Ausloesegrund (z.B. "manual", "resource_rebalance").
        reason: String,
    },
    /// PSI-Band hat sich geaendert (#491, TM-3): die diegetische Hardware-Last (CPU/Mem PSI)
    /// ueberschreitet die Bio-Stress-Schwellen ja/nein. Da `apply_psi_stress` rein
    /// schwellenbasiert ist, genuegen die zwei Booleans als exakter, sparse aufgezeichneter
    /// Replay-Input — nur bei einem tatsaechlichen Band-Wechsel emittiert.
    PsiBandChanged {
        /// CPU-PSI ueber `PSI_CPU_STRESS_THRESHOLD`.
        cpu_above: bool,
        /// Mem-PSI ueber `PSI_MEM_STRESS_THRESHOLD`.
        mem_above: bool,
    },
    /// Eine Bare-VM-Huelle wurde zu einem Cluster-Knoten provisioniert (#495, G3):
    /// die Seed-Node hat das Binary gepusht, Config/systemd/Token-Gates gerendert,
    /// den Daemon gestartet und seine Gesundheit bestaetigt. Time-Machine-Audit der
    /// Cluster-Topologie.
    NodeProvisioned {
        /// Die fuer den neuen Knoten vergebene Node-ID.
        node_id: String,
        /// Menschenlesbarer Alias.
        alias: String,
        /// Allowlist-Target, aus dem provisioniert wurde.
        pending_target_id: String,
        /// Target-IP (aus der Allowlist, nie aus dem Request).
        target_ip: String,
        /// Wall-Clock-Dauer der Bootstrap-Saga in Millisekunden.
        duration_ms: u64,
    },
    /// #427: Token-/Kosten-Telemetrie eines einzelnen LLM-Calls (pro Agent + Tier,
    /// cache-aware). Vom Daemon (`llm_bridge`) aus der echten Provider-Usage emittiert;
    /// die Cost-Information lebt EINMAL im Event-Store (SSOT) und wird per Cost-Projektion
    /// gelesen (1:n, kein zweiter Puffer). Hoechstfrequentes Event (jeder Call inkl.
    /// 0-Token-Synthese). Replay-neutral: projection-only, KEIN ECS-Input (der Daemon
    /// `reconstruct_inputs` ueberspringt es), beruehrt den STRICT/CORE-Hash nicht.
    AgentLlmUsage {
        agent_id: AgentId,
        /// Aufgeloester Model-Tier (low/mid/high) bzw. synthesis/apicp/intercept/unknown.
        tier: String,
        /// Gateway-resolved organization hierarchy class. Missing on v1 events.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hierarchy_tier: Option<HierarchyTier>,
        /// Cost provenance. Missing on v1 events.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_source: Option<CostSource>,
        /// Frische (nicht gecachte) Input-Tokens.
        input_tokens: u32,
        output_tokens: u32,
        /// Aus dem Cache gelesene Input-Tokens (Anthropic cache_read).
        cache_read: u32,
        /// In den Cache geschriebene Input-Tokens (Anthropic cache_creation).
        cache_creation: u32,
        /// Vom Gateway aufgeloeste Kosten dieses Calls in USD.
        cost_usd: f64,
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
            Self::RoomStimulusApplied { .. } => "room_stimulus_applied",
            Self::TickSnapshot { .. } => "tick_snapshot",
            Self::AgentSpawned { .. } => "agent_spawned",
            Self::AgentDespawned { .. } => "agent_despawned",
            Self::TaskCreated { .. } => "task_created",
            Self::TaskAssigned { .. } => "task_assigned",
            Self::TaskStatusChanged { .. } => "task_status_changed",
            Self::TaskCompleted { .. } => "task_completed",
            Self::TaskBlocked { .. } => "task_blocked",
            Self::ShiftTransitionCompleted { .. } => "shift_transition_completed",
            Self::AgentStatusChanged { .. } => "agent_status_changed",
            Self::NightRunStarted { .. } => "nightrun_started",
            Self::NightRunCompleted { .. } => "nightrun_completed",
            Self::AgentConsolidated { .. } => "agent_consolidated",
            Self::AgentConsolidationFailed { .. } => "agent_consolidation_failed",
            Self::JudgeAlertReceived { .. } => "judge_alert_received",
            Self::HallwayEncounterDetected { .. } => "hallway_encounter_detected",
            Self::SmellEventTriggered { .. } => "smell_event_triggered",
            Self::PlatformIntervention { .. } => "platform_intervention",
            Self::PlatformAnalysis { .. } => "platform_analysis",
            Self::ResourceProfileChanged { .. } => "resource_profile_changed",
            Self::SecurityExecBlocked { .. } => "security_exec_blocked",
            Self::OperatorGaiaSent { .. } => "operator_gaia_sent",
            Self::OperatorBroadcastSent { .. } => "operator_broadcast_sent",
            Self::OperatorDmSent { .. } => "operator_dm_sent",
            Self::ConfigApplied { .. } => "config_applied",
            Self::MigrationCompleted { .. } => "migration_completed",
            Self::PsiBandChanged { .. } => "psi_band_changed",
            Self::NodeProvisioned { .. } => "node_provisioned",
            Self::AgentLlmUsage { .. } => "agent_llm_usage",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_llm_usage_event_type_and_roundtrip() {
        // #427: cache-aware per-agent/tier usage event. serde(tag="type") sets
        // type="AgentLlmUsage"; the cache breakdown survives the roundtrip.
        let usage = DomainEventPayload::AgentLlmUsage {
            agent_id: AgentId(8),
            tier: "high".to_string(),
            hierarchy_tier: Some(HierarchyTier::TIER_2),
            cost_source: Some(CostSource::ProviderReported),
            input_tokens: 1000,
            output_tokens: 500,
            cache_read: 200,
            cache_creation: 100,
            cost_usd: 0.0195,
        };
        assert_eq!(usage.event_type_str(), "agent_llm_usage");
        let json = usage.to_json();
        assert!(json.contains("\"type\":\"AgentLlmUsage\""));
        assert!(json.contains("\"cache_read\":200"));
        assert!(json.contains("\"cache_creation\":100"));
        assert!(json.contains("\"hierarchy_tier\":2"));
        assert!(json.contains("\"cost_source\":\"provider_reported\""));
        let back: DomainEventPayload = serde_json::from_str(&json).expect("roundtrip");
        assert_eq!(back.to_json(), json);
    }

    #[test]
    fn agent_llm_usage_v1_remains_deserializable() {
        let json = r#"{"type":"AgentLlmUsage","agent_id":8,"tier":"high","input_tokens":1,"output_tokens":2,"cache_read":0,"cache_creation":0,"cost_usd":0.0}"#;
        let payload: DomainEventPayload = serde_json::from_str(json).expect("v1 usage parses");
        match payload {
            DomainEventPayload::AgentLlmUsage {
                hierarchy_tier,
                cost_source,
                ..
            } => {
                assert_eq!(hierarchy_tier, None);
                assert_eq!(cost_source, None);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn task_event_types_and_roundtrip() {
        // #438 AC1: Task-Events haben korrekte event_type_str + serde-Roundtrip.
        let created = DomainEventPayload::TaskCreated {
            task_id: TaskId(7),
            title: "Landingpage bauen".to_string(),
            assigned_to: AgentId(3),
            parent_task: Some(TaskId(1)),
        };
        assert_eq!(created.event_type_str(), "task_created");
        let json = created.to_json();
        let back: DomainEventPayload = serde_json::from_str(&json).expect("roundtrip");
        assert_eq!(back.to_json(), json);

        assert_eq!(
            DomainEventPayload::TaskAssigned {
                task_id: TaskId(7),
                assigned_to: AgentId(4),
                assigned_by: Some(AgentId(3)),
            }
            .event_type_str(),
            "task_assigned"
        );
        assert_eq!(
            DomainEventPayload::TaskStatusChanged {
                task_id: TaskId(7),
                old_status: "pending".to_string(),
                new_status: "in_progress".to_string(),
            }
            .event_type_str(),
            "task_status_changed"
        );
        assert_eq!(
            DomainEventPayload::TaskCompleted {
                task_id: TaskId(7),
                result: None
            }
            .event_type_str(),
            "task_completed"
        );
        assert_eq!(
            DomainEventPayload::TaskBlocked {
                task_id: TaskId(7),
                reason: "wartet auf Zulieferung".to_string(),
            }
            .event_type_str(),
            "task_blocked"
        );
    }

    #[test]
    fn task_status_default_is_pending() {
        assert_eq!(crate::TaskStatus::default(), crate::TaskStatus::Pending);
        assert_eq!(crate::TaskStatus::InProgress.as_str(), "in_progress");
    }

    #[test]
    fn event_hardening_is_backward_compatible() {
        // #491 (TM-3): altes JSON OHNE die neuen Felder muss weiter deserialisieren (serde default),
        // damit bereits persistierte Events (Event-Store ist append-only) lesbar bleiben.
        let old_aar = r#"{"type":"AgentActionReceived","agent_id":3,"action_type":"Chat","target_room":null,"content":"Hi"}"#;
        match serde_json::from_str::<DomainEventPayload>(old_aar).expect("alt-AAR deser") {
            DomainEventPayload::AgentActionReceived { source, .. } => assert_eq!(source, None),
            _ => panic!("falsche Variante"),
        }
        let old_chaos = r#"{"type":"ChaosTriggered","event_type":"PrinterBroken","target_room":null,"description":"x"}"#;
        match serde_json::from_str::<DomainEventPayload>(old_chaos).expect("alt-Chaos deser") {
            DomainEventPayload::ChaosTriggered { duration_ticks, .. } => {
                assert_eq!(duration_ticks, 0)
            }
            _ => panic!("falsche Variante"),
        }
    }

    #[test]
    fn psi_band_changed_event_type_and_roundtrip() {
        // #491 (TM-3): PsiBandChanged hat den richtigen event_type + serde-Roundtrip (Replay-Input).
        let payload = DomainEventPayload::PsiBandChanged {
            cpu_above: true,
            mem_above: false,
        };
        assert_eq!(payload.event_type_str(), "psi_band_changed");
        let json = payload.to_json();
        let back: DomainEventPayload = serde_json::from_str(&json).expect("roundtrip");
        assert_eq!(back.to_json(), json);
        match back {
            DomainEventPayload::PsiBandChanged {
                cpu_above,
                mem_above,
            } => {
                assert!(cpu_above);
                assert!(!mem_above);
            }
            _ => panic!("falsche Variante"),
        }
    }

    #[test]
    fn hallway_encounter_serializes_room_id() {
        let payload = DomainEventPayload::HallwayEncounterDetected {
            agent_a: AgentId(24),
            agent_b: AgentId(28),
            room_id: "flur-eg".to_string(),
        };

        let json = payload.to_json();
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");

        assert_eq!(value["type"], "HallwayEncounterDetected");
        assert_eq!(value["room_id"], "flur-eg");
        assert!(
            value.get("location").is_none(),
            "new payloads must not keep the legacy location field"
        );
    }

    #[test]
    fn hallway_encounter_deserializes_legacy_location_alias() {
        let json =
            r#"{"type":"HallwayEncounterDetected","agent_a":24,"agent_b":28,"location":"flur-eg"}"#;

        let payload: DomainEventPayload = serde_json::from_str(json).expect("legacy payload");

        match payload {
            DomainEventPayload::HallwayEncounterDetected {
                agent_a,
                agent_b,
                room_id,
            } => {
                assert_eq!(agent_a, AgentId(24));
                assert_eq!(agent_b, AgentId(28));
                assert_eq!(room_id, "flur-eg");
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn platform_analysis_serializes_all_required_fields() {
        let mut parameters = BTreeMap::new();
        parameters.insert("profile".to_string(), serde_json::json!("Idle"));
        parameters.insert("value".to_string(), serde_json::json!(0.75));

        let payload = DomainEventPayload::PlatformAnalysis {
            trigger: "manual".to_string(),
            severity: "warning".to_string(),
            summary: "codex analysis".to_string(),
            recommendation: "force idle profile".to_string(),
            suggested_action: Some("force_profile".to_string()),
            target: "AGENT-03".to_string(),
            provider: Some("claude-code".to_string()),
            model: Some("haiku".to_string()),
            unresolved_keys: vec!["agent_stall:AGENT-03".to_string()],
            parameters,
        };

        let json = payload.to_json();
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");

        assert_eq!(value["type"], "PlatformAnalysis");
        assert_eq!(value["trigger"], "manual");
        assert_eq!(value["severity"], "warning");
        assert_eq!(value["summary"], "codex analysis");
        assert_eq!(value["recommendation"], "force idle profile");
        assert_eq!(value["suggested_action"], "force_profile");
        assert_eq!(value["target"], "AGENT-03");
        assert_eq!(value["provider"], "claude-code");
        assert_eq!(value["model"], "haiku");
        assert_eq!(value["unresolved_keys"][0], "agent_stall:AGENT-03");
        assert_eq!(value["parameters"]["profile"], "Idle");
        assert_eq!(value["parameters"]["value"], 0.75);
        assert_eq!(payload.event_type_str(), "platform_analysis");
    }
}
