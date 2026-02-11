//! ECS Systems fuer Agent-Simulation.
//!
//! Definiert 9 Systems in strikter Ausfuehrungsreihenfolge:
//! 1. input_system - Empfaengt Agent-Aktionen (via Zenoh)
//! 2. bio_system - Aktualisiert biologische Zustaende
//! 3. physics_system - Berechnet Raum-Physik
//! 4. transit_system - Verarbeitet Raumwechsel
//! 5. chaos_system - Generiert Zufallsereignisse
//! 6. mood_system - Berechnet Stimmung
//! 7. perception_system - Generiert Wahrnehmungstext
//! 8. output_system - Sendet Wahrnehmung via Zenoh
//! 9. persist_system - Persistiert Zustand

use super::components::*;
use bevy_ecs::prelude::*;

/// Ausfuehrungsreihenfolge der Simulation-Systems
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum SimulationPhase {
    Input,
    Biology,
    Physics,
    Transit,
    Chaos,
    Mood,
    Perception,
    Output,
    Persist,
}

/// 1. Empfaengt Agent-Aktionen (via Zenoh)
pub fn input_system() {
    // Stub - wird in Phase 3 (LLM Bridge) implementiert
}

/// 2. Aktualisiert biologische Zustaende (Hunger, Energie, Koffein...)
pub fn bio_system(mut _query: Query<(&mut BioState, &Personality, &WorkContext)>) {
    // Stub - Logik in sentinel-bio
}

/// 3. Berechnet Raum-Physik (Temperatur, Laerm, CO2)
pub fn physics_system(_query: Query<&Position>) {
    // Stub - Logik in sentinel-physics
}

/// 4. Verarbeitet Raumwechsel
pub fn transit_system(mut _query: Query<&mut Position>) {
    // Stub - Logik in sentinel-physics
}

/// 5. Generiert Zufallsereignisse
pub fn chaos_system() {
    // Stub - Logik in sentinel-physics
}

/// 6. Berechnet Stimmung aus Bio-Zustand und Kontext
pub fn mood_system(mut _query: Query<(&BioState, &mut Mood, &WorkContext)>) {
    // Stub
}

/// 7. Generiert Wahrnehmungstext fuer LLM-Prompt
pub fn perception_system(mut _query: Query<(&BioState, &Position, &Mood, &mut Perception)>) {
    // Stub
}

/// 8. Sendet Wahrnehmung via Zenoh an LLM
pub fn output_system(_query: Query<(&AgentIdentity, &Perception)>) {
    // Stub - wird in Phase 3 (LLM Bridge) implementiert
}

/// 9. Persistiert Zustand in redb/Limbo (BATCHED)
pub fn persist_system() {
    // Stub - wird mit sentinel-redb/sentinel-limbo verbunden
}
