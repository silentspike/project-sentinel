use serde::{Deserialize, Serialize};
use std::fmt;

// ──────────────────────────────────────────────
// Validation Error
// ──────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("AgentId {id} out of range (1-{max})")]
    InvalidAgentId { id: u16, max: u16 },
    #[error("RoomId {0} out of range")]
    InvalidRoomId(u16),
    #[error("Value {value} out of range [{min}, {max}] for {field}")]
    OutOfRange {
        field: String,
        value: f32,
        min: f32,
        max: f32,
    },
}

// ──────────────────────────────────────────────
// Newtypes
// ──────────────────────────────────────────────

/// Default upper AgentId bound for the shipped 60-agent PixelPerfekt config.
pub const DEFAULT_MAX_AGENT_ID: u16 = 60;

/// Validation bounds for AgentId-bearing config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentIdBounds {
    pub max: u16,
}

impl AgentIdBounds {
    pub const fn new(max: u16) -> Self {
        Self { max }
    }

    pub fn validate(self, id: u16) -> Result<AgentId, ValidationError> {
        if (1..=self.max).contains(&id) {
            Ok(AgentId(id))
        } else {
            Err(ValidationError::InvalidAgentId { id, max: self.max })
        }
    }
}

impl Default for AgentIdBounds {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_AGENT_ID)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub u16);

impl AgentId {
    pub fn new(id: u16) -> Result<Self, ValidationError> {
        Self::new_with_bounds(id, AgentIdBounds::default())
    }

    pub fn new_with_bounds(id: u16, bounds: AgentIdBounds) -> Result<Self, ValidationError> {
        bounds.validate(id)
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AGENT-{:02}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RoomId(pub u16);

/// Identitaet eines Tasks/Auftrags (#438). Eigener Schluesselraum (u32, NICHT AgentId).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub u32);

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TASK-{}", self.0)
    }
}

/// Lebenszyklus-Status eines Tasks (#438).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Pending,
    InProgress,
    Done,
    Blocked,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }
}

impl RoomId {
    pub fn new(id: u16) -> Result<Self, ValidationError> {
        if id > 0 {
            Ok(Self(id))
        } else {
            Err(ValidationError::InvalidRoomId(id))
        }
    }
}

impl fmt::Display for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ROOM-{}", self.0)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct Tick(pub u64);

impl fmt::Display for Tick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "t{}", self.0)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct Timestamp(pub u64);

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ms", self.0)
    }
}

// ──────────────────────────────────────────────
// Enums
// ──────────────────────────────────────────────

/// Action types an agent can perform
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ActionType {
    Chat,
    Move,
    ToolUse,
    Emote,
    PhoneCall,
}

/// Emotional state of an agent
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Emotion {
    Neutral,
    Happy,
    Frustrated,
    Stressed,
    Relaxed,
    Excited,
    Bored,
    Anxious,
    Focused,
    Tired,
}

/// Chaos event types
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EventType {
    PhoneRing,
    PrinterBroken,
    PackageDelivery,
    SBahnDelay,
    FireAlarmDrill,
    CakeInKitchen,
    AirConBroken,
    InternetOutage,
}

/// User-steuerbarer Raumreiz fuer direkte Physics-/Perception-Tests.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RoomStimulusType {
    Temperature,
    Noise,
    Co2,
}

impl RoomStimulusType {
    /// Standard-Delta fuer UI-seitige Schnelltests.
    pub fn default_delta(self) -> f32 {
        match self {
            Self::Temperature => 4.0,
            Self::Noise => 24.0,
            Self::Co2 => 900.0,
        }
    }

    /// Menschlich lesbare Standardbeschreibung fuer Event-Trail und UI.
    pub fn default_description(self, delta: f32) -> String {
        match self {
            Self::Temperature => format!("Temperaturreiz {delta:+.1} °C"),
            Self::Noise => format!("Laermreiz {delta:+.0} dB"),
            Self::Co2 => format!("CO2-Reiz {delta:+.0} ppm"),
        }
    }
}

impl EventType {
    /// Default-Beschreibung fuer Operator- und Zufalls-Chaos.
    pub fn default_description(self) -> &'static str {
        match self {
            Self::PhoneRing => "Telefon klingelt",
            Self::PrinterBroken => "Drucker defekt",
            Self::PackageDelivery => "Paketlieferung",
            Self::SBahnDelay => "S-Bahn Verspaetung",
            Self::FireAlarmDrill => "Feueralarm-Uebung",
            Self::CakeInKitchen => "Kuchen in der Kueche",
            Self::AirConBroken => "Klimaanlage defekt",
            Self::InternetOutage => "Internetausfall",
        }
    }
}

