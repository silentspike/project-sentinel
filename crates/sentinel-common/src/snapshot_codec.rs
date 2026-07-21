use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};

use crate::{EcsSnapshot, FsMetadataDump, RedbDump, SnapshotTier, WorldSnapshot};

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

/// Eingefrorene `EcsSnapshot`-Form VOR #491 (Schema v2): ohne `autonomy_cooldowns` und ohne die
/// vier Buffer-JSON-Felder. Wird ausschliesslich vom Decoder fuer die Rueckwaerts-Kompatibilitaet
/// genutzt — bincode liest positional, deshalb braucht jede fruehere Form ihre eigene Struktur.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EcsSnapshotV2 {
    positions: Vec<(u16, crate::components::Position)>,
    bio_states: Vec<(u16, crate::components::BioState)>,
    personalities: Vec<(u16, crate::components::Personality)>,
    moods: Vec<(u16, crate::components::Mood)>,
    perception_states: Vec<(u16, crate::components::PerceptionState)>,
    work_contexts: Vec<(u16, crate::components::WorkContext)>,
    agent_capabilities: Vec<(u16, crate::components::AgentCapabilities)>,
    event_queues: Vec<(u16, crate::components::EventQueue)>,
    identities: Vec<(u16, crate::components::AgentIdentity)>,
    shift_infos: Vec<(u16, crate::components::ShiftInfo)>,
    relationships: Vec<(u16, crate::components::Relationships)>,
    llm_configs: Vec<(u16, crate::components::LlmConfig)>,
    #[serde(default)]
    task_states: Vec<crate::components::TaskState>,
    sim_tick: u64,
    sim_hour: f32,
    sim_delta_seconds: f32,
    active_chaos_json: Vec<u8>,
    active_stimuli_json: Vec<u8>,
}

impl From<EcsSnapshotV2> for EcsSnapshot {
    fn from(ecs: EcsSnapshotV2) -> Self {
        Self {
            positions: ecs.positions,
            bio_states: ecs.bio_states,
            personalities: ecs.personalities,
            moods: ecs.moods,
            perception_states: ecs.perception_states,
            work_contexts: ecs.work_contexts,
            agent_capabilities: ecs.agent_capabilities,
            event_queues: ecs.event_queues,
            identities: ecs.identities,
            shift_infos: ecs.shift_infos,
            relationships: ecs.relationships,
            llm_configs: ecs.llm_configs,
            task_states: ecs.task_states,
            sim_tick: ecs.sim_tick,
            sim_hour: ecs.sim_hour,
            sim_delta_seconds: ecs.sim_delta_seconds,
            active_chaos_json: ecs.active_chaos_json,
            active_stimuli_json: ecs.active_stimuli_json,
            // v2-Snapshots kennen diese Felder nicht -> leer. Restore behandelt leer als "Default
            // belassen" (Vor-#491-Verhalten); Replay (#491 PR-B) verlangt ohnehin schema_version >= 3.
            autonomy_cooldowns: Vec::new(),
            smells_json: Vec::new(),
            room_chat_json: Vec::new(),
            gaia_json: Vec::new(),
            broadcast_json: Vec::new(),
        }
    }
}

/// World-Snapshot Schema v2: form before #491 (with `fs_metadata`, without v3 ECS fields).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorldSnapshotV2 {
    snapshot_id: String,
    schema_version: u32,
    tick: u64,
    sim_hour: f32,
    timestamp_ms: u64,
    tier: SnapshotTier,
    last_event_id: i64,
    redb: RedbDump,
    ecs: EcsSnapshotV2,
    projection_offsets: Vec<(String, i64)>,
    #[serde(default)]
    fs_metadata: Option<FsMetadataDump>,
}

/// World-Snapshot Schema v3: current ECS shape before runtime-owned Nano snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorldSnapshotV3 {
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
    #[serde(default)]
    fs_metadata: Option<FsMetadataDump>,
}

impl From<WorldSnapshotV3> for WorldSnapshot {
    fn from(snapshot: WorldSnapshotV3) -> Self {
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
            fs_metadata: snapshot.fs_metadata,
            nano_runtime_snapshots: Vec::new(),
        }
    }
}

impl From<WorldSnapshotV2> for WorldSnapshot {
    fn from(snapshot: WorldSnapshotV2) -> Self {
        Self {
            snapshot_id: snapshot.snapshot_id,
            schema_version: snapshot.schema_version,
            tick: snapshot.tick,
            sim_hour: snapshot.sim_hour,
            timestamp_ms: snapshot.timestamp_ms,
            tier: snapshot.tier,
            last_event_id: snapshot.last_event_id,
            redb: snapshot.redb,
            ecs: snapshot.ecs.into(),
            projection_offsets: snapshot.projection_offsets,
            fs_metadata: snapshot.fs_metadata,
            nano_runtime_snapshots: Vec::new(),
        }
    }
}

/// World-Snapshot Schema v1: aelteste Form, VOR `fs_metadata`.
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
    ecs: EcsSnapshotV2,
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
            ecs: snapshot.ecs.into(),
            projection_offsets: snapshot.projection_offsets,
            fs_metadata: None,
            nano_runtime_snapshots: Vec::new(),
        }
    }
}

