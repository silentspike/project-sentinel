//! Acceptance Tests fuer Issue #20: Agent Migration (TOML Config)
//!
//! Testet load_agent_config, load_all_agents, PersonalityConfig::validate,
//! shift_set und Sortierung.

use std::path::PathBuf;

use sentinel_common::agent_config::{load_agent_config, load_all_agents, PersonalityConfig};

fn config_dir() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    PathBuf::from(manifest).join("../../config/agents")
}

// AC #20.01: Mindestens 5 AGENT-*.toml Dateien vorhanden
#[test]
fn ac_20_01_five_toml_files() {
    let dir = config_dir();
    let count = std::fs::read_dir(&dir)
        .expect("config/agents directory should exist")
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with("AGENT-") && name.ends_with(".toml")
        })
        .count();

    assert!(
        count >= 5,
        "Expected at least 5 AGENT-*.toml files, found: {}",
        count
    );
}

// AC #20.02: load_agent_config(AGENT-01) parsed alle Sektionen korrekt
#[test]
fn ac_20_02_parser_all_sections() {
    let path = config_dir().join("AGENT-01-THOMAS-CEO.toml");
    let config = load_agent_config(&path).expect("AGENT-01 should parse successfully");

    // Identity
    assert_eq!(config.identity.id, 1);
    assert!(!config.identity.name.is_empty(), "name should not be empty");
    assert!(!config.identity.role.is_empty(), "role should not be empty");
    assert!(
        !config.identity.department.is_empty(),
        "department should not be empty"
    );

    // Personality (Big Five values should be in 0-1 range)
    assert!(
        (0.0..=1.0).contains(&config.personality.openness),
        "openness should be 0-1, got: {}",
        config.personality.openness
    );
    assert!(
        (0.0..=1.0).contains(&config.personality.conscientiousness),
        "conscientiousness should be 0-1, got: {}",
        config.personality.conscientiousness
    );
    assert!(
        (0.0..=1.0).contains(&config.personality.extraversion),
        "extraversion should be 0-1, got: {}",
        config.personality.extraversion
    );

    // Preferences
    assert!(
        !config.preferences.favorite_room.is_empty(),
        "favorite_room should not be empty"
    );

    // Background
    assert!(
        !config.background.bio.is_empty(),
        "bio should not be empty"
    );
}

// AC #20.03: PersonalityConfig Validation: Wert 1.5 -> Error, Wert -0.1 -> Error
#[test]
fn ac_20_03_personality_validation() {
    // Wert 1.5 -> Error
    let invalid_high = PersonalityConfig {
        openness: 1.5,
        conscientiousness: 0.5,
        extraversion: 0.5,
        agreeableness: 0.5,
        neuroticism: 0.5,
        caffeine_tolerance: 0.5,
        morning_person: true,
    };
    assert!(
        invalid_high.validate().is_err(),
        "openness=1.5 should fail validation"
    );

    // Wert -0.1 -> Error
    let invalid_low = PersonalityConfig {
        openness: 0.5,
        conscientiousness: 0.5,
        extraversion: -0.1,
        agreeableness: 0.5,
        neuroticism: 0.5,
        caffeine_tolerance: 0.5,
        morning_person: true,
    };
    assert!(
        invalid_low.validate().is_err(),
        "extraversion=-0.1 should fail validation"
    );
}

// AC #20.04: load_all_agents() gibt sortierte Liste zurueck (IDs aufsteigend)
#[test]
fn ac_20_04_load_all_sorted() {
    let agents = load_all_agents(&config_dir()).expect("load_all_agents should succeed");

    assert!(
        !agents.is_empty(),
        "load_all_agents should return at least one agent"
    );

    // Pruefen dass IDs aufsteigend sortiert sind
    for window in agents.windows(2) {
        assert!(
            window[0].identity.id < window[1].identity.id,
            "Agents should be sorted by ID: {} should be before {}",
            window[0].identity.id,
            window[1].identity.id
        );
    }
}

// AC #20.05: AGENT-01 hat shift_set=1
#[test]
fn ac_20_05_shift_set() {
    let path = config_dir().join("AGENT-01-THOMAS-CEO.toml");
    let config = load_agent_config(&path).expect("AGENT-01 should parse");

    assert_eq!(
        config.identity.shift_set, 1,
        "AGENT-01 (Thomas, Frueh-Schicht) should have shift_set=1, got: {}",
        config.identity.shift_set
    );
}
