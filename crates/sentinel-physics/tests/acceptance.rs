//! Acceptance Tests fuer sentinel-physics (Issue #12)
//!
//! Tests fuer Akustik, Temperatur, CO2, Geruchs-Propagation,
//! Transit-Lifecycle und Flurbegegnungen.

use approx::assert_relative_eq;
use sentinel_common::EventType;
use sentinel_physics::{
    calculate_co2, calculate_noise_level, calculate_temperature, chaos_noise_bonus_db,
    chaos_temperature_delta_celsius, check_hallway_encounter, smell_intensity_at_distance,
    start_transit, tick_transit, SmellEvent, SmellType,
};

// ── #12 AC2: Akustik ──

/// AC #12.2: 0 Agents=30dB, 5 Agents erhoehen Pegel leicht (logarithmisch)
#[test]
fn ac_12_02_acoustics() {
    // Leerer Raum: Basis 30dB
    let noise_empty = calculate_noise_level(0, false, false, &[]);
    assert_relative_eq!(noise_empty, 30.0, epsilon = 0.5);

    // 5 Agents: logarithmisch addiert, ~30.07dB (NICHT 55dB wie linear)
    let noise_5 = calculate_noise_level(5, false, false, &[]);
    assert!(noise_5 > 30.0, "5 agents should raise noise, got {noise_5}");
    assert!(
        noise_5 < 35.0,
        "5 agents logarithmic should stay < 35dB, got {noise_5}"
    );
}

/// Regression #244 groundwork: PrinterBroken muss lokalen Laerm klar erhoehen,
/// ohne absurde Werte zu erzeugen.
#[test]
fn regression_printer_broken_noise_bonus_is_bounded() {
    let quiet_room = calculate_noise_level(2, false, false, &[]);
    let broken_printer_room = quiet_room + chaos_noise_bonus_db(EventType::PrinterBroken);

    assert!(broken_printer_room > quiet_room + 20.0);
    assert!(broken_printer_room < 90.0);
}

// ── #12 AC3: Temperatur ──

/// AC #12.3: Basis-Temperatur + Koerperwaerme + Fenster
#[test]
fn ac_12_03_temperature() {
    // Leerer Raum, kein Fenster, keine Sonne: base = 21.0
    let temp_base = calculate_temperature(21.0, 0, false, 15.0, 0.0);
    assert_relative_eq!(temp_base, 21.0, epsilon = 0.5);

    // 5 Agents: 21 + 5*0.3 = 22.5
    let temp_agents = calculate_temperature(21.0, 5, false, 15.0, 0.0);
    assert_relative_eq!(temp_agents, 22.5, epsilon = 0.5);

    // Fenster offen bei 10 Grad aussen: 21 + (10-21)*0.5 = 21 - 5.5 = 15.5
    let temp_window = calculate_temperature(21.0, 0, true, 10.0, 0.0);
    assert_relative_eq!(temp_window, 15.5, epsilon = 0.5);

    // Sonne (max): 21 + 1.0*2.0 = 23.0
    let temp_sun = calculate_temperature(21.0, 0, false, 15.0, 1.0);
    assert_relative_eq!(temp_sun, 23.0, epsilon = 0.5);
}

/// Regression #243: AirConBroken muss einen messbaren Temperaturanstieg liefern.
#[test]
fn regression_aircon_broken_increases_temperature() {
    let base = calculate_temperature(21.0, 2, false, 15.0, 0.3);
    let after_hour = base + chaos_temperature_delta_celsius(EventType::AirConBroken, 1.0);
    let after_two_hours = base + chaos_temperature_delta_celsius(EventType::AirConBroken, 2.0);

    assert!(after_hour > base + 2.0, "expected visible heatup after 1h");
    assert!(after_two_hours > after_hour, "heat should continue rising");
}

// ── #12 AC4: CO2 ──

