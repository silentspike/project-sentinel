//! Projection Worker: Poll-Loop fuer Event-Consumption und View-Updates.
//!
//! Sync API (kein async) — passt zum EventStore Pattern.
//! Poll-Loop mit `std::thread::sleep` bei leeren Batches.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use anyhow::Context;
use sentinel_common::DomainEventPayload;
use sentinel_limbo::EventStore;
use tracing::{debug, error, info, warn};

use crate::config::ProjectionConfig;
use crate::handlers::agent_live_view::AgentLiveViewHandler;
use crate::handlers::kpi::KpiHandler;
use crate::handlers::room_live_view::RoomLiveViewHandler;
use crate::handlers::task_kanban_view::TaskKanbanHandler;
use crate::handlers::ProjectionHandler;
use crate::store::ReadModelStore;

/// Alle 26 Raum-IDs aus config/rooms.toml (statisches Gebaeudelayout).
pub const ROOM_IDS: &[&str] = &[
    "empfang",
    "flur-eg",
    "kueche",
    "buero-dev-1",
    "buero-dev-2",
    "meetingraum-01",
    "toilette-eg-damen",
    "toilette-eg-herren",
    "treppenhaus",
    "flur-og",
    "buero-design-1",
    "buero-design-2",
    "buero-ceo",
    "meetingraum-02",
    "meetingraum-03",
    "toilette-og-damen",
    "toilette-og-herren",
    "buero-sales",
    "buero-pm",
    "buero-marketing",
    "buero-admin",
    "buero-qa",
    "buero-it",
    "buero-betriebsrat",
    "buero-betriebspsych",
    "buero-betriebsarzt",
];

const PROJECTION_NAME: &str = "sentinel-projection";

/// CQRS-lite Projection Worker.
///
/// Konsumiert Events aus dem EventStore und pflegt drei materialisierte
/// Read Models: `agent_live_view`, `room_live_view`, `kpi_1m`.
pub struct ProjectionWorker {
    event_store: Arc<EventStore>,
    read_store: ReadModelStore,
    config: ProjectionConfig,
    handlers: Vec<Box<dyn ProjectionHandler>>,
}

impl ProjectionWorker {
    /// Erstellt einen neuen Worker mit eigener Read-Model-DB.
    pub fn new(event_store: Arc<EventStore>, config: ProjectionConfig) -> anyhow::Result<Self> {
        let read_store = ReadModelStore::open(&config.db_path)
            .with_context(|| format!("Failed to open read model store: {}", config.db_path))?;

        let handlers: Vec<Box<dyn ProjectionHandler>> = vec![
            Box::new(AgentLiveViewHandler),
            Box::new(RoomLiveViewHandler),
            Box::new(KpiHandler),
            Box::new(TaskKanbanHandler),
        ];

        Ok(Self {
            event_store,
            read_store,
            config,
            handlers,
        })
    }

    /// Gibt Referenz auf den ReadModelStore (fuer Queries).
    pub fn read_store(&self) -> &ReadModelStore {
        &self.read_store
    }

    /// Live-Modus: Endlos-Poll-Loop.
    ///
    /// Blockiert den aktuellen Thread. Bricht ab bei Fehler.
    pub fn run(&self) -> anyhow::Result<()> {
        // Rooms initialisieren (idempotent)
        self.read_store.initialize_rooms(ROOM_IDS)?;
        let mut next_rebuild_poll = Instant::now();

        info!(
            poll_interval_ms = self.config.poll_interval.as_millis() as u64,
            batch_size = self.config.batch_size,
            rebuild_request_path = %self.config.rebuild_request_path,
            "Projection worker starting live mode"
        );

        loop {
            if Instant::now() >= next_rebuild_poll {
                self.handle_rebuild_request_if_present()?;
                next_rebuild_poll = Instant::now() + self.config.rebuild_request_poll_interval;
            }

            let offset = self.event_store.get_offset(PROJECTION_NAME)?.unwrap_or(0);

            let batch = self
                .event_store
                .get_events_since_with_id(offset, self.config.batch_size)?;

            if batch.is_empty() {
                thread::sleep(self.config.poll_interval);
                continue;
            }

            let count = self.process_batch(&batch)?;
            let last_row_id = batch.last().unwrap().0;

            // Abgelaufene Smells bereinigen (basierend auf dem hoechsten Tick im Batch)
            if let Some(max_tick) = batch.iter().map(|(_, e)| e.tick).max() {
                self.read_store.cleanup_expired_smells(max_tick)?;
            }

            // Guard: nur updaten wenn tatsaechlich Fortschritt (schuetzt vor
            // Race Conditions bei Auto-Restart und idempotenten Batches)
            if last_row_id > offset {
                self.event_store
                    .update_offset(PROJECTION_NAME, last_row_id)?;
            }

            debug!(events = count, offset = last_row_id, "Batch processed");
        }
    }

