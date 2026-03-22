//! Acceptance tests for sentinel-common (Issue #5).

use sentinel_common::*;
use std::path::Path;

/// AC 5.2: At least 4 FlatBuffer schemas exist in schemas/
#[test]
fn ac_05_02_four_flatbuffer_schemas() {
    // AC 5.2: Verify at least 4 .fbs schema files exist
    let schemas_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("schemas");

    let fbs_files: Vec<_> = std::fs::read_dir(&schemas_dir)
        .unwrap_or_else(|e| panic!("Failed to read schemas dir {:?}: {}", schemas_dir, e))
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("fbs") {
                Some(path)
            } else {
                None
            }
        })
        .collect();

    assert!(
        fbs_files.len() >= 4,
        "Expected at least 4 .fbs schemas, found {}: {:?}",
        fbs_files.len(),
        fbs_files
    );
}

/// AC 5.4: All message types roundtrip through serde JSON
#[test]
fn ac_05_04_serde_roundtrip_all_types() {
    // AC 5.4: Serde roundtrip for every public message struct

    // AgentAction
    let action = AgentAction {
        agent_id: AgentId(1),
        action_type: ActionType::Chat,
        target_room: Some("konferenz-1".to_string()),
        target_agent: Some(AgentId(5)),
        content: Some("Hallo Welt".to_string()),
        timestamp: Timestamp(1000),
        tick: Tick(42),
    };
    let json = serde_json::to_string(&action).unwrap();
    let deserialized: AgentAction = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.agent_id, action.agent_id);
    assert_eq!(deserialized.action_type, action.action_type);
    assert_eq!(deserialized.target_room, action.target_room);
    assert_eq!(deserialized.target_agent, action.target_agent);
    assert_eq!(deserialized.content, action.content);
    assert_eq!(deserialized.timestamp, action.timestamp);
    assert_eq!(deserialized.tick, action.tick);

    // Perception
    let perception = Perception {
        agent_id: AgentId(2),
        circadian_text: "11:42 Uhr".to_string(),
        body_text: "Hunger 85%".to_string(),
        environment_text: "Kaffeeduft".to_string(),
        acoustic_text: "Lebhaft".to_string(),
        heard_text: String::new(),
        presence_text: "Max, Sophie".to_string(),
        impulse_text: "Pause machen".to_string(),
        is_directly_addressed: false,
        timestamp: Timestamp(2000),
        tick: Tick(100),
    };
    let json = serde_json::to_string(&perception).unwrap();
    let deserialized: Perception = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.agent_id, perception.agent_id);
    assert_eq!(deserialized.circadian_text, perception.circadian_text);
    assert_eq!(deserialized.timestamp, perception.timestamp);

    // BioStateUpdate
    let bio = BioStateUpdate::new(
        AgentId(3),
        45.5,
        72.0,
        95.0,
        30.0,
        55.0,
        20.0,
        80.0,
        Timestamp(3000),
        Tick(150),
    )
    .unwrap();
    let json = serde_json::to_string(&bio).unwrap();
    let deserialized: BioStateUpdate = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.agent_id, bio.agent_id);
    assert!((deserialized.hunger - bio.hunger).abs() < f32::EPSILON);
    assert!((deserialized.energy - bio.energy).abs() < f32::EPSILON);

    // PositionUpdate
    let pos = PositionUpdate {
        agent_id: AgentId(4),
        room_id: RoomId(5),
        in_transit: true,
        transit_target: Some(RoomId(7)),
        timestamp: Timestamp(4000),
        tick: Tick(200),
    };
    let json = serde_json::to_string(&pos).unwrap();
    let deserialized: PositionUpdate = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.agent_id, pos.agent_id);
    assert_eq!(deserialized.room_id, pos.room_id);
    assert_eq!(deserialized.in_transit, pos.in_transit);
    assert_eq!(deserialized.transit_target, pos.transit_target);

    // MoodUpdate
    let mood = MoodUpdate::new(
        AgentId(5),
        0.5,
        0.7,
        Emotion::Happy,
        Timestamp(5000),
        Tick(250),
    )
    .unwrap();
    let json = serde_json::to_string(&mood).unwrap();
    let deserialized: MoodUpdate = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.agent_id, mood.agent_id);
    assert!((deserialized.valence - mood.valence).abs() < f32::EPSILON);
    assert_eq!(deserialized.dominant_emotion, mood.dominant_emotion);

    // ChaosEvent
    let chaos = ChaosEvent {
        event_type: EventType::PrinterBroken,
        target_room: Some(RoomId(5)),
        target_agent: None,
        description: "Drucker kaputt".to_string(),
        duration_minutes: Some(30),
        timestamp: Timestamp(6000),
        tick: Tick(300),
    };
    let json = serde_json::to_string(&chaos).unwrap();
    let deserialized: ChaosEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.event_type, chaos.event_type);
    assert_eq!(deserialized.target_room, chaos.target_room);
    assert!(deserialized.target_agent.is_none());
    assert_eq!(deserialized.description, chaos.description);
}
