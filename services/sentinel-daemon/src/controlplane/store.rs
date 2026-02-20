//! Controlplane-Persistenz mit redb.
//!
//! 4 Tabellen: CONFIG, RUNTIME_STATE, ACTION_LOG, INCIDENTS.
//! Separates redb-File (`controlplane.redb`) im data_dir.

use std::path::Path;

use anyhow::{Context, Result};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use tracing::debug;

use super::types::{ControlAction, Incident, RuntimeState};

/// Policy-Config (key=policy_name).
const CONTROL_CONFIG: TableDefinition<&str, &[u8]> = TableDefinition::new("control_config");

/// Runtime-State (key="state").
const CONTROL_RUNTIME_STATE: TableDefinition<&str, &[u8]> =
    TableDefinition::new("control_runtime_state");

/// Action-Log (key=action_id).
const CONTROL_ACTION_LOG: TableDefinition<&str, &[u8]> = TableDefinition::new("control_action_log");

/// Incidents (key=incident_id).
const CONTROL_INCIDENTS: TableDefinition<&str, &[u8]> = TableDefinition::new("control_incidents");

const RUNTIME_STATE_KEY: &str = "state";

/// Controlplane-spezifischer redb Store.
pub struct ControlplaneStore {
    db: Database,
}

impl ControlplaneStore {
    /// Oeffnet oder erstellt die Controlplane-Datenbank.
    pub fn open(path: &Path) -> Result<Self> {
        let db = Database::create(path)
            .with_context(|| format!("ControlplaneStore oeffnen: {}", path.display()))?;

        // Tabellen initialisieren
        let write_txn = db.begin_write()?;
        {
            let _ = write_txn.open_table(CONTROL_CONFIG)?;
            let _ = write_txn.open_table(CONTROL_RUNTIME_STATE)?;
            let _ = write_txn.open_table(CONTROL_ACTION_LOG)?;
            let _ = write_txn.open_table(CONTROL_INCIDENTS)?;
        }
        write_txn.commit()?;

        debug!(path = %path.display(), "ControlplaneStore geoeffnet");
        Ok(Self { db })
    }

    // -- Runtime State --

