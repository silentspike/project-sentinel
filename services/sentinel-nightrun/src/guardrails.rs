//! Deterministic Guardrail Controller for Nightrun.
//!
//! Extracts runtime limit checks from the runner into a standalone module.
//! Same inputs => same decision (deterministic for replay).

use crate::config::NightrunSettings;

/// Decision made by the guardrail controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardrailDecision {
    /// Continue processing.
    Proceed,
    /// Skip this agent with a reason.
    Skip { reason: String },
    /// Abort the entire run with a reason.
    Abort { reason: String },
}

/// Deterministic guardrail controller.
///
/// All checks are pure functions of their inputs — no side effects,
/// no randomness, no wall-clock reads.
pub struct GuardrailController {
    max_episodes_per_agent: usize,
    timeout_total_secs: u64,
    timeout_per_agent_secs: u64,
    max_jobs_per_run: usize,
}

impl GuardrailController {
    /// Create a controller from nightrun settings.
    pub fn from_settings(settings: &NightrunSettings) -> Self {
        Self {
            max_episodes_per_agent: settings.max_episodes_per_agent,
            timeout_total_secs: settings.timeout_total_secs,
            timeout_per_agent_secs: settings.timeout_per_agent_secs,
            max_jobs_per_run: settings.max_jobs_per_run,
        }
    }

    /// Check whether an agent's episode backlog is within limits.
    pub fn check_agent_backlog(&self, episode_count: usize) -> GuardrailDecision {
        if episode_count > self.max_episodes_per_agent {
            GuardrailDecision::Skip {
                reason: format!(
                    "Backlog zu gross: {episode_count} > {}",
                    self.max_episodes_per_agent
                ),
            }
        } else {
            GuardrailDecision::Proceed
        }
    }

    /// Check whether the total run timeout has been exceeded.
    pub fn check_total_timeout(&self, elapsed_secs: u64) -> GuardrailDecision {
        if elapsed_secs >= self.timeout_total_secs {
            GuardrailDecision::Abort {
                reason: format!(
                    "Total-Timeout erreicht: {elapsed_secs}s >= {}s",
                    self.timeout_total_secs
                ),
            }
        } else {
            GuardrailDecision::Proceed
        }
    }

    /// Check whether the job count has exceeded max_jobs_per_run.
    pub fn check_job_count(&self, current_jobs: usize) -> GuardrailDecision {
        if current_jobs >= self.max_jobs_per_run {
            GuardrailDecision::Abort {
                reason: format!(
                    "Max Jobs pro Run erreicht: {current_jobs} >= {}",
                    self.max_jobs_per_run
                ),
            }
        } else {
            GuardrailDecision::Proceed
        }
    }

    /// Check whether a single agent's processing time is concerning.
    ///
    /// Returns `Skip` (warning-level) — the caller decides whether to act on it.
    pub fn check_agent_timeout(&self, elapsed_secs: u64) -> GuardrailDecision {
        if elapsed_secs >= self.timeout_per_agent_secs {
            GuardrailDecision::Skip {
                reason: format!(
                    "Agent-Timeout ueberschritten: {elapsed_secs}s >= {}s",
                    self.timeout_per_agent_secs
                ),
            }
        } else {
            GuardrailDecision::Proceed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings() -> NightrunSettings {
        NightrunSettings {
            hippocampus_db: String::new(),
            event_store_db: String::new(),
            agent_config_dir: String::new(),
            job_queue_path: String::new(),
            timeout_per_agent_secs: 300,
            timeout_total_secs: 7200,
            max_episodes_per_agent: 1000,
            max_jobs_per_run: 100,
            max_agent_id: sentinel_common::DEFAULT_MAX_AGENT_ID,
        }
    }

    #[test]
    fn from_settings_preserves_values() {
        let s = test_settings();
        let gc = GuardrailController::from_settings(&s);
        assert_eq!(gc.max_episodes_per_agent, 1000);
        assert_eq!(gc.timeout_total_secs, 7200);
        assert_eq!(gc.timeout_per_agent_secs, 300);
    }

    #[test]
    fn backlog_proceed() {
        let gc = GuardrailController::from_settings(&test_settings());
        assert_eq!(gc.check_agent_backlog(500), GuardrailDecision::Proceed);
    }

    #[test]
    fn backlog_skip() {
        let gc = GuardrailController::from_settings(&test_settings());
        match gc.check_agent_backlog(1500) {
            GuardrailDecision::Skip { reason } => {
                assert!(reason.contains("1500"));
                assert!(reason.contains("1000"));
            }
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    #[test]
    fn backlog_boundary() {
        let gc = GuardrailController::from_settings(&test_settings());
        // Exactly at limit: proceed
        assert_eq!(gc.check_agent_backlog(1000), GuardrailDecision::Proceed);
        // One over: skip
        assert!(matches!(
            gc.check_agent_backlog(1001),
            GuardrailDecision::Skip { .. }
        ));
    }

    #[test]
    fn total_timeout_proceed() {
        let gc = GuardrailController::from_settings(&test_settings());
        assert_eq!(gc.check_total_timeout(3600), GuardrailDecision::Proceed);
    }

    #[test]
    fn total_timeout_abort() {
        let gc = GuardrailController::from_settings(&test_settings());
        match gc.check_total_timeout(7200) {
            GuardrailDecision::Abort { reason } => {
                assert!(reason.contains("7200"));
            }
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn agent_timeout_proceed() {
        let gc = GuardrailController::from_settings(&test_settings());
        assert_eq!(gc.check_agent_timeout(100), GuardrailDecision::Proceed);
    }

    #[test]
    fn agent_timeout_skip() {
        let gc = GuardrailController::from_settings(&test_settings());
        assert!(matches!(
            gc.check_agent_timeout(300),
            GuardrailDecision::Skip { .. }
        ));
    }

    #[test]
    fn job_count_proceed() {
        let gc = GuardrailController::from_settings(&test_settings());
        assert_eq!(gc.check_job_count(50), GuardrailDecision::Proceed);
    }

    #[test]
    fn job_count_abort_at_limit() {
        let gc = GuardrailController::from_settings(&test_settings());
        match gc.check_job_count(100) {
            GuardrailDecision::Abort { reason } => {
                assert!(reason.contains("100"));
            }
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn job_count_abort_over_limit() {
        let gc = GuardrailController::from_settings(&test_settings());
        assert!(matches!(
            gc.check_job_count(150),
            GuardrailDecision::Abort { .. }
        ));
    }

    #[test]
    fn deterministic_same_inputs_same_decision() {
        let gc = GuardrailController::from_settings(&test_settings());
        let d1 = gc.check_agent_backlog(1500);
        let d2 = gc.check_agent_backlog(1500);
        assert_eq!(d1, d2);
    }
}
