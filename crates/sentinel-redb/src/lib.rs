//! redb ACID KV-store for hot agent state and relationships.

use redb::{Database, ReadableTable, TableDefinition};
use sentinel_common::{AgentId, RoomId};
use tracing::instrument;

/// Histogram bucket boundaries for redb operation latencies (microseconds).
const LATENCY_BUCKETS: &[f64] = &[10.0, 50.0, 100.0, 500.0, 1000.0, 5000.0];

// Table definitions - u16 keys for agent/room IDs, u32 for relationship pairs
const AGENT_STATE: TableDefinition<u16, &[u8]> = TableDefinition::new("agent_state");
const RELATIONSHIPS: TableDefinition<u32, &[u8]> = TableDefinition::new("relationships");
const PERSONALITY: TableDefinition<u16, &[u8]> = TableDefinition::new("personality");
const ROOM_STATE: TableDefinition<u16, &[u8]> = TableDefinition::new("room_state");

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
        let result = Ok(table.get(agent_id.0)?.map(|v| v.value().to_vec()));
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
            let (key, _) = entry?;
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
        let result = Ok(table.get(key)?.map(|v| v.value().to_vec()));
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
        let result = Ok(table.get(agent_id.0)?.map(|v| v.value().to_vec()));
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
        let result = Ok(table.get(room_id.0)?.map(|v| v.value().to_vec()));
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
    fn test_db_file_size() {
        let (store, dir) = temp_store();
        store.set_agent_state(agent(1), b"small").unwrap();
        let path = dir.path().join("test.redb");
        let size = std::fs::metadata(&path).unwrap().len();
        // redb 2.x mit 4 Tabellen benoetigt ~1.5MB CoW B-Tree Overhead
        assert!(
            size < 2_097_152,
            "DB should be <2MB initially, was {size} bytes"
        );
    }
}