    /// Rebuild-Modus: Loescht alle Views und verarbeitet alle Events von Anfang.
    ///
    /// Gibt Anzahl verarbeiteter Events zurueck.
    /// Offset wird nur EINMAL am Ende gesetzt (verhindert Monotonicity-Konflikte
    /// falls ein anderer Prozess gleichzeitig den EventStore nutzt).
    pub fn rebuild(&self) -> anyhow::Result<usize> {
        info!("Starting full rebuild");

        self.read_store.clear_all()?;
        self.event_store.reset_offset(PROJECTION_NAME)?;
        self.read_store.initialize_rooms(ROOM_IDS)?;

        let mut total_processed = 0usize;
        let mut offset = 0i64;
        let mut final_offset = 0i64;

        loop {
            let batch = self
                .event_store
                .get_events_since_with_id(offset, self.config.batch_size)?;

            if batch.is_empty() {
                break;
            }

            let count = self.process_batch(&batch)?;
            total_processed += count;

            let last_row_id = batch.last().unwrap().0;
            final_offset = last_row_id;
            offset = last_row_id;

            debug!(
                events = count,
                total = total_processed,
                offset = last_row_id,
                "Rebuild batch processed"
            );
        }

        // Offset einmalig am Ende setzen — kein Risiko fuer Monotonicity-Konflikte
        if final_offset > 0 {
            self.event_store
                .update_offset(PROJECTION_NAME, final_offset)?;
        }

        // Post-rebuild consistency: recompute occupant_count from agent_live_view.
        // Delta-based counting drifts when the event stream has gaps (e.g. daemon
        // restarts without despawn events in historical data).
        self.read_store.recompute_occupant_counts()?;

        info!(total = total_processed, "Full rebuild complete");
        Ok(total_processed)
    }

    fn handle_rebuild_request_if_present(&self) -> anyhow::Result<bool> {
        let request_path = Path::new(&self.config.rebuild_request_path);
        if !request_path.exists() {
            return Ok(false);
        }

        let payload = fs::read_to_string(request_path).with_context(|| {
            format!(
                "Projection-Rebuild-Request konnte nicht gelesen werden: {}",
                request_path.display()
            )
        })?;
        info!(
            path = %request_path.display(),
            request = %payload,
            "Projection-Rebuild-Request erkannt"
        );

        let rebuilt = self
            .rebuild()
            .context("Projection-Rebuild aus Request-Datei fehlgeschlagen")?;
        fs::remove_file(request_path).with_context(|| {
            format!(
                "Projection-Rebuild-Request konnte nicht entfernt werden: {}",
                request_path.display()
            )
        })?;
        info!(
            path = %request_path.display(),
            events = rebuilt,
            "Projection-Rebuild-Request abgearbeitet"
        );
        Ok(true)
    }

