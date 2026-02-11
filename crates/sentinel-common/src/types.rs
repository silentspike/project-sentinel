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
        if id >= 1 && id <= 54 {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
pub struct Tick(pub u64);

impl fmt::Display for Tick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "t{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
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

// ──────────────────────────────────────────────
// Domain Structs
// ──────────────────────────────────────────────

/// An action performed by an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAction {
    pub agent_id: AgentId,
    pub action_type: ActionType,
    pub target_room: Option<RoomId>,
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
            if value >= 0.0 && value <= 100.0 {
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
        if valence < -1.0 || valence > 1.0 {
            return Err(ValidationError::OutOfRange {
                field: "valence".to_string(),
                value: valence,
                min: -1.0,
                max: 1.0,
            });
        }
        if arousal < 0.0 || arousal > 1.0 {
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