/// Schreibender Operator-Command fuer manuelles Chaos in der Runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorChaosCommand {
    pub event_id: String,
    pub correlation_id: String,
    pub operation_id: String,
    pub room_id: String,
    pub chaos_type: EventType,
    pub description: String,
    pub duration_ticks: Option<u64>,
}

/// Schreibender Operator-Command fuer direkte Raumreize in der Runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorRoomStimulusCommand {
    pub event_id: String,
    pub correlation_id: String,
    pub operation_id: String,
    pub room_id: String,
    pub stimulus_type: RoomStimulusType,
    pub delta: f32,
    pub description: String,
    pub duration_ticks: Option<u64>,
}

/// Operator-Trigger fuer Nightrun-Konsolidierung via Daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorNightrunCommand {
    /// Optionale Schicht-Nummer (1-3). None = letzte abgelaufene Schicht.
    pub shift_set: Option<u8>,
    /// Nur simulieren, nicht persistieren.
    #[serde(default)]
    pub dry_run: bool,
}

/// Gemeinsamer Runtime-Schreibpfad fuer Operator-Kommandos.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OperatorCommand {
    Chaos(OperatorChaosCommand),
    RoomStimulus(OperatorRoomStimulusCommand),
    Nightrun(OperatorNightrunCommand),
    Snapshot(OperatorSnapshotCommand),
    Restore(OperatorRestoreCommand),
    Chat(OperatorChatCommand),
    Gaia(OperatorGaiaCommand),
    Broadcast(OperatorBroadcastCommand),
    Task(OperatorTaskCommand),
}

/// Aktion eines Task-Kommandos (#438).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorTaskAction {
    Create,
    Assign,
    UpdateStatus,
    Complete,
}

/// Task-/Auftrags-Kommando (#438): Gaia/Operator erstellt, delegiert, aktualisiert oder schliesst
/// Tasks. Zustellung an den Agent erfolgt via Voice-of-Gaia; Felder je nach `action` relevant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorTaskCommand {
    pub action: OperatorTaskAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_by: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

/// Besucher-Chat: Operator redet mit Agents im selben Raum (asynchron via RoomChatBuffer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorChatCommand {
    pub room_id: String,
    pub message: String,
    pub sender_name: String,
}

/// Voice of Gaia: Raum-unabhaengige Gedanken-Injection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorGaiaCommand {
    pub target_agent_id: u16,
    pub thought: String,
}

/// Broadcast: System-weite Durchsage an alle Agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorBroadcastCommand {
    pub message: String,
    #[serde(rename = "type", default)]
    pub broadcast_type: String,
}

// ──────────────────────────────────────────────
// Domain Structs
// ──────────────────────────────────────────────

/// An action performed by an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAction {
    pub agent_id: AgentId,
    pub action_type: ActionType,
    pub target_room: Option<String>,
    pub target_agent: Option<AgentId>,
    pub content: Option<String>,
    pub timestamp: Timestamp,
    pub tick: Tick,
}

/// Perception data injected into agent prompts
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Perception {
    pub agent_id: AgentId,
    pub circadian_text: String,
    pub body_text: String,
    pub environment_text: String,
    pub acoustic_text: String,
    #[serde(default)]
    pub heard_text: String,
    pub presence_text: String,
    pub impulse_text: String,
    #[serde(default)]
    pub is_directly_addressed: bool,
    pub timestamp: Timestamp,
    pub tick: Tick,
    /// Room ID where this agent is located (for Chat-Sequencing)
    #[serde(default)]
    pub room_id: String,
    /// Highest priority from EventQueue ("P0"/"P1"/"P2"/"P3"/"NONE")
    #[serde(default)]
    pub max_priority: String,
    /// Synthesis fingerprint: Bio-Buckets + Room + Stimuli-Flags + Hour + Temp + Personality
    #[serde(default)]
    pub synth_fingerprint: String,
    /// Personality type based on Big Five Extraversion ("I" or "E")
    #[serde(default)]
    pub personality_type: String,
    /// True when GaiaBuffer or BroadcastBuffer has active content for this agent.
    /// Used by Synthesis to bypass templates and forward to real LLM.
    #[serde(default)]
    pub has_operator_impulse: bool,
}

