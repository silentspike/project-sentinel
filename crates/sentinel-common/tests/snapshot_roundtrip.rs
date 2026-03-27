//! Bincode roundtrip test fuer WorldSnapshot.

use sentinel_common::components::*;
use sentinel_common::{EcsSnapshot, RedbDump, SnapshotTier, WorldSnapshot};

#[test]
fn world_snapshot_bincode_roundtrip() {
    let snapshot = WorldSnapshot {
        snapshot_id: "test-snapshot-001".to_string(),
        schema_version: WorldSnapshot::SCHEMA_VERSION,
        tick: 3600,
        sim_hour: 14.5,
        timestamp_ms: 1710000000000,
        tier: SnapshotTier::Hourly,
        last_event_id: 50000,
        redb: RedbDump {
            agent_states: vec![(1, vec![1, 2, 3]), (2, vec![4, 5, 6])],
            room_states: vec![(1, vec![10, 20])],
            personalities: vec![],
            relationships: vec![(65537, vec![99])],
            voice_styles: vec![],
            behavioral_notes: vec![],
            narrative_summaries: vec![],
            evolution_versions: vec![(1, 42)],
            nmda_scores: vec![],
            agent_facts: vec![],
            sim_meta: vec![("sim_hour".to_string(), vec![0, 0, 0, 0])],
        },
        ecs: EcsSnapshot {
            positions: vec![(
                1,
                Position {
                    room_id: "buero-dev-1".to_string(),
                    in_transit: false,
                    transit_target: None,
                    transit_remaining_ms: 0,
                    transit_correlation_id: None,
                    transit_route: Vec::new(),
                    transit_total_ms: 0,
                    transit_paused: false,
                    transit_source: None,
                },
            )],
            bio_states: vec![(
                1,
                BioState {
                    hunger: 30.0,
                    energy: 70.0,
                    caffeine_mg: 50.0,
                    bladder: 20.0,
                    stress: 10.0,
                    social_need: 40.0,
                    comfort: 80.0,
                },
            )],
            personalities: vec![],
            moods: vec![(
                1,
                Mood {
                    valence: 0.5,
                    arousal: 0.3,
                    dominant_emotion: sentinel_common::Emotion::Neutral,
                },
            )],
            perception_states: vec![],
            work_contexts: vec![],
            agent_capabilities: vec![],
            event_queues: vec![],
            identities: vec![],
            shift_infos: vec![],
            relationships: vec![],
            llm_configs: vec![],
            active_chaos_json: vec![],
            active_stimuli_json: vec![],
            sim_tick: 3600,
            sim_hour: 14.5,
            sim_delta_seconds: 1.0,
        },
        projection_offsets: vec![("room_live_view".to_string(), 49000)],
    };

    // Serialize
    let bytes = bincode::serialize(&snapshot).expect("bincode serialize failed");
    assert!(!bytes.is_empty(), "serialized snapshot must not be empty");

    // Deserialize
    let restored: WorldSnapshot = bincode::deserialize(&bytes).expect("bincode deserialize failed");

    // Verify fields
    assert_eq!(restored.snapshot_id, snapshot.snapshot_id);
    assert_eq!(restored.schema_version, WorldSnapshot::SCHEMA_VERSION);
    assert_eq!(restored.tick, 3600);
    assert_eq!(restored.tier, SnapshotTier::Hourly);
    assert_eq!(restored.last_event_id, 50000);
    assert_eq!(restored.redb.agent_states.len(), 2);
    assert_eq!(restored.redb.evolution_versions[0], (1, 42));
    assert_eq!(restored.ecs.positions.len(), 1);
    assert_eq!(restored.ecs.positions[0].1.room_id, "buero-dev-1");
    assert_eq!(restored.ecs.bio_states[0].1.hunger, 30.0);
    assert_eq!(restored.ecs.moods[0].1.valence, 0.5);
    assert_eq!(restored.projection_offsets.len(), 1);
}

#[test]
fn snapshot_tier_display() {
    assert_eq!(SnapshotTier::Hourly.to_string(), "hourly");
    assert_eq!(SnapshotTier::Daily.to_string(), "daily");
    assert_eq!(SnapshotTier::Weekly.to_string(), "weekly");
    assert_eq!(SnapshotTier::Monthly.to_string(), "monthly");
}

#[test]
fn empty_world_snapshot_roundtrip() {
    let snapshot = WorldSnapshot {
        snapshot_id: "empty".to_string(),
        schema_version: WorldSnapshot::SCHEMA_VERSION,
        tick: 0,
        sim_hour: 0.0,
        timestamp_ms: 0,
        tier: SnapshotTier::Hourly,
        last_event_id: 0,
        redb: RedbDump {
            agent_states: vec![],
            room_states: vec![],
            personalities: vec![],
            relationships: vec![],
            voice_styles: vec![],
            behavioral_notes: vec![],
            narrative_summaries: vec![],
            evolution_versions: vec![],
            nmda_scores: vec![],
            agent_facts: vec![],
            sim_meta: vec![],
        },
        ecs: EcsSnapshot {
            positions: vec![],
            bio_states: vec![],
            personalities: vec![],
            moods: vec![],
            perception_states: vec![],
            work_contexts: vec![],
            agent_capabilities: vec![],
            event_queues: vec![],
            identities: vec![],
            shift_infos: vec![],
            relationships: vec![],
            llm_configs: vec![],
            active_chaos_json: vec![],
            active_stimuli_json: vec![],
            sim_tick: 0,
            sim_hour: 0.0,
            sim_delta_seconds: 1.0,
        },
        projection_offsets: vec![],
    };

    let bytes = bincode::serialize(&snapshot).unwrap();
    let restored: WorldSnapshot = bincode::deserialize(&bytes).unwrap();
    assert_eq!(restored.snapshot_id, "empty");
    assert_eq!(restored.ecs.positions.len(), 0);
}
