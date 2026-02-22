//! Sleep cycle implementation for NMDA-based memory consolidation.
//!
//! This module simulates the biological sleep cycle where episodic memories
//! from the day are scored, selected, and consolidated into long-term storage.

use crate::episode::{nmda_score, Episode};

/// Sleep cycle phases representing different stages of memory processing.
#[derive(Debug, Clone, PartialEq)]
pub enum SleepPhase {
    /// Agent is awake, no processing
    Awake,
    /// Collecting episodes from the day
    Collecting,
    /// Scoring episodes using NMDA algorithm
    Scoring,
    /// Selecting top episodes for consolidation
    Selecting,
    /// Consolidating selected episodes into long-term memory
    Consolidating,
    /// Waking up, returning to normal operation
    WakingUp,
}

/// Sleep cycle manager for agent memory consolidation.
///
/// Orchestrates the full sleep cycle: collect daily episodes, score them using
/// NMDA algorithm, select candidates, consolidate to long-term storage, and wake up.
pub struct SleepCycle {
    pub agent_name: String,
    pub phase: SleepPhase,
    pub episodes: Vec<Episode>,
    pub selected: Vec<Episode>,
    pub consolidation_threshold: f64,
    pub max_consolidation_episodes: usize,
    /// Narrative built during consolidation phase, survives wake_up().
    consolidated_narrative: Option<String>,
}

impl SleepCycle {
    /// Create a new sleep cycle with default parameters.
    ///
    /// Default threshold: 0.1, max episodes: 10
    pub fn new(agent_name: &str) -> Self {
        Self {
            agent_name: agent_name.to_string(),
            phase: SleepPhase::Awake,
            episodes: Vec::new(),
            selected: Vec::new(),
            consolidation_threshold: 0.1,
            max_consolidation_episodes: 10,
            consolidated_narrative: None,
        }
    }

    /// Create a sleep cycle with custom parameters.
    pub fn with_params(agent_name: &str, threshold: f64, max_episodes: usize) -> Self {
        Self {
            agent_name: agent_name.to_string(),
            phase: SleepPhase::Awake,
            episodes: Vec::new(),
            selected: Vec::new(),
            consolidation_threshold: threshold,
            max_consolidation_episodes: max_episodes,
            consolidated_narrative: None,
        }
    }

    /// Begin the sleep cycle.
    ///
    /// Transitions from Awake to Collecting phase.
    pub fn begin_sleep(&mut self) {
        self.phase = SleepPhase::Collecting;
    }

    /// Add episodes from the day for processing.
    ///
    /// Transitions to Scoring phase after episodes are added.
    pub fn add_episodes(&mut self, episodes: Vec<Episode>) {
        self.episodes = episodes;
        self.phase = SleepPhase::Scoring;
    }

    /// Score all episodes and select top candidates for consolidation.
    ///
    /// Returns a list of (summary, score) tuples for selected episodes.
    /// Transitions to Selecting phase.
    pub fn score_and_select(&mut self) -> Vec<(String, f64)> {
        // Score all episodes
        let mut scored: Vec<(Episode, f64)> = self
            .episodes
            .iter()
            .map(|ep| {
                let score = nmda_score(ep);
                (ep.clone(), score)
            })
            .collect();

        // Sort by score descending (highest first)
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Filter by threshold and limit to max episodes
        let selected_scored: Vec<(Episode, f64)> = scored
            .into_iter()
            .filter(|(_, score)| *score >= self.consolidation_threshold)
            .take(self.max_consolidation_episodes)
            .collect();

        // Extract episodes for consolidation
        self.selected = selected_scored.iter().map(|(ep, _)| ep.clone()).collect();

        // Prepare return value (summary, score) tuples
        let result = selected_scored
            .into_iter()
            .map(|(ep, score)| (ep.summary.clone(), score))
            .collect();

        self.phase = SleepPhase::Selecting;
        result
    }

