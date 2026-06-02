//! Calibrated NMDA episode-selection profile.
//!
//! The score formula stays in `episode.rs`; this module defines the policy
//! boundary that decides which scored episodes become consolidation candidates.

use crate::episode::{nmda_score, Episode};

/// Final calibrated threshold for Night-Run consolidation.
///
/// Rationale:
/// - A normal relevant work episode around relevance=0.8, emotion=0.7,
///   repetitions=1, hours_ago=1 scores 0.28 and should be retained.
/// - A medium-context meeting around relevance=0.5, emotion=0.5,
///   repetitions=1, hours_ago=1 scores 0.125 and should stay archive-only.
/// - Routine low-signal events stay far below 0.01.
pub const NMDA_CONSOLIDATION_THRESHOLD: f64 = 0.25;

/// Narrative inclusion follows the same calibrated boundary as consolidation.
pub const NMDA_NARRATIVE_INCLUSION_THRESHOLD: f64 = NMDA_CONSOLIDATION_THRESHOLD;

/// Keep the strongest memories only; selected episodes remain sorted by score.
pub const NMDA_MAX_CONSOLIDATION_EPISODES: usize = 10;

/// Human-readable rationale used by verification/docs.
pub const NMDA_SELECTION_RATIONALE: &str =
    "0.25 keeps relevant work episodes while rejecting medium-context and routine noise";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NmdaSelectionProfile {
    pub consolidation_threshold: f64,
    pub narrative_inclusion_threshold: f64,
    pub max_consolidation_episodes: usize,
    pub rationale: &'static str,
}

pub const CALIBRATED_NMDA_SELECTION_PROFILE: NmdaSelectionProfile = NmdaSelectionProfile {
    consolidation_threshold: NMDA_CONSOLIDATION_THRESHOLD,
    narrative_inclusion_threshold: NMDA_NARRATIVE_INCLUSION_THRESHOLD,
    max_consolidation_episodes: NMDA_MAX_CONSOLIDATION_EPISODES,
    rationale: NMDA_SELECTION_RATIONALE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NmdaSelectionDecision {
    Consolidate,
    ArchiveOnly,
}

pub fn selection_decision(score: f64) -> NmdaSelectionDecision {
    if score >= NMDA_CONSOLIDATION_THRESHOLD {
        NmdaSelectionDecision::Consolidate
    } else {
        NmdaSelectionDecision::ArchiveOnly
    }
}

pub fn should_consolidate(episode: &Episode) -> bool {
    selection_decision(nmda_score(episode)) == NmdaSelectionDecision::Consolidate
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    fn episode(relevance: f64, emotion: f64, repetitions: u32, hours_ago: f64) -> Episode {
        Episode {
            id: 1,
            agent_name: "Thomas".to_string(),
            summary: "Test episode".to_string(),
            relevance,
            emotion,
            repetitions,
            hours_ago,
            participants: Vec::new(),
            tags: Vec::new(),
        }
    }

    #[test]
    fn calibrated_profile_documents_final_thresholds() {
        assert_eq!(
            CALIBRATED_NMDA_SELECTION_PROFILE.consolidation_threshold,
            0.25
        );
        assert_eq!(
            CALIBRATED_NMDA_SELECTION_PROFILE.narrative_inclusion_threshold,
            0.25
        );
        assert_eq!(
            CALIBRATED_NMDA_SELECTION_PROFILE.max_consolidation_episodes,
            10
        );
        assert!(CALIBRATED_NMDA_SELECTION_PROFILE
            .rationale
            .contains("relevant work episodes"));
    }

    #[test]
    fn calibrated_threshold_accepts_relevant_work_episode() {
        let ep = episode(0.8, 0.7, 1, 1.0);
        assert_relative_eq!(nmda_score(&ep), 0.28, epsilon = 1e-12);
        assert!(should_consolidate(&ep));
    }

    #[test]
    fn calibrated_threshold_rejects_medium_context_episode() {
        let ep = episode(0.5, 0.5, 1, 1.0);
        assert_eq!(nmda_score(&ep), 0.125);
        assert!(!should_consolidate(&ep));
    }

    #[test]
    fn calibrated_threshold_rejects_routine_noise() {
        let ep = episode(0.1, 0.05, 1, 1.0);
        assert_eq!(nmda_score(&ep), 0.0025000000000000005);
        assert!(!should_consolidate(&ep));
    }
}
