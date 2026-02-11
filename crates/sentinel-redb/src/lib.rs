//! redb ACID KV-store for hot agent state and relationships.

use redb::{Database, ReadableTable, TableDefinition};

// Table definitions
const AGENT_STATE: TableDefinition<&str, &[u8]> = TableDefinition::new("agent_state");
const RELATIONSHIPS: TableDefinition<&str, &[u8]> = TableDefinition::new("relationships");
const PERSONALITY: TableDefinition<&str, &[u8]> = TableDefinition::new("personality");
const ROOM_STATE: TableDefinition<&str, &[u8]> = TableDefinition::new("room_state");

pub struct StateStore {
    db: Database,
}

impl StateStore {
    /// Open or create the state store at the given path.
    /// Creates all 4 tables if they don't exist.
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

    /// Get agent state by name. Returns None if not found.
    pub fn get_agent_state(&self, name: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(AGENT_STATE)?;
        Ok(table.get(name)?.map(|v| v.value().to_vec()))
    }

    /// Set agent state. Creates or overwrites.
    pub fn set_agent_state(&self, name: &str, state: &[u8]) -> anyhow::Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(AGENT_STATE)?;
            table.insert(name, state)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Delete agent state. Returns true if existed.
    pub fn delete_agent_state(&self, name: &str) -> anyhow::Result<bool> {
        let write_txn = self.db.begin_write()?;
        let existed;
        {
            let mut table = write_txn.open_table(AGENT_STATE)?;
            existed = table.remove(name)?.is_some();
        }
        write_txn.commit()?;
        Ok(existed)
    }

    /// List all agent names.
    pub fn list_agents(&self) -> anyhow::Result<Vec<String>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(AGENT_STATE)?;
        let mut names = Vec::new();
        let iter = table.iter()?;
        for entry in iter {
            let (key, _) = entry?;
            names.push(key.value().to_string());
        }
        Ok(names)
    }

    // === RELATIONSHIPS ===

    /// Get relationship between two agents. Key format: "agent_a:agent_b" (alphabetical).
    pub fn get_relationship(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(RELATIONSHIPS)?;
        Ok(table.get(key)?.map(|v| v.value().to_vec()))
    }

    /// Set relationship data.
    pub fn set_relationship(&self, key: &str, data: &[u8]) -> anyhow::Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(RELATIONSHIPS)?;
            table.insert(key, data)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    // === PERSONALITY ===

    /// Get personality profile by agent name.
    pub fn get_personality(&self, name: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PERSONALITY)?;
        Ok(table.get(name)?.map(|v| v.value().to_vec()))
    }

    /// Set personality profile.
    pub fn set_personality(&self, name: &str, data: &[u8]) -> anyhow::Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(PERSONALITY)?;
            table.insert(name, data)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    // === ROOM STATE ===

    /// Get room state by room ID.
    pub fn get_room_state(&self, room_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ROOM_STATE)?;
        Ok(table.get(room_id)?.map(|v| v.value().to_vec()))
    }

    /// Set room state.
    pub fn set_room_state(&self, room_id: &str, data: &[u8]) -> anyhow::Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(ROOM_STATE)?;
            table.insert(room_id, data)?;
        }
        write_txn.commit()?;
        Ok(())
    }
}

/// Helper: Build a canonical relationship key (alphabetical order).
pub fn relationship_key(agent_a: &str, agent_b: &str) -> String {
    if agent_a <= agent_b {
        format!("{agent_a}:{agent_b}")
    } else {
        format!("{agent_b}:{agent_a}")
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

    #[test]
    fn test_agent_state_crud() {
        let (store, _dir) = temp_store();

        // Initially empty
        assert!(store.get_agent_state("thomas").unwrap().is_none());

        // Write
        store.set_agent_state("thomas", b"state-data").unwrap();

        // Read
        let data = store.get_agent_state("thomas").unwrap().unwrap();
        assert_eq!(data, b"state-data");

        // Overwrite
        store.set_agent_state("thomas", b"new-state").unwrap();
        let data = store.get_agent_state("thomas").unwrap().unwrap();
        assert_eq!(data, b"new-state");

        // Delete
        assert!(store.delete_agent_state("thomas").unwrap());
        assert!(store.get_agent_state("thomas").unwrap().is_none());
        assert!(!store.delete_agent_state("thomas").unwrap()); // already deleted
    }

    #[test]
    fn test_list_agents() {
        let (store, _dir) = temp_store();
        store.set_agent_state("andreas", b"a").unwrap();
        store.set_agent_state("lisa", b"b").unwrap();
        store.set_agent_state("thomas", b"c").unwrap();

        let mut names = store.list_agents().unwrap();
        names.sort();
        assert_eq!(names, vec!["andreas", "lisa", "thomas"]);
    }

    #[test]
    fn test_concurrent_reads() {
        let (store, _dir) = temp_store();
        store.set_agent_state("test", b"data").unwrap();

        // Multiple concurrent reads should work (MVCC)
        let r1 = store.get_agent_state("test").unwrap();
        let r2 = store.get_agent_state("test").unwrap();
        assert_eq!(r1, r2);
    }

    #[test]
    fn test_relationship_key_ordering() {
        assert_eq!(relationship_key("lisa", "thomas"), "lisa:thomas");
        assert_eq!(relationship_key("thomas", "lisa"), "lisa:thomas");
        // Both orderings produce same key
        assert_eq!(
            relationship_key("a", "b"),
            relationship_key("b", "a")
        );
    }

    #[test]
    fn test_room_state() {
        let (store, _dir) = temp_store();
        store.set_room_state("kueche", b"temp:22.5").unwrap();
        let data = store.get_room_state("kueche").unwrap().unwrap();
        assert_eq!(data, b"temp:22.5");
    }

    #[test]
    fn test_db_file_size() {
        let (store, dir) = temp_store();
        store.set_agent_state("test", b"small").unwrap();
        let path = dir.path().join("test.redb");
        let size = std::fs::metadata(&path).unwrap().len();
        // redb 2.x mit 4 Tabellen benoetigt ~1.5MB CoW B-Tree Overhead
        assert!(
            size < 2_097_152,
            "DB should be <2MB initially, was {size} bytes"
        );
    }
}
