//! redb ACID KV-store for hot agent state and relationships.

use std::collections::HashMap;

use anyhow::Context;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use sentinel_common::{
    AgentId, FencedStore, OwnerRegistry, OwnerWriteGuard, RoomId, StateTransferScope,
};
use serde::{Deserialize, Serialize};
use tracing::instrument;

mod cluster_meta;
pub use cluster_meta::{
    ClusterMetaStore, InstallOutcome, OwnerSnapshotInstallMarker, OwnerSnapshotInstallStatus,
};

/// Histogram bucket boundaries for redb operation latencies (microseconds).
const LATENCY_BUCKETS: &[f64] = &[10.0, 50.0, 100.0, 500.0, 1000.0, 5000.0];

// Table definitions - u16 keys for agent/room IDs, u32 for relationship pairs
const AGENT_STATE: TableDefinition<u16, &[u8]> = TableDefinition::new("agent_state");
const RELATIONSHIPS: TableDefinition<u32, &[u8]> = TableDefinition::new("relationships");
const PERSONALITY: TableDefinition<u16, &[u8]> = TableDefinition::new("personality");
const ROOM_STATE: TableDefinition<u16, &[u8]> = TableDefinition::new("room_state");

// Evolution tables — written by Night-Run/Daemon consolidation, read by LLM Bridge
const VOICE_STYLE: TableDefinition<u16, &[u8]> = TableDefinition::new("voice_style");
const BEHAVIORAL_NOTES: TableDefinition<u16, &[u8]> = TableDefinition::new("behavioral_notes");
const NARRATIVE_SUMMARY: TableDefinition<u16, &[u8]> = TableDefinition::new("narrative_summary");
const EVOLUTION_VERSION: TableDefinition<u16, u64> = TableDefinition::new("evolution_version");
// NMDA scores from last consolidation — JSON-serialized Vec<f64>
const NMDA_SCORES: TableDefinition<u16, &[u8]> = TableDefinition::new("nmda_scores");

// Agent facts from hippocampus FactRetriever — JIT context injection for LLM Bridge
const AGENT_FACTS: TableDefinition<u16, &[u8]> = TableDefinition::new("agent_facts");

