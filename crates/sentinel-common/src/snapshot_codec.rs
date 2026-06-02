use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};

use crate::{EcsSnapshot, RedbDump, SnapshotTier, WorldSnapshot};

/// Bincode 2 in legacy mode keeps wire compatibility with the historic
/// `bincode::serialize` / `deserialize` snapshots from bincode 1.x.
fn legacy_config() -> impl bincode::config::Config {
    bincode::config::legacy()
}

pub fn encode_world_snapshot(snapshot: &WorldSnapshot) -> anyhow::Result<Vec<u8>> {
    bincode::serde::encode_to_vec(snapshot, legacy_config()).context("World Snapshot serialisieren")
}

/// Heap-free cursor subset of a world snapshot.
///
/// Kani verifies this small contract with the same bincode legacy config as
/// full world snapshots. The full `WorldSnapshot` codec remains covered by
/// unit tests because its `String`/`Vec` payload graph is too large for the
/// baseline solver budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotCursor {
    pub schema_version: u32,
    pub tick: u64,
    pub ecs_tick: u64,
    pub last_event_id: i64,
}

impl SnapshotCursor {
    pub fn from_snapshot(snapshot: &WorldSnapshot) -> Self {
        Self {
            schema_version: snapshot.schema_version,
            tick: snapshot.tick,
            ecs_tick: snapshot.ecs.sim_tick,
            last_event_id: snapshot.last_event_id,
        }
    }
}

pub fn encode_snapshot_cursor(cursor: SnapshotCursor) -> anyhow::Result<Vec<u8>> {
    bincode::serde::encode_to_vec(cursor, legacy_config()).context("Snapshot Cursor serialisieren")
}

