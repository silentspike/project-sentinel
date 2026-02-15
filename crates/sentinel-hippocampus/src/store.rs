//! Persistent storage for hippocampus memory data via redb.
//!
//! Separate database file (`hippocampus.redb`) from the main StateStore.
//! 4 tables: episodes, narratives, facts, cache_state.

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use crate::episode::Episode;
use crate::facts::FactStore;

// Table definitions — all &str keys with &[u8] values (JSON-serialized)
const EPISODES: TableDefinition<&str, &[u8]> = TableDefinition::new("episodes");
const NARRATIVES: TableDefinition<&str, &[u8]> = TableDefinition::new("narratives");
const FACTS: TableDefinition<&str, &[u8]> = TableDefinition::new("facts");
const CACHE_STATE: TableDefinition<&str, &[u8]> = TableDefinition::new("cache_state");

/// Persistent state for narrative memory (serializable for redb storage).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NarrativeState {
    pub agent_name: String,
    pub summary: String,
    pub episode_count: usize,
}

/// ACID KV-store for hippocampus memory persistence.
///
/// Each agent's episodes, narratives, facts, and cache state are stored
/// in separate redb tables with string keys and JSON-serialized values.
pub struct HippocampusStore {
    db: Database,
}

impl HippocampusStore {
    /// Open or create the hippocampus store at the given path.
    ///
    /// Creates all 4 tables if they don't exist.
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let db = Database::create(path)
            .map_err(|e| anyhow::anyhow!("Failed to create/open hippocampus.redb at {path}: {e}"))?;

        // Initialize all tables
        let write_txn = db.begin_write()?;
        {
            write_txn.open_table(EPISODES)?;
            write_txn.open_table(NARRATIVES)?;
            write_txn.open_table(FACTS)?;
            write_txn.open_table(CACHE_STATE)?;
        }
        write_txn.commit()?;

        Ok(Self { db })
    }

    // === EPISODES ===

    /// Store episodes for an agent (overwrites existing).
    pub fn store_episodes(&self, agent: &str, eps: &[Episode]) -> anyhow::Result<()> {
        let json = serde_json::to_vec(eps)?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(EPISODES)?;
            table.insert(agent, json.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Load episodes for an agent. Returns empty vec if none stored.
    pub fn load_episodes(&self, agent: &str) -> anyhow::Result<Vec<Episode>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(EPISODES)?;
        match table.get(agent)? {
            Some(guard) => {
                let bytes: &[u8] = guard.value();
                let episodes: Vec<Episode> = serde_json::from_slice(bytes)?;
                Ok(episodes)
            }
            None => Ok(Vec::new()),
        }
    }

    /// Append episodes to an agent's existing list.
    pub fn append_episodes(&self, agent: &str, new: &[Episode]) -> anyhow::Result<()> {
        let mut existing = self.load_episodes(agent)?;
        existing.extend_from_slice(new);
        self.store_episodes(agent, &existing)
    }

    /// Clear all episodes for an agent.
    pub fn clear_episodes(&self, agent: &str) -> anyhow::Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(EPISODES)?;
            table.remove(agent)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    // === NARRATIVES ===

    /// Store narrative state for an agent.
    pub fn store_narrative(&self, agent: &str, state: &NarrativeState) -> anyhow::Result<()> {
        let json = serde_json::to_vec(state)?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(NARRATIVES)?;
            table.insert(agent, json.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Load narrative state for an agent.
    pub fn load_narrative(&self, agent: &str) -> anyhow::Result<Option<NarrativeState>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(NARRATIVES)?;
        match table.get(agent)? {
            Some(guard) => {
                let bytes: &[u8] = guard.value();
                let state: NarrativeState = serde_json::from_slice(bytes)?;
                Ok(Some(state))
            }
            None => Ok(None),
        }
    }

    // === FACTS ===

    /// Store a fact by key.
    pub fn store_fact(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(FACTS)?;
            table.insert(key, value.as_bytes())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Load a fact by key.
    pub fn load_fact(&self, key: &str) -> anyhow::Result<Option<String>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(FACTS)?;
        match table.get(key)? {
            Some(guard) => {
                let bytes: &[u8] = guard.value();
                let value = std::str::from_utf8(bytes)?;
                Ok(Some(value.to_string()))
            }
            None => Ok(None),
        }
    }

    /// Delete a fact by key. Returns true if it existed.
    pub fn delete_fact(&self, key: &str) -> anyhow::Result<bool> {
        let write_txn = self.db.begin_write()?;
        let existed;
        {
            let mut table = write_txn.open_table(FACTS)?;
            existed = table.remove(key)?.is_some();
        }
        write_txn.commit()?;
        Ok(existed)
    }

    // === CACHE STATE ===

    /// Store cache state (hot/cold) for an agent.
    pub fn store_cache_state(&self, agent: &str, is_hot: bool) -> anyhow::Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(CACHE_STATE)?;
            table.insert(agent, &[is_hot as u8][..])?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Load cache state for an agent. None if never stored.
    pub fn load_cache_state(&self, agent: &str) -> anyhow::Result<Option<bool>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CACHE_STATE)?;
        match table.get(agent)? {
            Some(guard) => {
                let bytes: &[u8] = guard.value();
                Ok(Some(bytes.first().copied().unwrap_or(0) != 0))
            }
            None => Ok(None),
        }
    }

    // === UTILITY ===

    /// List all agents that have stored episodes.
    pub fn list_agents_with_episodes(&self) -> anyhow::Result<Vec<String>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(EPISODES)?;
        let mut agents = Vec::new();
        let iter = table.iter()?;
        for entry in iter {
            let (key, _): (redb::AccessGuard<'_, &str>, redb::AccessGuard<'_, &[u8]>) = entry?;
            agents.push(key.value().to_string());
        }
        Ok(agents)
    }
}

