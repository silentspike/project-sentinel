//! Decide-Phase: Wendet Policies auf Incidents an, erzeugt Actions.
//!
//! Rein regelbasiert — KEIN LLM im Echtzeitpfad (AC-N1).
//! Jede Action hat `ttl_ticks`, `rollback_condition` und `verify_after_tick` (AC-5).

use std::collections::HashSet;

use tracing::debug;

use super::config::ControlplaneConfig;
use super::types::{ActionStatus, ControlAction, ControlActionType, Incident, IncidentType};

/// Entscheidet welche Actions fuer die erkannten Incidents ausgefuehrt werden sollen.
///
/// Beruecksichtigt:
/// - Guarded Mode (nur LogOnly)
/// - Cooldown (keine Action wenn kuerzlich bereits eine fuer denselben Agent+Typ lief)
/// - Keine Duplikate innerhalb eines Zyklus
pub fn decide(
    incidents: &[Incident],
    config: &ControlplaneConfig,
    recent_action_keys: &HashSet<String>,
) -> Vec<ControlAction> {
    let mut actions = Vec::new();

    for incident in incidents {
        let cooldown_key = cooldown_key_for(incident);

        // Cooldown-Check: Skip wenn kuerzlich bereits gehandelt
        if recent_action_keys.contains(&cooldown_key) {
            debug!(
                incident_id = %incident.id,
                cooldown_key = %cooldown_key,
                "Incident uebersprungen (Cooldown aktiv)"
            );
            continue;
        }

        let action = if config.guarded_mode {
            // Guarded Mode: Nur loggen, keine Zustandsaenderung
            create_log_action(incident, config)
        } else {
            // Produktiv: Action basierend auf Incident-Typ
            create_action_for_incident(incident, config)
        };

        actions.push(action);
    }

    debug!(
        incident_count = incidents.len(),
        action_count = actions.len(),
        guarded = config.guarded_mode,
        "Entscheidungen getroffen"
    );
    actions
}

/// Erzeugt einen Cooldown-Key fuer einen Incident.
/// Format: `{incident_type}:{agent_id_oder_room}`
fn cooldown_key_for(incident: &Incident) -> String {
    let type_str = match incident.incident_type {
        IncidentType::HungerCritical => "hunger",
        IncidentType::EnergyDepleted => "energy",
        IncidentType::StressCritical => "stress",
        IncidentType::BladderCritical => "bladder",
        IncidentType::AgentStuck => "stuck",
        IncidentType::HighStressCluster => "cluster",
    };
    match incident.agent_id {
        Some(aid) => format!("{type_str}:{aid}"),
        None => format!("{type_str}:system"),
    }
}

/// Erzeugt eine LogOnly-Action (fuer Guarded Mode).
fn create_log_action(incident: &Incident, config: &ControlplaneConfig) -> ControlAction {
    ControlAction {
        id: format!("act-{}-log", incident.id),
        incident_id: incident.id.clone(),
        action_type: ControlActionType::LogOnly {
            message: format!(
                "[GUARDED] {}: {}",
                format_incident_type(incident.incident_type),
                incident.description
            ),
        },
        agent_id: incident.agent_id,
        ttl_ticks: config.default_ttl_ticks,
        rollback_condition: "none".into(),
        status: ActionStatus::Pending,
        created_tick: incident.tick,
        verify_after_tick: incident.tick + config.default_ttl_ticks,
        verify_outcome: None,
    }
}

