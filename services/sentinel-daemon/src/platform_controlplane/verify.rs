//! Minimal-Verify: Hat die letzte Platform-Action gewirkt?

use std::collections::HashMap;

use super::metrics::PlatformMetrics;
use super::rules::PlatformAction;
use crate::config::PlatformControlplaneConfig;

/// Prueft ob die letzten Actions ihre Probleme geloest haben.
///
/// Gibt `HashMap<"rule:target", resolved>` zurueck.
/// `false` = Problem besteht weiterhin (Eskalation moeglich).
pub fn verify_last_actions(
    last_actions: &[PlatformAction],
    current_metrics: &PlatformMetrics,
    config: &PlatformControlplaneConfig,
) -> HashMap<String, bool> {
    let mut results = HashMap::new();

    for action in last_actions {
        let key = format!("{}:{}", action.rule_name, action.target);
        let resolved = match action.rule_name.as_str() {
            "event_store_size" => {
                current_metrics.event_store_size_bytes <= config.max_event_store_bytes
            }
            "projection_lag" => current_metrics.projection_lag <= config.max_projection_lag,
            "memory_pressure" => {
                // Pruefe ob der spezifische Agent noch unter Druck steht
                current_metrics
                    .agent_memory_pressure
                    .iter()
                    .find(|(name, _)| *name == action.target)
                    .map(|(_, pressure)| *pressure <= config.memory_pressure_threshold)
                    .unwrap_or(true) // Agent nicht mehr da = resolved
            }
            "agent_stall" => {
                // Agent nicht mehr in stalled_agents
                !current_metrics.stalled_agents.contains(&action.target)
            }
            _ => true, // Unbekannte Regeln gelten als resolved
        };
        results.insert(key, resolved);
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> PlatformControlplaneConfig {
        PlatformControlplaneConfig {
            enabled: true,
            cycle_interval_ticks: 1,
            stall_cooldown_ticks: 60,
            prune_cooldown_ticks: 3600,
            max_event_store_bytes: 500 * 1024 * 1024,
            max_projection_lag: 10_000,
            memory_pressure_threshold: 0.9,
            max_escalation: 3,
            ..PlatformControlplaneConfig::default()
        }
    }

    #[test]
    fn test_verify_event_store_resolved() {
        let actions = vec![PlatformAction {
            rule_name: "event_store_size".to_string(),
            target: "system".to_string(),
            action_label: "prune_triggered".to_string(),
            description: String::new(),
            side_effect: None,
        }];
        let metrics = PlatformMetrics {
            event_store_size_bytes: 100 * 1024 * 1024, // 100 MB < 500 MB
            ..Default::default()
        };
        let results = verify_last_actions(&actions, &metrics, &test_config());
        assert_eq!(results.get("event_store_size:system"), Some(&true));
    }

    #[test]
    fn test_verify_event_store_unresolved() {
        let actions = vec![PlatformAction {
            rule_name: "event_store_size".to_string(),
            target: "system".to_string(),
            action_label: "prune_triggered".to_string(),
            description: String::new(),
            side_effect: None,
        }];
        let metrics = PlatformMetrics {
            event_store_size_bytes: 600 * 1024 * 1024, // 600 MB > 500 MB
            ..Default::default()
        };
        let results = verify_last_actions(&actions, &metrics, &test_config());
        assert_eq!(results.get("event_store_size:system"), Some(&false));
    }
}
