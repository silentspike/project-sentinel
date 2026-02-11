//! ECS Components fuer Agent-Simulation.
//!
//! Definiert 10 Components die den Zustand eines Agenten beschreiben:
//! - AgentIdentity: Identitaet und Metadaten
//! - Position: Raum-Position im Buerogebaeude
//! - BioState: Biologische Parameter (Hunger, Energie, Koffein, etc.)
//! - Personality: Big-Five-Persoenlichkeit
//! - Mood: Valenz-Arousal-Stimmung
//! - Perception: LLM-Prompt-Text
//! - WorkContext: Arbeitskontext
//! - Relationships: Beziehungen zu anderen Agenten
//! - LlmConfig: LLM-Provider-Konfiguration
//! - ShiftInfo: Schichtinformationen

use bevy_ecs::prelude::*;
use sentinel_common::{AgentId, Emotion, Tick};

/// Identitaet und Metadaten eines Agenten
#[derive(Component, Debug, Clone)]
pub struct AgentIdentity {
    pub agent_id: AgentId,
    pub name: String,
    pub role: String,
}

/// Position im Buerogebaeude (String-basierte Raum-IDs aus rooms.toml)
#[derive(Component, Debug, Clone)]
pub struct Position {
    pub room_id: String, // z.B. "buero-dev-1", "kueche"
    pub in_transit: bool,
    pub transit_target: Option<String>,
    pub transit_remaining_ms: u32,
}

/// Biologischer Zustand (Hunger, Energie, Koffein, Blase, Stress, Sozial)
#[derive(Component, Debug, Clone)]
pub struct BioState {
    pub hunger: f32,      // 0-100
    pub energy: f32,      // 0-100
    pub caffeine_mg: f32, // 0-∞ (Halbwertszeit 5.7h)
    pub bladder: f32,     // 0-100
    pub stress: f32,      // 0-100
    pub social_need: f32, // 0-100
    pub comfort: f32,     // 0-100
}

/// Persoenlichkeit (Big Five + chronotype)
#[derive(Component, Debug, Clone)]
pub struct Personality {
    pub openness: f32,          // 0-1
    pub conscientiousness: f32, // 0-1
    pub extraversion: f32,      // 0-1
    pub agreeableness: f32,     // 0-1
    pub neuroticism: f32,       // 0-1
    pub is_morning_person: bool,
}

/// Stimmung (Valenz-Arousal-Modell)
#[derive(Component, Debug, Clone)]
pub struct Mood {
    pub valence: f32, // -1.0 (negativ) bis 1.0 (positiv)
    pub arousal: f32, // 0.0 (ruhig) bis 1.0 (erregt)
    pub dominant_emotion: Emotion,
}

/// Wahrnehmung (wird pro Tick neu generiert fuer LLM-Prompt)
#[derive(Component, Debug, Clone)]
pub struct Perception {
    pub environment_text: String,
    pub body_text: String,
    pub social_text: String,
    pub last_updated: Tick,
}

/// Arbeitskontext
#[derive(Component, Debug, Clone)]
pub struct WorkContext {
    pub current_task: Option<String>,
    pub in_meeting: bool,
    pub has_deadline: bool,
    pub has_conflict: bool,
}

/// Beziehungen zu anderen Agenten
#[derive(Component, Debug, Clone)]
pub struct Relationships {
    pub affinity: Vec<(AgentId, f32)>, // -1.0 bis 1.0 pro Agent
}

/// LLM-Konfiguration
#[derive(Component, Debug, Clone)]
pub struct LlmConfig {
    pub provider: String, // z.B. "claude", "bitnet"
    pub model: String,    // z.B. "claude-sonnet-4-5-20250929"
    pub temperature: f32,
    pub max_tokens: u32,
}

/// Schichtinformationen
#[derive(Component, Debug, Clone)]
pub struct ShiftInfo {
    pub shift_set: u8,        // 0=Sonder, 1=Frueh, 2=Mittel, 3=Spaet
    pub shift_start_hour: u8, // 0-23
    pub shift_end_hour: u8,   // 0-23
    pub is_on_duty: bool,
}