/// Erzeugt eine produktive Action basierend auf dem Incident-Typ.
fn create_action_for_incident(incident: &Incident, config: &ControlplaneConfig) -> ControlAction {
    let (action_type, rollback_condition) = match incident.incident_type {
        IncidentType::HungerCritical => (
            ControlActionType::EmitEvent {
                event_type: "controlplane_intervention".into(),
                description: format!(
                    "Hunger-Intervention fuer AGENT-{:02}",
                    incident.agent_id.unwrap_or(0)
                ),
            },
            "hunger < 0.5".into(),
        ),
        IncidentType::EnergyDepleted => (
            ControlActionType::EmitEvent {
                event_type: "controlplane_intervention".into(),
                description: format!(
                    "Energy-Intervention fuer AGENT-{:02}",
                    incident.agent_id.unwrap_or(0)
                ),
            },
            "energy > 0.3".into(),
        ),
        IncidentType::StressCritical => (
            ControlActionType::EmitEvent {
                event_type: "controlplane_intervention".into(),
                description: format!(
                    "Stress-Intervention fuer AGENT-{:02}",
                    incident.agent_id.unwrap_or(0)
                ),
            },
            "stress < 0.5".into(),
        ),
        IncidentType::BladderCritical => (
            ControlActionType::EmitEvent {
                event_type: "controlplane_intervention".into(),
                description: format!(
                    "Bladder-Intervention fuer AGENT-{:02}",
                    incident.agent_id.unwrap_or(0)
                ),
            },
            "bladder < 0.3".into(),
        ),
        IncidentType::AgentStuck => (
            ControlActionType::EmitEvent {
                event_type: "controlplane_intervention".into(),
                description: format!(
                    "Stuck-Agent-Intervention fuer AGENT-{:02}",
                    incident.agent_id.unwrap_or(0)
                ),
            },
            "agent_moved".into(),
        ),
        IncidentType::HighStressCluster => (
            ControlActionType::EmitEvent {
                event_type: "controlplane_cluster_alert".into(),
                description: incident.description.clone(),
            },
            "cluster_dissolved".into(),
        ),
    };

    ControlAction {
        id: format!("act-{}", incident.id),
        incident_id: incident.id.clone(),
        action_type,
        agent_id: incident.agent_id,
        ttl_ticks: config.default_ttl_ticks,
        rollback_condition,
        status: ActionStatus::Pending,
        created_tick: incident.tick,
        verify_after_tick: incident.tick + config.default_ttl_ticks,
        verify_outcome: None,
    }
}

fn format_incident_type(it: IncidentType) -> &'static str {
    match it {
        IncidentType::HungerCritical => "HUNGER_CRITICAL",
        IncidentType::EnergyDepleted => "ENERGY_DEPLETED",
        IncidentType::StressCritical => "STRESS_CRITICAL",
        IncidentType::BladderCritical => "BLADDER_CRITICAL",
        IncidentType::AgentStuck => "AGENT_STUCK",
        IncidentType::HighStressCluster => "HIGH_STRESS_CLUSTER",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controlplane::config::{ControlplaneConfig, ThresholdConfig};
    use crate::controlplane::types::Severity;

    fn test_config() -> ControlplaneConfig {
        ControlplaneConfig {
            cycle_interval_ticks: 10,
            guarded_mode: false,
            thresholds: ThresholdConfig {
                hunger_critical: 0.9,
                energy_critical: 0.15,
                stress_critical: 0.85,
                bladder_critical: 0.9,
            },
            default_ttl_ticks: 30,
            cooldown_ticks: 60,
        }
    }

    fn make_incident(incident_type: IncidentType, agent_id: Option<u16>) -> Incident {
        Incident {
            id: format!("inc-100-test-{}", agent_id.unwrap_or(0)),
            tick: 100,
            timestamp_ms: 100_000,
            incident_type,
            severity: Severity::High,
            agent_id,
            description: "Test incident".into(),
        }
    }

    #[test]
    fn test_decide_produces_actions() {
        let incidents = vec![make_incident(IncidentType::HungerCritical, Some(1))];
        let actions = decide(&incidents, &test_config(), &HashSet::new());
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].agent_id, Some(1));
        assert_eq!(actions[0].status, ActionStatus::Pending);
        assert_eq!(actions[0].ttl_ticks, 30);
    }

    #[test]
    fn test_guarded_mode_log_only() {
        let mut config = test_config();
        config.guarded_mode = true;
        let incidents = vec![make_incident(IncidentType::StressCritical, Some(5))];
        let actions = decide(&incidents, &config, &HashSet::new());
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            actions[0].action_type,
            ControlActionType::LogOnly { .. }
        ));
    }

    #[test]
    fn test_cooldown_skips_duplicate() {
        let incidents = vec![make_incident(IncidentType::HungerCritical, Some(1))];
        let mut recent = HashSet::new();
        recent.insert("hunger:1".into());
        let actions = decide(&incidents, &test_config(), &recent);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_action_has_ttl_and_rollback() {
        let incidents = vec![make_incident(IncidentType::EnergyDepleted, Some(3))];
        let actions = decide(&incidents, &test_config(), &HashSet::new());
        assert_eq!(actions[0].ttl_ticks, 30);
        assert_eq!(actions[0].rollback_condition, "energy > 0.3");
        assert_eq!(actions[0].verify_after_tick, 130);
    }

    #[test]
    fn test_cluster_incident_no_agent_id() {
        let incidents = vec![make_incident(IncidentType::HighStressCluster, None)];
        let actions = decide(&incidents, &test_config(), &HashSet::new());
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].agent_id, None);
    }
}