/// Persistent FactStore implementation backed by HippocampusStore.
pub struct RedbFactStore<'a> {
    store: &'a HippocampusStore,
}

impl<'a> RedbFactStore<'a> {
    pub fn new(store: &'a HippocampusStore) -> Self {
        Self { store }
    }
}

impl FactStore for RedbFactStore<'_> {
    fn get_fact(&self, key: &str) -> anyhow::Result<Option<String>> {
        self.store.load_fact(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (HippocampusStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-hippocampus.redb");
        let store = HippocampusStore::open(path.to_str().unwrap()).unwrap();
        (store, dir)
    }

    fn make_episode(id: u64, summary: &str) -> Episode {
        Episode {
            id,
            agent_name: "Thomas".to_string(),
            summary: summary.to_string(),
            relevance: 0.8,
            emotion: 0.7,
            repetitions: 1,
            hours_ago: 1.0,
            participants: vec!["Lisa".to_string()],
            tags: vec!["meeting".to_string()],
        }
    }

    #[test]
    fn test_episode_store_load_roundtrip() {
        let (store, _dir) = temp_store();
        let episodes = vec![
            make_episode(1, "Wichtiges Meeting"),
            make_episode(2, "Kundengespraech"),
        ];

        store.store_episodes("Thomas", &episodes).unwrap();
        let loaded = store.load_episodes("Thomas").unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].summary, "Wichtiges Meeting");
        assert_eq!(loaded[1].summary, "Kundengespraech");
        assert_eq!(loaded[0].participants, vec!["Lisa"]);
    }

    #[test]
    fn test_episode_append() {
        let (store, _dir) = temp_store();
        store
            .store_episodes("Thomas", &[make_episode(1, "Erstes")])
            .unwrap();
        store
            .append_episodes("Thomas", &[make_episode(2, "Zweites")])
            .unwrap();

        let loaded = store.load_episodes("Thomas").unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].summary, "Erstes");
        assert_eq!(loaded[1].summary, "Zweites");
    }

    #[test]
    fn test_episode_clear() {
        let (store, _dir) = temp_store();
        store
            .store_episodes("Thomas", &[make_episode(1, "Test")])
            .unwrap();
        store.clear_episodes("Thomas").unwrap();

        let loaded = store.load_episodes("Thomas").unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_load_nonexistent_episodes() {
        let (store, _dir) = temp_store();
        let loaded = store.load_episodes("Nobody").unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_fact_store_crud() {
        let (store, _dir) = temp_store();

        // Store
        store
            .store_fact("facts/projects/aurora", "Projekt Aurora: Webseite Redesign")
            .unwrap();

        // Load
        let fact = store.load_fact("facts/projects/aurora").unwrap();
        assert_eq!(fact.unwrap(), "Projekt Aurora: Webseite Redesign");

        // Load nonexistent
        assert!(store.load_fact("nonexistent").unwrap().is_none());

        // Delete
        assert!(store.delete_fact("facts/projects/aurora").unwrap());
        assert!(store.load_fact("facts/projects/aurora").unwrap().is_none());
        assert!(!store.delete_fact("facts/projects/aurora").unwrap());
    }

    #[test]
    fn test_redb_fact_store_trait() {
        let (store, _dir) = temp_store();
        store
            .store_fact("facts/hr/vacation", "30 Tage pro Jahr")
            .unwrap();

        let fact_store = RedbFactStore::new(&store);
        let result = fact_store.get_fact("facts/hr/vacation").unwrap();
        assert_eq!(result.unwrap(), "30 Tage pro Jahr");

        assert!(fact_store.get_fact("nonexistent").unwrap().is_none());
    }

    #[test]
    fn test_narrative_persistence() {
        let (store, _dir) = temp_store();
        let state = NarrativeState {
            agent_name: "Thomas".to_string(),
            summary: "- Wichtiges Meeting (Score: 0.56)".to_string(),
            episode_count: 3,
        };

        store.store_narrative("Thomas", &state).unwrap();
        let loaded = store.load_narrative("Thomas").unwrap().unwrap();

        assert_eq!(loaded.agent_name, "Thomas");
        assert!(loaded.summary.contains("Wichtiges Meeting"));
        assert_eq!(loaded.episode_count, 3);

        // Nonexistent
        assert!(store.load_narrative("Nobody").unwrap().is_none());
    }

    #[test]
    fn test_cache_state_persistence() {
        let (store, _dir) = temp_store();

        store.store_cache_state("Thomas", true).unwrap();
        assert_eq!(store.load_cache_state("Thomas").unwrap(), Some(true));

        store.store_cache_state("Thomas", false).unwrap();
        assert_eq!(store.load_cache_state("Thomas").unwrap(), Some(false));

        assert!(store.load_cache_state("Nobody").unwrap().is_none());
    }

    #[test]
    fn test_list_agents_with_episodes() {
        let (store, _dir) = temp_store();
        store
            .store_episodes("Thomas", &[make_episode(1, "A")])
            .unwrap();
        store
            .store_episodes("Lisa", &[make_episode(2, "B")])
            .unwrap();
        store
            .store_episodes("Andreas", &[make_episode(3, "C")])
            .unwrap();

        let mut agents = store.list_agents_with_episodes().unwrap();
        agents.sort();
        assert_eq!(agents, vec!["Andreas", "Lisa", "Thomas"]);
    }

    #[test]
    fn test_data_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("persist-test.redb");
        let path_str = path.to_str().unwrap();

        // Write data
        {
            let store = HippocampusStore::open(path_str).unwrap();
            store
                .store_episodes("Thomas", &[make_episode(1, "Survivor")])
                .unwrap();
            store
                .store_fact("facts/test", "Persistent Value")
                .unwrap();
            store
                .store_narrative(
                    "Thomas",
                    &NarrativeState {
                        agent_name: "Thomas".to_string(),
                        summary: "Survived".to_string(),
                        episode_count: 1,
                    },
                )
                .unwrap();
            store.store_cache_state("Thomas", true).unwrap();
        } // store dropped here

        // Reopen and verify
        {
            let store = HippocampusStore::open(path_str).unwrap();
            let episodes = store.load_episodes("Thomas").unwrap();
            assert_eq!(episodes.len(), 1);
            assert_eq!(episodes[0].summary, "Survivor");

            let fact = store.load_fact("facts/test").unwrap().unwrap();
            assert_eq!(fact, "Persistent Value");

            let narrative = store.load_narrative("Thomas").unwrap().unwrap();
            assert_eq!(narrative.summary, "Survived");

            assert_eq!(store.load_cache_state("Thomas").unwrap(), Some(true));
        }
    }
}