    /// Verarbeitet einen Batch von Events innerhalb einer Transaktion.
    ///
    /// Gibt Anzahl erfolgreich verarbeiteter Events zurueck.
    /// Unbekannte Event-Typen werden uebersprungen (Forward-Compatibility).
    fn process_batch(
        &self,
        batch: &[(i64, sentinel_common::DomainEvent)],
    ) -> anyhow::Result<usize> {
        let txn = self.read_store.begin_transaction()?;
        txn.begin()?;

        let mut processed = 0usize;

        for (row_id, event) in batch {
            // Payload deserialisieren (mit Fallback fuer alte Events ohne "type" Tag)
            let payload: DomainEventPayload = match serde_json::from_str(&event.payload) {
                Ok(p) => p,
                Err(_first_err) => {
                    // Fallback: Alte Events (vor serde tag="type") haben kein "type" Feld.
                    // Wir injizieren den Tag aus event.event_type (DB-Spalte).
                    match deserialize_legacy_payload(&event.event_type, &event.payload) {
                        Some(p) => p,
                        None => {
                            warn!(
                                row_id,
                                event_type = event.event_type,
                                error = %_first_err,
                                "Unknown or malformed event payload, skipping"
                            );
                            continue;
                        }
                    }
                }
            };

            // Alle Handler aufrufen (Reihenfolge: agent -> room -> kpi)
            for handler in &self.handlers {
                if let Err(e) = handler.handle(*row_id, event, &payload, &txn) {
                    txn.rollback().with_context(|| {
                        format!(
                            "Rollback nach Projection-Handlerfehler fehlgeschlagen row_id={row_id} event_type={}",
                            event.event_type
                        )
                    })?;
                    error!(
                        row_id,
                        event_type = event.event_type,
                        error = %e,
                        "Handler error, aborting batch"
                    );
                    return Err(e).context(format!(
                        "Projection-Handlerfehler row_id={row_id} event_type={}",
                        event.event_type
                    ));
                }
            }

            processed += 1;
        }

        if let Some((last_row_id, _)) = batch.last() {
            txn.update_projection_watermark(PROJECTION_NAME, *last_row_id)?;
        }

        txn.commit()?;
        Ok(processed)
    }
}

