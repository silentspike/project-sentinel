//! Act-Phase: Fuehrt entschiedene Actions aus.
//!
//! Jede Action wird ausgefuehrt, ihr Status auf `Executed` gesetzt
//! und im ControlplaneStore persistiert.

use anyhow::Result;
use tracing::{debug, info, warn};

use super::store::ControlplaneStore;
use super::types::{ActionStatus, ControlAction, ControlActionType};

/// Fuehrt eine Liste von Actions aus und persistiert sie.
///
/// Actions werden sequentiell ausgefuehrt. Bei Fehler wird die
/// Action als `Pending` belassen (retry im naechsten Zyklus).
pub fn execute_actions(actions: &mut [ControlAction], store: &ControlplaneStore) -> Result<usize> {
    let executed_count = execute_actions_no_store(actions)?;

    // Batch-Write: alle ausgefuehrten Actions in einer Transaktion persistieren
    let executed: Vec<_> = actions
        .iter()
        .filter(|a| a.status == ActionStatus::Executed)
        .cloned()
        .collect();
    store.log_actions_batch(&executed)?;

    Ok(executed_count)
}

/// Fuehrt Actions in-memory aus OHNE Store-Write.
///
/// Fuer den Single-Transaction-Pfad: cycle() sammelt alle Writes
/// und persistiert sie in einer einzigen redb-Transaktion.
pub fn execute_actions_no_store(actions: &mut [ControlAction]) -> Result<usize> {
    let mut executed_count = 0;

    for action in actions.iter_mut() {
        match execute_single(action) {
            Ok(()) => {
                action.status = ActionStatus::Executed;
                executed_count += 1;
            }
            Err(e) => {
                warn!(
                    action_id = %action.id,
                    error = %e,
                    "Action-Ausfuehrung fehlgeschlagen, bleibt Pending"
                );
            }
        }
    }

    debug!(
        total = actions.len(),
        executed = executed_count,
        "Actions ausgefuehrt"
    );
    Ok(executed_count)
}

/// Fuehrt eine einzelne Action aus.
fn execute_single(action: &ControlAction) -> Result<()> {
    match &action.action_type {
        ControlActionType::LogOnly { message } => {
            info!(
                action_id = %action.id,
                incident_id = %action.incident_id,
                agent_id = ?action.agent_id,
                message = %message,
                "Controlplane LogOnly"
            );
            Ok(())
        }
        ControlActionType::EmitEvent {
            event_type,
            description,
        } => {
            // Events werden geloggt — die eigentliche Event-Emission
            // erfolgt ueber den bestehenden EventStore-Pfad im ECS.
            // Hier protokollieren wir die Intervention.
            info!(
                action_id = %action.id,
                incident_id = %action.incident_id,
                agent_id = ?action.agent_id,
                event_type = %event_type,
                description = %description,
                ttl_ticks = action.ttl_ticks,
                rollback_condition = %action.rollback_condition,
                "Controlplane EmitEvent"
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controlplane::types::{ActionStatus, ControlAction, ControlActionType};

    fn make_log_action(id: &str) -> ControlAction {
        ControlAction {
            id: id.into(),
            incident_id: "inc-test".into(),
            action_type: ControlActionType::LogOnly {
                message: "test log".into(),
            },
            agent_id: Some(1),
            ttl_ticks: 30,
            rollback_condition: "none".into(),
            status: ActionStatus::Pending,
            created_tick: 100,
            verify_after_tick: 130,
            verify_outcome: None,
        }
    }

    fn make_emit_action(id: &str) -> ControlAction {
        ControlAction {
            id: id.into(),
            incident_id: "inc-test".into(),
            action_type: ControlActionType::EmitEvent {
                event_type: "controlplane_intervention".into(),
                description: "test intervention".into(),
            },
            agent_id: Some(2),
            ttl_ticks: 30,
            rollback_condition: "stress < 0.5".into(),
            status: ActionStatus::Pending,
            created_tick: 100,
            verify_after_tick: 130,
            verify_outcome: None,
        }
    }

    #[test]
    fn test_execute_log_action() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ControlplaneStore::open(&tmp.path().join("cp.redb")).unwrap();

        let mut actions = vec![make_log_action("act-1")];
        let count = execute_actions(&mut actions, &store).unwrap();
        assert_eq!(count, 1);
        assert_eq!(actions[0].status, ActionStatus::Executed);
    }

    #[test]
    fn test_execute_emit_action() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ControlplaneStore::open(&tmp.path().join("cp.redb")).unwrap();

        let mut actions = vec![make_emit_action("act-2")];
        let count = execute_actions(&mut actions, &store).unwrap();
        assert_eq!(count, 1);
        assert_eq!(actions[0].status, ActionStatus::Executed);
    }

    #[test]
    fn test_execute_multiple_actions() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ControlplaneStore::open(&tmp.path().join("cp.redb")).unwrap();

        let mut actions = vec![
            make_log_action("act-a"),
            make_emit_action("act-b"),
            make_log_action("act-c"),
        ];
        let count = execute_actions(&mut actions, &store).unwrap();
        assert_eq!(count, 3);

        // Alle als Executed persistiert (get_pending_actions liefert Pending + Executed)
        let stored = store.get_pending_actions().unwrap();
        assert_eq!(stored.len(), 3);
        assert!(stored.iter().all(|a| a.status == ActionStatus::Executed));
    }

    #[test]
    fn test_action_persisted_in_store() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ControlplaneStore::open(&tmp.path().join("cp.redb")).unwrap();

        let mut actions = vec![make_log_action("act-persist")];
        execute_actions(&mut actions, &store).unwrap();

        // Verify: Action im Store mit Status Executed
        // get_pending_actions filtert nur Pending/Executed
        // Nach Execute ist Status = Executed, also sollte es noch da sein
        // Wait, get_pending_actions returns Pending OR Executed
        let stored = store.get_pending_actions().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].status, ActionStatus::Executed);
    }
}
