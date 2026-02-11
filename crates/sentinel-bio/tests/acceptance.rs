//! Acceptance Tests fuer sentinel-bio (Issue #10)
//!
//! Tests fuer Bio-Formeln, Action-Funktionen und Koffein-Halbwertszeit.

use approx::assert_relative_eq;
use sentinel_bio::{drink_coffee, eat_meal, update_bio_state, use_bathroom};
use sentinel_common::components::{BioState, Personality, WorkContext};

// ── Helper ──

fn default_bio() -> BioState {
    BioState {
        hunger: 0.0,
        energy: 80.0,
        caffeine_mg: 0.0,
        bladder: 0.0,
        stress: 0.0,
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

fn default_work() -> WorkContext {
    WorkContext {
        current_task: None,
        in_meeting: false,
        has_deadline: false,
        has_conflict: false,
    }
}

// ── #10 AC2: Bio-Formeln mit bekannten Inputs ──

/// AC #10.2: update_bio() mit bekannten Inputs, assert Outputs (Hunger +12.5/h, Energie circadian)
#[test]
fn ac_10_02_bio_formulas() {
    let mut bio = default_bio();
    let personality = default_personality();
    let work = default_work();

    // 1 Stunde in 1-Minuten-Schritten simulieren
    for _ in 0..60 {
        update_bio_state(&mut bio, &personality, &work, 60.0, 10.0);
    }

    // Hunger: 0 + 12.5/h * 1h = 12.5
    assert_relative_eq!(bio.hunger, 12.5, epsilon = 1.0);

    // Energie: circadian + penalties (morning person bei 10:00 = Peak)
    // base = 80 + 15*sin((10-2)*PI/12) = 80 + 15*sin(2.09) = 80 + 15*0.866 = 92.99
    // keine penalties bei niedrigem Hunger/Stress
    assert!(
        bio.energy > 70.0,
        "Energy should be high for morning person at 10:00, got {}",
        bio.energy
    );

    // Sozial: Extraversion 0.5 >= 0.5 = extrovertiert, +10/h
    // 50 + 10 = 60
    assert_relative_eq!(bio.social_need, 60.0, epsilon = 1.0);
}

// ── #10 AC3: Action-Funktionen ──

/// AC #10.3: drink_coffee() +95mg, eat_meal() hunger=0, use_bathroom() bladder=0
#[test]
fn ac_10_03_action_functions() {
    let mut bio = default_bio();

    // drink_coffee() addiert 95mg Koffein
    drink_coffee(&mut bio);
    assert_relative_eq!(bio.caffeine_mg, 95.0, epsilon = 0.1);

    // Nochmals trinken: 190mg
    drink_coffee(&mut bio);
    assert_relative_eq!(bio.caffeine_mg, 190.0, epsilon = 0.1);

    // eat_meal() setzt Hunger auf 0 und gibt +15 Energie
    bio.hunger = 75.0;
    let energy_before = bio.energy;
    eat_meal(&mut bio);
    assert_relative_eq!(bio.hunger, 0.0, epsilon = 0.1);
    assert!(
        bio.energy >= energy_before,
        "Energy should not decrease after eating"
    );

    // use_bathroom() setzt Blasendrang auf 0
    bio.bladder = 85.0;
    use_bathroom(&mut bio);
    assert_relative_eq!(bio.bladder, 0.0, epsilon = 0.1);
}

// ── #10 AC4: Koffein-Halbwertszeit ──

/// AC #10.4: 95mg Start, 5.7h simulieren, assert ~47.5mg (epsilon=2.0)
#[test]
fn ac_10_04_caffeine_halflife() {
    let mut bio = default_bio();
    bio.caffeine_mg = 95.0;
    let personality = default_personality();
    let work = default_work();

    // 5.7 Stunden = 342 Minuten in 1-Minuten-Schritten
    for _ in 0..342 {
        update_bio_state(&mut bio, &personality, &work, 60.0, 10.0);
    }

    // Nach einer Halbwertszeit: ~47.5mg
    assert_relative_eq!(bio.caffeine_mg, 47.5, epsilon = 2.0);
}