/// Biological state of an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BioStateUpdate {
    pub agent_id: AgentId,
    pub hunger: f32,
    pub energy: f32,
    pub caffeine_mg: f32,
    pub bladder: f32,
    pub stress: f32,
    pub social_need: f32,
    pub comfort: f32,
    pub timestamp: Timestamp,
    pub tick: Tick,
}

impl BioStateUpdate {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent_id: AgentId,
        hunger: f32,
        energy: f32,
        caffeine_mg: f32,
        bladder: f32,
        stress: f32,
        social_need: f32,
        comfort: f32,
        timestamp: Timestamp,
        tick: Tick,
    ) -> Result<Self, ValidationError> {
        fn validate(field: &str, value: f32) -> Result<(), ValidationError> {
            if (0.0..=100.0).contains(&value) {
                Ok(())
            } else {
                Err(ValidationError::OutOfRange {
                    field: field.to_string(),
                    value,
                    min: 0.0,
                    max: 100.0,
                })
            }
        }
        validate("hunger", hunger)?;
        validate("energy", energy)?;
        validate("caffeine_mg", caffeine_mg)?;
        validate("bladder", bladder)?;
        validate("stress", stress)?;
        validate("social_need", social_need)?;
        validate("comfort", comfort)?;
        Ok(Self {
            agent_id,
            hunger,
            energy,
            caffeine_mg,
            bladder,
            stress,
            social_need,
            comfort,
            timestamp,
            tick,
        })
    }
}

/// Position of an agent in the building
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionUpdate {
    pub agent_id: AgentId,
    pub room_id: RoomId,
    pub in_transit: bool,
    pub transit_target: Option<RoomId>,
    pub timestamp: Timestamp,
    pub tick: Tick,
}

/// Mood/emotional state of an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoodUpdate {
    pub agent_id: AgentId,
    pub valence: f32,
    pub arousal: f32,
    pub dominant_emotion: Emotion,
    pub timestamp: Timestamp,
    pub tick: Tick,
}

impl MoodUpdate {
    pub fn new(
        agent_id: AgentId,
        valence: f32,
        arousal: f32,
        dominant_emotion: Emotion,
        timestamp: Timestamp,
        tick: Tick,
    ) -> Result<Self, ValidationError> {
        if !(-1.0..=1.0).contains(&valence) {
            return Err(ValidationError::OutOfRange {
                field: "valence".to_string(),
                value: valence,
                min: -1.0,
                max: 1.0,
            });
        }
        if !(0.0..=1.0).contains(&arousal) {
            return Err(ValidationError::OutOfRange {
                field: "arousal".to_string(),
                value: arousal,
                min: 0.0,
                max: 1.0,
            });
        }
        Ok(Self {
            agent_id,
            valence,
            arousal,
            dominant_emotion,
            timestamp,
            tick,
        })
    }
}

/// A chaos event that disrupts the simulation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChaosEvent {
    pub event_type: EventType,
    pub target_room: Option<RoomId>,
    pub target_agent: Option<AgentId>,
    pub description: String,
    pub duration_minutes: Option<u32>,
    pub timestamp: Timestamp,
    pub tick: Tick,
}

// ──────────────────────────────────────────────
// Time Machine: World Snapshots
// ──────────────────────────────────────────────

/// Granularitaets-Tier fuer World Snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotTier {
    Hourly,
    Daily,
    Weekly,
    Monthly,
}

impl fmt::Display for SnapshotTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hourly => write!(f, "hourly"),
            Self::Daily => write!(f, "daily"),
            Self::Weekly => write!(f, "weekly"),
            Self::Monthly => write!(f, "monthly"),
        }
    }
}

/// Metadaten eines World Snapshots (ohne Payload — fuer Listings).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub id: String,
    pub tier: SnapshotTier,
    pub tick: u64,
    pub sim_hour: f32,
    pub last_event_id: i64,
    pub payload_size_bytes: u64,
    pub created_at_ms: i64,
}

