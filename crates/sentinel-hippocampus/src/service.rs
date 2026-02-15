//! HippocampusService — Central facade for persistent memory operations.
//!
//! Combines HippocampusStore (persistence) with SleepCycle (scoring/selection)
//! to provide episode recording, night-run consolidation, and prioritized retrieval.

use crate::episode::{nmda_score, Episode};
use crate::facts::FactRetriever;
use crate::sleep::SleepCycle;
use crate::store::{HippocampusStore, NarrativeState, RedbFactStore};

/// Result of a consolidation run for one agent.
#[derive(Debug)]
pub struct ConsolidationResult {
    pub agent_name: String,
    pub episodes_processed: usize,
    pub episodes_consolidated: usize,
    pub consolidated_summaries: Vec<(String, f64)>,
}

/// Central facade for all hippocampus memory operations.
///
/// Provides:
/// - Episode recording (daily operations)
/// - Night-run consolidation (scoring + narrative building + persistence)
/// - Prioritized memory retrieval (NMDA-sorted)
/// - Fact retrieval (trigger-based)
pub struct HippocampusService {
    store: HippocampusStore,
}

impl HippocampusService {
    /// Open or create the hippocampus service with a persistent store.
    pub fn open(db_path: &str) -> anyhow::Result<Self> {
        let store = HippocampusStore::open(db_path)?;
        Ok(Self { store })
    }

    /// Get a reference to the underlying store.
    pub fn store(&self) -> &HippocampusStore {
        &self.store
    }

    // === DAY OPERATIONS ===

    /// Record a single episode for an agent (appends to existing).
    pub fn record_episode(&self, episode: Episode) -> anyhow::Result<()> {
        let agent_name = episode.agent_name.clone();
        self.store.append_episodes(&agent_name, &[episode])
    }

    /// Record multiple episodes for an agent (appends to existing).
    pub fn record_episodes(&self, agent: &str, eps: &[Episode]) -> anyhow::Result<()> {
        self.store.append_episodes(agent, eps)
    }

    // === NIGHT-RUN CONSOLIDATION ===

    /// Consolidate episodes for a single agent.
    ///
    /// 1. Load episodes from redb
    /// 2. Run SleepCycle scoring + selection
    /// 3. Build narrative from selected episodes
    /// 4. Store narrative persistently
    /// 5. Clear processed episodes
    pub fn consolidate_agent(&self, agent: &str) -> anyhow::Result<ConsolidationResult> {
        let episodes = self.store.load_episodes(agent)?;
        let episodes_processed = episodes.len();

        if episodes.is_empty() {
            return Ok(ConsolidationResult {
                agent_name: agent.to_string(),
                episodes_processed: 0,
                episodes_consolidated: 0,
                consolidated_summaries: Vec::new(),
            });
        }

        // Run sleep cycle (scoring + selection)
        let mut cycle = SleepCycle::new(agent);
        let selected = cycle.run_full_cycle(episodes)?;
        let episodes_consolidated = selected.len();

        // Build narrative from consolidated summaries
        let summary = selected
            .iter()
            .map(|(s, score)| format!("- {} (Score: {:.2})", s, score))
            .collect::<Vec<_>>()
            .join("\n");

        // Persist narrative
        let narrative_state = NarrativeState {
            agent_name: agent.to_string(),
            summary,
            episode_count: episodes_consolidated,
        };
        self.store.store_narrative(agent, &narrative_state)?;

        // Clear processed episodes
        self.store.clear_episodes(agent)?;

        Ok(ConsolidationResult {
            agent_name: agent.to_string(),
            episodes_processed,
            episodes_consolidated,
            consolidated_summaries: selected,
        })
    }

    /// Consolidate episodes for ALL agents that have stored episodes.
    pub fn consolidate_all(&self) -> anyhow::Result<Vec<ConsolidationResult>> {
        let agents = self.store.list_agents_with_episodes()?;
        let mut results = Vec::new();
        for agent in agents {
            results.push(self.consolidate_agent(&agent)?);
        }
        Ok(results)
    }

    // === RETRIEVAL ===