// Simulation metadata (sim_hour persistence, time virtualization)
const SIM_META: TableDefinition<&str, &[u8]> = TableDefinition::new("sim_meta");
const API_PATTERNS: TableDefinition<&str, &[u8]> = TableDefinition::new("api_patterns");
const API_PATTERNS_LEGACY_SNAPSHOT_KEY: &str = "snapshot";
const API_PATTERNS_META_SYNTH_COUNT_KEY: &str = "meta:synth_count";
const API_PATTERNS_META_EVOLUTION_PREFIX: &str = "meta:evolution:";
const API_PATTERNS_PATTERN_PREFIX: &str = "pattern:";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ApiCpPatternSnapshot {
    pub agent_id: String,
    pub fingerprint: String,
    pub count: usize,
    #[serde(default)]
    pub response_hashes: HashMap<u64, usize>,
    pub top_hash: u64,
    #[serde(default)]
    pub top_content: String,
    pub confidence: f64,
    pub last_seen: String,
    #[serde(default)]
    pub promoted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ApiCpSnapshot {
    #[serde(default)]
    pub patterns: Vec<ApiCpPatternSnapshot>,
    #[serde(default)]
    pub synth_count: i64,
    #[serde(default)]
    pub last_evolution_versions: HashMap<String, String>,
}

pub struct StateStore {
    db: Database,
}

impl StateStore {
    /// Open or create the state store at the given path.
    /// Creates all 4 tables if they don't exist.
    #[instrument(level = "debug", fields(path = %path))]
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let db = Database::create(path)
            .map_err(|e| anyhow::anyhow!("Failed to create/open redb at {path}: {e}"))?;

        // Initialize all tables
        let write_txn = db.begin_write()?;
        {
            write_txn.open_table(AGENT_STATE)?;
            write_txn.open_table(RELATIONSHIPS)?;
            write_txn.open_table(PERSONALITY)?;
            write_txn.open_table(ROOM_STATE)?;
            write_txn.open_table(VOICE_STYLE)?;
            write_txn.open_table(BEHAVIORAL_NOTES)?;
            write_txn.open_table(NARRATIVE_SUMMARY)?;
            write_txn.open_table(EVOLUTION_VERSION)?;
            write_txn.open_table(NMDA_SCORES)?;
            write_txn.open_table(AGENT_FACTS)?;
            write_txn.open_table(SIM_META)?;
            write_txn.open_table(API_PATTERNS)?;
        }
        write_txn.commit()?;

        Ok(Self { db })
    }

    // The single fenced write entry (#496 V3/V19) is `impl FencedStore for StateStore`
    // below: it re-checks the guard at begin *and* the returned `FencedRedbWrite`
    // re-checks again at commit (TOCTOU). Writers call `self.begin_fenced_write(..)`.

    // === AGENT STATE ===

    /// Get agent state by ID. Returns None if not found.
    #[instrument(skip(self), level = "trace", fields(agent_id = %agent_id))]
    pub fn get_agent_state(&self, agent_id: AgentId) -> anyhow::Result<Option<Vec<u8>>> {
        let start = std::time::Instant::now();
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(AGENT_STATE)?;
        let result = Ok(table
            .get(agent_id.0)?
            .map(|v: redb::AccessGuard<'_, &[u8]>| v.value().to_vec()));
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.redb.read.count").increment();
            reg.histogram("sentinel.redb.read.duration_us", LATENCY_BUCKETS)
                .observe(start.elapsed().as_micros() as f64);
        }
        result
    }

    /// Set agent state. Creates or overwrites.
    #[instrument(skip(self, state), level = "trace", fields(agent_id = %agent_id))]
    pub fn set_agent_state(&self, agent_id: AgentId, state: &[u8]) -> anyhow::Result<()> {
        let start = std::time::Instant::now();
        let write_txn = self.begin_fenced_write(
            &OwnerRegistry::global().issue(StateTransferScope::for_agent(agent_id.to_string()))?,
        )?;
        {
            let mut table = write_txn.open_table(AGENT_STATE)?;
            table.insert(agent_id.0, state)?;
        }
        write_txn.commit()?;
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.redb.write.count").increment();
            reg.histogram("sentinel.redb.write.duration_us", LATENCY_BUCKETS)
                .observe(start.elapsed().as_micros() as f64);
        }
        Ok(())
    }

    /// Batch set for many agent states in a single write transaction.
    ///
    /// This is the preferred hot-path API for per-tick persistence because it
    /// amortizes commit overhead across many entities.
    #[instrument(skip(self, entries), level = "trace", fields(batch_size = entries.len()))]
    pub fn set_agent_states_batch(&self, entries: &[(AgentId, Vec<u8>)]) -> anyhow::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        let start = std::time::Instant::now();
        let write_txn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        {
            let mut table = write_txn.open_table(AGENT_STATE)?;
            for (agent_id, state) in entries {
                table.insert(agent_id.0, state.as_slice())?;
            }
        }
        write_txn.commit()?;
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.redb.write_batch.count").increment();
            reg.histogram("sentinel.redb.write_batch.duration_us", LATENCY_BUCKETS)
                .observe(start.elapsed().as_micros() as f64);
        }
        Ok(())
    }

    /// Delete agent state. Returns true if existed.
    #[instrument(skip(self), level = "trace", fields(agent_id = %agent_id))]
    pub fn delete_agent_state(&self, agent_id: AgentId) -> anyhow::Result<bool> {
        let start = std::time::Instant::now();
        let write_txn = self.begin_fenced_write(
            &OwnerRegistry::global().issue(StateTransferScope::for_agent(agent_id.to_string()))?,
        )?;
        let existed;
        {
            let mut table = write_txn.open_table(AGENT_STATE)?;
            existed = table.remove(agent_id.0)?.is_some();
        }
        write_txn.commit()?;
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.redb.write.count").increment();
            reg.histogram("sentinel.redb.write.duration_us", LATENCY_BUCKETS)
                .observe(start.elapsed().as_micros() as f64);
        }
        Ok(existed)
    }

    /// List all stored agent IDs.
    #[instrument(skip(self), level = "trace")]
    pub fn list_agents(&self) -> anyhow::Result<Vec<AgentId>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(AGENT_STATE)?;
        let mut ids = Vec::new();
        let iter = table.iter()?;
        for entry in iter {
            let (key, _): (redb::AccessGuard<'_, u16>, redb::AccessGuard<'_, &[u8]>) = entry?;
            ids.push(AgentId(key.value()));
        }
        Ok(ids)
    }

    // === RELATIONSHIPS ===

    /// Get relationship between two agents. Key is automatically canonicalized.
    #[instrument(skip(self), level = "trace", fields(agent_a = %a, agent_b = %b))]
    pub fn get_relationship(&self, a: AgentId, b: AgentId) -> anyhow::Result<Option<Vec<u8>>> {
        let start = std::time::Instant::now();
        let key = relationship_key(a, b);
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(RELATIONSHIPS)?;
        let result = Ok(table
            .get(key)?
            .map(|v: redb::AccessGuard<'_, &[u8]>| v.value().to_vec()));
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.redb.read.count").increment();
            reg.histogram("sentinel.redb.read.duration_us", LATENCY_BUCKETS)
                .observe(start.elapsed().as_micros() as f64);
        }
        result
    }

    /// Set relationship data. Key is automatically canonicalized.
    #[instrument(skip(self, data), level = "trace", fields(agent_a = %a, agent_b = %b))]
    pub fn set_relationship(&self, a: AgentId, b: AgentId, data: &[u8]) -> anyhow::Result<()> {
        let start = std::time::Instant::now();
        let key = relationship_key(a, b);
        let write_txn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        {
            let mut table = write_txn.open_table(RELATIONSHIPS)?;
            table.insert(key, data)?;
        }
        write_txn.commit()?;
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.redb.write.count").increment();
            reg.histogram("sentinel.redb.write.duration_us", LATENCY_BUCKETS)
                .observe(start.elapsed().as_micros() as f64);
        }
        Ok(())
    }

    // === PERSONALITY ===

    /// Get personality profile by agent ID.
    #[instrument(skip(self), level = "trace", fields(agent_id = %agent_id))]
    pub fn get_personality(&self, agent_id: AgentId) -> anyhow::Result<Option<Vec<u8>>> {
        let start = std::time::Instant::now();
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PERSONALITY)?;
        let result = Ok(table
            .get(agent_id.0)?
            .map(|v: redb::AccessGuard<'_, &[u8]>| v.value().to_vec()));
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.redb.read.count").increment();
            reg.histogram("sentinel.redb.read.duration_us", LATENCY_BUCKETS)
                .observe(start.elapsed().as_micros() as f64);
        }
        result
    }

    /// Set personality profile.
    #[instrument(skip(self, data), level = "trace", fields(agent_id = %agent_id))]
    pub fn set_personality(&self, agent_id: AgentId, data: &[u8]) -> anyhow::Result<()> {
        let start = std::time::Instant::now();
        let write_txn = self.begin_fenced_write(
            &OwnerRegistry::global().issue(StateTransferScope::for_agent(agent_id.to_string()))?,
        )?;
        {
            let mut table = write_txn.open_table(PERSONALITY)?;
            table.insert(agent_id.0, data)?;
        }
        write_txn.commit()?;
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.redb.write.count").increment();
            reg.histogram("sentinel.redb.write.duration_us", LATENCY_BUCKETS)
                .observe(start.elapsed().as_micros() as f64);
        }
        Ok(())
    }

    // === ROOM STATE ===

    /// Get room state by room ID.
    #[instrument(skip(self), level = "trace", fields(room_id = %room_id))]
    pub fn get_room_state(&self, room_id: RoomId) -> anyhow::Result<Option<Vec<u8>>> {
        let start = std::time::Instant::now();
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ROOM_STATE)?;
        let result = Ok(table
            .get(room_id.0)?
            .map(|v: redb::AccessGuard<'_, &[u8]>| v.value().to_vec()));
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.redb.read.count").increment();
            reg.histogram("sentinel.redb.read.duration_us", LATENCY_BUCKETS)
                .observe(start.elapsed().as_micros() as f64);
        }
        result
    }

    /// Set room state.
    #[instrument(skip(self, data), level = "trace", fields(room_id = %room_id))]
    pub fn set_room_state(&self, room_id: RoomId, data: &[u8]) -> anyhow::Result<()> {
        let start = std::time::Instant::now();
        let write_txn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        {
            let mut table = write_txn.open_table(ROOM_STATE)?;
            table.insert(room_id.0, data)?;
        }
        write_txn.commit()?;
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.redb.write.count").increment();
            reg.histogram("sentinel.redb.write.duration_us", LATENCY_BUCKETS)
                .observe(start.elapsed().as_micros() as f64);
        }
        Ok(())
    }

    // === EVOLUTION (Personality Evolution from Night-Run/Daemon) ===

    /// Get voice style for an agent (JSON bytes from consolidation).
    pub fn get_voice_style(&self, agent_id: AgentId) -> anyhow::Result<Option<Vec<u8>>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(VOICE_STYLE)?;
        Ok(table
            .get(agent_id.0)?
            .map(|v: redb::AccessGuard<'_, &[u8]>| v.value().to_vec()))
    }

    /// Set voice style for an agent.
    pub fn set_voice_style(&self, agent_id: AgentId, data: &[u8]) -> anyhow::Result<()> {
        let write_txn = self.begin_fenced_write(
            &OwnerRegistry::global().issue(StateTransferScope::for_agent(agent_id.to_string()))?,
        )?;
        {
            let mut table = write_txn.open_table(VOICE_STYLE)?;
            table.insert(agent_id.0, data)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Get behavioral notes for an agent.
    pub fn get_behavioral_notes(&self, agent_id: AgentId) -> anyhow::Result<Option<Vec<u8>>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(BEHAVIORAL_NOTES)?;
        Ok(table
            .get(agent_id.0)?
            .map(|v: redb::AccessGuard<'_, &[u8]>| v.value().to_vec()))
    }

    /// Set behavioral notes for an agent.
    pub fn set_behavioral_notes(&self, agent_id: AgentId, data: &[u8]) -> anyhow::Result<()> {
        let write_txn = self.begin_fenced_write(
            &OwnerRegistry::global().issue(StateTransferScope::for_agent(agent_id.to_string()))?,
        )?;
        {
            let mut table = write_txn.open_table(BEHAVIORAL_NOTES)?;
            table.insert(agent_id.0, data)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Get narrative summary for an agent.
    pub fn get_narrative_summary(&self, agent_id: AgentId) -> anyhow::Result<Option<Vec<u8>>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(NARRATIVE_SUMMARY)?;
        Ok(table
            .get(agent_id.0)?
            .map(|v: redb::AccessGuard<'_, &[u8]>| v.value().to_vec()))
    }

    /// Set narrative summary for an agent.
    pub fn set_narrative_summary(&self, agent_id: AgentId, data: &[u8]) -> anyhow::Result<()> {
        let write_txn = self.begin_fenced_write(
            &OwnerRegistry::global().issue(StateTransferScope::for_agent(agent_id.to_string()))?,
        )?;
        {
            let mut table = write_txn.open_table(NARRATIVE_SUMMARY)?;
            table.insert(agent_id.0, data)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Get evolution version counter for an agent.
    pub fn get_evolution_version(&self, agent_id: AgentId) -> anyhow::Result<u64> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(EVOLUTION_VERSION)?;
        Ok(table.get(agent_id.0)?.map(|v| v.value()).unwrap_or(0))
    }

    /// Increment evolution version for an agent, returns the new version.
    pub fn increment_evolution_version(&self, agent_id: AgentId) -> anyhow::Result<u64> {
        let write_txn = self.begin_fenced_write(
            &OwnerRegistry::global().issue(StateTransferScope::for_agent(agent_id.to_string()))?,
        )?;
        let new_version;
        {
            let mut table = write_txn.open_table(EVOLUTION_VERSION)?;
            let current = table.get(agent_id.0)?.map(|v| v.value()).unwrap_or(0);
            new_version = current + 1;
            table.insert(agent_id.0, new_version)?;
        }
        write_txn.commit()?;
        Ok(new_version)
    }

    /// Batch write all evolution fields for an agent in a single transaction.
    pub fn set_evolution_batch(
        &self,
        agent_id: AgentId,
        voice_style: Option<&[u8]>,
        behavioral_notes: Option<&[u8]>,
        narrative_summary: Option<&[u8]>,
        agent_facts: Option<&[u8]>,
    ) -> anyhow::Result<u64> {
        let write_txn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let new_version;
        {
            if let Some(data) = voice_style {
                let mut table = write_txn.open_table(VOICE_STYLE)?;
                table.insert(agent_id.0, data)?;
            }
            if let Some(data) = behavioral_notes {
                let mut table = write_txn.open_table(BEHAVIORAL_NOTES)?;
                table.insert(agent_id.0, data)?;
            }
            if let Some(data) = narrative_summary {
                let mut table = write_txn.open_table(NARRATIVE_SUMMARY)?;
                table.insert(agent_id.0, data)?;
            }
            if let Some(data) = agent_facts {
                let mut table = write_txn.open_table(AGENT_FACTS)?;
                table.insert(agent_id.0, data)?;
            }
            let mut ver_table = write_txn.open_table(EVOLUTION_VERSION)?;
            let current = ver_table.get(agent_id.0)?.map(|v| v.value()).unwrap_or(0);
            new_version = current + 1;
            ver_table.insert(agent_id.0, new_version)?;
        }
        write_txn.commit()?;
        Ok(new_version)
    }

    // === AGENT FACTS ===

    /// Get agent facts (JSON bytes from FactRetriever bridge).
    pub fn get_agent_facts(&self, agent_id: AgentId) -> anyhow::Result<Option<Vec<u8>>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(AGENT_FACTS)?;
        Ok(table
            .get(agent_id.0)?
            .map(|v: redb::AccessGuard<'_, &[u8]>| v.value().to_vec()))
    }

    /// Set agent facts (JSON bytes).
    pub fn set_agent_facts(&self, agent_id: AgentId, data: &[u8]) -> anyhow::Result<()> {
        let write_txn = self.begin_fenced_write(
            &OwnerRegistry::global().issue(StateTransferScope::for_agent(agent_id.to_string()))?,
        )?;
        {
            let mut table = write_txn.open_table(AGENT_FACTS)?;
            table.insert(agent_id.0, data)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Store NMDA scores from consolidation for an agent.
    /// Scores are serialized as JSON array of f64.
    pub fn set_nmda_scores(&self, agent_id: AgentId, scores: &[f64]) -> anyhow::Result<()> {
        let json = serde_json::to_vec(scores)?;
        let write_txn = self.begin_fenced_write(
            &OwnerRegistry::global().issue(StateTransferScope::for_agent(agent_id.to_string()))?,
        )?;
        {
            let mut table = write_txn.open_table(NMDA_SCORES)?;
            table.insert(agent_id.0, json.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Get NMDA scores from last consolidation for an agent.
    pub fn get_nmda_scores(&self, agent_id: AgentId) -> anyhow::Result<Vec<f64>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(NMDA_SCORES)?;
        match table.get(agent_id.0)? {
            Some(guard) => Ok(serde_json::from_slice(guard.value())?),
            None => Ok(Vec::new()),
        }
    }

    // === SIMULATION METADATA ===

    /// Get persisted sim_hour. Returns None on first start.
    pub fn get_sim_hour(&self) -> anyhow::Result<Option<f32>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(SIM_META)?;
        match table.get("sim_hour")? {
            Some(guard) => {
                let bytes: &[u8] = guard.value();
                if bytes.len() == 4 {
                    let hour = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    if (0.0..24.0).contains(&hour) {
                        Ok(Some(hour))
                    } else {
                        Ok(None) // Corrupted value, treat as missing
                    }
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    /// Persist sim_hour for restart recovery.
    pub fn set_sim_hour(&self, hour: f32) -> anyhow::Result<()> {
        let write_txn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        {
            let mut table = write_txn.open_table(SIM_META)?;
            table.insert("sim_hour", hour.to_le_bytes().as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Batch set for room states in a single write transaction.
    #[instrument(skip(self, entries), level = "trace", fields(batch_size = entries.len()))]
    pub fn set_room_states_batch(&self, entries: &[(RoomId, Vec<u8>)]) -> anyhow::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        let start = std::time::Instant::now();
        let write_txn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        {
            let mut table = write_txn.open_table(ROOM_STATE)?;
            for (room_id, data) in entries {
                table.insert(room_id.0, data.as_slice())?;
            }
        }
        write_txn.commit()?;
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.redb.write_batch.count").increment();
            reg.histogram("sentinel.redb.write_batch.duration_us", LATENCY_BUCKETS)
                .observe(start.elapsed().as_micros() as f64);
        }
        Ok(())
    }

    // === API-CP PATTERNS ===

    /// Get the persisted API-CP snapshot JSON payload.
    ///
    /// Storage is structured per-pattern/per-meta entry inside `API_PATTERNS`.
    /// The JSON blob remains the transport shape for the gateway sync API.
    pub fn get_api_patterns_snapshot(&self) -> anyhow::Result<Option<Vec<u8>>> {
        let snapshot = self.load_api_patterns_state()?;
        if snapshot.patterns.is_empty()
            && snapshot.synth_count == 0
            && snapshot.last_evolution_versions.is_empty()
        {
            return Ok(None);
        }
        let data = serde_json::to_vec(&snapshot).context("API-CP Snapshot serialisieren")?;
        Ok(Some(data))
    }

    /// Persist the full API-CP snapshot JSON payload into structured `API_PATTERNS`
    /// entries instead of a single opaque blob.
    pub fn set_api_patterns_snapshot(&self, data: &[u8]) -> anyhow::Result<()> {
        let snapshot: ApiCpSnapshot =
            serde_json::from_slice(data).context("API-CP Snapshot deserialisieren")?;
        self.replace_api_patterns_state(&snapshot)
    }

    /// Load the daemon-owned API-CP state from structured `API_PATTERNS` keys.
    ///
    /// Falls back to the legacy `"snapshot"` blob if a deployment has not yet
    /// been rewritten to structured storage.
    pub fn load_api_patterns_state(&self) -> anyhow::Result<ApiCpSnapshot> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(API_PATTERNS)?;
        let mut snapshot = ApiCpSnapshot::default();
        let mut saw_structured_entries = false;
        let mut legacy_snapshot: Option<Vec<u8>> = None;

        for entry in table.iter()? {
            let (key, value) = entry?;
            let key = key.value();
            let value = value.value();
            match key {
                API_PATTERNS_LEGACY_SNAPSHOT_KEY => {
                    legacy_snapshot = Some(value.to_vec());
                }
                API_PATTERNS_META_SYNTH_COUNT_KEY => {
                    snapshot.synth_count = serde_json::from_slice(value)
                        .context("API-CP synth_count deserialisieren")?;
                    saw_structured_entries = true;
                }
                _ if key.starts_with(API_PATTERNS_META_EVOLUTION_PREFIX) => {
                    let agent_id = key
                        .trim_start_matches(API_PATTERNS_META_EVOLUTION_PREFIX)
                        .to_string();
                    let version = String::from_utf8(value.to_vec())
                        .context("API-CP evolution version ist nicht UTF-8")?;
                    snapshot.last_evolution_versions.insert(agent_id, version);
                    saw_structured_entries = true;
                }
                _ if key.starts_with(API_PATTERNS_PATTERN_PREFIX) => {
                    let pattern: ApiCpPatternSnapshot =
                        serde_json::from_slice(value).context("API-CP Pattern deserialisieren")?;
                    snapshot.patterns.push(pattern);
                    saw_structured_entries = true;
                }
                _ => {}
            }
        }

        if saw_structured_entries {
            snapshot.patterns.sort_by(|a, b| {
                if a.agent_id == b.agent_id {
                    a.fingerprint.cmp(&b.fingerprint)
                } else {
                    a.agent_id.cmp(&b.agent_id)
                }
            });
            return Ok(snapshot);
        }

        match legacy_snapshot {
            Some(data) => {
                serde_json::from_slice(&data).context("Legacy API-CP Snapshot deserialisieren")
            }
            None => Ok(ApiCpSnapshot::default()),
        }
    }

    /// Replace the full structured API-CP state atomically.
    pub fn replace_api_patterns_state(&self, snapshot: &ApiCpSnapshot) -> anyhow::Result<()> {
        let write_txn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        {
            let mut table = write_txn.open_table(API_PATTERNS)?;
            let keys: Vec<String> = table
                .iter()?
                .filter_map(|entry| entry.ok().map(|(k, _)| k.value().to_string()))
                .collect();
            for key in &keys {
                table.remove(key.as_str())?;
            }

            if snapshot.synth_count != 0 {
                let data = serde_json::to_vec(&snapshot.synth_count)
                    .context("API-CP synth_count serialisieren")?;
                table.insert(API_PATTERNS_META_SYNTH_COUNT_KEY, data.as_slice())?;
            }

            let mut evolution_entries: Vec<_> = snapshot.last_evolution_versions.iter().collect();
            evolution_entries.sort_by(|a, b| a.0.cmp(b.0));
            for (agent_id, version) in evolution_entries {
                let key = format!("{API_PATTERNS_META_EVOLUTION_PREFIX}{agent_id}");
                table.insert(key.as_str(), version.as_bytes())?;
            }

            let mut patterns = snapshot.patterns.clone();
            patterns.sort_by(|a, b| {
                if a.agent_id == b.agent_id {
                    a.fingerprint.cmp(&b.fingerprint)
                } else {
                    a.agent_id.cmp(&b.agent_id)
                }
            });
            for pattern in patterns {
                let key = api_pattern_key(&pattern.agent_id, &pattern.fingerprint)
                    .context("API-CP Pattern-Key serialisieren")?;
                let data = serde_json::to_vec(&pattern).context("API-CP Pattern serialisieren")?;
                table.insert(key.as_str(), data.as_slice())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    /// #497: dump exactly ONE agent's per-agent rows out of the shared StateStore.
    ///
    /// Never a whole-table dump (AC-2): each per-agent table is read by the agent's key only.
    /// Room/global tables (ROOM_STATE/SIM_META/API_PATTERNS) are deliberately excluded — they are
    /// World-scope, not container state (G0). `RELATIONSHIPS` is u32-keyed by `agent_id`,
    /// `EVOLUTION_VERSION` is u16->u64; the rest are u16->bytes.
    pub fn dump_agent_tables(
        &self,
        agent_id: AgentId,
    ) -> anyhow::Result<sentinel_common::NanoContainerRedbRows> {
        let txn = self.db.begin_read()?;
        let k = agent_id.0;
        let read_u16 = |def: TableDefinition<u16, &[u8]>| -> anyhow::Result<Option<Vec<u8>>> {
            Ok(txn.open_table(def)?.get(k)?.map(|v| v.value().to_vec()))
        };
        Ok(sentinel_common::NanoContainerRedbRows {
            agent_id: k,
            agent_state: read_u16(AGENT_STATE)?,
            personality: read_u16(PERSONALITY)?,
            relationships: txn
                .open_table(RELATIONSHIPS)?
                .get(k as u32)?
                .map(|v| v.value().to_vec()),
            voice_style: read_u16(VOICE_STYLE)?,
            behavioral_notes: read_u16(BEHAVIORAL_NOTES)?,
            narrative_summary: read_u16(NARRATIVE_SUMMARY)?,
            nmda_scores: read_u16(NMDA_SCORES)?,
            agent_facts: read_u16(AGENT_FACTS)?,
            evolution_version: txn
                .open_table(EVOLUTION_VERSION)?
                .get(k)?
                .map(|v| v.value()),
        })
    }

    /// Dumpt alle 12 Tables inklusive api_patterns in einer Read-Transaktion.
    pub fn dump_all_tables(&self) -> anyhow::Result<sentinel_common::RedbDump> {
        let txn = self.db.begin_read()?;
        Ok(sentinel_common::RedbDump {
            agent_states: Self::dump_u16_bytes(&txn, AGENT_STATE)?,
            room_states: Self::dump_u16_bytes(&txn, ROOM_STATE)?,
            personalities: Self::dump_u16_bytes(&txn, PERSONALITY)?,
            relationships: Self::dump_u32_bytes(&txn, RELATIONSHIPS)?,
            voice_styles: Self::dump_u16_bytes(&txn, VOICE_STYLE)?,
            behavioral_notes: Self::dump_u16_bytes(&txn, BEHAVIORAL_NOTES)?,
            narrative_summaries: Self::dump_u16_bytes(&txn, NARRATIVE_SUMMARY)?,
            evolution_versions: Self::dump_u16_u64(&txn, EVOLUTION_VERSION)?,
            nmda_scores: Self::dump_u16_bytes(&txn, NMDA_SCORES)?,
            agent_facts: Self::dump_u16_bytes(&txn, AGENT_FACTS)?,
            sim_meta: Self::dump_str_bytes(&txn, SIM_META)?,
            api_patterns: Self::dump_str_bytes(&txn, API_PATTERNS)?,
        })
    }

    /// Restored alle 12 Tables inklusive api_patterns aus einem Dump in einer atomaren Write-Transaktion.
    pub fn restore_all_tables(&self, dump: &sentinel_common::RedbDump) -> anyhow::Result<()> {
        let txn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        {
            Self::restore_u16_bytes(&txn, AGENT_STATE, &dump.agent_states)?;
            Self::restore_u16_bytes(&txn, ROOM_STATE, &dump.room_states)?;
            Self::restore_u16_bytes(&txn, PERSONALITY, &dump.personalities)?;
            Self::restore_u32_bytes(&txn, RELATIONSHIPS, &dump.relationships)?;
            Self::restore_u16_bytes(&txn, VOICE_STYLE, &dump.voice_styles)?;
            Self::restore_u16_bytes(&txn, BEHAVIORAL_NOTES, &dump.behavioral_notes)?;
            Self::restore_u16_bytes(&txn, NARRATIVE_SUMMARY, &dump.narrative_summaries)?;
            Self::restore_u16_u64(&txn, EVOLUTION_VERSION, &dump.evolution_versions)?;
            Self::restore_u16_bytes(&txn, NMDA_SCORES, &dump.nmda_scores)?;
            Self::restore_u16_bytes(&txn, AGENT_FACTS, &dump.agent_facts)?;
            Self::restore_str_bytes(&txn, SIM_META, &dump.sim_meta)?;
            Self::restore_str_bytes(&txn, API_PATTERNS, &dump.api_patterns)?;
        }
        txn.commit()?;
        Ok(())
    }

    // ── Dump Helpers ──

    fn dump_u16_bytes(
        txn: &redb::ReadTransaction,
        table_def: TableDefinition<u16, &[u8]>,
    ) -> anyhow::Result<Vec<(u16, Vec<u8>)>> {
        let table = txn.open_table(table_def)?;
        let mut entries = Vec::new();
        for entry in table.iter()? {
            let (k, v) = entry?;
            entries.push((k.value(), v.value().to_vec()));
        }
        Ok(entries)
    }

    fn dump_u32_bytes(
        txn: &redb::ReadTransaction,
        table_def: TableDefinition<u32, &[u8]>,
    ) -> anyhow::Result<Vec<(u32, Vec<u8>)>> {
        let table = txn.open_table(table_def)?;
        let mut entries = Vec::new();
        for entry in table.iter()? {
            let (k, v) = entry?;
            entries.push((k.value(), v.value().to_vec()));
        }
        Ok(entries)
    }

    fn dump_u16_u64(
        txn: &redb::ReadTransaction,
        table_def: TableDefinition<u16, u64>,
    ) -> anyhow::Result<Vec<(u16, u64)>> {
        let table = txn.open_table(table_def)?;
        let mut entries = Vec::new();
        for entry in table.iter()? {
            let (k, v) = entry?;
            entries.push((k.value(), v.value()));
        }
        Ok(entries)
    }

    fn dump_str_bytes(
        txn: &redb::ReadTransaction,
        table_def: TableDefinition<&str, &[u8]>,
    ) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
        let table = txn.open_table(table_def)?;
        let mut entries = Vec::new();
        for entry in table.iter()? {
            let (k, v) = entry?;
            entries.push((k.value().to_string(), v.value().to_vec()));
        }
        Ok(entries)
    }

    // ── Restore Helpers ──

    fn restore_u16_bytes(
        txn: &FencedRedbWrite,
        table_def: TableDefinition<u16, &[u8]>,
        entries: &[(u16, Vec<u8>)],
    ) -> anyhow::Result<()> {
        let mut table = txn.open_table(table_def)?;
        // Clear existing entries
        let keys: Vec<u16> = table
            .iter()?
            .filter_map(|e| e.ok().map(|(k, _)| k.value()))
            .collect();
        for key in keys {
            table.remove(key)?;
        }
        for (key, value) in entries {
            table.insert(*key, value.as_slice())?;
        }
        Ok(())
    }

    fn restore_u32_bytes(
        txn: &FencedRedbWrite,
        table_def: TableDefinition<u32, &[u8]>,
        entries: &[(u32, Vec<u8>)],
    ) -> anyhow::Result<()> {
        let mut table = txn.open_table(table_def)?;
        let keys: Vec<u32> = table
            .iter()?
            .filter_map(|e| e.ok().map(|(k, _)| k.value()))
            .collect();
        for key in keys {
            table.remove(key)?;
        }
        for (key, value) in entries {
            table.insert(*key, value.as_slice())?;
        }
        Ok(())
    }

    fn restore_u16_u64(
        txn: &FencedRedbWrite,
        table_def: TableDefinition<u16, u64>,
        entries: &[(u16, u64)],
    ) -> anyhow::Result<()> {
        let mut table = txn.open_table(table_def)?;
        let keys: Vec<u16> = table
            .iter()?
            .filter_map(|e| e.ok().map(|(k, _)| k.value()))
            .collect();
        for key in keys {
            table.remove(key)?;
        }
        for (key, value) in entries {
            table.insert(*key, *value)?;
        }
        Ok(())
    }

    fn restore_str_bytes(
        txn: &FencedRedbWrite,
        table_def: TableDefinition<&str, &[u8]>,
        entries: &[(String, Vec<u8>)],
    ) -> anyhow::Result<()> {
        let mut table = txn.open_table(table_def)?;
        let keys: Vec<String> = table
            .iter()?
            .filter_map(|e| e.ok().map(|(k, _)| k.value().to_string()))
            .collect();
        for key in &keys {
            table.remove(key.as_str())?;
        }
        for (key, value) in entries {
            table.insert(key.as_str(), value.as_slice())?;
        }
        Ok(())
    }
}

/// Build a canonical relationship key from two AgentIds.
/// Packs sorted IDs into a u32: `min_id << 16 | max_id`.
pub fn relationship_key(a: AgentId, b: AgentId) -> u32 {
    let min = a.0.min(b.0);
    let max = a.0.max(b.0);
    (min as u32) << 16 | max as u32
}

fn api_pattern_key(agent_id: &str, fingerprint: &str) -> anyhow::Result<String> {
    let suffix = serde_json::to_string(&(agent_id, fingerprint))?;
    Ok(format!("{API_PATTERNS_PATTERN_PREFIX}{suffix}"))
}

/// A fenced redb write transaction (#496 V19). The owner guard is re-checked at begin
/// (in `begin_fenced_write`) **and** again at [`commit`](FencedRedbWrite::commit), so a
/// write that became stale between the two — a cross-node handoff committed a newer
/// owner term while the transaction was open — is rejected at commit (the TOCTOU
/// window), not silently committed. Single-node the owner never changes mid-write, so
/// this always commits. The inner transaction is private, so a write cannot reach
/// `commit` without the re-check.
pub struct FencedRedbWrite {
    inner: redb::WriteTransaction,
    guard: OwnerWriteGuard,
}

impl FencedRedbWrite {
    /// Open a table in the fenced write transaction (delegates to the inner txn).
    pub fn open_table<K: redb::Key + 'static, V: redb::Value + 'static>(
        &self,
        definition: TableDefinition<K, V>,
    ) -> Result<redb::Table<'_, K, V>, redb::TableError> {
        self.inner.open_table(definition)
    }

    /// Commit the write after re-checking the owner term (V19 TOCTOU). A guard that
    /// went stale since begin is rejected with `StaleEpochError` and the write is
    /// dropped without committing.
    pub fn commit(self) -> anyhow::Result<()> {
        OwnerRegistry::global().validate(&self.guard)?;
        self.inner.commit()?;
        Ok(())
    }
}

impl FencedStore for StateStore {
    type Txn<'a> = FencedRedbWrite;

    fn begin_fenced_write(&self, guard: &OwnerWriteGuard) -> anyhow::Result<FencedRedbWrite> {
        // V19: re-check the guard at begin; the returned txn re-checks again at commit.
        OwnerRegistry::global().validate(guard)?;
        Ok(FencedRedbWrite {
            inner: self.db.begin_write()?,
            guard: guard.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (StateStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");
        let store = StateStore::open(path.to_str().unwrap()).unwrap();
        (store, dir)
    }

    /// #496 PR1b: the fenced write entry is the single choke point every redb
    /// writer routes through. In PR1b it is a behavior-preserving no-op fence that
    /// accepts any `OwnerWriteGuard` and yields the same `WriteTransaction` a raw
    /// writer used before, so a write through it must persist exactly as before.
    /// Both the `World` and a `NanoContainer(agent)` scope are accepted (PR2 adds
    /// the epoch check; PR1b must reject nothing).
    #[test]
    fn test_begin_fenced_write_is_behavior_preserving_choke_point() {
        let (store, _dir) = temp_store();

        // A routed writer (set_agent_state → begin_fenced_write) persists as before.
        store.set_agent_state(agent(7), b"state-bytes").unwrap();
        assert_eq!(
            store.get_agent_state(agent(7)).unwrap(),
            Some(b"state-bytes".to_vec())
        );

        // The choke point itself yields a usable, committable write transaction under
        // both scopes — single-node the registry owns every scope, so the guard
        // validates and the write is handed out.
        for scope in [
            StateTransferScope::World,
            StateTransferScope::NanoContainer("AGENT-07".to_string()),
        ] {
            let txn = store
                .begin_fenced_write(&OwnerRegistry::global().issue(scope).unwrap())
                .unwrap();
            {
                let mut table = txn.open_table(AGENT_STATE).unwrap();
                let v: &[u8] = b"via-fence";
                table.insert(agent(7).0, v).unwrap();
            }
            txn.commit().unwrap();
        }
        assert_eq!(
            store.get_agent_state(agent(7)).unwrap(),
            Some(b"via-fence".to_vec())
        );
    }

    /// #496 PR2b-1b (V19 TOCTOU): the fenced write re-checks the owner term at **commit**,
    /// not just at begin. Single-node the owner never changes mid-write, so this never
    /// fires in production; the test models the PR2b-2 case where a cross-node handoff
    /// commits a newer owner term while the transaction is open — `commit()` must reject
    /// the now-stale write instead of committing it.
    #[test]
    fn commit_rechecks_owner_term_and_rejects_stale() {
        let (store, _dir) = temp_store();

        // A write whose guard is stale by the time it commits (epoch 0 < the committed
        // single-node epoch 1). The write transaction is real and open, but commit must
        // re-validate and reject.
        let stale = super::FencedRedbWrite {
            inner: store.db.begin_write().unwrap(),
            guard: OwnerWriteGuard::for_test(
                StateTransferScope::World,
                OwnerRegistry::global().this_node(),
                0,
            ),
        };
        assert!(
            stale.commit().is_err(),
            "stale guard must be rejected at commit"
        );

        // A freshly registry-issued (current) guard commits.
        let ok = store
            .begin_fenced_write(
                &OwnerRegistry::global()
                    .issue(StateTransferScope::World)
                    .unwrap(),
            )
            .unwrap();
        ok.commit().unwrap();
    }

    fn agent(id: u16) -> AgentId {
        AgentId::new(id).unwrap()
    }

    fn room(id: u16) -> RoomId {
        RoomId::new(id).unwrap()
    }

    #[test]
    fn test_agent_state_crud() {
        let (store, _dir) = temp_store();

        // Initially empty
        assert!(store.get_agent_state(agent(1)).unwrap().is_none());

        // Write
        store.set_agent_state(agent(1), b"state-data").unwrap();

        // Read
        let data = store.get_agent_state(agent(1)).unwrap().unwrap();
        assert_eq!(data, b"state-data");

        // Overwrite
        store.set_agent_state(agent(1), b"new-state").unwrap();
        let data = store.get_agent_state(agent(1)).unwrap().unwrap();
        assert_eq!(data, b"new-state");

        // Delete
        assert!(store.delete_agent_state(agent(1)).unwrap());
        assert!(store.get_agent_state(agent(1)).unwrap().is_none());
        assert!(!store.delete_agent_state(agent(1)).unwrap()); // already deleted
    }

    /// #497 AC-2: dump_agent_tables reads exactly ONE agent's rows, never a whole-table dump.
    #[test]
    fn test_dump_agent_tables_is_per_agent_filtered() {
        let (store, _dir) = temp_store();

        store.set_agent_state(agent(1), b"a1-state").unwrap();
        store.set_agent_state(agent(2), b"a2-state").unwrap();

        let d1 = store.dump_agent_tables(agent(1)).unwrap();
        assert_eq!(d1.agent_id, 1);
        assert_eq!(d1.agent_state.as_deref(), Some(&b"a1-state"[..]));
        assert_ne!(
            d1.agent_state.as_deref(),
            Some(&b"a2-state"[..]),
            "agent 1's dump must not contain agent 2's row (per-agent filtered, not whole-table)"
        );

        let d2 = store.dump_agent_tables(agent(2)).unwrap();
        assert_eq!(d2.agent_id, 2);
        assert_eq!(d2.agent_state.as_deref(), Some(&b"a2-state"[..]));

        let d_unset = store.dump_agent_tables(agent(40)).unwrap();
        assert!(
            d_unset.agent_state.is_none()
                && d_unset.personality.is_none()
                && d_unset.relationships.is_none(),
            "an in-range agent with no rows yields all-None, not defaults"
        );
    }

    #[test]
    fn test_list_agents() {
        let (store, _dir) = temp_store();
        store.set_agent_state(agent(3), b"a").unwrap();
        store.set_agent_state(agent(7), b"b").unwrap();
        store.set_agent_state(agent(1), b"c").unwrap();

        let mut ids = store.list_agents().unwrap();
        ids.sort_by_key(|id| id.0);
        assert_eq!(ids, vec![AgentId(1), AgentId(3), AgentId(7)]);
    }

    #[test]
    fn test_concurrent_reads() {
        let (store, _dir) = temp_store();
        store.set_agent_state(agent(1), b"data").unwrap();

        // Multiple concurrent reads should work (MVCC)
        let r1 = store.get_agent_state(agent(1)).unwrap();
        let r2 = store.get_agent_state(agent(1)).unwrap();
        assert_eq!(r1, r2);
    }

    #[test]
    fn test_relationship_key_ordering() {
        // Both orderings produce same key
        assert_eq!(
            relationship_key(agent(3), agent(7)),
            relationship_key(agent(7), agent(3))
        );
        // Verify the packed format: min << 16 | max
        assert_eq!(relationship_key(agent(3), agent(7)), (3u32 << 16) | 7);
    }

    #[test]
    fn test_room_state() {
        let (store, _dir) = temp_store();
        store.set_room_state(room(1), b"temp:22.5").unwrap();
        let data = store.get_room_state(room(1)).unwrap().unwrap();
        assert_eq!(data, b"temp:22.5");
    }

    #[test]
    fn test_agent_state_batch_write() {
        let (store, _dir) = temp_store();
        let batch = vec![
            (agent(1), b"a".to_vec()),
            (agent(2), b"bb".to_vec()),
            (agent(3), b"ccc".to_vec()),
        ];
        store.set_agent_states_batch(&batch).unwrap();

        assert_eq!(store.get_agent_state(agent(1)).unwrap().unwrap(), b"a");
        assert_eq!(store.get_agent_state(agent(2)).unwrap().unwrap(), b"bb");
        assert_eq!(store.get_agent_state(agent(3)).unwrap().unwrap(), b"ccc");
    }

    #[test]
    fn test_evolution_voice_style() {
        let (store, _dir) = temp_store();
        assert!(store.get_voice_style(agent(1)).unwrap().is_none());
        store.set_voice_style(agent(1), b"formal, precise").unwrap();
        let data = store.get_voice_style(agent(1)).unwrap().unwrap();
        assert_eq!(data, b"formal, precise");
    }

    #[test]
    fn test_evolution_behavioral_notes() {
        let (store, _dir) = temp_store();
        store
            .set_behavioral_notes(agent(2), b"tends to interrupt")
            .unwrap();
        let data = store.get_behavioral_notes(agent(2)).unwrap().unwrap();
        assert_eq!(data, b"tends to interrupt");
    }

    #[test]
    fn test_evolution_narrative_summary() {
        let (store, _dir) = temp_store();
        store
            .set_narrative_summary(agent(3), b"had a productive meeting")
            .unwrap();
        let data = store.get_narrative_summary(agent(3)).unwrap().unwrap();
        assert_eq!(data, b"had a productive meeting");
    }

    #[test]
    fn test_evolution_version() {
        let (store, _dir) = temp_store();
        assert_eq!(store.get_evolution_version(agent(1)).unwrap(), 0);
        let v1 = store.increment_evolution_version(agent(1)).unwrap();
        assert_eq!(v1, 1);
        let v2 = store.increment_evolution_version(agent(1)).unwrap();
        assert_eq!(v2, 2);
        assert_eq!(store.get_evolution_version(agent(1)).unwrap(), 2);
    }

    #[test]
    fn test_evolution_batch() {
        let (store, _dir) = temp_store();
        let version = store
            .set_evolution_batch(
                agent(5),
                Some(b"casual tone"),
                Some(b"reliable worker"),
                Some(b"had a quiet shift"),
                None,
            )
            .unwrap();
        assert_eq!(version, 1);
        assert_eq!(
            store.get_voice_style(agent(5)).unwrap().unwrap(),
            b"casual tone"
        );
        assert_eq!(
            store.get_behavioral_notes(agent(5)).unwrap().unwrap(),
            b"reliable worker"
        );
        assert_eq!(
            store.get_narrative_summary(agent(5)).unwrap().unwrap(),
            b"had a quiet shift"
        );

        // Second batch increments version
        let v2 = store
            .set_evolution_batch(agent(5), Some(b"updated tone"), None, None, None)
            .unwrap();
        assert_eq!(v2, 2);
        assert_eq!(
            store.get_voice_style(agent(5)).unwrap().unwrap(),
            b"updated tone"
        );
        // Notes unchanged
        assert_eq!(
            store.get_behavioral_notes(agent(5)).unwrap().unwrap(),
            b"reliable worker"
        );
    }

    #[test]
    fn test_sim_hour_crud() {
        let (store, _dir) = temp_store();

        // Initially None
        assert!(store.get_sim_hour().unwrap().is_none());

        // Set
        store.set_sim_hour(14.5).unwrap();
        let hour = store.get_sim_hour().unwrap().unwrap();
        assert!((hour - 14.5).abs() < f32::EPSILON);

        // Overwrite
        store.set_sim_hour(22.75).unwrap();
        let hour = store.get_sim_hour().unwrap().unwrap();
        assert!((hour - 22.75).abs() < f32::EPSILON);
    }

    #[test]
    fn test_sim_hour_invalid_returns_none() {
        let (store, _dir) = temp_store();

        // Write an out-of-range value directly (simulating corruption)
        store.set_sim_hour(25.0).unwrap();
        // get_sim_hour validates range [0, 24) — corrupted returns None
        assert!(store.get_sim_hour().unwrap().is_none());

        // Negative value
        store.set_sim_hour(-1.0).unwrap();
        assert!(store.get_sim_hour().unwrap().is_none());
    }

    #[test]
    fn test_agent_facts_crud() {
        let (store, _dir) = temp_store();

        // Initially empty
        assert!(store.get_agent_facts(agent(1)).unwrap().is_none());

        // Write
        store
            .set_agent_facts(agent(1), b"[\"Projekt Aurora: Redesign\"]")
            .unwrap();
        let data = store.get_agent_facts(agent(1)).unwrap().unwrap();
        assert_eq!(data, b"[\"Projekt Aurora: Redesign\"]");

        // Overwrite
        store
            .set_agent_facts(agent(1), b"[\"Budget: 150k\"]")
            .unwrap();
        let data = store.get_agent_facts(agent(1)).unwrap().unwrap();
        assert_eq!(data, b"[\"Budget: 150k\"]");
    }

    #[test]
    fn test_evolution_batch_with_facts() {
        let (store, _dir) = temp_store();
        let version = store
            .set_evolution_batch(
                agent(8),
                Some(b"direct tone"),
                None,
                Some(b"productive day"),
                Some(b"[\"Sprint 12: Dashboard\"]"),
            )
            .unwrap();
        assert_eq!(version, 1);
        assert_eq!(
            store.get_voice_style(agent(8)).unwrap().unwrap(),
            b"direct tone"
        );
        assert_eq!(
            store.get_narrative_summary(agent(8)).unwrap().unwrap(),
            b"productive day"
        );
        assert_eq!(
            store.get_agent_facts(agent(8)).unwrap().unwrap(),
            b"[\"Sprint 12: Dashboard\"]"
        );
        // behavioral_notes was None — should remain empty
        assert!(store.get_behavioral_notes(agent(8)).unwrap().is_none());
    }

    #[test]
    fn test_api_patterns_snapshot_crud() {
        let (store, _dir) = temp_store();
        assert!(store.get_api_patterns_snapshot().unwrap().is_none());

        let snapshot = ApiCpSnapshot {
            patterns: vec![ApiCpPatternSnapshot {
                agent_id: "AGENT-01".to_string(),
                fingerprint: "fp1".to_string(),
                count: 3,
                response_hashes: HashMap::from([(42_u64, 3_usize)]),
                top_hash: 42,
                top_content: "ok".to_string(),
                confidence: 1.0,
                last_seen: "2026-03-29T12:00:00Z".to_string(),
                promoted: true,
            }],
            synth_count: 7,
            last_evolution_versions: HashMap::from([("AGENT-01".to_string(), "v2".to_string())]),
        };
        let data = serde_json::to_vec(&snapshot).unwrap();
        store.set_api_patterns_snapshot(&data).unwrap();

        let data = store.get_api_patterns_snapshot().unwrap().unwrap();
        let restored: ApiCpSnapshot = serde_json::from_slice(&data).unwrap();
        assert_eq!(restored, snapshot);

        let dump = store.dump_all_tables().unwrap();
        assert!(dump.api_patterns.len() >= 3);
        assert!(dump
            .api_patterns
            .iter()
            .all(|(key, _)| key != API_PATTERNS_LEGACY_SNAPSHOT_KEY));
    }

    #[test]
    fn test_dump_restore_includes_api_patterns() {
        let (store, _dir) = temp_store();
        let snapshot = ApiCpSnapshot {
            patterns: vec![ApiCpPatternSnapshot {
                agent_id: "AGENT-02".to_string(),
                fingerprint: "fp1".to_string(),
                count: 50,
                response_hashes: HashMap::from([(7_u64, 50_usize)]),
                top_hash: 7,
                top_content: "same".to_string(),
                confidence: 1.0,
                last_seen: "2026-03-29T12:30:00Z".to_string(),
                promoted: true,
            }],
            synth_count: 9,
            last_evolution_versions: HashMap::from([("AGENT-02".to_string(), "v3".to_string())]),
        };
        store
            .set_api_patterns_snapshot(&serde_json::to_vec(&snapshot).unwrap())
            .unwrap();

        let dump = store.dump_all_tables().unwrap();
        assert!(dump.api_patterns.len() >= 3);

        let (restored_store, _restored_dir) = temp_store();
        restored_store.restore_all_tables(&dump).unwrap();
        let data = restored_store.get_api_patterns_snapshot().unwrap().unwrap();
        let restored: ApiCpSnapshot = serde_json::from_slice(&data).unwrap();
        assert_eq!(restored, snapshot);
    }

    #[test]
    fn test_load_api_patterns_state_falls_back_to_legacy_snapshot_blob() {
        let (store, _dir) = temp_store();
        let write_txn = store.db.begin_write().unwrap();
        {
            let mut table = write_txn.open_table(API_PATTERNS).unwrap();
            table
                .insert(
                    API_PATTERNS_LEGACY_SNAPSHOT_KEY,
                    br#"{"patterns":[{"agent_id":"AGENT-09","fingerprint":"legacy","count":1,"top_hash":99,"confidence":1.0,"last_seen":"2026-03-29T13:00:00Z"}],"synth_count":4,"last_evolution_versions":{"AGENT-09":"v1"}}"#
                        .as_slice(),
                )
                .unwrap();
        }
        write_txn.commit().unwrap();

        let snapshot = store.load_api_patterns_state().unwrap();
        assert_eq!(snapshot.patterns.len(), 1);
        assert_eq!(snapshot.patterns[0].agent_id, "AGENT-09");
        assert_eq!(snapshot.patterns[0].fingerprint, "legacy");
        assert_eq!(snapshot.synth_count, 4);
        assert_eq!(
            snapshot.last_evolution_versions.get("AGENT-09"),
            Some(&"v1".to_string())
        );
    }

    #[test]
    fn test_db_file_size() {
        let (store, dir) = temp_store();
        store.set_agent_state(agent(1), b"small").unwrap();
        let path = dir.path().join("test.redb");
        let size = std::fs::metadata(&path).unwrap().len();
        // redb 3.x mit 8 Tabellen: ~2MB (CoW B-Tree Overhead)
        assert!(
            size < 4_194_304,
            "DB should be <4MB initially, was {size} bytes"
        );
    }
}