/// Dump aller 11 redb-Tables (Key-Value Paare als Bytes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedbDump {
    pub agent_states: Vec<(u16, Vec<u8>)>,
    pub room_states: Vec<(u16, Vec<u8>)>,
    pub personalities: Vec<(u16, Vec<u8>)>,
    pub relationships: Vec<(u32, Vec<u8>)>,
    pub voice_styles: Vec<(u16, Vec<u8>)>,
    pub behavioral_notes: Vec<(u16, Vec<u8>)>,
    pub narrative_summaries: Vec<(u16, Vec<u8>)>,
    pub evolution_versions: Vec<(u16, u64)>,
    pub nmda_scores: Vec<(u16, Vec<u8>)>,
    pub agent_facts: Vec<(u16, Vec<u8>)>,
    pub sim_meta: Vec<(String, Vec<u8>)>,
    pub api_patterns: Vec<(String, Vec<u8>)>,
}

/// Dump des sentinel-fs Runtime-Metadatenpfads.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FsMetadataDump {
    pub inodes: Vec<(String, u64, Vec<u8>)>,
    pub dirents: Vec<(String, u64, String, u64)>,
    pub refcounts: Vec<([u8; 32], u32)>,
    pub trash_queue: Vec<([u8; 32], u64)>,
}

/// ECS World-State Snapshot (alle Components + Resources).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcsSnapshot {
    pub positions: Vec<(u16, crate::components::Position)>,
    pub bio_states: Vec<(u16, crate::components::BioState)>,
    pub personalities: Vec<(u16, crate::components::Personality)>,
    pub moods: Vec<(u16, crate::components::Mood)>,
    pub perception_states: Vec<(u16, crate::components::PerceptionState)>,
    pub work_contexts: Vec<(u16, crate::components::WorkContext)>,
    pub agent_capabilities: Vec<(u16, crate::components::AgentCapabilities)>,
    pub event_queues: Vec<(u16, crate::components::EventQueue)>,
    pub identities: Vec<(u16, crate::components::AgentIdentity)>,
    pub shift_infos: Vec<(u16, crate::components::ShiftInfo)>,
    pub relationships: Vec<(u16, crate::components::Relationships)>,
    pub llm_configs: Vec<(u16, crate::components::LlmConfig)>,
    /// Task-/Auftrags-Entities (#438) — eigener Schluessel (task_id in TaskState), kein Agent-u16.
    #[serde(default)]
    pub task_states: Vec<crate::components::TaskState>,
    pub sim_tick: u64,
    pub sim_hour: f32,
    pub sim_delta_seconds: f32,
    /// Serialisierte ephemere Resources (ActiveChaos, ActiveRoomStimuli)
    /// als JSON-Bytes — vermeidet zirkulaere Dependency zu sentinel-ecs.
    pub active_chaos_json: Vec<u8>,
    pub active_stimuli_json: Vec<u8>,
}

/// Vollstaendiger World Snapshot (redb + ECS + Cursor).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldSnapshot {
    pub snapshot_id: String,
    pub schema_version: u32,
    pub tick: u64,
    pub sim_hour: f32,
    pub timestamp_ms: u64,
    pub tier: SnapshotTier,
    pub last_event_id: i64,
    pub redb: RedbDump,
    pub ecs: EcsSnapshot,
    pub projection_offsets: Vec<(String, i64)>,
    #[serde(default)]
    pub fs_metadata: Option<FsMetadataDump>,
}

impl WorldSnapshot {
    /// Aktuelle Schema-Version fuer bincode Kompatibilitaet.
    pub const SCHEMA_VERSION: u32 = 2;
}

/// Operator-Trigger fuer Point-in-Time Restore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorRestoreCommand {
    pub snapshot_id: String,
}

/// Operator-Trigger fuer manuellen Snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorSnapshotCommand {
    pub tier: Option<SnapshotTier>,
}

/// Apply-Modus fuer Runtime-Config-Apply (#425).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApplyMode {
    /// Inkrementelles Diff-Apply gegen die laufende Welt (Editieren bestehender Firma).
    Live,
    /// Volle Welt-Initialisierung aus neuer Config (brandneue Firma).
    Fresh,
}

/// Operator-Trigger fuer Runtime-Config-Apply (#425).
/// Self-contained Inline-JSON: traegt die ganze ECS-Firma (agents + building).
/// `company-context` ist NICHT enthalten (laeuft via Gateway-Hot-Reload #440).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorConfigApplyCommand {
    pub mode: ApplyMode,
    pub agents: Vec<crate::agent_config::AgentConfig>,
    pub building: crate::room::BuildingConfig,
}