pub fn decode_world_snapshot(bytes: &[u8]) -> anyhow::Result<WorldSnapshot> {
    // v4 (current): accept only when fully consumed AND schema_version == 4.
    // The schema-version guard prevents an older (v3/v2/v1) byte stream from being
    // incorrectly decoded as v4 through accidental bincode alignment. `schema_version` is
    // in the same position in every version, directly after the leading snapshot id.
    if let Ok((snapshot, consumed)) =
        bincode::serde::decode_from_slice::<WorldSnapshot, _>(bytes, legacy_config())
    {
        if consumed == bytes.len() && snapshot.schema_version == WorldSnapshot::SCHEMA_VERSION {
            return Ok(snapshot);
        }
    }

    // v3: current ECS snapshot shape, before NanoRuntime snapshots.
    if let Ok((snapshot, consumed)) =
        bincode::serde::decode_from_slice::<WorldSnapshotV3, _>(bytes, legacy_config())
    {
        if consumed == bytes.len() && snapshot.schema_version == 3 {
            return Ok(WorldSnapshot::from(snapshot));
        }
    }

    // v2: form before #491 (with fs_metadata, without v3 ECS fields).
    if let Ok((snapshot, consumed)) =
        bincode::serde::decode_from_slice::<WorldSnapshotV2, _>(bytes, legacy_config())
    {
        if consumed == bytes.len() {
            return Ok(WorldSnapshot::from(snapshot));
        }
    }

    // v1: aelteste Form vor fs_metadata.
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
                autonomy_cooldowns: vec![(3, 100), (7, 250)],
                smells_json: b"{\"smells\":{}}".to_vec(),
                room_chat_json: Vec::new(),
                gaia_json: Vec::new(),
                broadcast_json: Vec::new(),
            },
            projection_offsets: Vec::new(),
            fs_metadata: Some(FsMetadataDump::default()),
            nano_runtime_snapshots: Vec::new(),
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
            ecs: EcsSnapshotV2 {
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
        // v1 kennt die v3-ECS-Felder nicht -> muessen leer/Default sein.
        assert!(decoded.ecs.autonomy_cooldowns.is_empty());
        assert!(decoded.ecs.smells_json.is_empty());
    }

    fn ecs_v2_fixture(sim_tick: u64) -> EcsSnapshotV2 {
        EcsSnapshotV2 {
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
            sim_tick,
            sim_hour: 4.0,
            sim_delta_seconds: 1.0,
            active_chaos_json: b"{\"events\":{}}".to_vec(),
            active_stimuli_json: Vec::new(),
        }
    }

    fn redb_empty() -> RedbDump {
        RedbDump {
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
        }
    }

    #[test]
    fn roundtrip_preserves_v4_fields() {
        let snapshot = base_snapshot();
        let bytes = encode_world_snapshot(&snapshot).unwrap();
        let decoded = decode_world_snapshot(&bytes).unwrap();
        assert_eq!(decoded.schema_version, WorldSnapshot::SCHEMA_VERSION);
        assert_eq!(decoded.ecs.autonomy_cooldowns, vec![(3, 100), (7, 250)]);
        assert_eq!(decoded.ecs.smells_json, b"{\"smells\":{}}".to_vec());
        assert!(decoded.nano_runtime_snapshots.is_empty());
    }

    #[test]
    fn decode_falls_back_to_v3_snapshots() {
        let current = base_snapshot();
        let legacy = WorldSnapshotV3 {
            snapshot_id: "snap-v3".to_string(),
            schema_version: 3,
            tick: current.tick,
            sim_hour: current.sim_hour,
            timestamp_ms: current.timestamp_ms,
            tier: current.tier,
            last_event_id: current.last_event_id,
            redb: current.redb,
            ecs: current.ecs,
            projection_offsets: current.projection_offsets,
            fs_metadata: current.fs_metadata,
        };
        let bytes = bincode::serde::encode_to_vec(&legacy, legacy_config()).unwrap();
        let decoded = decode_world_snapshot(&bytes).unwrap();
        assert_eq!(decoded.snapshot_id, "snap-v3");
        assert_eq!(decoded.schema_version, 3);
        assert_eq!(decoded.ecs.autonomy_cooldowns, vec![(3, 100), (7, 250)]);
        assert!(decoded.nano_runtime_snapshots.is_empty());
    }

    #[test]
    fn decode_falls_back_to_v2_snapshots() {
        // v2 = form before #491 (with fs_metadata, without v3 ECS fields), schema_version == 2.
        let legacy = WorldSnapshotV2 {
            snapshot_id: "snap-v2".to_string(),
            schema_version: 2,
            tick: 21,
            sim_hour: 4.0,
            timestamp_ms: 88,
            tier: SnapshotTier::Hourly,
            last_event_id: 42,
            redb: redb_empty(),
            ecs: ecs_v2_fixture(21),
            projection_offsets: vec![("agent_live".to_string(), 42)],
            fs_metadata: Some(FsMetadataDump::default()),
        };
        let bytes = bincode::serde::encode_to_vec(&legacy, legacy_config()).unwrap();
        let decoded = decode_world_snapshot(&bytes).unwrap();
        // Must use the v2 branch (not be accepted as a newer schema): preserve old data and
        // initialize newer fields empty.
        assert_eq!(decoded.snapshot_id, "snap-v2");
        assert_eq!(decoded.schema_version, 2);
        assert_eq!(decoded.tick, 21);
        assert_eq!(decoded.ecs.sim_tick, 21);
        assert_eq!(decoded.ecs.active_chaos_json, b"{\"events\":{}}".to_vec());
        assert!(decoded.fs_metadata.is_some());
        assert!(decoded.ecs.autonomy_cooldowns.is_empty());
        assert!(decoded.ecs.room_chat_json.is_empty());
        assert_eq!(
            decoded.projection_offsets,
            vec![("agent_live".to_string(), 42)]
        );
    }
}
