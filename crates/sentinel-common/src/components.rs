//! ECS Components fuer Agent-Simulation.
//!
//! Definiert 11 Components die den Zustand eines Agenten beschreiben.
//! Liegt in sentinel-common, damit sentinel-bio und sentinel-physics
//! diese Typen nutzen koennen OHNE eine zirkulaere Abhaengigkeit
//! zu sentinel-ecs zu erzeugen.

use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{AgentId, Emotion, Tick};

/// Interrupt-Prioritaet fuer Decision Engine (P0 = hoechste Prioritaet)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Priority {
    /// Sofort: Biologischer Notfall (Blase >90, Energie <15, Hunger >95, Stress >90)
    P0,
    /// Naechster Call: Direkte Interaktion (Meeting, angesprochen, Blase >70)
    P1,
    /// Bald: Umgebungsaenderung (Stress >60, Social-Need Extremwerte)
    P2,
    /// Wenn Platz: Hintergrund (Chaos-Event, Langeweile)
    P3,
}

/// Einzelnes Event in der Agent-Queue
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingEvent {
    pub priority: Priority,
    /// Deutscher Impuls-Text
    pub text: String,
    /// Verbleibende Lebensdauer in Ticks (P3: 10, P2: 30, P1: 60, P0: 255 = effektiv unbegrenzt)
    pub ttl_ticks: u16,
    pub created_tick: u64,
}

/// Event-Queue Component pro Agent (max 5 Events pro Injection)
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct EventQueue {
    /// Sortiert nach Priority (P0 zuerst)
    pub events: Vec<PendingEvent>,
}

impl Default for EventQueue {
    fn default() -> Self {
        Self {
            events: Vec::with_capacity(5),
        }
    }
}

/// Identitaet und Metadaten eines Agenten
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub agent_id: AgentId,
    pub name: String,
    pub role: String,
}

/// Position im Buerogebaeude (String-basierte Raum-IDs aus rooms.toml)
///
/// Waehrend Transit wird `room_id` auf den aktuellen Zwischen-Raum gesetzt
/// (z.B. "flur-eg" wenn Agent durch EG-Flur geht). `in_transit` bleibt `true`
/// um "durchgehend" von "stationaer" zu unterscheiden.
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub room_id: String, // z.B. "buero-dev-1", "kueche", waehrend Transit: aktueller Zwischen-Raum
    pub in_transit: bool,
    pub transit_target: Option<String>,
    pub transit_remaining_ms: u32,
    /// Correlation-ID vom Move-Action-Event (fuer Causation-Chain bei TransitCompleted)
    pub transit_correlation_id: Option<String>,
    /// BFS Zwischen-Raeume (ohne Start/Ziel). Leer wenn nicht in Transit.
    #[serde(default)]
    pub transit_route: Vec<String>,
    /// Original-Gesamtdauer in ms (fuer elapsed_ratio Berechnung des Zwischen-Raums).
    #[serde(default)]
    pub transit_total_ms: u32,
    /// Transit pausiert fuer Encounter-Chat. remaining_ms stoppt.
    #[serde(default)]
    pub transit_paused: bool,
    /// Tick bei dem Encounter-Pause begann (Mindest-Pause: 30 Ticks).
    #[serde(default)]
    pub transit_pause_tick: u64,
    /// Urspruenglicher Start-Raum (fuer Perception "Du bist auf dem Weg von X").
    #[serde(default)]
    pub transit_source: Option<String>,
}

/// Biologischer Zustand (Hunger, Energie, Koffein, Blase, Stress, Sozial)
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct BioState {
    pub hunger: f32,      // 0-100
    pub energy: f32,      // 0-100
    pub caffeine_mg: f32, // 0-∞ (Halbwertszeit 5.7h)
    pub bladder: f32,     // 0-100
    pub stress: f32,      // 0-100
    pub social_need: f32, // 0-100
    pub comfort: f32,     // 0-100
}

/// Persoenlichkeit (Big Five + chronotype + Koffein-Toleranz)
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct Personality {
    pub openness: f32,           // 0-1
    pub conscientiousness: f32,  // 0-1
    pub extraversion: f32,       // 0-1
    pub agreeableness: f32,      // 0-1
    pub neuroticism: f32,        // 0-1
    pub caffeine_tolerance: f32, // 0-1, Koffein-Toleranz
    pub is_morning_person: bool,
}

/// Stimmung (Valenz-Arousal-Modell)
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct Mood {
    pub valence: f32, // -1.0 (negativ) bis 1.0 (positiv)
    pub arousal: f32, // 0.0 (ruhig) bis 1.0 (erregt)
    pub dominant_emotion: Emotion,
}

/// Wahrnehmung (wird pro Tick neu generiert fuer LLM-Prompt)
#[derive(Component, Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerceptionState {
    pub environment_text: String,
    pub body_text: String,
    pub social_text: String,
    pub last_updated: Tick,
}

/// Arbeitskontext
#[derive(Component, Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkContext {
    pub current_task: Option<String>,
    pub in_meeting: bool,
    pub has_deadline: bool,
    pub has_conflict: bool,
    /// Verbleibende Ticks mit Conflict-Stress (gesetzt durch Chaos-Events, zerfaellt pro Tick)
    pub conflict_cooldown: u32,
}

/// Beziehungen zu anderen Agenten
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct Relationships {
    pub affinity: Vec<(AgentId, f32)>, // -1.0 bis 1.0 pro Agent
}

/// LLM-Konfiguration
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub provider: String, // z.B. "claude", "bitnet"
    pub model: String,    // z.B. "claude-sonnet-4-5-20250929"
    pub temperature: f32,
    pub max_tokens: u32,
}

/// Schichtinformationen
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct ShiftInfo {
    pub shift_set: u8,        // 0=Sonder, 1=Frueh, 2=Mittel, 3=Spaet
    pub shift_start_hour: u8, // 0-23
    pub shift_end_hour: u8,   // 0-23
    pub is_on_duty: bool,
}

/// Tool-Capabilities des Agents (aus AgentConfig `[capabilities]` Sektion).
/// Leere tools-Liste = kein Tool-Zugriff (sicherer Default).
#[derive(Component, Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentCapabilities {
    pub tools: Vec<String>,
    pub sandbox_allowed_paths: Vec<String>,
}
