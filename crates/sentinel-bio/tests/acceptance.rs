//! Acceptance Tests fuer sentinel-bio (Issue #10)
//!
//! Tests fuer Bio-Formeln, Action-Funktionen und Koffein-Halbwertszeit.

use approx::assert_relative_eq;
use sentinel_bio::{apply_psi_stress, drink_coffee, eat_meal, update_bio_state, use_bathroom};
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
        extraversion: 0.6, // Above threshold (0.5) to avoid boundary flakiness under tarpaulin
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
        conflict_cooldown: 0,
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

// ── #74 AC2: PSI CPU Stress ──

/// AC #74.2: CPU PSI avg10 > 50 erhoht Stress um 10
#[test]
fn ac_74_02_psi_cpu_stress() {
    let mut bio = default_bio();
    bio.stress = 25.0;

    // CPU PSI ueber Schwelle (50), Memory unter Schwelle
    apply_psi_stress(&mut bio, 60.0, 30.0);

    assert_relative_eq!(bio.stress, 35.0, epsilon = 0.01);
    // Comfort unveraendert (Memory unter Schwelle)
    assert_relative_eq!(bio.comfort, 70.0, epsilon = 0.01);
}

// ── #74 AC3: PSI Memory Comfort ──

/// AC #74.3: Memory PSI avg10 > 70 senkt Comfort um 15, erhoht Stress um 20
#[test]
fn ac_74_03_psi_mem_comfort() {
    let mut bio = default_bio();
    bio.stress = 10.0;
    bio.comfort = 80.0;

    // Memory PSI ueber Schwelle (70), CPU unter Schwelle
    apply_psi_stress(&mut bio, 20.0, 85.0);

    assert_relative_eq!(bio.stress, 30.0, epsilon = 0.01); // +20
    assert_relative_eq!(bio.comfort, 65.0, epsilon = 0.01); // -15
}

// ── #74 AC-N1: PSI unter Schwelle keine Aenderung ──

/// AC #74.N1: Keine Aenderung wenn beide PSI-Werte unter Schwelle
#[test]
fn ac_74_n1_psi_no_change_below_threshold() {
    let mut bio = default_bio();
    bio.stress = 40.0;
    bio.comfort = 60.0;

    apply_psi_stress(&mut bio, 30.0, 50.0);

    assert_relative_eq!(bio.stress, 40.0, epsilon = 0.01);
    assert_relative_eq!(bio.comfort, 60.0, epsilon = 0.01);
}
