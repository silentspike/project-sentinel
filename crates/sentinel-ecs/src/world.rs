//! ECS World Setup und Agent Spawning.
//!
//! Erstellt die ECS World mit allen Systems in korrekter Reihenfolge
//! und bietet die Funktion zum Spawnen von Agenten.

use super::components::*;
use super::systems::*;
use bevy_ecs::prelude::*;
use sentinel_common::{AgentId, Emotion, Tick};

/// Simulationszeit-Resource (muss vor jedem Schedule::run() aktualisiert werden)
#[derive(Resource, Debug, Clone)]
pub struct SimulationTime {
    pub tick: Tick,
    pub tick_count: u64,
    pub delta_seconds: f32,
    pub sim_hour: f32, // 0.0-24.0 simulierte Tageszeit
}

impl Default for SimulationTime {
    fn default() -> Self {
        Self {
            tick: Tick(0),
            tick_count: 0,
            delta_seconds: 1.0, // 1 Sekunde pro Tick default
            sim_hour: 8.0,      // Arbeitsbeginn 08:00
        }
    }
}

/// Erstellt einen neuen ECS World mit allen Systems in korrekter Reihenfolge
pub fn create_simulation_world() -> (World, Schedule) {
    let mut world = World::new();
    let mut schedule = Schedule::default();

    // Resources einfuegen
    world.insert_resource(SimulationTime::default());

    // System-Reihenfolge via configure_sets
    schedule.configure_sets(
        (
            SimulationPhase::Input,
            SimulationPhase::Biology,
            SimulationPhase::Physics,
            SimulationPhase::Transit,
            SimulationPhase::Chaos,
            SimulationPhase::Mood,
            SimulationPhase::Perception,
            SimulationPhase::Output,
            SimulationPhase::Persist,
        )
            .chain(),
    );

    // Systems in ihre Sets einsortieren
    schedule.add_systems(input_system.in_set(SimulationPhase::Input));
    schedule.add_systems(bio_system.in_set(SimulationPhase::Biology));
    schedule.add_systems(physics_system.in_set(SimulationPhase::Physics));
    schedule.add_systems(transit_system.in_set(SimulationPhase::Transit));
    schedule.add_systems(chaos_system.in_set(SimulationPhase::Chaos));
    schedule.add_systems(mood_system.in_set(SimulationPhase::Mood));
    schedule.add_systems(perception_system.in_set(SimulationPhase::Perception));
    schedule.add_systems(output_system.in_set(SimulationPhase::Output));
    schedule.add_systems(persist_system.in_set(SimulationPhase::Persist));

    (world, schedule)
}

/// Spawnt einen Agenten mit allen 10 Components und Default-Werten
pub fn spawn_agent(
    world: &mut World,
    agent_id: AgentId,
    name: &str,
    role: &str,
    shift_set: u8,
) -> Entity {
    let (shift_start, shift_end) = match shift_set {
        1 => (6, 14),  // Fruehschicht
        2 => (14, 22), // Mittelschicht
        3 => (22, 6),  // Spaetschicht
        0 => (0, 0),   // Sonder (24/7)
        _ => (6, 14),  // Fallback: Fruehschicht
    };

    world
        .spawn((
            AgentIdentity {
                agent_id,
                name: name.to_string(),
                role: role.to_string(),
            },
            Position {
                room_id: "empfang".to_string(),
                in_transit: false,
                transit_target: None,
                transit_remaining_ms: 0,
            },
            BioState {
                hunger: 20.0,
                energy: 80.0,
                caffeine_mg: 0.0,
                bladder: 10.0,
                stress: 15.0,
                social_need: 50.0,
                comfort: 70.0,
            },
            Personality {
                openness: 0.5,
                conscientiousness: 0.5,
                extraversion: 0.5,
                agreeableness: 0.5,
                neuroticism: 0.3,
                caffeine_tolerance: 0.5,
                is_morning_person: true,
            },
            Mood {
                valence: 0.2,
                arousal: 0.3,
                dominant_emotion: Emotion::Neutral,
            },
            PerceptionState {
                environment_text: String::new(),
                body_text: String::new(),
                social_text: String::new(),
                last_updated: Tick(0),
            },
            WorkContext {
                current_task: None,
                in_meeting: false,
                has_deadline: false,
                has_conflict: false,
            },
            Relationships {
                affinity: Vec::new(),
            },
            LlmConfig {
                provider: "claude".to_string(),
                model: "claude-sonnet-4-5-20250929".to_string(),
                temperature: 0.7,
                max_tokens: 4096,
            },
            ShiftInfo {
                shift_set,
                shift_start_hour: shift_start,
                shift_end_hour: shift_end,
                is_on_duty: false,
            },
        ))
        .id()
}
