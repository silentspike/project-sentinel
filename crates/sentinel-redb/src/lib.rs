//! redb ACID KV-store for hot agent state and relationships.

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use sentinel_common::{AgentId, RoomId};
use tracing::instrument;

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

// Simulation metadata (sim_hour persistence, time virtualization)
const SIM_META: TableDefinition<&str, &[u8]> = TableDefinition::new("sim_meta");

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
            write_txn.open_table(SIM_META)?;
        }
        write_txn.commit()?;

        Ok(Self { db })
    }

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
        let write_txn = self.db.begin_write()?;
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
        let write_txn = self.db.begin_write()?;
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
        let write_txn = self.db.begin_write()?;
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
        let write_txn = self.db.begin_write()?;
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
        let write_txn = self.db.begin_write()?;
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
        let write_txn = self.db.begin_write()?;
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
        let write_txn = self.db.begin_write()?;
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
        let write_txn = self.db.begin_write()?;
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
        let write_txn = self.db.begin_write()?;
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
        let write_txn = self.db.begin_write()?;
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
    ) -> anyhow::Result<u64> {
        let write_txn = self.db.begin_write()?;
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
            let mut ver_table = write_txn.open_table(EVOLUTION_VERSION)?;
            let current = ver_table.get(agent_id.0)?.map(|v| v.value()).unwrap_or(0);
            new_version = current + 1;
            ver_table.insert(agent_id.0, new_version)?;
        }
        write_txn.commit()?;
        Ok(new_version)
    }

    /// Store NMDA scores from consolidation for an agent.
    /// Scores are serialized as JSON array of f64.
    pub fn set_nmda_scores(&self, agent_id: AgentId, scores: &[f64]) -> anyhow::Result<()> {
        let json = serde_json::to_vec(scores)?;
        let write_txn = self.db.begin_write()?;
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
        let write_txn = self.db.begin_write()?;
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
        let write_txn = self.db.begin_write()?;
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
}

/// Build a canonical relationship key from two AgentIds.
/// Packs sorted IDs into a u32: `min_id << 16 | max_id`.
pub fn relationship_key(a: AgentId, b: AgentId) -> u32 {
    let min = a.0.min(b.0);
    let max = a.0.max(b.0);
    (min as u32) << 16 | max as u32
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
            .set_evolution_batch(agent(5), Some(b"updated tone"), None, None)
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