/// Fallback-Deserializer fuer Legacy-Events (vor `serde(tag = "type")` Einfuehrung).
///
/// Alte Events haben kein `"type"` Discriminator-Feld im JSON-Payload.
/// Diese Funktion mappt `event_type` (DB-Spalte) auf den serde-Tag und
/// konvertiert abweichende Feldnamen (z.B. `"target"` → `"target_room"`).
fn deserialize_legacy_payload(event_type: &str, payload: &str) -> Option<DomainEventPayload> {
    // event_type (DB) → serde tag name Mapping
    let serde_tag = match event_type {
        "agent_action_received" => "AgentActionReceived",
        "transit_started" => "TransitStarted",
        "transit_completed" => "TransitCompleted",
        "chaos_triggered" => "ChaosTriggered",
        "bio_action_performed" => "BioActionPerformed",
        "bio_state_updated" => "BioStateUpdated",
        "room_physics_updated" => "RoomPhysicsUpdated",
        "tick_snapshot" => "TickSnapshot",
        "agent_spawned" => "AgentSpawned",
        "agent_despawned" => "AgentDespawned",
        "shift_transition_completed" => "ShiftTransitionCompleted",
        "agent_status_changed" => "AgentStatusChanged",
        "nightrun_started" => "NightRunStarted",
        "nightrun_completed" => "NightRunCompleted",
        "agent_consolidated" => "AgentConsolidated",
        "agent_consolidation_failed" => "AgentConsolidationFailed",
        "smell_event_triggered" => "SmellEventTriggered",
        "hallway_encounter_detected" => "HallwayEncounterDetected",
        "judge_alert_received" => "JudgeAlertReceived",
        _ => return None,
    };

    // JSON parsen, Tag injizieren, Legacy-Felder remappen
    let mut value: serde_json::Value = serde_json::from_str(payload).ok()?;
    let obj = value.as_object_mut()?;

    // Discriminator-Tag setzen
    obj.insert(
        "type".to_string(),
        serde_json::Value::String(serde_tag.to_string()),
    );

    // Legacy-Feld-Remapping fuer agent_action_received
    if event_type == "agent_action_received" {
        // "target" → "target_room"
        if let Some(target) = obj.remove("target") {
            obj.entry("target_room".to_string()).or_insert(target);
        }
        // "emotion" existierte in alten Events, wird ignoriert (nicht im Struct)
        obj.remove("emotion");
        // agent_id fehlte in alten Events → Default 0
        obj.entry("agent_id".to_string())
            .or_insert(serde_json::Value::Number(0.into()));
    }

    serde_json::from_value(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;
    use sentinel_common::{AgentId, DomainEvent, DomainEventPayload};
    use tempfile::tempdir;

    struct FailingHandler;

    impl crate::handlers::ProjectionHandler for FailingHandler {
        fn handle(
            &self,
            _row_id: i64,
            _event: &DomainEvent,
            _payload: &DomainEventPayload,
            _txn: &crate::store::ReadModelTransaction<'_>,
        ) -> anyhow::Result<()> {
            bail!("synthetic handler failure");
        }
    }

    fn append_event(store: &EventStore, tick: u64, payload: &DomainEventPayload) {
        let mut event = DomainEvent::new(
            payload.event_type_str(),
            "test-aggregate",
            &payload.to_json(),
            &format!("corr-{tick}"),
            tick,
        );
        event.timestamp_ms = tick * 1000;
        store.append_event(&event).unwrap();
    }

    #[test]
    fn rebuild_request_file_triggers_full_rebuild_and_is_removed() {
        let dir = tempdir().unwrap();
        let event_store =
            Arc::new(EventStore::open(dir.path().join("events.db").to_str().unwrap()).unwrap());
        append_event(
            &event_store,
            1,
            &DomainEventPayload::AgentSpawned {
                agent_id: AgentId(1),
                name: "Test Agent".to_string(),
                role: "QA".to_string(),
                shift_set: 1,
                room_id: "empfang".to_string(),
            },
        );

        let request_path = dir.path().join(".projection-rebuild-request");
        let config = ProjectionConfig {
            poll_interval: std::time::Duration::from_millis(1),
            batch_size: 16,
            db_path: dir
                .path()
                .join("projection.db")
                .to_string_lossy()
                .to_string(),
            rebuild_request_path: request_path.to_string_lossy().to_string(),
            rebuild_request_poll_interval: std::time::Duration::from_secs(1),
        };
        let worker = ProjectionWorker::new(Arc::clone(&event_store), config).unwrap();

        fs::write(
            &request_path,
            r#"{"requested_by":"runtime_reconcile","reason":"projection_drift","tick":42}"#,
        )
        .unwrap();

        assert!(worker.handle_rebuild_request_if_present().unwrap());
        assert!(!request_path.exists());
        assert_eq!(worker.read_store().active_agent_count().unwrap(), 1);
    }

    #[test]
    fn handler_error_rolls_back_batch_and_returns_err() {
        let dir = tempdir().unwrap();
        let event_store =
            Arc::new(EventStore::open(dir.path().join("events.db").to_str().unwrap()).unwrap());
        append_event(
            &event_store,
            1,
            &DomainEventPayload::AgentSpawned {
                agent_id: AgentId(1),
                name: "Test Agent".to_string(),
                role: "QA".to_string(),
                shift_set: 1,
                room_id: "empfang".to_string(),
            },
        );

        let config = ProjectionConfig {
            poll_interval: std::time::Duration::from_millis(1),
            batch_size: 16,
            db_path: dir
                .path()
                .join("projection.db")
                .to_string_lossy()
                .to_string(),
            rebuild_request_path: dir
                .path()
                .join(".projection-rebuild-request")
                .to_string_lossy()
                .to_string(),
            rebuild_request_poll_interval: std::time::Duration::from_secs(1),
        };
        let mut worker = ProjectionWorker::new(Arc::clone(&event_store), config).unwrap();
        worker.handlers = vec![Box::new(FailingHandler)];

        let batch = event_store.get_events_since_with_id(0, 16).unwrap();
        let err = worker.process_batch(&batch).unwrap_err();
        assert!(format!("{err:#}").contains("synthetic handler failure"));
        assert_eq!(worker.read_store().active_agent_count().unwrap(), 0);
    }
}
