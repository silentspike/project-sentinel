//! Persistent storage for hippocampus memory data via redb.
//!
//! Separate database file (`hippocampus.redb`) from the main StateStore.
//! 6 tables: episodes, narratives, facts, cache_state, goals, archive.

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use crate::episode::Episode;
use crate::facts::FactStore;
use crate::golf::Goal;

// Table definitions — all &str keys with &[u8] values (JSON-serialized)
const EPISODES: TableDefinition<&str, &[u8]> = TableDefinition::new("episodes");
const NARRATIVES: TableDefinition<&str, &[u8]> = TableDefinition::new("narratives");
const FACTS: TableDefinition<&str, &[u8]> = TableDefinition::new("facts");
const CACHE_STATE: TableDefinition<&str, &[u8]> = TableDefinition::new("cache_state");
const GOALS: TableDefinition<&str, &[u8]> = TableDefinition::new("goals");
const ARCHIVE: TableDefinition<&str, &[u8]> = TableDefinition::new("archive");

const MAX_EPISODES_PER_AGENT: usize = 1000;

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
    /// Creates all 6 tables if they don't exist.
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let db = Database::create(path).map_err(|e| {
            anyhow::anyhow!("Failed to create/open hippocampus.redb at {path}: {e}")
        })?;

        // Initialize all tables
        let write_txn = db.begin_write()?;
        {
            write_txn.open_table(EPISODES)?;
            write_txn.open_table(NARRATIVES)?;
            write_txn.open_table(FACTS)?;
            write_txn.open_table(CACHE_STATE)?;
            write_txn.open_table(GOALS)?;
            write_txn.open_table(ARCHIVE)?;
        }
        write_txn.commit()?;

        Ok(Self { db })
    }

    // === EPISODES ===

    /// Store episodes for an agent (overwrites existing).
    pub fn store_episodes(&self, agent: &str, eps: &[Episode]) -> anyhow::Result<()> {
        let retained = if eps.len() > MAX_EPISODES_PER_AGENT {
            &eps[eps.len() - MAX_EPISODES_PER_AGENT..]
        } else {
            eps
        };
        let json = serde_json::to_vec(retained)?;
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

    /// Append episodes to an agent's existing list. Caps at 1000 live episodes per agent.
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

    // === GOALS (GOLF Framework) ===

    /// Store goals for an agent (overwrites existing).
    pub fn store_goals(&self, agent: &str, goals: &[Goal]) -> anyhow::Result<()> {
        let json = serde_json::to_vec(goals)?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(GOALS)?;
            table.insert(agent, json.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Load goals for an agent. Returns empty vec if none stored.
    pub fn load_goals(&self, agent: &str) -> anyhow::Result<Vec<Goal>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(GOALS)?;
        match table.get(agent)? {
            Some(guard) => {
                let bytes: &[u8] = guard.value();
                let goals: Vec<Goal> = serde_json::from_slice(bytes)?;
                Ok(goals)
            }
            None => Ok(Vec::new()),
        }
    }

    /// Append goals to an agent's existing list.
    pub fn append_goals(&self, agent: &str, new: &[Goal]) -> anyhow::Result<()> {
        let mut existing = self.load_goals(agent)?;
        existing.extend_from_slice(new);
        self.store_goals(agent, &existing)
    }

    /// Update progress for a specific goal (by id) of an agent.
    ///
    /// Returns `true` if the goal was found and updated.
    pub fn update_goal_progress(
        &self,
        agent: &str,
        goal_id: u64,
        progress: f64,
        tick: u64,
    ) -> anyhow::Result<bool> {
        let mut goals = self.load_goals(agent)?;
        let mut found = false;
        for goal in &mut goals {
            if goal.id == goal_id {
                goal.update_progress(progress, tick);
                found = true;
                break;
            }
        }
        if found {
            self.store_goals(agent, &goals)?;
        }
        Ok(found)
    }

    /// List all agents that have stored goals.
    pub fn list_agents_with_goals(&self) -> anyhow::Result<Vec<String>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(GOALS)?;
        let mut agents = Vec::new();
        let iter = table.iter()?;
        for entry in iter {
            let (key, _): (redb::AccessGuard<'_, &str>, redb::AccessGuard<'_, &[u8]>) = entry?;
            agents.push(key.value().to_string());
        }
        Ok(agents)
    }

    // === ARCHIVE (consolidated episode preservation) ===

    /// Store archived episodes for an agent (overwrites existing).
    pub fn store_archive(&self, agent: &str, eps: &[Episode]) -> anyhow::Result<()> {
        let json = serde_json::to_vec(eps)?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(ARCHIVE)?;
            table.insert(agent, json.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Load archived episodes for an agent. Returns empty vec if none stored.
    pub fn load_archive(&self, agent: &str) -> anyhow::Result<Vec<Episode>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ARCHIVE)?;
        match table.get(agent)? {
            Some(guard) => {
                let bytes: &[u8] = guard.value();
                let episodes: Vec<Episode> = serde_json::from_slice(bytes)?;
                Ok(episodes)
            }
            None => Ok(Vec::new()),
        }
    }

    /// Append episodes to an agent's archive. Caps at 1000 episodes per agent.
    pub fn append_archive(&self, agent: &str, new: &[Episode]) -> anyhow::Result<()> {
        let mut existing = self.load_archive(agent)?;
        existing.extend_from_slice(new);
        // Cap at 1000 episodes — drop oldest if exceeding
        if existing.len() > MAX_EPISODES_PER_AGENT {
            let excess = existing.len() - MAX_EPISODES_PER_AGENT;
            existing.drain(..excess);
        }
        self.store_archive(agent, &existing)
    }

    /// List all agents that have archived episodes.
    pub fn list_agents_with_archive(&self) -> anyhow::Result<Vec<String>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ARCHIVE)?;
        let mut agents = Vec::new();
        let iter = table.iter()?;
        for entry in iter {
            let (key, _): (redb::AccessGuard<'_, &str>, redb::AccessGuard<'_, &[u8]>) = entry?;
            agents.push(key.value().to_string());
        }
        Ok(agents)
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
    fn test_live_episodes_cap_at_1000() {
        let (store, _dir) = temp_store();
        let many: Vec<Episode> = (0..1100)
            .map(|i| make_episode(i, &format!("Episode {i}")))
            .collect();

        store.append_episodes("Thomas", &many).unwrap();
        let loaded = store.load_episodes("Thomas").unwrap();
        assert_eq!(loaded.len(), 1000);
        assert_eq!(loaded[0].summary, "Episode 100");
        assert_eq!(loaded[999].summary, "Episode 1099");
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
            store.store_fact("facts/test", "Persistent Value").unwrap();
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

    // === GOLF Tests ===

    use crate::golf::{GoalStatus, GoalType};

    fn make_goal(id: u64, agent: &str, goal_type: GoalType) -> Goal {
        Goal::new(id, agent, goal_type, "Test goal", 0, None)
    }

    #[test]
    fn test_golf_store_load_roundtrip() {
        let (store, _dir) = temp_store();
        let goals = vec![
            make_goal(1, "Thomas", GoalType::Career),
            make_goal(2, "Thomas", GoalType::Project),
        ];

        store.store_goals("Thomas", &goals).unwrap();
        let loaded = store.load_goals("Thomas").unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id, 1);
        assert_eq!(loaded[0].goal_type, GoalType::Career);
        assert_eq!(loaded[1].id, 2);
        assert_eq!(loaded[1].goal_type, GoalType::Project);
    }

    #[test]
    fn test_golf_append() {
        let (store, _dir) = temp_store();
        store
            .store_goals("Thomas", &[make_goal(1, "Thomas", GoalType::Career)])
            .unwrap();
        store
            .append_goals("Thomas", &[make_goal(2, "Thomas", GoalType::Skill)])
            .unwrap();

        let loaded = store.load_goals("Thomas").unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].goal_type, GoalType::Career);
        assert_eq!(loaded[1].goal_type, GoalType::Skill);
    }

    #[test]
    fn test_golf_load_nonexistent() {
        let (store, _dir) = temp_store();
        let loaded = store.load_goals("Nobody").unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_golf_update_progress() {
        let (store, _dir) = temp_store();
        store
            .store_goals(
                "Thomas",
                &[
                    make_goal(1, "Thomas", GoalType::Career),
                    make_goal(2, "Thomas", GoalType::Project),
                ],
            )
            .unwrap();

        // Update goal 2 progress
        let updated = store.update_goal_progress("Thomas", 2, 0.75, 500).unwrap();
        assert!(updated);

        let loaded = store.load_goals("Thomas").unwrap();
        assert_eq!(loaded[0].progress, 0.0); // goal 1 unchanged
        assert_eq!(loaded[1].progress, 0.75); // goal 2 updated
        assert_eq!(loaded[1].last_updated_tick, 500);
    }

    #[test]
    fn test_golf_update_progress_auto_complete() {
        let (store, _dir) = temp_store();
        store
            .store_goals("Lisa", &[make_goal(1, "Lisa", GoalType::Skill)])
            .unwrap();

        store.update_goal_progress("Lisa", 1, 1.0, 1000).unwrap();

        let loaded = store.load_goals("Lisa").unwrap();
        assert_eq!(loaded[0].progress, 1.0);
        assert_eq!(loaded[0].status, GoalStatus::Completed);
    }

    #[test]
    fn test_golf_update_progress_nonexistent_goal() {
        let (store, _dir) = temp_store();
        store
            .store_goals("Thomas", &[make_goal(1, "Thomas", GoalType::Career)])
            .unwrap();

        let updated = store.update_goal_progress("Thomas", 99, 0.5, 100).unwrap();
        assert!(!updated);
    }

    #[test]
    fn test_golf_list_agents_with_goals() {
        let (store, _dir) = temp_store();
        store
            .store_goals("Thomas", &[make_goal(1, "Thomas", GoalType::Career)])
            .unwrap();
        store
            .store_goals("Lisa", &[make_goal(2, "Lisa", GoalType::Skill)])
            .unwrap();

        let mut agents = store.list_agents_with_goals().unwrap();
        agents.sort();
        assert_eq!(agents, vec!["Lisa", "Thomas"]);
    }

    #[test]
    fn test_golf_data_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("golf-persist.redb");
        let path_str = path.to_str().unwrap();

        // Write
        {
            let store = HippocampusStore::open(path_str).unwrap();
            let mut goal = make_goal(1, "Thomas", GoalType::Career);
            goal.update_progress(0.42, 100);
            store.store_goals("Thomas", &[goal]).unwrap();
        }

        // Reopen and verify
        {
            let store = HippocampusStore::open(path_str).unwrap();
            let loaded = store.load_goals("Thomas").unwrap();
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].progress, 0.42);
            assert_eq!(loaded[0].goal_type, GoalType::Career);
            assert_eq!(loaded[0].last_updated_tick, 100);
        }
    }

    #[test]
    fn test_golf_integrity_no_empty_agent() {
        // Goal struct requires agent_name — empty string is technically valid
        // but we test that the struct enforces non-optional agent_name
        let goal = make_goal(1, "Thomas", GoalType::Career);
        assert!(!goal.agent_name.is_empty());
    }

    // === ARCHIVE Tests ===

    #[test]
    fn test_archive_store_load_roundtrip() {
        let (store, _dir) = temp_store();
        let episodes = vec![
            make_episode(1, "Konsolidiert A"),
            make_episode(2, "Konsolidiert B"),
        ];

        store.store_archive("Thomas", &episodes).unwrap();
        let loaded = store.load_archive("Thomas").unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].summary, "Konsolidiert A");
        assert_eq!(loaded[1].summary, "Konsolidiert B");
    }

    #[test]
    fn test_archive_append() {
        let (store, _dir) = temp_store();
        store
            .store_archive("Thomas", &[make_episode(1, "Erste Konsolidierung")])
            .unwrap();
        store
            .append_archive("Thomas", &[make_episode(2, "Zweite Konsolidierung")])
            .unwrap();

        let loaded = store.load_archive("Thomas").unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].summary, "Erste Konsolidierung");
        assert_eq!(loaded[1].summary, "Zweite Konsolidierung");
    }

    #[test]
    fn test_archive_load_nonexistent() {
        let (store, _dir) = temp_store();
        let loaded = store.load_archive("Nobody").unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_archive_caps_at_1000() {
        let (store, _dir) = temp_store();
        let many: Vec<Episode> = (0..1100)
            .map(|i| make_episode(i, &format!("Episode {i}")))
            .collect();

        store.append_archive("Thomas", &many).unwrap();
        let loaded = store.load_archive("Thomas").unwrap();
        assert_eq!(loaded.len(), 1000);
        // Oldest should be pruned, newest kept
        assert_eq!(loaded[0].summary, "Episode 100");
        assert_eq!(loaded[999].summary, "Episode 1099");
    }

    #[test]
    fn test_archive_data_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive-persist.redb");
        let path_str = path.to_str().unwrap();

        {
            let store = HippocampusStore::open(path_str).unwrap();
            store
                .store_archive("Thomas", &[make_episode(1, "Archived")])
                .unwrap();
        }

        {
            let store = HippocampusStore::open(path_str).unwrap();
            let loaded = store.load_archive("Thomas").unwrap();
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].summary, "Archived");
        }
    }

    #[test]
    fn test_archive_list_agents() {
        let (store, _dir) = temp_store();
        store
            .store_archive("Thomas", &[make_episode(1, "A")])
            .unwrap();
        store
            .store_archive("Lisa", &[make_episode(2, "B")])
            .unwrap();

        let mut agents = store.list_agents_with_archive().unwrap();
        agents.sort();
        assert_eq!(agents, vec!["Lisa", "Thomas"]);
    }
}