/// AC #12.4: CO2 buildup, assert >400ppm ohne Ventilation
#[test]
fn ac_12_04_co2() {
    // Leerer Raum: immer 400ppm Basis
    let co2_empty = calculate_co2(400.0, 0, 0.0, 1.0);
    assert_relative_eq!(co2_empty, 400.0, epsilon = 1.0);

    // 5 Agents, 1 Stunde, keine Ventilation: 400 + 5*40*1 = 600
    let co2_5_agents = calculate_co2(400.0, 5, 0.0, 1.0);
    assert!(
        co2_5_agents > 400.0,
        "CO2 should be above 400 with 5 agents, got {}",
        co2_5_agents
    );
    assert_relative_eq!(co2_5_agents, 600.0, epsilon = 1.0);

    // 5 Agents, 1 Stunde, 50% Ventilation: 400 + 200 - 100 = 500
    let co2_ventilated = calculate_co2(400.0, 5, 0.5, 1.0);
    assert_relative_eq!(co2_ventilated, 500.0, epsilon = 1.0);
}

// ── #12 AC5: Geruchs-Propagation ──

/// AC #12.5: Intensity Decay 0.8 -> 0.55 -> 0.30 -> 0.05
#[test]
fn ac_12_05_smell_propagation() {
    let event = SmellEvent {
        source_room: "kueche".to_string(),
        smell_type: SmellType::Coffee,
        intensity: 0.8,
        decay_per_room: 0.25,
        created_tick: 10,
        duration_ticks: 100,
    };

    // Distance 0 (Quelle): 0.8
    assert_relative_eq!(smell_intensity_at_distance(&event, 0), 0.8, epsilon = 0.01);

    // Distance 1: 0.8 - 0.25 = 0.55
    assert_relative_eq!(smell_intensity_at_distance(&event, 1), 0.55, epsilon = 0.01);

    // Distance 2: 0.8 - 0.50 = 0.30
    assert_relative_eq!(smell_intensity_at_distance(&event, 2), 0.30, epsilon = 0.01);

    // Distance 3: 0.8 - 0.75 = 0.05
    assert_relative_eq!(smell_intensity_at_distance(&event, 3), 0.05, epsilon = 0.01);

    // Distance 4: 0.8 - 1.0 = -0.2 -> clamped to 0.0
    assert_relative_eq!(smell_intensity_at_distance(&event, 4), 0.0, epsilon = 0.01);
}

// ── #12 AC6: Transit-Lifecycle ──

/// AC #12.6: start_transit(), tick(), complete_transit()
#[test]
fn ac_12_06_transit_lifecycle() {
    // Transit starten: buero-dev-1 -> kueche, 40000ms (2 Hops * 20s)
    let mut state = start_transit("buero-dev-1", "kueche", 40_000);
    assert!(state.in_transit);
    assert_eq!(state.origin, "buero-dev-1");
    assert_eq!(state.target, "kueche");
    assert_eq!(state.remaining_ms, 40_000);

    // Erster Tick: 1000ms vergangen, nicht fertig
    let finished = tick_transit(&mut state, 1000);
    assert!(!finished);
    assert!(state.in_transit);
    assert_eq!(state.remaining_ms, 39_000);

    // Restliche Zeit: 39000ms vergangen, Transit abgeschlossen
    let finished = tick_transit(&mut state, 39_000);
    assert!(finished);
    assert!(!state.in_transit);
    assert_eq!(state.remaining_ms, 0);

    // Weiterer Tick auf bereits abgeschlossenen Transit: kein Effekt
    let finished = tick_transit(&mut state, 1000);
    assert!(!finished);
}

// ── #12 AC7: Flurbegegnungen ──

/// AC #12.7: 1000 Iterationen, assert Begegnungsrate 20-40%
#[test]
fn ac_12_07_hallway_encounters() {
    let mut encounter_count = 0u32;
    let iterations = 1000u32;

    // Einfacher Pseudo-RNG fuer deterministischen Test
    for i in 0..iterations {
        // Hash-basierter Pseudo-Zufall: gleichverteilt in [0, 1)
        let rng = {
            let mut x = (i as u64).wrapping_mul(6364136223846793005);
            x ^= x >> 33;
            x = x.wrapping_mul(0xff51afd7ed558ccd);
            x ^= x >> 33;
            (x % 10000) as f32 / 10000.0
        };
        if check_hallway_encounter(true, true, rng) {
            encounter_count += 1;
        }
    }

    let rate = encounter_count as f64 / iterations as f64;

    // Erwartet: ~30% (HALLWAY_ENCOUNTER_PROBABILITY = 0.3)
    // Akzeptabler Bereich: 20-40%
    assert!(
        (0.20..=0.40).contains(&rate),
        "Encounter rate {:.1}% outside expected range 20-40%",
        rate * 100.0
    );
}
