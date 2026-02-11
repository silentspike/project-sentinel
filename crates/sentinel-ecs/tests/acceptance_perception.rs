//! Acceptance Tests fuer Issue #14: Perception Injection
//!
//! Testet die public API von sentinel-ecs::perception:
//! generate_perception(), format_injection(), Schwellenwerte, Akustik-Mapping,
//! Anwesenheit, Impuls-Prioritaet und Format-Tags.

use sentinel_ecs::perception::{format_injection, generate_perception, SmellEvent};

use sentinel_common::components::{BioState, Personality, Position};

fn default_bio() -> BioState {
    BioState {
        hunger: 20.0,
        energy: 80.0,
        caffeine_mg: 50.0, // Explizit gesetzt um Koffein-Entzug zu vermeiden
        bladder: 10.0,
        stress: 15.0,
        social_need: 50.0,
        comfort: 70.0,
    }
}

fn default_personality() -> Personality {
    Personality {
        openness: 0.5,
        conscientiousness: 0.5,
        extraversion: 0.5,
        agreeableness: 0.5,
        neuroticism: 0.3,
        caffeine_tolerance: 0.5,
        is_morning_person: true,
    }
}

fn default_position() -> Position {
    Position {
        room_id: "buero-dev-1".to_string(),
        in_transit: false,
        transit_target: None,
        transit_remaining_ms: 0,
    }
}

// AC #14.03: generate_perception mit allen Parametern aufrufen, kein Panic
#[test]
fn ac_14_03_generate_perception_signature() {
    let bio = default_bio();
    let pos = default_position();
    let personality = default_personality();
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
        ("Lisa".to_string(), "Design".to_string()),
        ("Andreas".to_string(), "Coding".to_string()),
    ];

    let result = generate_perception(
        &bio,
        &pos,
        &personality,
        45.0,   // room_noise_db
        22.0,   // room_temp_c
        800.0,  // room_co2_ppm
        &smells,
        &agents,
        "10:30", // sim_time
        3.5,     // focus_hours
    );

    // Alle Felder muessen nicht-leer sein (mit Agenten und Kaffeeduft)
    assert!(
        !result.circadian_text.is_empty(),
        "circadian_text should not be empty"
    );
    assert!(
        !result.body_text.is_empty(),
        "body_text should not be empty"
    );
    assert!(
        !result.environment_text.is_empty(),
        "environment_text should not be empty"
    );
    assert!(
        !result.acoustic_text.is_empty(),
        "acoustic_text should not be empty"
    );
    assert!(
        !result.presence_text.is_empty(),
        "presence_text should not be empty"
    );
}

// AC #14.04: Schwellenwerte - hunger=85 erzeugt "Magen krampft", bladder=95 erzeugt dringenden Text
#[test]
fn ac_14_04_thresholds() {
    let mut bio = default_bio();
    bio.hunger = 85.0;
    let pos = default_position();
    let personality = default_personality();

    let result = generate_perception(
        &bio,
        &pos,
        &personality,
        40.0,
        22.0,
        600.0,
        &[],
        &[],
        "12:00",
        2.0,
    );

    assert!(
        result.body_text.contains("Magen krampft"),
        "hunger=85 should produce 'Magen krampft', got: '{}'",
        result.body_text
    );

    // Bladder=95 -> "JETZT"
    let mut bio2 = default_bio();
    bio2.bladder = 95.0;

    let result2 = generate_perception(
        &bio2,
        &pos,
        &personality,
        40.0,
        22.0,
        600.0,
        &[],
        &[],
        "12:00",
        2.0,
    );

    assert!(
        result2.body_text.contains("JETZT"),
        "bladder=95 should produce 'JETZT', got: '{}'",
        result2.body_text
    );
}

