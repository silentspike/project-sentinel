//! NarrativeMemory - Running summary of important daily events per agent.

use crate::episode::{nmda_score, Episode};

/// Maintains a running summary of the most important daily events for an agent.
///
/// Episodes are only included if their NMDA score exceeds the inclusion threshold.
/// The summary is rebuilt from the top-10 highest-scoring episodes.
pub struct NarrativeMemory {
    agent_name: String,
    /// Current summary text (max ~500 tokens target)
    summary: String,
    /// All episodes of the day that passed the threshold
    episodes: Vec<Episode>,
    /// Minimum NMDA score for inclusion (default: 0.3)
    inclusion_threshold: f64,
}

impl NarrativeMemory {
    pub fn new(agent_name: &str) -> Self {
        Self {
            agent_name: agent_name.to_string(),
            summary: String::new(),
            episodes: Vec::new(),
            inclusion_threshold: 0.3,
        }
    }

    /// Create with custom threshold for testing.
    pub fn with_threshold(agent_name: &str, threshold: f64) -> Self {
        Self {
            agent_name: agent_name.to_string(),
            summary: String::new(),
            episodes: Vec::new(),
            inclusion_threshold: threshold,
        }
    }

    /// Add a new episode if its NMDA score meets the threshold.
    ///
    /// Returns true if the episode was added, false if it was "forgotten".
    pub fn add_episode(&mut self, episode: Episode) -> bool {
        let score = nmda_score(&episode);
        if score >= self.inclusion_threshold {
            self.episodes.push(episode);
            self.rebuild_summary();
            true
        } else {
            false
        }
    }

    /// Rebuild the summary from the top-10 highest-scoring episodes.
    fn rebuild_summary(&mut self) {
        let mut scored: Vec<(&Episode, f64)> =
            self.episodes.iter().map(|e| (e, nmda_score(e))).collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        self.summary = scored
            .iter()
            .take(10)
            .map(|(e, s)| format!("- {} (Score: {:.2})", e.summary, s))
            .collect::<Vec<_>>()
            .join("\n");
    }

    /// Get the current narrative for prompt injection.
    pub fn get_narrative(&self) -> &str {
        &self.summary
    }

    /// Get the agent name.
    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    /// Get the number of stored episodes.
    pub fn episode_count(&self) -> usize {
        self.episodes.len()
    }

    /// Drain all episodes (for nightly consolidation in dream phase).
    /// Returns all stored episodes and clears the internal list.
    pub fn drain_episodes(&mut self) -> Vec<Episode> {
        let episodes = std::mem::take(&mut self.episodes);
        self.summary.clear();
        episodes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn important_episode(id: u64, summary: &str) -> Episode {
        Episode {
            id,
            agent_name: "Thomas".to_string(),
            summary: summary.to_string(),
            relevance: 0.95,
            emotion: 0.9,
            repetitions: 1,
            hours_ago: 0.1,
            participants: vec!["Michael".into()],
            tags: vec!["praise".into()],
        }
    }

    fn routine_episode(id: u64) -> Episode {
        Episode {
            id,
            agent_name: "Thomas".to_string(),
            summary: "Kaffee geholt".to_string(),
            relevance: 0.1,
            emotion: 0.05,
            repetitions: 1,
            hours_ago: 1.0,
            participants: vec![],
            tags: vec!["routine".into()],
        }
    }

    #[test]
    fn test_narrative_rejects_routine() {
        let mut memory = NarrativeMemory::new("Thomas");
        let added = memory.add_episode(routine_episode(1));
        assert!(!added, "Routine episode should be rejected");
        assert_eq!(memory.episode_count(), 0);
    }

    #[test]
    fn test_narrative_accepts_important() {
        let mut memory = NarrativeMemory::new("Thomas");
        let added = memory.add_episode(important_episode(1, "Befoerderung bekommen"));
        assert!(added, "Important episode should be accepted");
        assert_eq!(memory.episode_count(), 1);
        assert!(memory.get_narrative().contains("Befoerderung"));
    }

    #[test]
    fn test_narrative_summary_contains_scores() {
        let mut memory = NarrativeMemory::new("Lisa");
        memory.add_episode(important_episode(1, "Design Review bestanden"));
        let narrative = memory.get_narrative();
        assert!(
            narrative.contains("Score:"),
            "Summary should contain scores"
        );
        assert!(narrative.contains("Design Review"));
    }

    #[test]
    fn test_narrative_drain_episodes() {
        let mut memory = NarrativeMemory::new("Lisa");
        memory.add_episode(important_episode(1, "Design Review"));
        memory.add_episode(important_episode(2, "Kundenmeeting"));

        let drained = memory.drain_episodes();
        assert_eq!(drained.len(), 2);
        assert!(memory.drain_episodes().is_empty());
        assert!(memory.get_narrative().is_empty());
        assert_eq!(memory.episode_count(), 0);
    }

    #[test]
    fn test_narrative_top_10_limit() {
        let mut memory = NarrativeMemory::new("Thomas");
        for i in 0..15 {
            memory.add_episode(important_episode(i, &format!("Event {i}")));
        }
        assert_eq!(memory.episode_count(), 15);
        // Summary should only contain 10 entries
        let line_count = memory.get_narrative().lines().count();
        assert_eq!(line_count, 10, "Summary should have max 10 lines");
    }

    #[test]
    fn test_narrative_agent_name() {
        let memory = NarrativeMemory::new("Andreas");
        assert_eq!(memory.agent_name(), "Andreas");
    }

    #[test]
    fn test_narrative_empty_initially() {
        let memory = NarrativeMemory::new("Thomas");
        assert!(memory.get_narrative().is_empty());
        assert_eq!(memory.episode_count(), 0);
    }

    #[test]
    fn test_narrative_custom_threshold() {
        let mut memory = NarrativeMemory::with_threshold("Thomas", 0.01);
        // With very low threshold, even routine gets accepted
        let routine = Episode {
            id: 1,
            agent_name: "Thomas".to_string(),
            summary: "Kaffee".to_string(),
            relevance: 0.1,
            emotion: 0.2,
            repetitions: 1,
            hours_ago: 0.5,
            participants: vec![],
            tags: vec![],
        };
        // Score = 0.1 * 0.2 * 1.0 * (1/1.5) = 0.0133... > 0.01
        assert!(memory.add_episode(routine));
    }

    #[test]
    fn test_narrative_sorted_by_score() {
        let mut memory = NarrativeMemory::new("Thomas");

        // Add lower-scoring episode first
        let medium = Episode {
            id: 1,
            agent_name: "Thomas".to_string(),
            summary: "Mittelwichtiges Meeting".to_string(),
            relevance: 0.5,
            emotion: 0.8,
            repetitions: 1,
            hours_ago: 0.1,
            participants: vec![],
            tags: vec![],
        };
        memory.add_episode(medium);

        // Add higher-scoring episode
        let high = Episode {
            id: 2,
            agent_name: "Thomas".to_string(),
            summary: "Superwichtiger Durchbruch".to_string(),
            relevance: 1.0,
            emotion: 1.0,
            repetitions: 3,
            hours_ago: 0.0,
            participants: vec![],
            tags: vec![],
        };
        memory.add_episode(high);

        let narrative = memory.get_narrative();
        let lines: Vec<&str> = narrative.lines().collect();
        // Higher-scoring episode should be first
        assert!(
            lines[0].contains("Superwichtiger Durchbruch"),
            "First line should be highest-scoring: {lines:?}"
        );
    }
}