    /// Retrieve episodes for an agent sorted by NMDA score (highest first).
    ///
    /// Returns (Episode, score) tuples, limited to `limit` entries.
    pub fn retrieve_memories(
        &self,
        agent: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<(Episode, f64)>> {
        let episodes = self.store.load_episodes(agent)?;
        let mut scored: Vec<(Episode, f64)> = episodes
            .into_iter()
            .map(|ep| {
                let score = nmda_score(&ep);
                (ep, score)
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    /// Retrieve facts matching trigger words in the given context.
    pub fn retrieve_facts(&self, context: &str) -> Vec<String> {
        let fact_store = RedbFactStore::new(&self.store);
        let retriever = FactRetriever::new(fact_store);
        retriever.check_triggers(context)
    }

    /// Get the consolidated narrative for an agent.
    pub fn get_narrative(&self, agent: &str) -> anyhow::Result<Option<String>> {
        let state = self.store.load_narrative(agent)?;
        Ok(state.map(|s| s.summary))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_service() -> (HippocampusService, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-service.redb");
        let service = HippocampusService::open(path.to_str().unwrap()).unwrap();
        (service, dir)
    }

    fn make_episode(
        id: u64,
        agent: &str,
        summary: &str,
        relevance: f64,
        emotion: f64,
        repetitions: u32,
        hours_ago: f64,
    ) -> Episode {
        Episode {
            id,
            agent_name: agent.to_string(),
            summary: summary.to_string(),
            relevance,
            emotion,
            repetitions,
            hours_ago,
            participants: vec![],
            tags: vec![],
        }
    }

    #[test]
    fn test_consolidation_full_cycle() {
        let (service, _dir) = temp_service();

        // Record episodes
        let episodes = vec![
            make_episode(1, "Thomas", "Wichtiges Meeting", 0.9, 0.8, 2, 1.0),
            make_episode(2, "Thomas", "Kaffee getrunken", 0.1, 0.1, 1, 3.0),
            make_episode(3, "Thomas", "Konflikt mit Kunde", 0.95, 0.9, 1, 0.5),
        ];
        service.record_episodes("Thomas", &episodes).unwrap();

        // Consolidate
        let result = service.consolidate_agent("Thomas").unwrap();
        assert_eq!(result.agent_name, "Thomas");
        assert_eq!(result.episodes_processed, 3);
        assert!(
            result.episodes_consolidated > 0,
            "Should consolidate at least one episode"
        );

        // Narrative should be persistent
        let narrative = service.get_narrative("Thomas").unwrap().unwrap();
        assert!(!narrative.is_empty());

        // Episodes should be cleared after consolidation
        let remaining = service.store().load_episodes("Thomas").unwrap();
        assert!(remaining.is_empty(), "Episodes should be cleared");
    }

    #[test]
    fn test_retrieval_priority_order() {
        let (service, _dir) = temp_service();

        // Record episodes with different scores
        let episodes = vec![
            make_episode(1, "Thomas", "Low priority", 0.1, 0.1, 1, 5.0),
            make_episode(2, "Thomas", "High priority", 0.95, 0.9, 3, 0.1),
            make_episode(3, "Thomas", "Medium priority", 0.5, 0.5, 1, 1.0),
        ];
        service.record_episodes("Thomas", &episodes).unwrap();

        // Retrieve with NMDA sorting
        let memories = service.retrieve_memories("Thomas", 10).unwrap();
        assert_eq!(memories.len(), 3);

        // Verify descending score order
        assert_eq!(memories[0].0.summary, "High priority");
        assert!(
            memories[0].1 >= memories[1].1,
            "First should have highest score"
        );
        assert!(
            memories[1].1 >= memories[2].1,
            "Second should have higher score than third"
        );
    }

    #[test]
    fn test_retrieval_limit() {
        let (service, _dir) = temp_service();

        let episodes: Vec<Episode> = (0..10)
            .map(|i| make_episode(i, "Thomas", &format!("Event {i}"), 0.8, 0.7, 1, 1.0))
            .collect();
        service.record_episodes("Thomas", &episodes).unwrap();

        let memories = service.retrieve_memories("Thomas", 3).unwrap();
        assert_eq!(memories.len(), 3);
    }

    #[test]
    fn test_consolidate_all_agents() {
        let (service, _dir) = temp_service();

        service
            .record_episodes(
                "Thomas",
                &[make_episode(1, "Thomas", "Meeting", 0.9, 0.8, 1, 0.5)],
            )
            .unwrap();
        service
            .record_episodes(
                "Lisa",
                &[make_episode(2, "Lisa", "Design Review", 0.8, 0.7, 1, 1.0)],
            )
            .unwrap();

        let results = service.consolidate_all().unwrap();
        assert_eq!(results.len(), 2);

        let names: Vec<&str> = results.iter().map(|r| r.agent_name.as_str()).collect();
        assert!(names.contains(&"Thomas"));
        assert!(names.contains(&"Lisa"));
    }

    #[test]
    fn test_consolidate_empty_agent() {
        let (service, _dir) = temp_service();
        let result = service.consolidate_agent("Nobody").unwrap();
        assert_eq!(result.episodes_processed, 0);
        assert_eq!(result.episodes_consolidated, 0);
    }

    #[test]
    fn test_fact_retrieval_through_service() {
        let (service, _dir) = temp_service();

        // Store facts that match FACT_TRIGGERS
        service
            .store()
            .store_fact("facts/projects/aurora", "Projekt Aurora: Webseite Redesign")
            .unwrap();
        service
            .store()
            .store_fact("facts/finance/budget-q1", "Q1 Budget: 150k EUR")
            .unwrap();

        let facts = service.retrieve_facts("Wir besprechen Projekt Aurora und das Budget");
        assert_eq!(facts.len(), 2);
        assert!(facts.iter().any(|f| f.contains("Aurora")));
        assert!(facts.iter().any(|f| f.contains("150k")));
    }

    #[test]
    fn test_record_single_episode() {
        let (service, _dir) = temp_service();
        let ep = make_episode(1, "Thomas", "Solo event", 0.8, 0.7, 1, 1.0);
        service.record_episode(ep).unwrap();

        let loaded = service.store().load_episodes("Thomas").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].summary, "Solo event");
    }
}
