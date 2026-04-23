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
    /// Agent-Cgroup per SIGSTOP anhalten (Write-Anomalie).
    SuspendAgent(AgentId),
    /// Agent-Sandbox teardown + Despawn (Stall-Recovery, Respawn bei naechstem Shift-Check).
    RestartAgent(AgentId),
    /// Systemd-Service direkt neu starten.
    RestartService(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct WriteAnomalyAssessment {
    pub baseline_bytes_per_sec: Option<f64>,
    pub baseline_triggered: bool,
    pub absolute_triggered: bool,
}

/// Evaluiert alle Regeln gegen die aktuellen Metriken.
///
/// Prueft Cooldowns und gibt nur feuernde Regeln zurueck.
pub fn evaluate_rules(
    metrics: &PlatformMetrics,
    cooldowns: &HashMap<String, u64>,
    tick: u64,
    config: &PlatformControlplaneConfig,
    write_rate_baselines: &HashMap<String, f64>,
    agent_name_to_id: &HashMap<String, AgentId>,
) -> Vec<PlatformAction> {
    let mut actions = Vec::new();

    // Regel 1: Agent Stall → Restart
    // Agents mit kuerzlicher Activity (letzte 120 Ticks) ueberspringen:
    // Synthesis-Agents machen 0 Kernel-Syscalls → eBPF meldet sie als "stalled",
    // aber sie produzieren aktiv Actions. Erst despawnen wenn WIRKLICH tot.
    for agent_name in &metrics.stalled_agents {
        // Agent hat kuerzlich eine Action ausgefuehrt → nicht stalled
        if let Some(&last_tick) = metrics.last_action_ticks.get(agent_name) {
            if tick >= last_tick
                && tick.saturating_sub(last_tick) < config.stall_recent_activity_grace_ticks
            {
                continue;
            }
        }
        let key = format!("agent_stall:{agent_name}");
        if !is_cooled_down(cooldowns, &key, tick, config.stall_cooldown_ticks) {
            continue;
        }
        let side_effect = agent_name_to_id
            .get(agent_name)
            .map(|id| PlatformSideEffect::RestartAgent(*id));
        actions.push(PlatformAction {
            rule_name: "agent_stall".to_string(),
            target: agent_name.clone(),
            action_label: "restart_triggered".to_string(),
            description: format!("Agent {agent_name} ist gestalled — Restart getriggert"),
            side_effect,
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
                action_label: "restart_triggered".to_string(),
                description: format!(
                    "Projection Lag {} > {} Schwellwert — Restart von sentinel-projection getriggert",
                    metrics.projection_lag, config.max_projection_lag
                ),
                side_effect: Some(PlatformSideEffect::RestartService(
                    "sentinel-projection".to_string(),
                )),
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
            let agent_id = agent_name_to_id.get(agent_name).copied();
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

    // Regel 5: Write-Rate Anomalie
    for (agent_name, rate) in &metrics.agent_write_rates {
        let Some(assessment) =
            assess_write_anomaly(*rate, write_rate_baselines.get(agent_name).copied(), config)
        else {
            continue;
        };
        let Some(agent_id) = agent_name_to_id.get(agent_name).copied() else {
            continue;
        };
        let key = format!("write_anomaly:{agent_name}");
        if !is_cooled_down(cooldowns, &key, tick, config.write_anomaly_cooldown_ticks) {
            continue;
        }
        let rate_mb = rate / (1024.0 * 1024.0);
        let threshold_mb = config.write_anomaly_threshold_bytes_per_sec as f64 / (1024.0 * 1024.0);
        let baseline_clause = assessment
            .baseline_bytes_per_sec
            .filter(|baseline| *baseline > 0.0)
            .map(|baseline| {
                format!(
                    "{:.1}x Baseline ({:.2} MB/s)",
                    rate / baseline,
                    baseline / (1024.0 * 1024.0)
                )
            });
        let trigger_clause = match (
            assessment.absolute_triggered,
            assessment.baseline_triggered,
            baseline_clause,
        ) {
            (true, true, Some(baseline)) => {
                format!(">{threshold_mb:.1} MB/s absolute and > {baseline}")
            }
            (true, false, _) => format!(">{threshold_mb:.1} MB/s absolute"),
            (false, true, Some(baseline)) => format!("> {baseline}"),
            _ => format!(">{threshold_mb:.1} MB/s absolute"),
        };
        actions.push(PlatformAction {
            rule_name: "write_anomaly".to_string(),
            target: agent_name.clone(),
            action_label: "sigstop".to_string(),
            description: format!(
                "Write-Rate {rate_mb:.1} MB/s {trigger_clause} — SIGSTOP fuer Agent-Cgroup getriggert"
            ),
            side_effect: Some(PlatformSideEffect::SuspendAgent(agent_id)),
        });
    }

    // Regel 6: Service Health (aus ServiceHealthChecker Thread)
    for service_name in &metrics.failed_services {
        let key = format!("service_health:{service_name}");
        if !is_cooled_down(cooldowns, &key, tick, config.stall_cooldown_ticks) {
            continue;
        }
        actions.push(PlatformAction {
            rule_name: "service_health".to_string(),
            target: service_name.clone(),
            action_label: "restart_triggered".to_string(),
            description: format!("Service {service_name} ist nicht active — Restart getriggert"),
            side_effect: Some(PlatformSideEffect::RestartService(service_name.clone())),
        });
    }

    actions
}

pub fn assess_write_anomaly(
    rate_bytes_per_sec: f64,
    baseline_bytes_per_sec: Option<f64>,
    config: &PlatformControlplaneConfig,
) -> Option<WriteAnomalyAssessment> {
    let absolute_triggered =
        rate_bytes_per_sec > config.write_anomaly_threshold_bytes_per_sec as f64;
    let baseline_triggered = baseline_bytes_per_sec
        .filter(|baseline| *baseline > 0.0)
        .map(|baseline| rate_bytes_per_sec > baseline * config.write_anomaly_baseline_multiplier)
        .unwrap_or(false);

    if absolute_triggered || baseline_triggered {
        Some(WriteAnomalyAssessment {
            baseline_bytes_per_sec,
            baseline_triggered,
            absolute_triggered,
        })
    } else {
        None
    }
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
    fn test_stall_rule_fires_for_stalled_agent() {
        let metrics = PlatformMetrics {
            stalled_agents: vec!["Thomas Mueller".to_string()],
            ..Default::default()
        };
        let actions = evaluate_rules(
            &metrics,
            &HashMap::new(),
            100,
            &test_config(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].rule_name, "agent_stall");
        assert_eq!(actions[0].target, "Thomas Mueller");
    }

    #[test]
    fn test_stall_rule_respects_recent_activity_grace_from_config() {
        let metrics = PlatformMetrics {
            stalled_agents: vec!["Thomas Mueller".to_string()],
            last_action_ticks: HashMap::from([("Thomas Mueller".to_string(), 91)]),
            ..Default::default()
        };
        let config = PlatformControlplaneConfig {
            cycle_interval_ticks: 1,
            stall_recent_activity_grace_ticks: 10,
            ..test_config()
        };

        let actions = evaluate_rules(
            &metrics,
            &HashMap::new(),
            100,
            &config,
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(
            actions.is_empty(),
            "recent activity inside grace window must suppress stall restarts"
        );
    }

    #[test]
    fn test_stall_rule_does_not_skip_when_activity_tick_is_from_older_epoch() {
        let metrics = PlatformMetrics {
            stalled_agents: vec!["Thomas Mueller".to_string()],
            last_action_ticks: HashMap::from([("Thomas Mueller".to_string(), 70140)]),
            ..Default::default()
        };
        let config = PlatformControlplaneConfig {
            cycle_interval_ticks: 1,
            stall_recent_activity_grace_ticks: 10,
            ..test_config()
        };

        let actions = evaluate_rules(
            &metrics,
            &HashMap::new(),
            37,
            &config,
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(
            actions.len(),
            1,
            "stalled agents must not stay protected when current_tick is from a newer runtime epoch"
        );
        assert_eq!(actions[0].rule_name, "agent_stall");
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
        let actions = evaluate_rules(
            &metrics,
            &cooldowns,
            100,
            &test_config(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(actions.is_empty(), "Should be cooled down");
    }

    #[test]
    fn test_event_store_size_rule_fires() {
        let metrics = PlatformMetrics {
            event_store_size_bytes: 600 * 1024 * 1024, // 600 MB > 500 MB
            ..Default::default()
        };
        let actions = evaluate_rules(
            &metrics,
            &HashMap::new(),
            100,
            &test_config(),
            &HashMap::new(),
            &HashMap::new(),
        );
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
        let actions = evaluate_rules(
            &metrics,
            &HashMap::new(),
            100,
            &test_config(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].rule_name, "projection_lag");
        assert_eq!(actions[0].action_label, "restart_triggered");
        assert!(matches!(
            &actions[0].side_effect,
            Some(PlatformSideEffect::RestartService(service)) if service == "sentinel-projection"
        ));
    }

    #[test]
    fn test_memory_pressure_rule_fires() {
        let metrics = PlatformMetrics {
            agent_memory_pressure: vec![("Test Agent".to_string(), 0.95)],
            ..Default::default()
        };
        let actions = evaluate_rules(
            &metrics,
            &HashMap::new(),
            100,
            &test_config(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].rule_name, "memory_pressure");
    }

    #[test]
    fn test_write_anomaly_assessment_triggers_on_baseline_multiplier() {
        let config = PlatformControlplaneConfig {
            write_anomaly_threshold_bytes_per_sec: 50_000_000,
            write_anomaly_baseline_multiplier: 10.0,
            ..test_config()
        };
        let assessment = assess_write_anomaly(12_000.0, Some(1_000.0), &config)
            .expect("baseline should trigger");
        assert!(assessment.baseline_triggered);
        assert!(!assessment.absolute_triggered);
    }

    #[test]
    fn test_write_anomaly_assessment_triggers_on_absolute_threshold() {
        let config = PlatformControlplaneConfig {
            write_anomaly_threshold_bytes_per_sec: 5_000,
            write_anomaly_baseline_multiplier: 10.0,
            ..test_config()
        };
        let assessment =
            assess_write_anomaly(6_000.0, Some(1_000.0), &config).expect("absolute should trigger");
        assert!(assessment.absolute_triggered);
    }

    #[test]
    fn test_write_anomaly_rule_fires() {
        let metrics = PlatformMetrics {
            agent_write_rates: vec![("Test Agent".to_string(), 10_000_000.0)], // 10 MB/s > 5 MB/s
            ..Default::default()
        };
        let agent_name_to_id = HashMap::from([("Test Agent".to_string(), AgentId(7))]);
        let actions = evaluate_rules(
            &metrics,
            &HashMap::new(),
            100,
            &test_config(),
            &HashMap::new(),
            &agent_name_to_id,
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].rule_name, "write_anomaly");
        assert_eq!(actions[0].action_label, "sigstop");
        assert!(matches!(
            &actions[0].side_effect,
            Some(PlatformSideEffect::SuspendAgent(id)) if *id == AgentId(7)
        ));
    }

    #[test]
    fn test_write_anomaly_rule_healthy() {
        let metrics = PlatformMetrics {
            agent_write_rates: vec![("Test Agent".to_string(), 1_000_000.0)], // 1 MB/s < 5 MB/s
            ..Default::default()
        };
        let actions = evaluate_rules(
            &metrics,
            &HashMap::new(),
            100,
            &test_config(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn test_write_anomaly_rule_fires_on_baseline_without_absolute_threshold() {
        let config = PlatformControlplaneConfig {
            write_anomaly_threshold_bytes_per_sec: 50_000_000,
            write_anomaly_baseline_multiplier: 10.0,
            ..test_config()
        };
        let metrics = PlatformMetrics {
            agent_write_rates: vec![("Test Agent".to_string(), 12_000.0)],
            ..Default::default()
        };
        let baselines = HashMap::from([("Test Agent".to_string(), 1_000.0)]);
        let agent_name_to_id = HashMap::from([("Test Agent".to_string(), AgentId(9))]);
        let actions = evaluate_rules(
            &metrics,
            &HashMap::new(),
            100,
            &config,
            &baselines,
            &agent_name_to_id,
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].action_label, "sigstop");
        assert!(actions[0].description.contains("Baseline"));
    }

    #[test]
    fn test_stall_rule_generates_restart_side_effect() {
        let metrics = PlatformMetrics {
            stalled_agents: vec!["Thomas Mueller".to_string()],
            ..Default::default()
        };
        let mut name_to_id = HashMap::new();
        name_to_id.insert("Thomas Mueller".to_string(), AgentId(1));

        let actions = evaluate_rules(
            &metrics,
            &HashMap::new(),
            100,
            &test_config(),
            &HashMap::new(),
            &name_to_id,
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].action_label, "restart_triggered");
        assert!(matches!(
            &actions[0].side_effect,
            Some(PlatformSideEffect::RestartAgent(id)) if *id == AgentId(1)
        ));
    }

    #[test]
    fn test_service_health_rule_fires_for_down_service() {
        let metrics = PlatformMetrics {
            failed_services: vec!["sentinel-judge".to_string()],
            ..Default::default()
        };
        let actions = evaluate_rules(
            &metrics,
            &HashMap::new(),
            100,
            &test_config(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].rule_name, "service_health");
        assert_eq!(actions[0].target, "sentinel-judge");
        assert_eq!(actions[0].action_label, "restart_triggered");
        assert!(matches!(
            &actions[0].side_effect,
            Some(PlatformSideEffect::RestartService(service)) if service == "sentinel-judge"
        ));
    }

    #[test]
    fn test_no_rules_fire_when_healthy() {
        let metrics = PlatformMetrics {
            event_store_size_bytes: 100 * 1024 * 1024, // 100 MB < 500 MB
            projection_lag: 50,                        // < 10_000
            ..Default::default()
        };
        let actions = evaluate_rules(
            &metrics,
            &HashMap::new(),
            100,
            &test_config(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(actions.is_empty());
    }
}
