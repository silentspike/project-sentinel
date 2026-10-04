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
use crate::handlers::cost::CostHandler;
use crate::handlers::kpi::KpiHandler;
use crate::handlers::room_live_view::RoomLiveViewHandler;
use crate::handlers::task_kanban_view::TaskKanbanHandler;
use crate::handlers::workbench::WorkbenchHandler;
use crate::handlers::ProjectionHandler;
use crate::retry::{commit_then_mirror, live_batch, sqlite_busy, MIRROR_DEFER_BACKOFF};
use crate::store::{LlmHierarchyCostUpdate, ReadModelStore};

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
pub const HIERARCHY_PROJECTION_NAME: &str = "sentinel-projection-cost-hierarchy-v2";

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
        read_store.initialize_rooms(ROOM_IDS)?;

        let handlers: Vec<Box<dyn ProjectionHandler>> = vec![
            Box::new(AgentLiveViewHandler),
            Box::new(RoomLiveViewHandler),
            Box::new(KpiHandler),
            Box::new(TaskKanbanHandler),
            Box::new(CostHandler),
            Box::new(WorkbenchHandler),
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

    /// Projects one ordinary live batch, committing views before mirroring the offset.
    pub fn process_pending_batch(&self) -> anyhow::Result<usize> {
        let mirrored = self.event_store.get_offset(PROJECTION_NAME)?;
        let offset = mirrored.unwrap_or(0);
        let batch = self
            .event_store
            .get_events_since_with_id(offset, self.config.batch_size)?;
        let Some((last_row_id, _)) = batch.last() else {
            return Ok(0);
        };
        let count = commit_then_mirror(
            PROJECTION_NAME,
            mirrored.is_some(),
            || {
                let count = self.process_batch(&batch)?;
                if let Some(max_tick) = batch.iter().map(|(_, event)| event.tick).max() {
                    sqlite_busy("expired smell cleanup", || {
                        self.read_store.cleanup_expired_smells(max_tick)
                    })?;
                }
                Ok(count)
            },
            || {
                if *last_row_id > offset {
                    self.event_store
                        .update_offset(PROJECTION_NAME, *last_row_id)?;
                }
                Ok(())
            },
        )?;
        debug!(events = count, offset = last_row_id, "Batch processed");
        Ok(count)
    }

    /// Live-Modus: Endlos-Poll-Loop.
    ///
    /// Blockiert den aktuellen Thread. Bricht ab bei Fehler.
    pub fn run(&self) -> anyhow::Result<()> {
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

            let hierarchy_processed = live_batch(self.process_hierarchy_pending_batch())?;
            let processed = live_batch(self.process_pending_batch())?;
            if hierarchy_processed.is_none() || processed.is_none() {
                thread::sleep(self.config.poll_interval.max(MIRROR_DEFER_BACKOFF));
            } else if processed == Some(0) && hierarchy_processed == Some(0) {
                thread::sleep(self.config.poll_interval);
            }
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
        self.event_store.reset_offset(HIERARCHY_PROJECTION_NAME)?;
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
            sqlite_busy("rebuilt projection offset mirror", || {
                self.event_store
                    .update_offset(PROJECTION_NAME, final_offset)
            })?;
        }

        self.read_store.reconcile_room_presence()?;
        self.rebuild_hierarchy_projection()?;

        info!(total = total_processed, "Full rebuild complete");
        Ok(total_processed)
    }

    /// Replays the complete event stream through the independent hierarchy
    /// projection. This intentionally ignores the shared projection offset.
    fn rebuild_hierarchy_projection(&self) -> anyhow::Result<usize> {
        let mut total = 0usize;
        loop {
            let processed = self.process_hierarchy_pending_batch()?;
            if processed == 0 {
                return Ok(total);
            }
            total += processed;
        }
    }

    /// Catches the hierarchy projection up to the current end of the event
    /// stream without consulting or mutating the shared projection offset.
    pub fn catch_up_hierarchy(&self) -> anyhow::Result<usize> {
        self.rebuild_hierarchy_projection()
    }

    /// Advances one hierarchy-projection batch using its own EventStore offset.
    fn process_hierarchy_pending_batch(&self) -> anyhow::Result<usize> {
        let mirrored = self.event_store.get_offset(HIERARCHY_PROJECTION_NAME)?;
        let offset = mirrored.unwrap_or(0);
        let batch = self
            .event_store
            .get_events_since_with_id(offset, self.config.batch_size)?;
        if batch.is_empty() {
            return Ok(0);
        }

        let last_row_id = batch.last().expect("non-empty hierarchy batch").0;
        commit_then_mirror(
            HIERARCHY_PROJECTION_NAME,
            mirrored.is_some(),
            || self.process_hierarchy_batch(&batch),
            || {
                self.event_store
                    .update_offset(HIERARCHY_PROJECTION_NAME, last_row_id)
            },
        )
    }

    fn process_hierarchy_batch(
        &self,
        batch: &[(i64, sentinel_common::DomainEvent)],
    ) -> anyhow::Result<usize> {
        sqlite_busy("hierarchy projection batch", || {
            self.read_store.transaction(|txn| {
                let committed = txn.projection_watermark(HIERARCHY_PROJECTION_NAME)?;
                for (row_id, event) in batch {
                    if *row_id <= committed {
                        continue;
                    }
                    let payload = match deserialize_payload(event) {
                        Some(payload) => payload,
                        None if event.schema_version >= 2
                            && event.event_type == "agent_llm_usage" =>
                        {
                            anyhow::bail!("malformed v2 agent_llm_usage payload at row_id={row_id}")
                        }
                        None => continue,
                    };
                    if event.event_type != "agent_llm_usage" {
                        continue;
                    }
                    let DomainEventPayload::AgentLlmUsage {
                        agent_id,
                        tenant_id,
                        project_id,
                        work_item_id,
                        reservation_id,
                        assignment_id,
                        assignment_version,
                        provider,
                        requested_model,
                        caller_role,
                        tier,
                        hierarchy_tier,
                        cost_source,
                        effective_model,
                        input_tokens,
                        output_tokens,
                        cache_read,
                        cache_creation,
                        cost_usd,
                        ..
                    } = payload
                    else {
                        if event.schema_version >= 2 {
                            anyhow::bail!(
                                "v2 agent_llm_usage payload type mismatch at row_id={row_id}"
                            );
                        }
                        continue;
                    };
                    if event.schema_version >= 2 {
                        if tier.trim().is_empty() {
                            anyhow::bail!("v2 agent_llm_usage is missing model tier");
                        }
                        if !cost_usd.is_finite() || cost_usd < 0.0 {
                            anyhow::bail!("v2 agent_llm_usage has invalid cost_usd");
                        }
                        let hierarchy_key = hierarchy_tier
                            .context("v2 agent_llm_usage is missing hierarchy_tier")?
                            .get()
                            .to_string();
                        cost_source.context("v2 agent_llm_usage is missing cost_source")?;
                        if effective_model
                            .as_deref()
                            .is_none_or(|model| model.trim().is_empty())
                        {
                            anyhow::bail!("v2 agent_llm_usage is missing effective_model");
                        }
                        if event.schema_version > 6 {
                            anyhow::bail!("unsupported agent_llm_usage authority schema");
                        }
                        if event.schema_version == 6 {
                            // The leader owns the usage; assignment fields name the reviewed assignee.
                            let reservation = reservation_id.as_deref().unwrap_or_default();
                            if [
                                tenant_id.as_deref(),
                                project_id.as_deref(),
                                work_item_id.as_deref(),
                                reservation_id.as_deref(),
                                assignment_id.as_deref(),
                                provider.as_deref(),
                                requested_model.as_deref(),
                            ]
                            .into_iter()
                            .any(|value| value.is_none_or(|value| value.trim().is_empty()))
                                || assignment_version.is_none_or(|value| value == 0)
                                || caller_role.as_deref() != Some("agent_runtime")
                                || provider.as_deref() != Some("codex-cli")
                                || effective_model != requested_model
                                || cost_source == Some(sentinel_common::CostSource::NonProviderZero)
                                || !uuid::Uuid::parse_str(reservation)
                                    .is_ok_and(|id| !id.is_nil() && id.to_string() == reservation)
                                || event.aggregate_id != agent_id.to_string()
                                || event.correlation_id
                                    != format!("company-leadership-{reservation}")
                                || event.operation_id
                                    != format!("llm_usage_{}", event.correlation_id)
                            {
                                anyhow::bail!(
                                    "v6 agent_llm_usage has invalid leadership authority"
                                );
                            }
                        }
                        if event.schema_version == 5
                            && ([
                                tenant_id.as_deref(),
                                project_id.as_deref(),
                                reservation_id.as_deref(),
                                provider.as_deref(),
                                requested_model.as_deref(),
                            ]
                            .into_iter()
                            .any(|value| value.is_none_or(|value| value.trim().is_empty()))
                                || work_item_id.is_some()
                                || assignment_id.is_some()
                                || assignment_version.is_some()
                                || caller_role.as_deref() != Some("agent_runtime")
                                || event.correlation_id
                                    != format!(
                                        "company-planning-{}-{}",
                                        reservation_id.as_deref().unwrap_or_default(),
                                        project_id.as_deref().unwrap_or_default()
                                    )
                                || event.operation_id
                                    != format!("llm_usage_{}", event.correlation_id))
                        {
                            anyhow::bail!("v5 agent_llm_usage has invalid planning authority");
                        }
                        if event.schema_version == 4
                            && ([
                                tenant_id.as_deref(),
                                reservation_id.as_deref(),
                                provider.as_deref(),
                                requested_model.as_deref(),
                            ]
                            .into_iter()
                            .any(|value| value.is_none_or(|value| value.trim().is_empty()))
                                || project_id.is_some()
                                || work_item_id.is_some()
                                || assignment_id.is_some()
                                || assignment_version.is_some()
                                || caller_role.as_deref() != Some("agent_runtime")
                                || event.correlation_id
                                    != format!(
                                        "company-provider-{}",
                                        reservation_id.as_deref().unwrap_or_default()
                                    )
                                || event.operation_id
                                    != format!("llm_usage_{}", event.correlation_id))
                        {
                            anyhow::bail!("v4 agent_llm_usage has invalid request authority");
                        }
                        if event.schema_version == 3 {
                            if [
                                tenant_id.as_deref(),
                                project_id.as_deref(),
                                work_item_id.as_deref(),
                                reservation_id.as_deref(),
                                assignment_id.as_deref(),
                                provider.as_deref(),
                                requested_model.as_deref(),
                            ]
                            .into_iter()
                            .any(|value| value.is_none_or(|value| value.trim().is_empty()))
                            {
                                anyhow::bail!("v3 agent_llm_usage is missing project authority");
                            }
                            if assignment_version.is_none_or(|value| value == 0) {
                                anyhow::bail!("v3 agent_llm_usage has invalid assignment_version");
                            }
                            if caller_role.as_deref() != Some("agent_runtime") {
                                anyhow::bail!("v3 agent_llm_usage has invalid caller_role");
                            }
                        }
                        txn.record_hierarchy_cost(
                            &LlmHierarchyCostUpdate {
                                hierarchy_tier: &hierarchy_key,
                                input_tokens,
                                output_tokens,
                                cache_read,
                                cache_creation,
                                cost_usd,
                            },
                            *row_id,
                        )?;
                        txn.record_hierarchy_usage_meta(*row_id, true)?;
                    } else {
                        txn.record_hierarchy_usage_meta(*row_id, false)?;
                    }
                }

                let last_row_id = batch.last().expect("non-empty hierarchy batch").0;
                txn.update_projection_watermark(HIERARCHY_PROJECTION_NAME, last_row_id)?;
                Ok(batch.len())
            })
        })
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
        sqlite_busy("ordinary projection batch", || {
            self.read_store.transaction(|txn| {
                let mut processed = 0usize;
                let committed = txn.projection_watermark(PROJECTION_NAME)?;

                for (row_id, event) in batch {
                    if *row_id <= committed {
                        continue;
                    }
                    // Payload deserialisieren (mit Fallback fuer alte Events ohne "type" Tag)
                    let Some(payload) = deserialize_payload(event) else {
                        warn!(
                            row_id,
                            event_type = event.event_type,
                            "Unknown or malformed event payload, skipping"
                        );
                        continue;
                    };

                    // Alle Handler aufrufen (Reihenfolge: agent -> room -> kpi)
                    for handler in &self.handlers {
                        if let Err(e) = handler.handle(*row_id, event, &payload, txn) {
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

                Ok(processed)
            })
        })
    }
}

fn deserialize_payload(event: &sentinel_common::DomainEvent) -> Option<DomainEventPayload> {
    serde_json::from_str(&event.payload)
        .ok()
        .or_else(|| deserialize_legacy_payload(&event.event_type, &event.payload))
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
    use sentinel_common::{AgentId, CostSource, DomainEvent, DomainEventPayload, HierarchyTier};
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
        store
            .legacy_append_gateway(sentinel_limbo::LegacyEventProducer::TestHarness)
            .append_event(&event)
            .unwrap();
    }

    fn append_raw_event(store: &EventStore, event: &DomainEvent) {
        store
            .legacy_append_gateway(sentinel_limbo::LegacyEventProducer::TestHarness)
            .append_event(event)
            .unwrap();
    }

    fn mirror_test_worker(
        dir: &tempfile::TempDir,
        event_store: &Arc<EventStore>,
    ) -> ProjectionWorker {
        ProjectionWorker::new(
            Arc::clone(event_store),
            ProjectionConfig {
                batch_size: 1,
                poll_interval: std::time::Duration::from_millis(1),
                db_path: dir
                    .path()
                    .join("projection.db")
                    .to_string_lossy()
                    .into_owned(),
                rebuild_request_path: dir
                    .path()
                    .join(".projection-rebuild-request")
                    .to_string_lossy()
                    .into_owned(),
                ..ProjectionConfig::default()
            },
        )
        .unwrap()
    }

    fn planning_usage_event(project_id: &str) -> DomainEvent {
        let allowance = "subscription-planning-a";
        let request_id = format!("company-planning-{allowance}-{project_id}");
        let payload = DomainEventPayload::AgentLlmUsage {
            agent_id: AgentId(9),
            tenant_id: Some("m0-company".to_owned()),
            project_id: Some(project_id.to_owned()),
            work_item_id: None,
            reservation_id: Some(allowance.to_owned()),
            assignment_id: None,
            assignment_version: None,
            provider: Some("codex-cli".to_owned()),
            requested_model: Some("gpt-5.6-terra".to_owned()),
            caller_role: Some("agent_runtime".to_owned()),
            tier: "mid".to_owned(),
            hierarchy_tier: Some(HierarchyTier::TIER_2),
            cost_source: Some(CostSource::ProviderReported),
            effective_model: Some("gpt-5.6-terra".to_owned()),
            input_tokens: 10,
            output_tokens: 20,
            cache_read: 0,
            cache_creation: 0,
            cost_usd: 0.0,
        };
        DomainEvent::new(
            payload.event_type_str(),
            "AGENT-09",
            &payload.to_json(),
            &request_id,
            1,
        )
        .with_operation_id(&format!("llm_usage_{request_id}"))
        .with_schema_version(5)
    }

    fn leadership_usage_event() -> DomainEvent {
        let review_id = "00000000-0000-7000-8000-000000000001";
        let mut event = planning_usage_event("project-a");
        let mut payload: serde_json::Value = serde_json::from_str(&event.payload).unwrap();
        payload["work_item_id"] = serde_json::json!("build-source");
        payload["reservation_id"] = serde_json::json!(review_id);
        payload["assignment_id"] = serde_json::json!("assignment-developer-a");
        payload["assignment_version"] = serde_json::json!(3);
        event.payload = payload.to_string();
        event.correlation_id = format!("company-leadership-{review_id}");
        event.operation_id = format!("llm_usage_{}", event.correlation_id);
        event.schema_version = 6;
        event
    }

    fn assert_hierarchy_usage_rejected(event: &DomainEvent, expected_message: &str) {
        let dir = tempdir().unwrap();
        let event_store =
            Arc::new(EventStore::open(dir.path().join("events.db").to_str().unwrap()).unwrap());
        let worker = ProjectionWorker::new(
            Arc::clone(&event_store),
            ProjectionConfig {
                db_path: dir
                    .path()
                    .join("projection.db")
                    .to_string_lossy()
                    .into_owned(),
                ..ProjectionConfig::default()
            },
        )
        .unwrap();
        let before = worker.read_store().hierarchy_projection_meta().unwrap();
        // The valid first row must roll back along with the rejected second row.
        let error = worker
            .process_hierarchy_batch(&[(1, planning_usage_event("project-a")), (2, event.clone())])
            .unwrap_err();
        assert!(format!("{error:#}").contains(expected_message), "{error:#}");
        assert!(worker
            .read_store()
            .cost_by_hierarchy_tier()
            .unwrap()
            .is_empty());
        assert_eq!(
            worker.read_store().hierarchy_projection_meta().unwrap(),
            before
        );
        assert_eq!(
            worker
                .read_store()
                .transaction(|txn| txn.projection_watermark(HIERARCHY_PROJECTION_NAME))
                .unwrap(),
            0
        );
        assert_eq!(
            event_store.get_offset(HIERARCHY_PROJECTION_NAME).unwrap(),
            None
        );
    }

    #[test]
    fn hierarchy_projection_accepts_leader_usage_with_reviewed_assignee_authority() {
        for output_tokens in [20, 0] {
            let dir = tempdir().unwrap();
            let event_store =
                Arc::new(EventStore::open(dir.path().join("events.db").to_str().unwrap()).unwrap());
            let mut event = leadership_usage_event();
            let mut payload: serde_json::Value = serde_json::from_str(&event.payload).unwrap();
            // Usage is retained even when the provider response was not admissible.
            payload["output_tokens"] = serde_json::json!(output_tokens);
            event.payload = payload.to_string();
            append_raw_event(&event_store, &event);
            let worker = ProjectionWorker::new(
                Arc::clone(&event_store),
                ProjectionConfig {
                    db_path: dir
                        .path()
                        .join("projection.db")
                        .to_string_lossy()
                        .into_owned(),
                    ..ProjectionConfig::default()
                },
            )
            .unwrap();

            assert_eq!(worker.catch_up_hierarchy().unwrap(), 1);
            let costs = worker.read_store().cost_by_hierarchy_tier().unwrap();
            assert_eq!(costs.len(), 1);
            assert_eq!(costs[0].call_count, 1);
            assert_eq!(costs[0].input_tokens, 10);
            assert_eq!(costs[0].output_tokens, output_tokens);
            assert_eq!(
                event_store.get_offset(HIERARCHY_PROJECTION_NAME).unwrap(),
                Some(1)
            );
            assert_eq!(worker.catch_up_hierarchy().unwrap(), 0);
            assert_eq!(
                worker.read_store().cost_by_hierarchy_tier().unwrap()[0].call_count,
                1
            );
            let batch = event_store.get_events_since_with_id(0, 16).unwrap();
            worker.process_batch(&batch).unwrap();
            let agents = worker.read_store().cost_by_agent().unwrap();
            assert_eq!(agents.len(), 1);
            assert_eq!(agents[0].key, "AGENT-09");
        }
    }

    #[test]
    fn hierarchy_projection_rejects_missing_or_blank_leadership_authority() {
        for field in [
            "tenant_id",
            "project_id",
            "work_item_id",
            "reservation_id",
            "assignment_id",
            "provider",
            "requested_model",
            "caller_role",
        ] {
            for value in [
                None,
                Some(serde_json::Value::Null),
                Some(serde_json::json!("  ")),
            ] {
                let mut event = leadership_usage_event();
                let mut payload: serde_json::Value = serde_json::from_str(&event.payload).unwrap();
                if let Some(value) = value {
                    payload[field] = value;
                } else {
                    payload.as_object_mut().unwrap().remove(field);
                }
                event.payload = payload.to_string();
                assert_hierarchy_usage_rejected(&event, "invalid leadership authority");
            }
        }
        for value in [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::json!(0)),
        ] {
            let mut event = leadership_usage_event();
            let mut payload: serde_json::Value = serde_json::from_str(&event.payload).unwrap();
            if let Some(value) = value {
                payload["assignment_version"] = value;
            } else {
                payload
                    .as_object_mut()
                    .unwrap()
                    .remove("assignment_version");
            }
            event.payload = payload.to_string();
            assert_hierarchy_usage_rejected(&event, "invalid leadership authority");
        }
    }

    #[test]
    fn hierarchy_projection_rejects_invalid_leadership_usage_policy() {
        for (field, value) in [
            ("caller_role", serde_json::json!("developer")),
            ("provider", serde_json::json!("different-provider")),
            ("effective_model", serde_json::json!("different-model")),
            ("reservation_id", serde_json::json!("not-a-review-id")),
            (
                "reservation_id",
                serde_json::json!(uuid::Uuid::nil().to_string()),
            ),
            (
                "cost_source",
                serde_json::to_value(CostSource::NonProviderZero).unwrap(),
            ),
        ] {
            let mut event = leadership_usage_event();
            let mut payload: serde_json::Value = serde_json::from_str(&event.payload).unwrap();
            payload[field] = value;
            event.payload = payload.to_string();
            assert_hierarchy_usage_rejected(&event, "invalid leadership authority");
        }
    }

    #[test]
    fn hierarchy_projection_rejects_borrowed_leadership_subject_and_request_identity() {
        for change in 0..5 {
            let mut event = leadership_usage_event();
            match change {
                0 => event.aggregate_id = "AGENT-02".into(),
                1 => {
                    let mut payload: serde_json::Value =
                        serde_json::from_str(&event.payload).unwrap();
                    payload["agent_id"] = serde_json::json!(2);
                    event.payload = payload.to_string();
                }
                2 => {
                    // Another well-formed review ID is not this usage's reservation.
                    event.correlation_id =
                        "company-leadership-00000000-0000-7000-8000-000000000002".into();
                    event.operation_id = format!("llm_usage_{}", event.correlation_id);
                }
                3 => event.operation_id = "llm_usage_borrowed-request".into(),
                _ => {
                    event.correlation_id = "company-provider-borrowed-developer-reservation".into();
                    event.operation_id = format!("llm_usage_{}", event.correlation_id);
                }
            }
            assert_hierarchy_usage_rejected(&event, "invalid leadership authority");
        }
    }

    #[test]
    fn hierarchy_projection_rejects_usage_authority_schemas_above_six() {
        for schema in [7, u32::MAX] {
            let event = leadership_usage_event().with_schema_version(schema);
            assert_hierarchy_usage_rejected(&event, "unsupported agent_llm_usage authority schema");
        }
    }

    #[test]
    fn hierarchy_projection_preserves_pre_leadership_usage_schemas() {
        for schema in 1..=5 {
            let mut event = planning_usage_event("project-a").with_schema_version(schema);
            let mut payload: serde_json::Value = serde_json::from_str(&event.payload).unwrap();
            match schema {
                1 | 2 => {
                    for field in [
                        "tenant_id",
                        "project_id",
                        "reservation_id",
                        "provider",
                        "requested_model",
                        "caller_role",
                    ] {
                        payload.as_object_mut().unwrap().remove(field);
                    }
                    if schema == 1 {
                        for field in ["hierarchy_tier", "cost_source", "effective_model"] {
                            payload.as_object_mut().unwrap().remove(field);
                        }
                    }
                    event.correlation_id = "legacy-usage".into();
                    event.operation_id = "llm_usage_legacy-usage".into();
                }
                3 => {
                    payload["work_item_id"] = serde_json::json!("build-source");
                    payload["assignment_id"] = serde_json::json!("assignment-developer-a");
                    payload["assignment_version"] = serde_json::json!(3);
                    // V3 authority did not impose leadership's model/provider/UUID binding.
                    payload["provider"] = serde_json::json!("legacy-provider");
                    payload["effective_model"] = serde_json::json!("legacy-effective-model");
                    event.correlation_id = "company-provider-subscription-planning-a".into();
                    event.operation_id = format!("llm_usage_{}", event.correlation_id);
                }
                4 => {
                    payload.as_object_mut().unwrap().remove("project_id");
                    event.correlation_id = "company-provider-subscription-planning-a".into();
                    event.operation_id = format!("llm_usage_{}", event.correlation_id);
                }
                _ => {}
            }
            event.payload = payload.to_string();
            let dir = tempdir().unwrap();
            let event_store =
                Arc::new(EventStore::open(dir.path().join("events.db").to_str().unwrap()).unwrap());
            let worker = ProjectionWorker::new(
                Arc::clone(&event_store),
                ProjectionConfig {
                    db_path: dir
                        .path()
                        .join("projection.db")
                        .to_string_lossy()
                        .into_owned(),
                    ..ProjectionConfig::default()
                },
            )
            .unwrap();
            assert_eq!(worker.process_hierarchy_batch(&[(1, event)]).unwrap(), 1);
            let costs = worker.read_store().cost_by_hierarchy_tier().unwrap();
            if schema == 1 {
                assert!(costs.is_empty());
                assert_eq!(
                    worker
                        .read_store()
                        .hierarchy_projection_meta()
                        .unwrap()
                        .unattributed_v1_usage_events,
                    1
                );
            } else {
                assert_eq!(costs.len(), 1);
                assert_eq!(costs[0].call_count, 1);
            }
        }
    }

    #[test]
    fn hierarchy_projection_applies_existing_accounting_validation_to_leadership_usage() {
        for (field, value, expected_message) in [
            ("tier", serde_json::json!("  "), "missing model tier"),
            (
                "hierarchy_tier",
                serde_json::Value::Null,
                "missing hierarchy_tier",
            ),
            (
                "cost_source",
                serde_json::Value::Null,
                "missing cost_source",
            ),
            (
                "effective_model",
                serde_json::json!("  "),
                "missing effective_model",
            ),
            ("cost_usd", serde_json::json!(-0.01), "invalid cost_usd"),
        ] {
            let mut event = leadership_usage_event();
            let mut payload: serde_json::Value = serde_json::from_str(&event.payload).unwrap();
            payload[field] = value;
            event.payload = payload.to_string();
            assert_hierarchy_usage_rejected(&event, expected_message);
        }
    }

    #[test]
    fn hierarchy_projection_accepts_exact_project_planning_usage_authority() {
        let dir = tempdir().unwrap();
        let event_store =
            Arc::new(EventStore::open(dir.path().join("events.db").to_str().unwrap()).unwrap());
        append_raw_event(&event_store, &planning_usage_event("project-a"));
        let worker = ProjectionWorker::new(
            Arc::clone(&event_store),
            ProjectionConfig {
                db_path: dir
                    .path()
                    .join("projection.db")
                    .to_string_lossy()
                    .into_owned(),
                ..ProjectionConfig::default()
            },
        )
        .unwrap();

        assert_eq!(worker.catch_up_hierarchy().unwrap(), 1);
    }

    #[test]
    fn hierarchy_projection_rejects_unbound_project_planning_usage() {
        let dir = tempdir().unwrap();
        let event_store =
            Arc::new(EventStore::open(dir.path().join("events.db").to_str().unwrap()).unwrap());
        let mut event = planning_usage_event("project-a");
        event.correlation_id = "company-planning-subscription-planning-a-project-b".to_owned();
        event.operation_id = format!("llm_usage_{}", event.correlation_id);
        append_raw_event(&event_store, &event);
        let worker = ProjectionWorker::new(
            Arc::clone(&event_store),
            ProjectionConfig {
                db_path: dir
                    .path()
                    .join("projection.db")
                    .to_string_lossy()
                    .into_owned(),
                ..ProjectionConfig::default()
            },
        )
        .unwrap();

        let error = worker.catch_up_hierarchy().unwrap_err();
        assert!(format!("{error:#}").contains("invalid planning authority"));
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
    fn restart_after_unmirrored_commit_does_not_recount_kpis() {
        let dir = tempdir().unwrap();
        let event_store =
            Arc::new(EventStore::open(dir.path().join("events.db").to_str().unwrap()).unwrap());
        let projection_path = dir.path().join("projection.db");
        let make_worker = || {
            ProjectionWorker::new(
                Arc::clone(&event_store),
                ProjectionConfig {
                    db_path: projection_path.to_string_lossy().into_owned(),
                    ..ProjectionConfig::default()
                },
            )
            .unwrap()
        };
        let payload = DomainEventPayload::AgentSpawned {
            agent_id: AgentId(1),
            name: "Test Agent".into(),
            role: "QA".into(),
            shift_set: 1,
            room_id: "empfang".into(),
        };
        append_event(&event_store, 1, &payload);
        let batch = event_store.get_events_since_with_id(0, 16).unwrap();
        let worker = make_worker();
        assert_eq!(worker.process_batch(&batch).unwrap(), 1);
        assert_eq!(event_store.get_offset(PROJECTION_NAME).unwrap(), None);
        drop(worker);

        append_event(&event_store, 2, &payload);
        let mixed = event_store.get_events_since_with_id(0, 16).unwrap();
        let restarted = make_worker();
        assert_eq!(restarted.process_batch(&mixed).unwrap(), 1);
        assert_eq!(restarted.process_batch(&mixed).unwrap(), 0);
        let conn = rusqlite::Connection::open(&projection_path).unwrap();
        let active: i64 = conn
            .query_row("SELECT SUM(active_agents) FROM kpi_1m", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(active, 2);
        assert_eq!(event_store.get_offset(PROJECTION_NAME).unwrap(), None);
        event_store
            .update_offset(PROJECTION_NAME, mixed.last().unwrap().0)
            .unwrap();
        assert_eq!(
            event_store.get_offset(PROJECTION_NAME).unwrap(),
            Some(mixed.last().unwrap().0)
        );
    }

    #[test]
    fn ordinary_mirror_contention_preserves_exactly_once_effects_frontier_and_reopen() {
        let dir = tempdir().unwrap();
        let event_path = dir.path().join("events.db");
        let event_store = Arc::new(EventStore::open(event_path.to_str().unwrap()).unwrap());
        for id in [1, 2] {
            append_event(
                &event_store,
                u64::from(id),
                &DomainEventPayload::AgentSpawned {
                    agent_id: AgentId(id),
                    name: format!("Fixture Agent {id}"),
                    role: "QA".into(),
                    shift_set: 1,
                    room_id: "empfang".into(),
                },
            );
        }
        event_store.update_offset(PROJECTION_NAME, 0).unwrap();
        event_store
            .update_offset(HIERARCHY_PROJECTION_NAME, 0)
            .unwrap();
        let worker = mirror_test_worker(&dir, &event_store);
        let competitor = rusqlite::Connection::open(&event_path).unwrap();
        competitor.execute_batch("BEGIN IMMEDIATE").unwrap();
        let error = worker.process_pending_batch().unwrap_err();
        assert!(error
            .downcast_ref::<sentinel_limbo::event_store::ProjectionOffsetAcquisitionBusy>()
            .is_some());
        assert_eq!(live_batch(Err(error)).unwrap(), None);
        assert_eq!(event_store.get_offset(PROJECTION_NAME).unwrap(), Some(0));
        assert_eq!(worker.read_store().active_agent_count().unwrap(), 1);
        assert!(worker.read_store().get_agent(2).unwrap().is_none());
        let committed = worker
            .read_store()
            .transaction(|txn| txn.projection_watermark(PROJECTION_NAME))
            .unwrap();
        assert_eq!(committed, 1);
        drop(worker);

        let reopened = mirror_test_worker(&dir, &event_store);
        let retained = event_store.get_events_since_with_id(0, 1).unwrap();
        for _ in 0..3 {
            assert_eq!(reopened.process_batch(&retained).unwrap(), 0);
        }
        let connection = rusqlite::Connection::open(dir.path().join("projection.db")).unwrap();
        let active_kpi: i64 = connection
            .query_row("SELECT SUM(active_agents) FROM kpi_1m", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(active_kpi, 1);
        assert_eq!(
            reopened
                .read_store()
                .get_room("empfang")
                .unwrap()
                .unwrap()
                .occupant_count,
            1
        );
        assert_eq!(event_store.get_offset(PROJECTION_NAME).unwrap(), Some(0));
        competitor.execute_batch("ROLLBACK").unwrap();

        assert_eq!(
            live_batch(reopened.process_pending_batch()).unwrap(),
            Some(0)
        );
        assert_eq!(event_store.get_offset(PROJECTION_NAME).unwrap(), Some(1));
        assert_eq!(reopened.process_pending_batch().unwrap(), 1);
        assert_eq!(reopened.process_pending_batch().unwrap(), 0);
        assert_eq!(event_store.get_offset(PROJECTION_NAME).unwrap(), Some(2));
        assert_eq!(
            event_store.get_offset(HIERARCHY_PROJECTION_NAME).unwrap(),
            Some(0)
        );
        assert_eq!(reopened.read_store().active_agent_count().unwrap(), 2);
        assert_eq!(
            reopened
                .read_store()
                .get_room("empfang")
                .unwrap()
                .unwrap()
                .occupant_count,
            2
        );
        let active_kpi: i64 = connection
            .query_row("SELECT SUM(active_agents) FROM kpi_1m", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(active_kpi, 2);
        assert_eq!(
            reopened
                .read_store()
                .transaction(|txn| txn.projection_watermark(PROJECTION_NAME))
                .unwrap(),
            2
        );
    }

    #[test]
    fn hierarchy_mirror_contention_preserves_exactly_once_costs_frontier_and_strict_catchup() {
        let dir = tempdir().unwrap();
        let event_path = dir.path().join("events.db");
        let event_store = Arc::new(EventStore::open(event_path.to_str().unwrap()).unwrap());
        for project in ["project-a", "project-b"] {
            append_raw_event(&event_store, &planning_usage_event(project));
        }
        event_store.update_offset(PROJECTION_NAME, 0).unwrap();
        event_store
            .update_offset(HIERARCHY_PROJECTION_NAME, 0)
            .unwrap();
        let worker = mirror_test_worker(&dir, &event_store);
        let competitor = rusqlite::Connection::open(&event_path).unwrap();
        competitor.execute_batch("BEGIN IMMEDIATE").unwrap();
        // Explicit catch-up must return the error; only its live-loop adapter defers it.
        let error = worker.catch_up_hierarchy().unwrap_err();
        assert_eq!(live_batch(Err(error)).unwrap(), None);
        assert_eq!(
            event_store.get_offset(HIERARCHY_PROJECTION_NAME).unwrap(),
            Some(0)
        );
        assert_eq!(event_store.get_offset(PROJECTION_NAME).unwrap(), Some(0));
        assert_eq!(
            worker
                .read_store()
                .transaction(|txn| txn.projection_watermark(HIERARCHY_PROJECTION_NAME))
                .unwrap(),
            1
        );
        let meta = worker.read_store().hierarchy_projection_meta().unwrap();
        assert_eq!(meta.first_v2_event_id, Some(1));
        assert_eq!(meta.last_hierarchy_event_id, 1);
        let costs = worker.read_store().cost_by_hierarchy_tier().unwrap();
        assert_eq!(costs.len(), 1);
        assert_eq!(
            (
                costs[0].call_count,
                costs[0].input_tokens,
                costs[0].output_tokens
            ),
            (1, 10, 20)
        );
        drop(worker);

        let reopened = mirror_test_worker(&dir, &event_store);
        let retained = event_store.get_events_since_with_id(0, 1).unwrap();
        for _ in 0..3 {
            reopened.process_hierarchy_batch(&retained).unwrap();
        }
        assert_eq!(
            reopened.read_store().hierarchy_projection_meta().unwrap(),
            meta
        );
        let costs = reopened.read_store().cost_by_hierarchy_tier().unwrap();
        assert_eq!(
            (
                costs[0].call_count,
                costs[0].input_tokens,
                costs[0].output_tokens
            ),
            (1, 10, 20)
        );
        assert_eq!(
            event_store.get_offset(HIERARCHY_PROJECTION_NAME).unwrap(),
            Some(0)
        );
        competitor.execute_batch("ROLLBACK").unwrap();

        assert_eq!(
            live_batch(reopened.process_hierarchy_pending_batch()).unwrap(),
            Some(1)
        );
        assert_eq!(
            event_store.get_offset(HIERARCHY_PROJECTION_NAME).unwrap(),
            Some(1)
        );
        assert_eq!(reopened.catch_up_hierarchy().unwrap(), 1);
        assert_eq!(reopened.catch_up_hierarchy().unwrap(), 0);
        assert_eq!(
            event_store.get_offset(HIERARCHY_PROJECTION_NAME).unwrap(),
            Some(2)
        );
        assert_eq!(event_store.get_offset(PROJECTION_NAME).unwrap(), Some(0));
        let costs = reopened.read_store().cost_by_hierarchy_tier().unwrap();
        assert_eq!(
            (
                costs[0].call_count,
                costs[0].input_tokens,
                costs[0].output_tokens
            ),
            (2, 20, 40)
        );
        assert_eq!(
            reopened
                .read_store()
                .transaction(|txn| txn.projection_watermark(HIERARCHY_PROJECTION_NAME))
                .unwrap(),
            2
        );
        let meta = reopened.read_store().hierarchy_projection_meta().unwrap();
        assert_eq!(meta.last_usage_event_id, 2);
        assert_eq!(meta.last_hierarchy_event_id, 2);
    }

    #[test]
    fn missing_ordinary_and_hierarchy_mirrors_remain_terminal_under_real_contention() {
        for hierarchy in [false, true] {
            let dir = tempdir().unwrap();
            let event_path = dir.path().join("events.db");
            let event_store = Arc::new(EventStore::open(event_path.to_str().unwrap()).unwrap());
            append_event(
                &event_store,
                1,
                &DomainEventPayload::AgentSpawned {
                    agent_id: AgentId(1),
                    name: "Fixture Agent".into(),
                    role: "QA".into(),
                    shift_set: 1,
                    room_id: "empfang".into(),
                },
            );
            let worker = mirror_test_worker(&dir, &event_store);
            let competitor = rusqlite::Connection::open(&event_path).unwrap();
            competitor.execute_batch("BEGIN IMMEDIATE").unwrap();
            let (projection, result) = if hierarchy {
                (HIERARCHY_PROJECTION_NAME, worker.catch_up_hierarchy())
            } else {
                (PROJECTION_NAME, worker.process_pending_batch())
            };
            let error = result.unwrap_err();
            assert!(error
                .downcast_ref::<sentinel_limbo::event_store::ProjectionOffsetAcquisitionBusy>()
                .is_some());
            assert!(live_batch(Err(error)).is_err());
            assert_eq!(event_store.get_offset(projection).unwrap(), None);
            assert_eq!(
                worker
                    .read_store()
                    .transaction(|txn| txn.projection_watermark(projection))
                    .unwrap(),
                1
            );
            competitor.execute_batch("ROLLBACK").unwrap();
        }
    }

    #[test]
    fn explicit_rebuild_does_not_defer_writer_contention() {
        let dir = tempdir().unwrap();
        let event_path = dir.path().join("events.db");
        let event_store = Arc::new(EventStore::open(event_path.to_str().unwrap()).unwrap());
        event_store.update_offset(PROJECTION_NAME, 0).unwrap();
        event_store
            .update_offset(HIERARCHY_PROJECTION_NAME, 0)
            .unwrap();
        let worker = mirror_test_worker(&dir, &event_store);
        let competitor = rusqlite::Connection::open(&event_path).unwrap();
        competitor.execute_batch("BEGIN IMMEDIATE").unwrap();
        let error = worker.rebuild().unwrap_err();
        assert!(live_batch(Err(error)).is_err());
        assert_eq!(event_store.get_offset(PROJECTION_NAME).unwrap(), Some(0));
        assert_eq!(
            event_store.get_offset(HIERARCHY_PROJECTION_NAME).unwrap(),
            Some(0)
        );
        competitor.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn pending_batch_failure_rolls_back_presence_before_offset_mirroring() {
        let dir = tempdir().unwrap();
        let event_store =
            Arc::new(EventStore::open(dir.path().join("events.db").to_str().unwrap()).unwrap());
        append_event(
            &event_store,
            1,
            &DomainEventPayload::AgentSpawned {
                agent_id: AgentId(1),
                name: "Test Agent".into(),
                role: "QA".into(),
                shift_set: 1,
                room_id: "empfang".into(),
            },
        );
        let mut worker = ProjectionWorker::new(
            Arc::clone(&event_store),
            ProjectionConfig {
                db_path: dir
                    .path()
                    .join("projection.db")
                    .to_string_lossy()
                    .into_owned(),
                ..ProjectionConfig::default()
            },
        )
        .unwrap();
        worker.handlers.push(Box::new(FailingHandler));
        assert!(worker.process_pending_batch().is_err());
        assert!(worker.read_store().get_agent(1).unwrap().is_none());
        assert_eq!(
            worker
                .read_store()
                .get_room("empfang")
                .unwrap()
                .unwrap()
                .occupant_count,
            0
        );
        assert_eq!(event_store.get_offset(PROJECTION_NAME).unwrap(), None);

        worker.handlers.pop();
        assert_eq!(worker.process_pending_batch().unwrap(), 1);
        let projected = worker.read_store().get_agent(1).unwrap().unwrap();
        assert_eq!(projected.current_room.as_deref(), Some("empfang"));
        assert_eq!(
            worker
                .read_store()
                .get_room("empfang")
                .unwrap()
                .unwrap()
                .occupant_count,
            1
        );
        assert_eq!(
            event_store.get_offset(PROJECTION_NAME).unwrap(),
            Some(projected.last_event_id)
        );
        assert_eq!(worker.process_pending_batch().unwrap(), 0);
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
