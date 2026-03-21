//! Deterministische Regeln fuer die Platform-Gesundheit.

use std::collections::HashMap;

use sentinel_common::AgentId;

use super::metrics::PlatformMetrics;
use crate::config::PlatformControlplaneConfig;

/// Auszufuehrende Aktion einer Regel.
#[derive(Debug, Clone)]
pub struct PlatformAction {
    pub rule_name: String,
    pub target: String,
    pub action_label: String,
    pub description: String,
    pub side_effect: Option<PlatformSideEffect>,
}

/// Side-Effects die der Orchestrator ausfuehrt (nicht der Platform-CP selbst).
#[derive(Debug, Clone)]
pub enum PlatformSideEffect {
    /// Prune im Event Store triggern (cutoff event_id).
    TriggerPrune(i64),
    /// Agent-Profil auf Idle setzen (Memory-Pressure).
    ForceIdleProfile(AgentId),
}

/// Evaluiert alle Regeln gegen die aktuellen Metriken.
///
/// Prueft Cooldowns und gibt nur feuernde Regeln zurueck.
pub fn evaluate_rules(
    metrics: &PlatformMetrics,
    cooldowns: &HashMap<String, u64>,
    tick: u64,
    config: &PlatformControlplaneConfig,
) -> Vec<PlatformAction> {
    let mut actions = Vec::new();

    // Regel 1: Agent Stall
    for agent_name in &metrics.stalled_agents {
        let key = format!("agent_stall:{agent_name}");
        if !is_cooled_down(cooldowns, &key, tick, config.stall_cooldown_ticks) {
            continue;
        }
        actions.push(PlatformAction {
            rule_name: "agent_stall".to_string(),
            target: agent_name.clone(),
            action_label: "alert".to_string(),
            description: format!("Agent {agent_name} ist gestalled — keine I/O seit > 30s"),
            side_effect: None, // Phase 2: Respawn via #279
        });
    }

    // Regel 2: Event Store Groesse
    if metrics.event_store_size_bytes > config.max_event_store_bytes {
        let key = "event_store_size:system".to_string();
        if is_cooled_down(cooldowns, &key, tick, config.prune_cooldown_ticks) {
            let size_mb = metrics.event_store_size_bytes / (1024 * 1024);
            actions.push(PlatformAction {
                rule_name: "event_store_size".to_string(),
                target: "system".to_string(),
                action_label: "prune_triggered".to_string(),
                description: format!(
                    "Event Store {size_mb} MB > {} MB Schwellwert — Prune getriggert",
                    config.max_event_store_bytes / (1024 * 1024)
                ),
                side_effect: Some(PlatformSideEffect::TriggerPrune(0)), // 0 = auto-detect cutoff
            });
        }
    }

    // Regel 3: Projection Lag
    if metrics.projection_lag > config.max_projection_lag {
        let key = "projection_lag:system".to_string();
        if is_cooled_down(cooldowns, &key, tick, 60) {
            actions.push(PlatformAction {
                rule_name: "projection_lag".to_string(),
                target: "system".to_string(),
                action_label: "alert".to_string(),
                description: format!(
                    "Projection Lag {} > {} Schwellwert",
                    metrics.projection_lag, config.max_projection_lag
                ),
                side_effect: None,
            });
        }
    }

    // Regel 4: Memory Pressure
    for (agent_name, pressure) in &metrics.agent_memory_pressure {
        if *pressure > config.memory_pressure_threshold {
            let key = format!("memory_pressure:{agent_name}");
            if !is_cooled_down(cooldowns, &key, tick, 30) {
                continue;
            }
            // Agent-ID aus Name extrahieren (best-effort)
            let agent_id = extract_agent_id(agent_name);
            actions.push(PlatformAction {
                rule_name: "memory_pressure".to_string(),
                target: agent_name.clone(),
                action_label: "idle_forced".to_string(),
                description: format!(
                    "Memory Pressure {:.0}% > {:.0}% — Profil auf Idle gesetzt",
                    pressure * 100.0,
                    config.memory_pressure_threshold * 100.0
                ),
                side_effect: agent_id.map(PlatformSideEffect::ForceIdleProfile),
            });
        }
    }

    actions
}

/// Prueft ob der Cooldown fuer eine Regel abgelaufen ist.
fn is_cooled_down(
    cooldowns: &HashMap<String, u64>,
    key: &str,
    tick: u64,
    cooldown_ticks: u64,
) -> bool {
    match cooldowns.get(key) {
        Some(&last_tick) => tick.saturating_sub(last_tick) >= cooldown_ticks,
        None => true,
    }
}

/// Versucht eine AgentId aus dem Agent-Namen zu extrahieren.
///
/// Agents haben keine 1:1 Name→ID Mapping im Platform-CP Scope.
/// Fuer force_profile() brauchen wir die ID. Fallback: None.
fn extract_agent_id(_name: &str) -> Option<AgentId> {
    // Phase 2: Lookup aus RuntimeOrchestrator
    // Phase 1: Kein Mapping verfuegbar, Side-Effect wird ignoriert
    None
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
        }
    }

    #[test]
    fn test_stall_rule_fires_for_stalled_agent() {
        let metrics = PlatformMetrics {
            stalled_agents: vec!["Thomas Mueller".to_string()],
            ..Default::default()
        };
        let actions = evaluate_rules(&metrics, &HashMap::new(), 100, &test_config());
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].rule_name, "agent_stall");
        assert_eq!(actions[0].target, "Thomas Mueller");
    }

    #[test]
    fn test_stall_rule_cooldown_prevents_repeat() {
        let metrics = PlatformMetrics {
            stalled_agents: vec!["Thomas Mueller".to_string()],
            ..Default::default()
        };
        let mut cooldowns = HashMap::new();
        cooldowns.insert("agent_stall:Thomas Mueller".to_string(), 90);

        // Tick 100, Cooldown 60 → last action at 90, diff=10 < 60 → cooled down = false
        let actions = evaluate_rules(&metrics, &cooldowns, 100, &test_config());
        assert!(actions.is_empty(), "Should be cooled down");
    }

    #[test]
    fn test_event_store_size_rule_fires() {
        let metrics = PlatformMetrics {
            event_store_size_bytes: 600 * 1024 * 1024, // 600 MB > 500 MB
            ..Default::default()
        };
        let actions = evaluate_rules(&metrics, &HashMap::new(), 100, &test_config());
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].rule_name, "event_store_size");
        assert!(actions[0].side_effect.is_some());
    }

    #[test]
    fn test_projection_lag_rule_fires() {
        let metrics = PlatformMetrics {
            projection_lag: 15_000, // > 10_000
            ..Default::default()
        };
        let actions = evaluate_rules(&metrics, &HashMap::new(), 100, &test_config());
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].rule_name, "projection_lag");
    }

    #[test]
    fn test_memory_pressure_rule_fires() {
        let metrics = PlatformMetrics {
            agent_memory_pressure: vec![("Test Agent".to_string(), 0.95)],
            ..Default::default()
        };
        let actions = evaluate_rules(&metrics, &HashMap::new(), 100, &test_config());
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].rule_name, "memory_pressure");
    }

    #[test]
    fn test_no_rules_fire_when_healthy() {
        let metrics = PlatformMetrics {
            event_store_size_bytes: 100 * 1024 * 1024, // 100 MB < 500 MB
            projection_lag: 50,                        // < 10_000
            ..Default::default()
        };
        let actions = evaluate_rules(&metrics, &HashMap::new(), 100, &test_config());
        assert!(actions.is_empty());
    }
}
