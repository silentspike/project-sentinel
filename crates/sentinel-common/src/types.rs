use serde::{Deserialize, Serialize};
use std::fmt;

// ──────────────────────────────────────────────
// Validation Error
// ──────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("AgentId {0} out of range (1-54)")]
    InvalidAgentId(u16),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub u16);

impl AgentId {
    pub fn new(id: u16) -> Result<Self, ValidationError> {
        if (1..=54).contains(&id) {
            Ok(Self(id))
        } else {
            Err(ValidationError::InvalidAgentId(id))
        }
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AGENT-{:02}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RoomId(pub u16);

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
    pub presence_text: String,
    pub impulse_text: String,
    pub timestamp: Timestamp,
    pub tick: Tick,
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