pub fn decode_snapshot_cursor(bytes: &[u8]) -> anyhow::Result<SnapshotCursor> {
    let (cursor, consumed) =
        bincode::serde::decode_from_slice::<SnapshotCursor, _>(bytes, legacy_config())
            .context("Snapshot Cursor deserialisieren")?;
    if consumed != bytes.len() {
        return Err(anyhow!(
            "Snapshot Cursor enthaelt {} ungenutzte Bytes",
            bytes.len() - consumed
        ));
    }
    Ok(cursor)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorldSnapshotV1 {
    snapshot_id: String,
    schema_version: u32,
    tick: u64,
    sim_hour: f32,
    timestamp_ms: u64,
    tier: SnapshotTier,
    last_event_id: i64,
    redb: RedbDump,
    ecs: EcsSnapshot,
    projection_offsets: Vec<(String, i64)>,
}

impl From<WorldSnapshotV1> for WorldSnapshot {
    fn from(snapshot: WorldSnapshotV1) -> Self {
        Self {
            snapshot_id: snapshot.snapshot_id,
            schema_version: snapshot.schema_version,
            tick: snapshot.tick,
            sim_hour: snapshot.sim_hour,
            timestamp_ms: snapshot.timestamp_ms,
            tier: snapshot.tier,
            last_event_id: snapshot.last_event_id,
            redb: snapshot.redb,
            ecs: snapshot.ecs,
            projection_offsets: snapshot.projection_offsets,
            fs_metadata: None,
        }
    }
}

pub fn decode_world_snapshot(bytes: &[u8]) -> anyhow::Result<WorldSnapshot> {
    if let Ok((snapshot, consumed)) =
        bincode::serde::decode_from_slice::<WorldSnapshot, _>(bytes, legacy_config())
    {
        if consumed != bytes.len() {
            return Err(anyhow!(
                "World Snapshot enthaelt {} ungenutzte Bytes",
                bytes.len() - consumed
            ));
        }
        return Ok(snapshot);
    }

    let (snapshot, consumed) =
        bincode::serde::decode_from_slice::<WorldSnapshotV1, _>(bytes, legacy_config())
            .context("World Snapshot deserialisieren")?;
    if consumed != bytes.len() {
        return Err(anyhow!(
            "World Snapshot enthaelt {} ungenutzte Bytes",
            bytes.len() - consumed
        ));
    }
    Ok(WorldSnapshot::from(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FsMetadataDump;

    fn base_snapshot() -> WorldSnapshot {
        WorldSnapshot {
            snapshot_id: "snap-new".to_string(),
            schema_version: WorldSnapshot::SCHEMA_VERSION,
            tick: 42,
            sim_hour: 13.5,
            timestamp_ms: 1234,
            tier: SnapshotTier::Hourly,
            last_event_id: 99,
            redb: RedbDump {
                agent_states: Vec::new(),
                room_states: Vec::new(),
                personalities: Vec::new(),
                relationships: Vec::new(),
                voice_styles: Vec::new(),
                behavioral_notes: Vec::new(),
                narrative_summaries: Vec::new(),
                evolution_versions: Vec::new(),
                nmda_scores: Vec::new(),
                agent_facts: Vec::new(),
                sim_meta: Vec::new(),
                api_patterns: Vec::new(),
            },
            ecs: EcsSnapshot {
                positions: Vec::new(),
                bio_states: Vec::new(),
                personalities: Vec::new(),
                moods: Vec::new(),
                perception_states: Vec::new(),
                work_contexts: Vec::new(),
                agent_capabilities: Vec::new(),
                event_queues: Vec::new(),
                identities: Vec::new(),
                shift_infos: Vec::new(),
                relationships: Vec::new(),
                llm_configs: Vec::new(),
                task_states: Vec::new(),
                sim_tick: 42,
                sim_hour: 13.5,
                sim_delta_seconds: 1.0,
                active_chaos_json: Vec::new(),
                active_stimuli_json: Vec::new(),
            },
            projection_offsets: Vec::new(),
            fs_metadata: Some(FsMetadataDump::default()),
        }
    }

    #[test]
    fn roundtrip_preserves_fs_metadata() {
        let snapshot = base_snapshot();
        let bytes = encode_world_snapshot(&snapshot).unwrap();
        let decoded = decode_world_snapshot(&bytes).unwrap();
        assert!(decoded.fs_metadata.is_some());
        assert_eq!(decoded.schema_version, WorldSnapshot::SCHEMA_VERSION);
    }

    #[test]
    fn cursor_roundtrip_preserves_replay_fields() {
        let snapshot = base_snapshot();
        let cursor = SnapshotCursor::from_snapshot(&snapshot);
        let bytes = encode_snapshot_cursor(cursor).unwrap();
        let decoded = decode_snapshot_cursor(&bytes).unwrap();

        assert_eq!(decoded, cursor);
        assert_eq!(decoded.tick, snapshot.tick);
        assert_eq!(decoded.ecs_tick, snapshot.ecs.sim_tick);
        assert_eq!(decoded.last_event_id, snapshot.last_event_id);
    }

    #[test]
    fn decode_falls_back_to_v1_snapshots() {
        let legacy = WorldSnapshotV1 {
            snapshot_id: "snap-old".to_string(),
            schema_version: 1,
            tick: 7,
            sim_hour: 9.25,
            timestamp_ms: 55,
            tier: SnapshotTier::Daily,
            last_event_id: 11,
            redb: RedbDump {
                agent_states: Vec::new(),
                room_states: Vec::new(),
                personalities: Vec::new(),
                relationships: Vec::new(),
                voice_styles: Vec::new(),
                behavioral_notes: Vec::new(),
                narrative_summaries: Vec::new(),
                evolution_versions: Vec::new(),
                nmda_scores: Vec::new(),
                agent_facts: Vec::new(),
                sim_meta: Vec::new(),
                api_patterns: Vec::new(),
            },
            ecs: EcsSnapshot {
                positions: Vec::new(),
                bio_states: Vec::new(),
                personalities: Vec::new(),
                moods: Vec::new(),
                perception_states: Vec::new(),
                work_contexts: Vec::new(),
                agent_capabilities: Vec::new(),
                event_queues: Vec::new(),
                identities: Vec::new(),
                shift_infos: Vec::new(),
                relationships: Vec::new(),
                llm_configs: Vec::new(),
                task_states: Vec::new(),
                sim_tick: 7,
                sim_hour: 9.25,
                sim_delta_seconds: 1.0,
                active_chaos_json: Vec::new(),
                active_stimuli_json: Vec::new(),
            },
            projection_offsets: Vec::new(),
        };
        let bytes = bincode::serde::encode_to_vec(&legacy, legacy_config()).unwrap();
        let decoded = decode_world_snapshot(&bytes).unwrap();
        assert_eq!(decoded.snapshot_id, "snap-old");
        assert!(decoded.fs_metadata.is_none());
        assert_eq!(decoded.tick, 7);
    }
}