    /// Laedt den Runtime-State.
    pub fn get_runtime_state(&self) -> Result<RuntimeState> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CONTROL_RUNTIME_STATE)?;
        match table.get(RUNTIME_STATE_KEY)? {
            Some(guard) => {
                let bytes = guard.value();
                let state: RuntimeState =
                    serde_json::from_slice(bytes).context("RuntimeState deserialisieren")?;
                Ok(state)
            }
            None => Ok(RuntimeState::default()),
        }
    }

    /// Speichert den Runtime-State.
    pub fn set_runtime_state(&self, state: &RuntimeState) -> Result<()> {
        let bytes = serde_json::to_vec(state).context("RuntimeState serialisieren")?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(CONTROL_RUNTIME_STATE)?;
            table.insert(RUNTIME_STATE_KEY, bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    // -- Incidents --

    /// Speichert einen Incident.
    pub fn log_incident(&self, incident: &Incident) -> Result<()> {
        let bytes = serde_json::to_vec(incident).context("Incident serialisieren")?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(CONTROL_INCIDENTS)?;
            table.insert(incident.id.as_str(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Laedt die letzten N Incidents (nach ID sortiert, neueste zuerst).
    pub fn get_recent_incidents(&self, limit: usize) -> Result<Vec<Incident>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CONTROL_INCIDENTS)?;

        let mut incidents = Vec::new();
        // redb iteriert ueber keys in sortierter Reihenfolge
        for entry in table.iter()? {
            let (_, value) = entry?;
            let incident: Incident = serde_json::from_slice(value.value())?;
            incidents.push(incident);
        }

        // Neueste zuerst (hoechste Tick-Werte)
        incidents.sort_by(|a, b| b.tick.cmp(&a.tick));
        incidents.truncate(limit);
        Ok(incidents)
    }

    // -- Actions --

    /// Speichert eine Action.
    pub fn log_action(&self, action: &ControlAction) -> Result<()> {
        let bytes = serde_json::to_vec(action).context("ControlAction serialisieren")?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(CONTROL_ACTION_LOG)?;
            table.insert(action.id.as_str(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Aktualisiert eine bestehende Action (z.B. Status-Update nach Verify).
    pub fn update_action(&self, action: &ControlAction) -> Result<()> {
        self.log_action(action) // Upsert via redb insert
    }

    /// Laedt alle Actions mit Status Pending oder Executed (fuer Verify-Phase).
    pub fn get_pending_actions(&self) -> Result<Vec<ControlAction>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CONTROL_ACTION_LOG)?;

        let mut actions = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let action: ControlAction = serde_json::from_slice(value.value())?;
            if matches!(
                action.status,
                super::types::ActionStatus::Pending | super::types::ActionStatus::Executed
            ) {
                actions.push(action);
            }
        }
        Ok(actions)
    }

    // -- Config --

    /// Speichert einen Config-Eintrag.
    pub fn set_config(&self, key: &str, value: &[u8]) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(CONTROL_CONFIG)?;
            table.insert(key, value)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Liest einen Config-Eintrag.
    pub fn get_config(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CONTROL_CONFIG)?;
        match table.get(key)? {
            Some(guard) => Ok(Some(guard.value().to_vec())),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controlplane::types::*;

    fn temp_store() -> (tempfile::TempDir, ControlplaneStore) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("controlplane.redb");
        let store = ControlplaneStore::open(&path).unwrap();
        (tmp, store)
    }

    #[test]
    fn test_runtime_state_default() {
        let (_tmp, store) = temp_store();
        let state = store.get_runtime_state().unwrap();
        assert_eq!(state.last_cycle_tick, 0);
        assert_eq!(state.total_cycles, 0);
    }

    #[test]
    fn test_runtime_state_roundtrip() {
        let (_tmp, store) = temp_store();
        let state = RuntimeState {
            last_cycle_tick: 42,
            total_cycles: 10,
            total_incidents: 3,
            total_actions: 5,
        };
        store.set_runtime_state(&state).unwrap();
        let loaded = store.get_runtime_state().unwrap();
        assert_eq!(loaded.last_cycle_tick, 42);
        assert_eq!(loaded.total_cycles, 10);
    }

    #[test]
    fn test_incident_roundtrip() {
        let (_tmp, store) = temp_store();
        let incident = Incident {
            id: "inc-001".into(),
            tick: 100,
            timestamp_ms: 1000,
            incident_type: IncidentType::HungerCritical,
            severity: Severity::High,
            agent_id: Some(1),
            description: "Agent AGENT-01 hunger at 0.95".into(),
        };
        store.log_incident(&incident).unwrap();

        let loaded = store.get_recent_incidents(10).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "inc-001");
        assert_eq!(loaded[0].agent_id, Some(1));
    }

    #[test]
    fn test_action_roundtrip() {
        let (_tmp, store) = temp_store();
        let action = ControlAction {
            id: "act-001".into(),
            incident_id: "inc-001".into(),
            action_type: ControlActionType::LogOnly {
                message: "hunger critical".into(),
            },
            agent_id: Some(1),
            ttl_ticks: 30,
            rollback_condition: "hunger < 0.5".into(),
            status: ActionStatus::Pending,
            created_tick: 100,
            verify_after_tick: 130,
            verify_outcome: None,
        };
        store.log_action(&action).unwrap();

        let pending = store.get_pending_actions().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "act-001");
    }

    #[test]
    fn test_action_update_status() {
        let (_tmp, store) = temp_store();
        let mut action = ControlAction {
            id: "act-002".into(),
            incident_id: "inc-002".into(),
            action_type: ControlActionType::LogOnly {
                message: "test".into(),
            },
            agent_id: None,
            ttl_ticks: 10,
            rollback_condition: "always".into(),
            status: ActionStatus::Pending,
            created_tick: 50,
            verify_after_tick: 60,
            verify_outcome: None,
        };
        store.log_action(&action).unwrap();

        // Update to Verified
        action.status = ActionStatus::Verified;
        action.verify_outcome = Some(VerifyOutcome {
            tick: 60,
            success: true,
            reason: "condition met".into(),
        });
        store.update_action(&action).unwrap();

        // Pending should be empty now
        let pending = store.get_pending_actions().unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn test_config_roundtrip() {
        let (_tmp, store) = temp_store();
        store.set_config("policy_a", b"test_value").unwrap();
        let val = store.get_config("policy_a").unwrap();
        assert_eq!(val, Some(b"test_value".to_vec()));
    }
}
