//! Projection Worker: Poll-Loop fuer Event-Consumption und View-Updates.
//!
//! Sync API (kein async) — passt zum EventStore Pattern.
//! Poll-Loop mit `std::thread::sleep` bei leeren Batches.

use std::sync::Arc;
use std::thread;

use anyhow::Context;
use sentinel_common::DomainEventPayload;
use sentinel_limbo::EventStore;
use tracing::{debug, error, info, warn};

use crate::config::ProjectionConfig;
use crate::handlers::agent_live_view::AgentLiveViewHandler;
use crate::handlers::kpi::KpiHandler;
use crate::handlers::room_live_view::RoomLiveViewHandler;
use crate::handlers::ProjectionHandler;
use crate::store::ReadModelStore;

/// Alle 17 Raum-IDs aus config/rooms.toml (statisches Gebaeudelayout).
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

        info!(
            poll_interval_ms = self.config.poll_interval.as_millis() as u64,
            batch_size = self.config.batch_size,
            "Projection worker starting live mode"
        );

        loop {
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
    pub fn rebuild(&self) -> anyhow::Result<usize> {
        info!("Starting full rebuild");

        self.read_store.clear_all()?;
        self.event_store.reset_offset(PROJECTION_NAME)?;
        self.read_store.initialize_rooms(ROOM_IDS)?;

        let mut total_processed = 0usize;
        let mut offset = 0i64;

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

            if last_row_id > offset {
                self.event_store
                    .update_offset(PROJECTION_NAME, last_row_id)?;
            }
            offset = last_row_id;

            debug!(
                events = count,
                total = total_processed,
                offset = last_row_id,
                "Rebuild batch processed"
            );
        }

        info!(total = total_processed, "Full rebuild complete");
        Ok(total_processed)
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
                    error!(
                        row_id,
                        event_type = event.event_type,
                        error = %e,
                        "Handler error, skipping event"
                    );
                    break;
                }
            }

            processed += 1;
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
