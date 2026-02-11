use serde::{Deserialize, Serialize};

/// Action types an agent can perform
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ActionType {
    Chat,
    Move,
    ToolUse,
    Emote,
    PhoneCall,
}

/// An action performed by an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAction {
    pub agent_name: String,
    pub action_type: ActionType,
    pub target_room: Option<String>,
    pub target_agent: Option<String>,
    pub content: Option<String>,
    pub timestamp_ms: u64,
    pub tick: u64,
}

/// Perception data injected into agent prompts
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Perception {
    pub agent_name: String,
    pub circadian_text: String,
    pub body_text: String,
    pub environment_text: String,
    pub acoustic_text: String,
    pub presence_text: String,
    pub impulse_text: String,
    pub timestamp_ms: u64,
    pub tick: u64,
}

/// Biological state of an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BioStateUpdate {
    pub agent_name: String,
    pub hunger: f32,
    pub energy: f32,
    pub caffeine_mg: f32,
    pub bladder: f32,
    pub stress: f32,
    pub social_need: f32,
    pub comfort: f32,
    pub timestamp_ms: u64,
    pub tick: u64,
}

/// Position of an agent in the building
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionUpdate {
    pub agent_name: String,
    pub room_id: String,
    pub in_transit: bool,
    pub transit_target: Option<String>,
    pub timestamp_ms: u64,
    pub tick: u64,
}

/// Mood/emotional state of an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoodUpdate {
    pub agent_name: String,
    pub valence: f32,       // -1.0 (negative) to 1.0 (positive)
    pub arousal: f32,       // 0.0 (calm) to 1.0 (excited)
    pub dominant_emotion: String,
    pub timestamp_ms: u64,
    pub tick: u64,
}

/// Chaos event types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

/// A chaos event that disrupts the simulation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChaosEvent {
    pub event_type: EventType,
    pub target_room: Option<String>,
    pub target_agent: Option<String>,
    pub description: String,
    pub duration_minutes: Option<u32>,
    pub timestamp_ms: u64,
    pub tick: u64,
}