    /// Consolidate selected episodes into a narrative summary.
    ///
    /// Builds a consolidated narrative from `self.selected` episodes scored by NMDA.
    /// The narrative is stored internally and accessible via `consolidated_narrative()`
    /// after the cycle completes (survives `wake_up()`).
    ///
    /// The service layer is responsible for persisting the narrative to the store.
    pub fn consolidate(&mut self) -> anyhow::Result<()> {
        self.phase = SleepPhase::Consolidating;

        if self.selected.is_empty() {
            self.consolidated_narrative = None;
        } else {
            let narrative = self
                .selected
                .iter()
                .map(|ep| {
                    let score = nmda_score(ep);
                    format!("- {} (Score: {:.2})", ep.summary, score)
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.consolidated_narrative = Some(narrative);
        }

        self.phase = SleepPhase::WakingUp;
        Ok(())
    }

    /// Returns the narrative built during consolidation.
    ///
    /// Available after `consolidate()` has run, persists through `wake_up()`.
    pub fn consolidated_narrative(&self) -> Option<&str> {
        self.consolidated_narrative.as_deref()
    }

    /// Wake up and clear cycle state.
    ///
    /// Clears episodes and selected memories, returns to Awake phase.
    pub fn wake_up(&mut self) {
        self.episodes.clear();
        self.selected.clear();
        self.phase = SleepPhase::Awake;
    }

    /// Run a complete sleep cycle end-to-end.
    ///
    /// Convenience method that orchestrates all phases:
    /// begin_sleep → add_episodes → score_and_select → consolidate → wake_up
    ///
    /// Returns the list of (summary, score) tuples for consolidated episodes.
    pub fn run_full_cycle(
        &mut self,
        day_episodes: Vec<Episode>,
    ) -> anyhow::Result<Vec<(String, f64)>> {
        self.begin_sleep();
        self.add_episodes(day_episodes);
        let selected = self.score_and_select();
        self.consolidate()?;
        self.wake_up();
        Ok(selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    fn make_episode(
        id: u64,
        summary: &str,
        relevance: f64,
        emotion: f64,
        repetitions: u32,
        hours_ago: f64,
    ) -> Episode {
        Episode {
            id,
            agent_name: "Thomas".to_string(),
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
    fn test_sleep_cycle_phases() {
        let mut cycle = SleepCycle::new("Thomas");
        assert_eq!(cycle.phase, SleepPhase::Awake);

        cycle.begin_sleep();
        assert_eq!(cycle.phase, SleepPhase::Collecting);

        cycle.add_episodes(vec![]);
        assert_eq!(cycle.phase, SleepPhase::Scoring);

        cycle.score_and_select();
        assert_eq!(cycle.phase, SleepPhase::Selecting);

        cycle.consolidate().unwrap();
        assert_eq!(cycle.phase, SleepPhase::WakingUp);

        cycle.wake_up();
        assert_eq!(cycle.phase, SleepPhase::Awake);
    }

    #[test]
    fn test_full_cycle() {
        let mut cycle = SleepCycle::new("Thomas");

        let episodes = vec![
            make_episode(1, "Wichtiges Meeting", 0.9, 0.8, 2, 1.0),
            make_episode(2, "Kaffee getrunken", 0.1, 0.1, 1, 3.0),
            make_episode(3, "Konflikt mit Kunde", 0.95, 0.9, 1, 0.5),
        ];

        let result = cycle.run_full_cycle(episodes).unwrap();

        // Should select high-scoring episodes
        assert!(!result.is_empty(), "Should select at least one episode");
        assert_eq!(cycle.phase, SleepPhase::Awake);
        assert_eq!(
            cycle.episodes.len(),
            0,
            "Episodes should be cleared after wake_up"
        );
        assert_eq!(
            cycle.selected.len(),
            0,
            "Selected should be cleared after wake_up"
        );
    }

    #[test]
    fn test_episode_selection() {
        let mut cycle = SleepCycle::with_params("Thomas", 0.5, 10);

        let episodes = vec![
            // High score: 0.9 * 0.9 * 2.0 * (1/(1+1)) = 0.81
            make_episode(1, "Konflikt", 0.9, 0.9, 2, 1.0),
            // Low score: 0.1 * 0.1 * 1.0 * (1/(1+5)) = 0.001666...
            make_episode(2, "Routine", 0.1, 0.1, 1, 5.0),
            // Medium score: 0.6 * 0.7 * 1.0 * (1/(1+2)) = 0.14
            make_episode(3, "Meeting", 0.6, 0.7, 1, 2.0),
        ];

        cycle.begin_sleep();
        cycle.add_episodes(episodes);
        let selected = cycle.score_and_select();

        // Should select episodes with score >= 0.5
        // Only episode 1 has score >= 0.5 (0.81)
        assert_eq!(
            selected.len(),
            1,
            "Should select only high-scoring episodes"
        );
        assert_eq!(selected[0].0, "Konflikt");
        assert_relative_eq!(selected[0].1, 0.81, epsilon = 0.01);
    }

    #[test]
    fn test_wake_up_clears_state() {
        let mut cycle = SleepCycle::new("Thomas");

        let episodes = vec![make_episode(1, "Test", 0.5, 0.5, 1, 1.0)];

        cycle.begin_sleep();
        cycle.add_episodes(episodes);
        cycle.score_and_select();

        assert!(
            !cycle.episodes.is_empty(),
            "Should have episodes before wake_up"
        );
        assert!(
            !cycle.selected.is_empty(),
            "Should have selected before wake_up"
        );

        cycle.wake_up();

        assert_eq!(cycle.episodes.len(), 0, "Episodes should be cleared");
        assert_eq!(cycle.selected.len(), 0, "Selected should be cleared");
        assert_eq!(cycle.phase, SleepPhase::Awake);
    }

    #[test]
    fn test_threshold_filtering() {
        let mut cycle = SleepCycle::with_params("Thomas", 0.8, 10);

        let episodes = vec![
            // Score: 0.9 * 0.9 * 2.0 * 0.5 = 0.81
            make_episode(1, "High score", 0.9, 0.9, 2, 1.0),
            // Score: 0.5 * 0.5 * 1.0 * 0.5 = 0.125
            make_episode(2, "Low score", 0.5, 0.5, 1, 1.0),
            // Score: 0.3 * 0.3 * 1.0 * 0.5 = 0.045
            make_episode(3, "Very low", 0.3, 0.3, 1, 1.0),
        ];

        cycle.begin_sleep();
        cycle.add_episodes(episodes);
        let selected = cycle.score_and_select();

        // High threshold (0.8) should filter most episodes
        assert_eq!(
            selected.len(),
            1,
            "High threshold should filter most episodes"
        );
        assert_eq!(selected[0].0, "High score");
    }
}