// AC #14.05: Akustik-Mapping: 30dB->Stille, 50dB->Normal/Buero, 70dB->Laut
#[test]
fn ac_14_05_acoustics_mapping() {
    let bio = default_bio();
    let pos = default_position();
    let personality = default_personality();

    // 30dB -> Stille
    let result_30 = generate_perception(
        &bio,
        &pos,
        &personality,
        30.0,
        22.0,
        600.0,
        &[],
        &[],
        "10:00",
        1.0,
    );
    assert!(
        result_30.acoustic_text.contains("Stille"),
        "30dB should produce 'Stille', got: '{}'",
        result_30.acoustic_text
    );

    // 50dB -> Normal/Buerogeraeusch
    let result_50 = generate_perception(
        &bio,
        &pos,
        &personality,
        50.0,
        22.0,
        600.0,
        &[],
        &[],
        "10:00",
        1.0,
    );
    assert!(
        result_50.acoustic_text.contains("Normal")
            || result_50.acoustic_text.contains("Buero"),
        "50dB should produce 'Normal' or 'Buero', got: '{}'",
        result_50.acoustic_text
    );

    // 70dB -> Laut
    let result_70 = generate_perception(
        &bio,
        &pos,
        &personality,
        70.0,
        22.0,
        600.0,
        &[],
        &[],
        "10:00",
        1.0,
    );
    assert!(
        result_70.acoustic_text.to_lowercase().contains("laut"),
        "70dB should produce text containing 'laut', got: '{}'",
        result_70.acoustic_text
    );
}

// AC #14.06: Leerer Raum -> "allein" im Text
#[test]
fn ac_14_06_empty_room() {
    let bio = default_bio();
    let pos = default_position();
    let personality = default_personality();

    let result = generate_perception(
        &bio,
        &pos,
        &personality,
        30.0,
        22.0,
        600.0,
        &[],
        &[], // Keine Agenten
        "10:00",
        1.0,
    );

    assert!(
        result.presence_text.to_lowercase().contains("allein"),
        "empty present_agents should produce 'allein', got: '{}'",
        result.presence_text
    );
}

// AC #14.07: Gesunder Agent -> positiver/neutraler Text
#[test]
fn ac_14_07_healthy_agent() {
    let bio = default_bio(); // caffeine_mg=50, alle Werte normal
    let pos = default_position();
    let personality = default_personality();

    let result = generate_perception(
        &bio,
        &pos,
        &personality,
        40.0,
        22.0,
        600.0,
        &[],
        &[],
        "10:00",
        2.0,
    );

    assert!(
        result.body_text.contains("gut"),
        "healthy agent should feel 'gut', got: '{}'",
        result.body_text
    );
}

// AC #14.08: Impuls-Prioritaet: Toilette UND Hunger gesetzt -> Toilette zuerst im Impuls
#[test]
fn ac_14_08_impulse_priority() {
    let mut bio = default_bio();
    bio.bladder = 90.0; // Toilette dringend
    bio.hunger = 90.0; // Hunger auch dringend
    let pos = default_position();
    let personality = default_personality();

    let result = generate_perception(
        &bio,
        &pos,
        &personality,
        40.0,
        22.0,
        600.0,
        &[],
        &[],
        "10:00",
        2.0,
    );

    assert!(
        result.impulse_text.contains("Toilette"),
        "bladder=90 + hunger=90: impulse should prioritize 'Toilette', got: '{}'",
        result.impulse_text
    );
}

// AC #14.09: format_injection Output enthaelt [SYSTEM_INJECTION] und [/SYSTEM_INJECTION]
#[test]
fn ac_14_09_format_injection() {
    let bio = default_bio();
    let pos = default_position();
    let personality = default_personality();

    let perception = generate_perception(
        &bio,
        &pos,
        &personality,
        40.0,
        22.0,
        600.0,
        &[],
        &[],
        "10:00",
        2.0,
    );

    let injection = format_injection(&perception);

    assert!(
        injection.contains("[SYSTEM_INJECTION]"),
        "format_injection must contain '[SYSTEM_INJECTION]', got: '{}'",
        injection
    );
    assert!(
        injection.contains("[/SYSTEM_INJECTION]"),
        "format_injection must contain '[/SYSTEM_INJECTION]', got: '{}'",
        injection
    );
}
