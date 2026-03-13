//! Snapshot-Test fuer Perception Injection Format (insta).
//!
//! Sichert das exakte `[SYSTEM_INJECTION]` Format ab, das in LLM-Prompts
//! injiziert wird. Jede strukturelle Aenderung wird durch Snapshot-Diff sichtbar.

use sentinel_common::components::{BioState, Personality, Position};
use sentinel_ecs::perception::{format_injection, generate_perception, SmellEvent};

fn test_bio() -> BioState {
    BioState {
        hunger: 75.0,
        energy: 45.0,
        caffeine_mg: 10.0,
        bladder: 65.0,
        stress: 55.0,
        social_need: 70.0,
        comfort: 60.0,
    }
}

fn test_personality() -> Personality {
    Personality {
        openness: 0.7,
        conscientiousness: 0.6,
        extraversion: 0.8,
        agreeableness: 0.5,
        neuroticism: 0.4,
        caffeine_tolerance: 0.5,
        is_morning_person: true,
    }
}

fn test_position() -> Position {
    Position {
        room_id: "buero-dev-1".to_string(),
        in_transit: false,
        transit_target: None,
        transit_remaining_ms: 0,
        transit_correlation_id: None,
    }
}

#[test]
fn snapshot_injection_stressed_hungry_agent() {
    let smells = vec![SmellEvent {
        source_room: "kueche".to_string(),
        smell_type: "coffee".to_string(),
        intensity: 0.8,
        radius_rooms: 2,
        decay_per_room: 0.3,
        created_tick: 100,
        duration_ticks: 500,
    }];
    let agents = vec![
        ("Lisa".to_string(), "Design-Review".to_string()),
        ("Andreas".to_string(), "Coding".to_string()),
    ];

    let perception = generate_perception(
        &test_bio(),
        &test_position(),
        &test_personality(),
        55.0,
        23.5,
        1100.0,
        &smells,
        &agents,
        "14:30",
        6.5,
    );
    let injection = format_injection(&perception);
    insta::assert_snapshot!("injection_stressed_hungry", injection);
}

#[test]
fn snapshot_injection_healthy_alone() {
    let bio = BioState {
        hunger: 20.0,
        energy: 80.0,
        caffeine_mg: 50.0,
        bladder: 10.0,
        stress: 15.0,
        social_need: 50.0,
        comfort: 70.0,
    };
    let personality = Personality {
        openness: 0.5,
        conscientiousness: 0.5,
        extraversion: 0.3,
        agreeableness: 0.5,
        neuroticism: 0.3,
        caffeine_tolerance: 0.2,
        is_morning_person: true,
    };

    let perception = generate_perception(
        &bio,
        &test_position(),
        &personality,
        32.0,
        21.5,
        500.0,
        &[],
        &[],
        "09:30",
        1.5,
    );
    let injection = format_injection(&perception);
    insta::assert_snapshot!("injection_healthy_alone", injection);
}
