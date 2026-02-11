//! Episode struct and NMDA scoring algorithm for memory consolidation.

/// An episode represents a daily event experienced by an agent.
///
/// NMDA scoring (inspired by biological NMDA receptors / long-term potentiation)
/// determines which episodes are consolidated into long-term memory.
#[derive(Debug, Clone)]
pub struct Episode {
    pub id: u64,
    pub agent_name: String,
    /// Short summary of the event
    pub summary: String,
    /// Relevance to company/project context (0.0-1.0)
    pub relevance: f64,
    /// Emotional intensity (0.0-1.0, e.g. conflict=0.9, coffee=0.1)
    pub emotion: f64,
    /// How often similar topics occurred
    pub repetitions: u32,
    /// Hours since the event
    pub hours_ago: f64,
    /// Agents involved in this episode
    pub participants: Vec<String>,
    /// Tags like "meeting", "conflict", "praise", "routine"
    pub tags: Vec<String>,
}

/// Compute the long-term memory score for an episode.
///
/// Hoher Score = emotional + relevant + wiederholt + kuerzlich.
/// Inspired by biological NMDA receptors (Langzeitpotenzierung im Hippocampus).
pub fn nmda_score(episode: &Episode) -> f64 {
    let relevance = episode.relevance;
    let emotional_intensity = episode.emotion;
    let repetition_freq = episode.repetitions as f64;
    let time_decay = 1.0 / (1.0 + episode.hours_ago);

    relevance * emotional_intensity * repetition_freq * time_decay
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    fn make_episode(relevance: f64, emotion: f64, repetitions: u32, hours_ago: f64) -> Episode {
        Episode {
            id: 1,
            agent_name: "Thomas".to_string(),
            summary: "Test episode".to_string(),
            relevance,
            emotion,
            repetitions,
            hours_ago,
            participants: vec![],
            tags: vec![],
        }
    }

    #[test]
    fn test_nmda_score_high_emotion_recent() {
        let episode = Episode {
            id: 1,
            agent_name: "Thomas".to_string(),
            summary: "Streit mit Kunde ueber Deadline".to_string(),
            relevance: 0.9,
            emotion: 0.9,
            repetitions: 1,
            hours_ago: 0.5,
            participants: vec!["Thomas".into(), "Lisa".into()],
            tags: vec!["conflict".into()],
        };
        let score = nmda_score(&episode);
        assert!(
            score > 0.5,
            "Emotional, relevant, recent event should have high score: {score}"
        );
    }

    #[test]
    fn test_nmda_score_routine_old() {
        let episode = Episode {
            id: 2,
            agent_name: "Thomas".to_string(),
            summary: "Dritte Tasse Kaffee".to_string(),
            relevance: 0.1,
            emotion: 0.05,
            repetitions: 1,
            hours_ago: 6.0,
            participants: vec![],
            tags: vec!["routine".into()],
        };
        let score = nmda_score(&episode);
        assert!(
            score < 0.01,
            "Routine event should have low score: {score}"
        );
    }

    #[test]
    fn test_nmda_score_zero_repetitions() {
        let episode = make_episode(0.9, 0.9, 0, 0.5);
        let score = nmda_score(&episode);
        assert_relative_eq!(score, 0.0, epsilon = 1e-10);
    }

    #[test]
    fn test_nmda_score_zero_hours_ago() {
        // time_decay = 1.0 / (1.0 + 0.0) = 1.0
        let episode = make_episode(0.8, 0.7, 2, 0.0);
        let score = nmda_score(&episode);
        let expected = 0.8 * 0.7 * 2.0 * 1.0;
        assert_relative_eq!(score, expected, epsilon = 1e-10);
    }

    #[test]
    fn test_nmda_score_high_repetition() {
        let episode = make_episode(0.5, 0.5, 10, 1.0);
        let score = nmda_score(&episode);
        // 0.5 * 0.5 * 10.0 * 0.5 = 1.25
        assert_relative_eq!(score, 1.25, epsilon = 1e-10);
    }

    #[test]
    fn test_nmda_score_time_decay_formula() {
        let ep1 = make_episode(1.0, 1.0, 1, 0.0);
        let ep2 = make_episode(1.0, 1.0, 1, 1.0);
        let ep3 = make_episode(1.0, 1.0, 1, 9.0);

        assert_relative_eq!(nmda_score(&ep1), 1.0, epsilon = 1e-10);
        assert_relative_eq!(nmda_score(&ep2), 0.5, epsilon = 1e-10);
        assert_relative_eq!(nmda_score(&ep3), 0.1, epsilon = 1e-10);
    }

    #[test]
    fn test_nmda_score_components_multiply() {
        // All components should multiply together
        let episode = make_episode(0.5, 0.4, 3, 1.0);
        let expected = 0.5 * 0.4 * 3.0 * (1.0 / 2.0);
        assert_relative_eq!(nmda_score(&episode), expected, epsilon = 1e-10);
    }
}
