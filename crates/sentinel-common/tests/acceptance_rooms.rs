//! Acceptance Tests fuer config/rooms.toml (Issue #11)
//!
//! Tests fuer Room-Layout: 17 Raeume (4 Toiletten: Damen/Herren je EG+OG),
//! bidirektionale Adjacency, Rust-Types-Parsing, Kapazitaets-Summe und Validierung.

use sentinel_common::room::BuildingConfig;
use std::collections::{HashMap, HashSet};
use std::path::Path;

// ── Helper ──

fn load_rooms_config() -> BuildingConfig {
    let config_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap() // crates/
        .parent()
        .unwrap() // project-sentinel/
        .join("config/rooms.toml");
    BuildingConfig::load(&config_path).expect("Failed to load config/rooms.toml")
}

// ── #11 AC1: 17 Raeume ──

/// AC #11.1: rooms.toml enthaelt genau 17 Raeume (4 Toiletten: Damen/Herren je EG+OG)
#[test]
fn ac_11_01_17_rooms() {
    let config = load_rooms_config();
    assert_eq!(
        config.rooms.len(),
        26,
        "Expected exactly 26 rooms, got {}",
        config.rooms.len()
    );
}

// ── #11 AC2: Bidirektionale Adjacency ──

/// AC #11.2: Fuer jeden Raum A->B muss B->A existieren
#[test]
fn ac_11_02_bidirectional_adjacency() {
    let config = load_rooms_config();

    // Build adjacency map
    let mut adjacency_map: HashMap<&str, HashSet<&str>> = HashMap::new();
    for room in &config.rooms {
        let adj_set: HashSet<&str> = room.adjacent.iter().map(|s| s.as_str()).collect();
        adjacency_map.insert(&room.id, adj_set);
    }

    // Fuer jede Adjacency A->B pruefe B->A
    for room in &config.rooms {
        for adj in &room.adjacent {
            let reverse_set = adjacency_map
                .get(adj.as_str())
                .unwrap_or_else(|| panic!("Adjacent room '{}' not found in room list", adj));
            assert!(
                reverse_set.contains(room.id.as_str()),
                "Adjacency not bidirectional: '{}' -> '{}' exists, but '{}' -> '{}' missing",
                room.id,
                adj,
                adj,
                room.id
            );
        }
    }
}

// ── #11 AC3: Rust Types parsen ──

/// AC #11.3: BuildingConfig::load() parsed ohne Fehler
#[test]
fn ac_11_03_rust_types_parse() {
    let config_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/rooms.toml");
    let result = BuildingConfig::load(&config_path);
    assert!(
        result.is_ok(),
        "BuildingConfig::load() failed: {:?}",
        result.err()
    );
}

// ── #11 AC4: Kapazitaets-Summe >= 15 ──

/// AC #11.4: Summe aller Kapazitaeten >= 15
#[test]
fn ac_11_04_capacity_sum() {
    let config = load_rooms_config();
    let total_capacity: u32 = config.rooms.iter().map(|r| r.capacity as u32).sum();
    assert!(
        total_capacity >= 15,
        "Total capacity {} is below minimum 15",
        total_capacity
    );
}

// ── #11 AC6: BuildingConfig::validate() returns Ok ──

/// AC #11.6: BuildingConfig::validate() returns Ok
#[test]
fn ac_11_06_validate() {
    let config = load_rooms_config();
    let result = config.validate(26);
    assert!(
        result.is_ok(),
        "BuildingConfig::validate(17) failed: {:?}",
        result.err()
    );
}
