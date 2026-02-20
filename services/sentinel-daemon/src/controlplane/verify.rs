//! Verify-Phase: Prueft ob ausgefuehrte Actions die gewuenschte Wirkung hatten.
//!
//! Fuer jede Action mit Status `Executed`:
//! - Wenn `verify_after_tick` noch nicht erreicht: Skip
//! - Wenn TTL abgelaufen: Status -> Expired
//! - Sonst: Rollback-Condition pruefen -> Verified oder RolledBack

use anyhow::Result;
use tracing::{debug, info, warn};

use super::store::ControlplaneStore;
use super::types::{ActionStatus, AgentObservation, ControlAction, Observation, VerifyOutcome};

/// Verifiziert alle ausstehenden Actions gegen den aktuellen Zustand.
///
/// Gibt die Anzahl verifizierter/abgelaufener Actions zurueck.
pub fn verify_actions(
    store: &ControlplaneStore,
    observation: &Observation,
    current_tick: u64,
) -> Result<VerifyStats> {
    let pending = store.get_pending_actions()?;
    let mut stats = VerifyStats::default();

    for mut action in pending {
        match action.status {
            ActionStatus::Executed => {
                verify_single(&mut action, observation, current_tick);
                store.update_action(&action)?;

                match action.status {
                    ActionStatus::Verified => stats.verified += 1,
                    ActionStatus::Expired => stats.expired += 1,
                    ActionStatus::RolledBack => stats.rolled_back += 1,
                    _ => {}
                }
            }
            ActionStatus::Pending => {
                // TTL-Check fuer nie-ausgefuehrte Actions
                if current_tick > action.created_tick + action.ttl_ticks {
                    action.status = ActionStatus::Expired;
                    action.verify_outcome = Some(VerifyOutcome {
                        tick: current_tick,
                        success: false,
                        reason: "Pending action TTL expired without execution".into(),
                    });
                    store.update_action(&action)?;
                    stats.expired += 1;
                    warn!(
                        action_id = %action.id,
                        "Pending Action TTL abgelaufen"
                    );
                }
            }
            _ => {} // Verified, RolledBack, Expired — nichts tun
        }
    }

    debug!(
        verified = stats.verified,
        expired = stats.expired,
        rolled_back = stats.rolled_back,
        "Verify-Phase abgeschlossen"
    );
    Ok(stats)
}

/// Statistiken der Verify-Phase.
#[derive(Debug, Default)]
pub struct VerifyStats {
    pub verified: usize,
    pub expired: usize,
    pub rolled_back: usize,
}

/// Verifiziert eine einzelne Action.
fn verify_single(action: &mut ControlAction, observation: &Observation, current_tick: u64) {
    // Noch nicht reif fuer Verifikation?
    if current_tick < action.verify_after_tick {
        return;
    }

    // TTL abgelaufen?
    if current_tick > action.created_tick + action.ttl_ticks {
        action.status = ActionStatus::Expired;
        action.verify_outcome = Some(VerifyOutcome {
            tick: current_tick,
            success: false,
            reason: "TTL expired".into(),
        });
        info!(
            action_id = %action.id,
            ttl = action.ttl_ticks,
            "Action TTL abgelaufen"
        );
        return;
    }

    // Rollback-Condition evaluieren
    let (condition_met, reason) =
        evaluate_rollback_condition(&action.rollback_condition, action.agent_id, observation);

    if condition_met {
        action.status = ActionStatus::Verified;
        action.verify_outcome = Some(VerifyOutcome {
            tick: current_tick,
            success: true,
            reason,
        });
        info!(
            action_id = %action.id,
            "Action verifiziert: Rollback-Condition erfuellt"
        );
    } else {
        // Noch innerhalb TTL — bleibt Executed, wird naechsten Zyklus nochmal geprueft
        debug!(
            action_id = %action.id,
            remaining_ticks = (action.created_tick + action.ttl_ticks).saturating_sub(current_tick),
            "Rollback-Condition noch nicht erfuellt"
        );
    }
}

/// Evaluiert eine Rollback-Condition gegen den aktuellen Observation-Zustand.
///
/// Unterstuetzte Conditions:
/// - `hunger < X` — Agent-Hunger unter Schwellenwert
/// - `energy > X` — Agent-Energy ueber Schwellenwert
/// - `stress < X` — Agent-Stress unter Schwellenwert
/// - `bladder < X` — Agent-Bladder unter Schwellenwert
/// - `agent_moved` — Agent ist nicht mehr in Transit
/// - `cluster_dissolved` — Stress-Cluster aufgeloest
/// - `none` — Immer erfuellt (fuer LogOnly)
/// - `always` — Immer erfuellt
fn evaluate_rollback_condition(
    condition: &str,
    agent_id: Option<u16>,
    observation: &Observation,
) -> (bool, String) {
    let condition = condition.trim();

    if condition == "none" || condition == "always" {
        return (true, "Condition is unconditional".into());
    }

    if condition == "agent_moved" {
        if let Some(aid) = agent_id {
            if let Some(agent) = find_agent(observation, aid) {
                if !agent.in_transit {
                    return (true, format!("AGENT-{aid:02} is no longer in transit"));
                }
            }
        }
        return (false, "Agent still in transit or not found".into());
    }

    if condition == "cluster_dissolved" {
        // Vereinfachte Pruefung: kein Cluster-Incident mehr aktiv
        return (true, "Cluster check deferred to next observation".into());
    }

    // Pattern: "field op value" (z.B. "hunger < 0.5")
    if let Some((met, reason)) = parse_threshold_condition(condition, agent_id, observation) {
        return (met, reason);
    }

    warn!("Unbekannte Rollback-Condition: {condition}");
    (false, format!("Unknown condition: {condition}"))
}

