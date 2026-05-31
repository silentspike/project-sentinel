//! Room layout configuration for PixelPerfekt GmbH office building.
//!
//! Parses `config/rooms.toml` and provides validated building/room data.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Raum-Typ im Bürogebäude
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomType {
    Office,
    Meeting,
    Common,
    Break,
    Transit,
    Bathroom,
}

/// Konfiguration eines einzelnen Raums aus rooms.toml
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomConfig {
    pub id: String,
    pub name: String,
    pub floor: i8,
    pub capacity: u16,
    pub room_type: RoomType,
    pub adjacent: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department: Option<String>,
    #[serde(default)]
    pub has_coffee_machine: bool,
    #[serde(default)]
    pub has_printer: bool,
}

/// Gebäude-Metadaten
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildingMeta {
    pub name: String,
    pub address: String,
    pub floors: u8,
}

/// Top-level Config aus rooms.toml
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildingConfig {
    pub building: BuildingMeta,
    pub rooms: Vec<RoomConfig>,
}

impl BuildingConfig {
    /// Lädt rooms.toml und parsed die Konfiguration
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read rooms.toml from {}", path.display()))?;
        let config: BuildingConfig =
            toml::from_str(&content).context("Failed to parse rooms.toml")?;
        Ok(config)
    }

    /// Validiert die Raum-Konfiguration
    ///
    /// Prüft:
    /// - Alle adjacent-Referenzen existieren
    /// - Adjacency ist bidirektional (A→B impliziert B→A)
    /// - Keine doppelten Room-IDs
    /// - Gesamtkapazität >= min_capacity
    ///
    /// Returns: Ok(()) wenn valide, sonst Err mit allen Validierungsfehlern
    pub fn validate(&self, min_capacity: u16) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        // Build room ID set für schnelle Lookups
        let room_ids: HashSet<&str> = self.rooms.iter().map(|r| r.id.as_str()).collect();

        // Check für doppelte IDs
        let mut seen_ids = HashSet::new();
        for room in &self.rooms {
            if !seen_ids.insert(&room.id) {
                errors.push(format!("Duplicate room ID: {}", room.id));
            }
        }

        // Check dass alle adjacent-Referenzen existieren
        for room in &self.rooms {
            for adj in &room.adjacent {
                if !room_ids.contains(adj.as_str()) {
                    errors.push(format!(
                        "Room '{}' references non-existent adjacent room '{}'",
                        room.id, adj
                    ));
                }
            }
        }

        // Build adjacency map für bidirektionale Prüfung
        let mut adjacency_map: HashMap<&str, HashSet<&str>> = HashMap::new();
        for room in &self.rooms {
            let adj_set: HashSet<&str> = room.adjacent.iter().map(|s| s.as_str()).collect();
            adjacency_map.insert(&room.id, adj_set);
        }

        // Check bidirektionale Adjacency
        for room in &self.rooms {
            for adj in &room.adjacent {
                // Prüfe ob der adjacent Raum zurück-referenziert
                if let Some(reverse_set) = adjacency_map.get(adj.as_str()) {
                    if !reverse_set.contains(room.id.as_str()) {
                        errors.push(format!(
                            "Adjacency not bidirectional: '{}' → '{}' exists, but '{}' → '{}' missing",
                            room.id, adj, adj, room.id
                        ));
                    }
                }
            }
        }

        // Check Gesamtkapazität
        let total_capacity: u32 = self.rooms.iter().map(|r| r.capacity as u32).sum();
        if total_capacity < min_capacity as u32 {
            errors.push(format!(
                "Total capacity {} is below minimum required capacity {}",
                total_capacity, min_capacity
            ));
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Findet einen Raum per ID
    pub fn get_room(&self, id: &str) -> Option<&RoomConfig> {
        self.rooms.iter().find(|r| r.id == id)
    }

    /// Gibt alle Räume eines bestimmten Typs zurück
    pub fn rooms_by_type(&self, room_type: &RoomType) -> Vec<&RoomConfig> {
        self.rooms
            .iter()
            .filter(|r| &r.room_type == room_type)
            .collect()
    }

    /// Gibt alle Räume einer Abteilung zurück
    pub fn rooms_by_department(&self, department: &str) -> Vec<&RoomConfig> {
        self.rooms
            .iter()
            .filter(|r| r.department.as_deref() == Some(department))
            .collect()
    }

    /// Berechnet kuerzesten Pfad (Hop-Count) zwischen zwei Raeumen via BFS.
    ///
    /// Gibt `None` zurueck wenn einer der Raeume nicht existiert oder
    /// kein Pfad vorhanden ist (sollte bei validem rooms.toml nicht vorkommen).
    pub fn shortest_distance(&self, from: &str, to: &str) -> Option<u32> {
        if from == to {
            return Some(0);
        }

        // Build adjacency map
        let adjacency: HashMap<&str, Vec<&str>> = self
            .rooms
            .iter()
            .map(|r| {
                (
                    r.id.as_str(),
                    r.adjacent.iter().map(|a| a.as_str()).collect(),
                )
            })
            .collect();

        if !adjacency.contains_key(from) || !adjacency.contains_key(to) {
            return None;
        }

        // BFS
        let mut visited: HashSet<&str> = HashSet::new();
        let mut queue: std::collections::VecDeque<(&str, u32)> = std::collections::VecDeque::new();
        visited.insert(from);
        queue.push_back((from, 0));

        while let Some((current, dist)) = queue.pop_front() {
            if let Some(neighbors) = adjacency.get(current) {
                for &neighbor in neighbors {
                    if neighbor == to {
                        return Some(dist + 1);
                    }
                    if visited.insert(neighbor) {
                        queue.push_back((neighbor, dist + 1));
                    }
                }
            }
        }

        None // Kein Pfad gefunden
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_test_config() -> BuildingConfig {
        let config_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap() // crates/
            .parent()
            .unwrap() // project-sentinel/
            .join("config/rooms.toml");
        BuildingConfig::load(&config_path).expect("Failed to load config/rooms.toml")
    }

    #[test]
    fn test_parse_rooms_toml() {
        let config = load_test_config();

        // 26 Räume (17 original + 9 neue Büros)
        assert_eq!(config.rooms.len(), 26);

        // Gebäudename korrekt
        assert_eq!(config.building.name, "PixelPerfekt GmbH");
        assert_eq!(config.building.floors, 2);
    }

    #[test]
    fn building_config_toml_round_trip() {
        // #425: Serialize muss fuer config_dir-Write-Back round-trippen.
        let original = load_test_config();
        let serialized = toml::to_string(&original).expect("serialize BuildingConfig to TOML");
        let reparsed: BuildingConfig =
            toml::from_str(&serialized).expect("re-parse serialized BuildingConfig");
        assert_eq!(
            original, reparsed,
            "BuildingConfig TOML round-trip must be identical"
        );
    }

    #[test]
    fn test_all_adjacent_references_valid() {
        let config = load_test_config();
        let result = config.validate(26);
        assert!(
            result.is_ok(),
            "Validation failed: {:?}",
            result.err().unwrap()
        );
    }

    #[test]
    fn test_total_capacity_sufficient() {
        let config = load_test_config();
        let total_capacity: u32 = config.rooms.iter().map(|r| r.capacity as u32).sum();
        assert!(total_capacity >= 15, "Total capacity is {}", total_capacity);
    }

    #[test]
    fn test_exactly_26_rooms() {
        let config = load_test_config();
        assert_eq!(config.rooms.len(), 26);
    }

    #[test]
    fn test_room_lookup_by_id() {
        let config = load_test_config();
        let kueche = config.get_room("kueche").expect("kueche not found");
        assert_eq!(kueche.name, "Kueche / Pausenraum");
        assert!(
            kueche.has_coffee_machine,
            "kueche should have coffee machine"
        );
    }

    #[test]
    fn test_rooms_by_type() {
        let config = load_test_config();

        let offices = config.rooms_by_type(&RoomType::Office);
        assert_eq!(offices.len(), 14, "Expected 14 office rooms");

        let meetings = config.rooms_by_type(&RoomType::Meeting);
        assert_eq!(meetings.len(), 3, "Expected 3 meeting rooms");
    }

    #[test]
    fn test_rooms_by_department() {
        let config = load_test_config();

        let dev_rooms = config.rooms_by_department("Entwicklung");
        assert_eq!(dev_rooms.len(), 2, "Expected 2 Entwicklung rooms");

        let design_rooms = config.rooms_by_department("Design");
        assert_eq!(design_rooms.len(), 2, "Expected 2 Design rooms");
    }

    #[test]
    fn test_shortest_distance_same_room() {
        let config = load_test_config();
        assert_eq!(config.shortest_distance("kueche", "kueche"), Some(0));
    }

    #[test]
    fn test_shortest_distance_adjacent() {
        let config = load_test_config();
        // kueche <-> flur-eg: 1 hop
        assert_eq!(config.shortest_distance("kueche", "flur-eg"), Some(1));
        assert_eq!(config.shortest_distance("flur-eg", "kueche"), Some(1));
    }

    #[test]
    fn test_shortest_distance_two_hops() {
        let config = load_test_config();
        // buero-dev-1 -> flur-eg -> kueche: 2 hops
        assert_eq!(config.shortest_distance("buero-dev-1", "kueche"), Some(2));
    }

    #[test]
    fn test_shortest_distance_cross_floor() {
        let config = load_test_config();
        // buero-dev-1 -> flur-eg -> treppenhaus -> flur-og -> buero-design-1: 4 hops
        assert_eq!(
            config.shortest_distance("buero-dev-1", "buero-design-1"),
            Some(4)
        );
    }

    #[test]
    fn test_shortest_distance_nonexistent_room() {
        let config = load_test_config();
        assert_eq!(config.shortest_distance("kueche", "nonexistent"), None);
        assert_eq!(config.shortest_distance("nonexistent", "kueche"), None);
    }

    #[test]
    fn test_adjacency_bidirectional() {
        let config = load_test_config();

        // Build adjacency map
        let mut adjacency_map: HashMap<&str, HashSet<&str>> = HashMap::new();
        for room in &config.rooms {
            let adj_set: HashSet<&str> = room.adjacent.iter().map(|s| s.as_str()).collect();
            adjacency_map.insert(&room.id, adj_set);
        }

        // Für jede Adjacency A→B muss B→A existieren
        for room in &config.rooms {
            for adj in &room.adjacent {
                let reverse_set = adjacency_map
                    .get(adj.as_str())
                    .unwrap_or_else(|| panic!("Adjacent room '{}' not found", adj));
                assert!(
                    reverse_set.contains(room.id.as_str()),
                    "Adjacency not bidirectional: '{}' → '{}' exists, but '{}' → '{}' missing",
                    room.id,
                    adj,
                    adj,
                    room.id
                );
            }
        }
    }
}