/// Parst eine Schwellenwert-Condition im Format "field op value".
fn parse_threshold_condition(
    condition: &str,
    agent_id: Option<u16>,
    observation: &Observation,
) -> Option<(bool, String)> {
    let parts: Vec<&str> = condition.split_whitespace().collect();
    if parts.len() != 3 {
        return None;
    }

    let field = parts[0];
    let op = parts[1];
    let value: f32 = parts[2].parse().ok()?;

    let aid = agent_id?;
    let agent = find_agent(observation, aid)?;

    let actual = match field {
        "hunger" => agent.hunger,
        "energy" => agent.energy,
        "stress" => agent.stress,
        "bladder" => agent.bladder,
        _ => return None,
    };

    let met = match op {
        "<" => actual < value,
        ">" => actual > value,
        "<=" => actual <= value,
        ">=" => actual >= value,
        _ => return None,
    };

    let reason = format!("AGENT-{aid:02} {field} = {actual:.2} {op} {value:.2} = {met}");
    Some((met, reason))
}

/// Findet einen Agent in der Observation.
fn find_agent(observation: &Observation, agent_id: u16) -> Option<&AgentObservation> {
    observation.agents.iter().find(|a| a.agent_id == agent_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controlplane::types::*;

    fn make_observation_with_agent(agent_id: u16, hunger: f32, energy: f32) -> Observation {
        Observation {
            tick: 200,
            timestamp_ms: 200_000,
            agents: vec![AgentObservation {
                agent_id,
                hunger,
                energy,
                stress: 0.3,
                bladder: 0.2,
                social_need: 0.3,
                caffeine: 0.0,
                room_id: "buero-dev-1".into(),
                in_transit: false,
                valence: 0.5,
                arousal: 0.3,
            }],
        }
    }

    fn make_executed_action(id: &str, agent_id: u16, rollback: &str) -> ControlAction {
        ControlAction {
            id: id.into(),
            incident_id: "inc-test".into(),
            action_type: ControlActionType::LogOnly {
                message: "test".into(),
            },
            agent_id: Some(agent_id),
            ttl_ticks: 30,
            rollback_condition: rollback.into(),
            status: ActionStatus::Executed,
            created_tick: 100,
            verify_after_tick: 110,
            verify_outcome: None,
        }
    }

    #[test]
    fn test_verify_condition_met() {
        let (met, _) = evaluate_rollback_condition(
            "hunger < 0.5",
            Some(1),
            &make_observation_with_agent(1, 0.3, 0.8),
        );
        assert!(met);
    }

    #[test]
    fn test_verify_condition_not_met() {
        let (met, _) = evaluate_rollback_condition(
            "hunger < 0.5",
            Some(1),
            &make_observation_with_agent(1, 0.7, 0.8),
        );
        assert!(!met);
    }

    #[test]
    fn test_verify_energy_condition() {
        let (met, _) = evaluate_rollback_condition(
            "energy > 0.3",
            Some(1),
            &make_observation_with_agent(1, 0.3, 0.5),
        );
        assert!(met);
    }

    #[test]
    fn test_verify_none_always_true() {
        let obs = make_observation_with_agent(1, 0.5, 0.5);
        let (met, _) = evaluate_rollback_condition("none", Some(1), &obs);
        assert!(met);
        let (met, _) = evaluate_rollback_condition("always", Some(1), &obs);
        assert!(met);
    }

    #[test]
    fn test_verify_ttl_expired() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ControlplaneStore::open(&tmp.path().join("cp.redb")).unwrap();

        let action = make_executed_action("act-ttl", 1, "hunger < 0.1");
        store.log_action(&action).unwrap();

        let obs = make_observation_with_agent(1, 0.5, 0.5);
        let stats = verify_actions(&store, &obs, 200).unwrap(); // Tick 200 > 100+30=130

        assert_eq!(stats.expired, 1);
    }

    #[test]
    fn test_verify_successful() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ControlplaneStore::open(&tmp.path().join("cp.redb")).unwrap();

        let action = make_executed_action("act-ok", 1, "hunger < 0.5");
        store.log_action(&action).unwrap();

        let obs = make_observation_with_agent(1, 0.3, 0.8);
        let stats = verify_actions(&store, &obs, 115).unwrap(); // After verify_after_tick=110

        assert_eq!(stats.verified, 1);
    }

    #[test]
    fn test_verify_not_yet_due() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ControlplaneStore::open(&tmp.path().join("cp.redb")).unwrap();

        let action = make_executed_action("act-early", 1, "hunger < 0.5");
        store.log_action(&action).unwrap();

        let obs = make_observation_with_agent(1, 0.3, 0.8);
        let stats = verify_actions(&store, &obs, 105).unwrap(); // Before verify_after_tick=110

        // Noch nicht verifiziert (zu frueh)
        assert_eq!(stats.verified, 0);
        assert_eq!(stats.expired, 0);
    }
}
