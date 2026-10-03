//! Event-Stream-Push (#432): liest den Engine-Event-Bus (NATS JetStream `SENTINEL_EVENTS`)
//! und pusht bei jeder agent-relevanten Engine-Aenderung einen `agent_live`-Frame in den
//! Broadcast-Kanal — ersetzt das alte 1s-Projection-Polling (kein Poll, idle ~0 Arbeit).
//!
//! #433 erweitert denselben Push-Pfad auf `room_live` und `kpi`: relevante Events setzen Dirty-Flags,
//! der 150ms-Tick liest jedes betroffene Read-Model hoechstens einmal und sendet ein Topic-Frame.
//! Der append-only Event-Log-Stream aktualisiert eine CAS-Plane aus `events.db`; Clients ziehen den
//! geordneten Block-Delta-Zustand per WebTransport-Bi-Stream (`HelloManifest -> EventLogCasResponse`).
//! Der Broadcast-Kanal sendet dafuer nur kleine `event_log_cas_tick`-Frames als Sync-Trigger.
//!
//! **Strategie: Voll-Satz pro Projection + client-`reconcile`**. Bei ~26-43 winzigen Agent-Zeilen
//! ist der zstd-komprimierte Voll-Satz <4 KB; `reconcile({key:"agent_id"})` erzeugt das DOM-Delta
//! (Despawn/Resync gratis). Der grosse append-only Event-Log nutzt CAS-Block-Deltas (#464).
//!
//! **Consumer: ephemeral** (kein `durable_name`) + **`DeliverPolicy::New`** — ab Connect nur NEUE
//! Events, KEIN Backlog-Replay des 7-Tage-Streams; der Connect-Snapshot (`wt.rs`) deckt den
//! Ist-Zustand. `InactiveThreshold` raeumt den Consumer nach Disconnect serverseitig auf.
//! A failed pull stream is discarded and recreated through the bounded reconnect loop.
//! Every new subscription refreshes all live projections for already connected clients;
//! this repairs missed notifications without replaying events or business effects.

use std::time::Duration;

use futures::StreamExt;

use crate::AppState;

pub(crate) const EVENT_LOG_BATCH_LIMIT: usize = 500;
pub(crate) const EVENT_LOG_BACKFILL_RECENT_LIMIT: usize = 9_000;
pub(crate) const EVENT_LOG_BACKFILL_FOCUS_LIMIT: usize = 500;
pub(crate) const EVENT_LOG_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DirtyModels {
    pub agents: bool,
    pub rooms: bool,
    pub kpi: bool,
}

impl DirtyModels {
    fn merge(&mut self, other: Self) {
        self.agents |= other.agents;
        self.rooms |= other.rooms;
        self.kpi |= other.kpi;
    }

    fn is_clean(self) -> bool {
        !self.agents && !self.rooms && !self.kpi
    }
}

/// Reine Raum-/KPI-/Task-Events: sie aendern das `agent_live`-Read-Model NICHT.
const NON_AGENT_EVENT_TYPES: &[&str] = &[
    "room_physics_updated",
    "chaos_triggered",
    "room_stimulus_applied",
    "smell_event_triggered",
    "tick_snapshot",
    "nightrun_started",
    "nightrun_completed",
    "task_created",
    "task_assigned",
    "task_status_changed",
    "task_completed",
    "task_blocked",
    // #427: cost telemetry is projection-only — it changes no live view.
    "agent_llm_usage",
    "migration_completed",
    "psi_band_changed",
    "node_provisioned",
    "workbench_invocation_updated",
    "project_closeout_published",
    "gaia_project_oversight_observed",
];

const ROOM_EVENT_TYPES: &[&str] = &[
    "agent_spawned",
    "agent_status_changed",
    "bio_state_updated",
    "transit_started",
    "transit_completed",
    "chaos_triggered",
    "room_physics_updated",
    "agent_despawned",
    "shift_transition_completed",
    "smell_event_triggered",
];

const KPI_EVENT_TYPES: &[&str] = &[
    "agent_spawned",
    "agent_despawned",
    "transit_started",
    "agent_action_received",
    "chaos_triggered",
    "tick_snapshot",
    "shift_transition_completed",
    "nightrun_started",
    "nightrun_completed",
    "agent_consolidated",
    "agent_consolidation_failed",
];

const KNOWN_EVENT_TYPES: &[&str] = &[
    "agent_action_received",
    "transit_started",
    "transit_completed",
    "chaos_triggered",
    "bio_action_performed",
    "bio_state_updated",
    "room_physics_updated",
    "room_stimulus_applied",
    "tick_snapshot",
    "agent_spawned",
    "agent_despawned",
    "task_created",
    "task_assigned",
    "task_status_changed",
    "task_completed",
    "task_blocked",
    "shift_transition_completed",
    "agent_status_changed",
    "nightrun_started",
    "nightrun_completed",
    "agent_consolidated",
    "agent_consolidation_failed",
    "judge_alert_received",
    "hallway_encounter_detected",
    "smell_event_triggered",
    "platform_intervention",
    "platform_analysis",
    "resource_profile_changed",
    "security_exec_blocked",
    "operator_gaia_sent",
    "operator_broadcast_sent",
    "operator_dm_sent",
    "config_applied",
    "migration_completed",
    "psi_band_changed",
    "node_provisioned",
    "workbench_invocation_updated",
    "project_closeout_published",
    "gaia_project_oversight_observed",
    "agent_llm_usage",
];

/// Welche Read-Models ein Event-Typ neu lesen/pushen muss.
///
/// Die Listen spiegeln die Projection-Handler (`agent_live_view`, `room_live_view`, `kpi_1m`).
/// `config_applied` und unbekannte/neue Typen triggern fail-safe alle drei Views.
pub(crate) fn classify(event_type: &str) -> DirtyModels {
    if event_type == "config_applied" || !KNOWN_EVENT_TYPES.contains(&event_type) {
        return DirtyModels {
            agents: true,
            rooms: true,
            kpi: true,
        };
    }
    DirtyModels {
        agents: !NON_AGENT_EVENT_TYPES.contains(&event_type),
        rooms: ROOM_EVENT_TYPES.contains(&event_type),
        kpi: KPI_EVENT_TYPES.contains(&event_type),
    }
}

/// Extrahiert `<type>` aus `sentinel.events.<type>.<aggregate_id>`.
pub(crate) fn event_type_from_subject(subject: &str) -> &str {
    subject
        .strip_prefix("sentinel.events.")
        .and_then(|rest| rest.split('.').next())
        .unwrap_or("")
}

/// Ob beim Coalescing-Tick der DB-Read uebersprungen wird (Idle-Paritaet zum alten 1s-Poll,
/// CHANGELOG #277): keine verbundenen WebTransport-Clients -> kein Read/encode.
pub(crate) fn should_skip_read(receiver_count: usize) -> bool {
    receiver_count == 0
}

/// Startet den Event-Subscriber als Daemon-Task (reconnect mit Backoff, kein Panic/Exit).
pub async fn run_event_subscriber(state: AppState) {
    let mut backoff = Duration::from_millis(500);
    loop {
        match subscribe_and_pump(&state).await {
            Ok(()) => tracing::warn!("event subscriber: NATS stream ended, reconnecting"),
            Err(e) => {
                tracing::warn!(error = %e, "event subscriber error, reconnecting with backoff")
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

/// Aktualisiert die Event-Log-CAS-Plane und pusht nur einen kleinen `event_log_cas_tick`.
///
/// Der vollstaendige Event-Log-Zustand wird nicht mehr als Topic-Frame broadcastet. Verbundene
/// Clients oeffnen beim Tick einen Bi-Stream und bekommen nur fehlende CAS-Bloecke.
pub async fn run_event_log_pusher(state: AppState) {
    let mut tick = tokio::time::interval(EVENT_LOG_POLL_INTERVAL);
    loop {
        tick.tick().await;
        if should_skip_read(state.broadcast_tx.receiver_count()) {
            continue;
        }
        match refresh_event_log_cas(&state) {
            Ok(inserted) if inserted > 0 => push_event_log_cas_tick(&state),
            Ok(_) => {}
            Err(e) => tracing::warn!(
                error = %e,
                path = %state.config.events_db,
                "event_log CAS refresh skipped"
            ),
        }
    }
}

/// Verbindet zu NATS, erstellt einen ephemeren `DeliverPolicy::New`-Consumer und pusht koalesziert
/// `agent_live`-Frames. Kehrt mit `Err` bei Verbindungs-/Streamfehlern zurueck (Caller retried).
async fn subscribe_and_pump(state: &AppState) -> anyhow::Result<()> {
    let client = async_nats::connect(&state.config.nats_url).await?;
    let jetstream = async_nats::jetstream::new(client);

    // Stream idempotent holen/erstellen (SSOT pkg/sentinel-go/messaging/streams.go).
    let stream = jetstream
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: "SENTINEL_EVENTS".to_string(),
            subjects: vec!["sentinel.events.>".to_string()],
            ..Default::default()
        })
        .await?;

    // Ephemeral (kein durable_name) + DeliverPolicy::New: nur neue Events, kein Backlog-Replay des
    // 7-Tage-Streams (sonst Last-/Latenz-Sturm + veraltete Deltas beim Restart). AckPolicy::None —
    // reiner Live-Push; verpasste Events deckt der Connect-Snapshot (wt.rs) ab. inactive_threshold
    // raeumt den Consumer nach Disconnect serverseitig auf (kein State-Leak).
    let consumer = stream
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::New,
            ack_policy: async_nats::jetstream::consumer::AckPolicy::None,
            filter_subject: "sentinel.events.>".to_string(),
            inactive_threshold: Duration::from_secs(30),
            ..Default::default()
        })
        .await?;
    tracing::info!(
        label = %state.config.nats_consumer,
        "event subscriber: subscribed to SENTINEL_EVENTS (ephemeral, DeliverPolicy::New)"
    );

    pump_live_messages(state, consumer.messages().await?).await
}

async fn pump_live_messages<S>(state: &AppState, mut messages: S) -> anyhow::Result<()>
where
    S: futures::Stream<
            Item = Result<
                async_nats::jetstream::Message,
                async_nats::jetstream::consumer::pull::MessagesError,
            >,
        > + Unpin,
{
    // DeliverPolicy::New skips events lost during disconnect; refresh existing clients too.
    let mut dirty = classify("config_applied");
    let mut tick = tokio::time::interval(Duration::from_millis(150));

    loop {
        tokio::select! {
            maybe = messages.next() => match maybe {
                Some(Ok(msg)) => {
                    dirty.merge(classify(event_type_from_subject(msg.subject.as_str())));
                }
                Some(Err(e)) => {
                    let context = format!("nats live consumer failed: {}", e.kind());
                    return Err(anyhow::Error::new(e).context(context));
                },
                None => anyhow::bail!("nats messages stream ended"),
            },
            _ = tick.tick() => {
                if dirty.is_clean() {
                    continue;
                }
                // Idle-Paritaet (#277): keine Clients -> kein DB-Read/encode, nur Flag clearen.
                if should_skip_read(state.broadcast_tx.receiver_count()) {
                    dirty = DirtyModels::default();
                    continue;
                }
                push_dirty(state, dirty);
                dirty = DirtyModels::default();
            }
        }
    }
}

fn push_dirty(state: &AppState, dirty: DirtyModels) {
    if dirty.agents {
        push_agent_live(state);
    }
    if dirty.rooms {
        push_room_live(state);
    }
    if dirty.kpi {
        push_kpi(state);
    }
}

pub(crate) fn max_event_log_id(events: &[serde_json::Value]) -> Option<i64> {
    events
        .iter()
        .filter_map(|event| event.get("id").and_then(serde_json::Value::as_i64))
        .max()
}

pub(crate) fn refresh_event_log_cas(state: &AppState) -> anyhow::Result<usize> {
    let (empty, mut offset) = {
        let guard = state
            .event_cas
            .lock()
            .map_err(|_| anyhow::anyhow!("event_log CAS lock poisoned"))?;
        (guard.is_empty(), guard.max_event_id())
    };

    let mut inserted = 0;
    if empty {
        let events = crate::events::event_log_backfill_events(
            &state.config.events_db,
            EVENT_LOG_BACKFILL_RECENT_LIMIT,
            EVENT_LOG_BACKFILL_FOCUS_LIMIT,
        )?;
        inserted += ingest_event_log_cas(state, &events)?;
        offset = {
            let guard = state
                .event_cas
                .lock()
                .map_err(|_| anyhow::anyhow!("event_log CAS lock poisoned"))?;
            guard.max_event_id()
        };
    }

    loop {
        let events =
            crate::events::events_after_id(&state.config.events_db, offset, EVENT_LOG_BATCH_LIMIT)?;
        if events.is_empty() {
            break;
        }
        if let Some(max_seen) = max_event_log_id(&events) {
            offset = offset.max(max_seen);
        }
        let batch_len = events.len();
        inserted += ingest_event_log_cas(state, &events)?;
        if batch_len < EVENT_LOG_BATCH_LIMIT {
            break;
        }
    }

    Ok(inserted)
}

fn ingest_event_log_cas(state: &AppState, events: &[serde_json::Value]) -> anyhow::Result<usize> {
    if events.is_empty() {
        return Ok(0);
    }
    let mut guard = state
        .event_cas
        .lock()
        .map_err(|_| anyhow::anyhow!("event_log CAS lock poisoned"))?;
    guard.ingest_events(events)
}

pub(crate) fn push_event_log_cas_tick(state: &AppState) {
    let stats = match state.event_cas.lock() {
        Ok(guard) => guard.stats_snapshot(),
        Err(_) => {
            tracing::warn!("event_log CAS tick skipped (lock poisoned)");
            return;
        }
    };
    match crate::codec::encode_frame("event_log_cas_tick", &serde_json::json!({ "stats": stats })) {
        Ok(frame) => {
            let _ = state.broadcast_tx.send(frame);
        }
        Err(e) => tracing::warn!(error = %e, "event_log_cas_tick encode_frame failed"),
    }
}

/// Liest den vollen `agent_live`-Satz (read-only) und broadcastet ihn als topic-Frame.
fn push_agent_live(state: &AppState) {
    match crate::projection::agents_rows(&state.config.projection_db) {
        Ok(rows) => {
            match crate::codec::encode_frame("agent_live", &serde_json::json!({ "agents": rows })) {
                Ok(frame) => {
                    // Err = 0 Receiver (Race mit Disconnect) — ignorierbar.
                    let _ = state.broadcast_tx.send(frame);
                }
                Err(e) => tracing::warn!(error = %e, "agent_live encode_frame failed"),
            }
        }
        Err(e) => tracing::warn!(error = %e, "agent_live delta skipped (projection read failed)"),
    }
}

fn push_room_live(state: &AppState) {
    match crate::projection::rooms_rows(&state.config.projection_db) {
        Ok(rows) => {
            match crate::codec::encode_frame("room_live", &serde_json::json!({ "rooms": rows })) {
                Ok(frame) => {
                    let _ = state.broadcast_tx.send(frame);
                }
                Err(e) => tracing::warn!(error = %e, "room_live encode_frame failed"),
            }
        }
        Err(e) => tracing::warn!(error = %e, "room_live delta skipped (projection read failed)"),
    }
}

fn push_kpi(state: &AppState) {
    match crate::projection::metrics_row(&state.config.projection_db) {
        Ok(kpi) => match crate::codec::encode_frame("kpi", &serde_json::json!({ "kpi": kpi })) {
            Ok(frame) => {
                let _ = state.broadcast_tx.send(frame);
            }
            Err(e) => tracing::warn!(error = %e, "kpi encode_frame failed"),
        },
        Err(e) => tracing::warn!(error = %e, "kpi delta skipped (projection read failed)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn live_consumer_errors_exit_before_polling_the_dead_stream_again() {
        use async_nats::jetstream::consumer::pull::{MessagesError, MessagesErrorKind};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let state = crate::AppState::new(crate::Config::from_env()).unwrap();
        for kind in [
            MessagesErrorKind::NoResponders,
            MessagesErrorKind::MissingHeartbeat,
            MessagesErrorKind::ConsumerDeleted,
            MessagesErrorKind::Pull,
            MessagesErrorKind::PushBasedConsumer,
            MessagesErrorKind::Other,
        ] {
            let polls_after_error = AtomicUsize::new(0);
            let stream = futures::stream::iter([Err(MessagesError::new(kind))]).chain(
                futures::stream::poll_fn(|_| {
                    polls_after_error.fetch_add(1, Ordering::SeqCst);
                    std::task::Poll::Pending
                }),
            );
            let failure =
                tokio::time::timeout(Duration::from_secs(2), pump_live_messages(&state, stream))
                    .await
                    .expect("consumer error must return to the bounded reconnect loop")
                    .unwrap_err();
            assert_eq!(
                failure.to_string(),
                format!("nats live consumer failed: {kind}")
            );
            assert_eq!(failure.root_cause().to_string(), kind.to_string());
            assert_eq!(polls_after_error.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn live_consumer_end_returns_to_the_reconnect_loop() {
        let state = crate::AppState::new(crate::Config::from_env()).unwrap();
        let failure = tokio::time::timeout(
            Duration::from_secs(2),
            pump_live_messages(&state, futures::stream::empty()),
        )
        .await
        .expect("ended consumer must not leave a ticking zombie task")
        .unwrap_err();
        assert_eq!(failure.to_string(), "nats messages stream ended");
    }

    #[tokio::test]
    async fn live_consumer_reconnect_refreshes_existing_clients_without_a_new_event() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute_batch(
            "CREATE TABLE agent_live_view (agent_id INTEGER PRIMARY KEY,name TEXT NOT NULL,role TEXT NOT NULL,\
             shift_set INTEGER NOT NULL,status TEXT NOT NULL,current_room TEXT,in_transit INTEGER NOT NULL,\
             transit_target TEXT,last_action TEXT,last_action_tick INTEGER,hunger REAL NOT NULL,energy REAL NOT NULL,\
             stress REAL NOT NULL,bladder REAL NOT NULL,social_need REAL NOT NULL,caffeine_mg REAL NOT NULL,mood TEXT,\
             last_event_id INTEGER NOT NULL,updated_at INTEGER NOT NULL);\
             INSERT INTO agent_live_view VALUES (36,'Nils','Developer',1,'active','office',0,NULL,NULL,NULL,\
             0.2,0.8,0.1,0.0,0.0,0.0,'focused',5,100);\
             CREATE TABLE room_live_view (room_id TEXT PRIMARY KEY,occupant_count INTEGER NOT NULL,\
             transit_count INTEGER NOT NULL,active_chaos TEXT,active_smells TEXT,temperature REAL,\
             co2_ppm REAL,noise_db REAL,last_event_tick INTEGER,last_event_id INTEGER NOT NULL,\
             updated_at INTEGER NOT NULL);\
             INSERT INTO room_live_view VALUES ('office',1,0,'[]','[]',22.0,650.0,40.0,7,11,1200);\
             CREATE TABLE kpi_1m (bucket_start INTEGER PRIMARY KEY,active_agents INTEGER NOT NULL,\
             total_actions INTEGER NOT NULL,total_transits INTEGER NOT NULL,chaos_events INTEGER NOT NULL,\
             tick_count INTEGER NOT NULL,shift_changes INTEGER NOT NULL,nightrun_events INTEGER NOT NULL,\
             updated_at INTEGER NOT NULL);\
             INSERT INTO kpi_1m VALUES (1000,1,30,4,1,60,0,0,1100);"
        ).unwrap();
        let projection_before = std::fs::read(&db).unwrap();
        let mut config = crate::Config::from_env();
        config.projection_db = db.to_string_lossy().into_owned();
        let state = crate::AppState::new(config).unwrap();
        let mut receiver = state.broadcast_tx.subscribe();
        let pump_state = state.clone();
        let pump = tokio::spawn(async move {
            pump_live_messages(&pump_state, futures::stream::pending()).await
        });
        let mut frames = std::collections::BTreeMap::new();
        for _ in 0..3 {
            let frame = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
                .await
                .expect("reconnect must refresh all live projections")
                .unwrap();
            let (topic, value): (String, serde_json::Value) =
                crate::codec::decode_frame_as(&frame).unwrap();
            frames.insert(topic, value);
        }
        pump.abort();
        assert!(pump.await.unwrap_err().is_cancelled());
        assert_eq!(frames.len(), 3);
        assert_eq!(frames["agent_live"]["agents"][0]["name"], "Nils");
        assert_eq!(frames["room_live"]["rooms"][0]["room_id"], "office");
        assert_eq!(frames["kpi"]["kpi"]["active_agents"], 1);
        assert_eq!(std::fs::read(&db).unwrap(), projection_before);
    }

    /// Alle Event-Typen aus `sentinel_common::DomainEventPayload::event_type_str()` (SSOT-Spiegel,
    /// events.rs:366-400). Neue Varianten faengt der Fail-safe-Default ab (siehe
    /// `unknown_event_type_is_push_relevant`).
    const ALL_KNOWN_EVENT_TYPES: &[&str] = &[
        "agent_action_received",
        "transit_started",
        "transit_completed",
        "chaos_triggered",
        "bio_action_performed",
        "bio_state_updated",
        "room_physics_updated",
        "room_stimulus_applied",
        "tick_snapshot",
        "agent_spawned",
        "agent_despawned",
        "task_created",
        "task_assigned",
        "task_status_changed",
        "task_completed",
        "task_blocked",
        "shift_transition_completed",
        "agent_status_changed",
        "nightrun_started",
        "nightrun_completed",
        "agent_consolidated",
        "agent_consolidation_failed",
        "judge_alert_received",
        "hallway_encounter_detected",
        "smell_event_triggered",
        "platform_intervention",
        "platform_analysis",
        "resource_profile_changed",
        "security_exec_blocked",
        "operator_gaia_sent",
        "operator_broadcast_sent",
        "operator_dm_sent",
        "config_applied",
        "migration_completed",
        "psi_band_changed",
        "node_provisioned",
        "workbench_invocation_updated",
        "project_closeout_published",
        "gaia_project_oversight_observed",
        "agent_llm_usage",
    ];

    /// Die im Plan/Issue explizit agent-relevanten Events MUESSEN einen Push ausloesen.
    const AGENT_RELEVANT: &[&str] = &[
        "agent_spawned",
        "agent_despawned",
        "agent_action_received",
        "transit_started",
        "transit_completed",
        "agent_status_changed",
        "bio_state_updated",
        "shift_transition_completed",
    ];

    #[test]
    fn classify_covers_full_event_type_list() {
        // Jeder bekannte Typ ist eindeutig pro Read-Model klassifiziert.
        for &et in ALL_KNOWN_EVENT_TYPES {
            let expected = if et == "config_applied" {
                DirtyModels {
                    agents: true,
                    rooms: true,
                    kpi: true,
                }
            } else {
                DirtyModels {
                    agents: !NON_AGENT_EVENT_TYPES.contains(&et),
                    rooms: ROOM_EVENT_TYPES.contains(&et),
                    kpi: KPI_EVENT_TYPES.contains(&et),
                }
            };
            assert_eq!(classify(et), expected, "Klassifikation fuer {et}");
        }
    }

    #[test]
    fn agent_relevant_events_trigger_push() {
        for &et in AGENT_RELEVANT {
            assert!(
                classify(et).agents,
                "{et} muss einen agent_live-Push ausloesen"
            );
        }
    }

    #[test]
    fn pure_room_kpi_task_events_do_not_push_agents() {
        for &et in NON_AGENT_EVENT_TYPES {
            assert!(!classify(et).agents, "{et} darf agent_live nicht ausloesen");
        }
    }

    #[test]
    fn config_applied_triggers_resync_push() {
        assert_eq!(
            classify("config_applied"),
            DirtyModels {
                agents: true,
                rooms: true,
                kpi: true,
            },
            "config_applied muss alle Live-Views resynchronisieren"
        );
    }

    #[test]
    fn unknown_event_type_is_push_relevant() {
        // Fail-safe: ein unbekannter / neu hinzugefuegter Typ darf die View NICHT still stale lassen.
        assert_eq!(
            classify("some_future_event_v2"),
            DirtyModels {
                agents: true,
                rooms: true,
                kpi: true,
            }
        );
        assert_eq!(
            classify(""),
            DirtyModels {
                agents: true,
                rooms: true,
                kpi: true,
            }
        );
    }

    #[test]
    fn idle_skip_only_when_no_receivers() {
        // Idle-Paritaet zum alten 1s-Poll (#277): 0 Clients -> DB-Read ueberspringen.
        assert!(should_skip_read(0));
        assert!(!should_skip_read(1));
        assert!(!should_skip_read(43));
    }

    #[test]
    fn status_and_bio_events_refresh_agents_and_rooms_without_kpi() {
        for event_type in ["agent_status_changed", "bio_state_updated"] {
            assert_eq!(
                classify(event_type),
                DirtyModels {
                    agents: true,
                    rooms: true,
                    kpi: false,
                },
                "{event_type} must refresh on-site room presence"
            );
        }
    }

    #[test]
    fn repeated_status_and_bio_events_coalesce_into_one_dirty_set() {
        let mut dirty = DirtyModels::default();
        assert!(dirty.is_clean());
        for _ in 0..1_000 {
            dirty.merge(classify("bio_state_updated"));
            dirty.merge(classify("agent_status_changed"));
        }
        assert_eq!(
            dirty,
            DirtyModels {
                agents: true,
                rooms: true,
                kpi: false,
            }
        );
        dirty = DirtyModels::default();
        assert!(dirty.is_clean());
    }

    #[test]
    fn event_type_parsed_from_subject() {
        assert_eq!(
            event_type_from_subject("sentinel.events.bio_state_updated.AGENT-07"),
            "bio_state_updated"
        );
        assert_eq!(
            event_type_from_subject("sentinel.events.config_applied.runtime"),
            "config_applied"
        );
        // Defensive: falsch geformtes Subject -> "" -> fail-safe push-relevant.
        assert_eq!(event_type_from_subject("garbage"), "");
        assert!(classify(event_type_from_subject("garbage")).agents);
    }

    #[test]
    fn max_event_log_id_reads_highest_numeric_id() {
        let events = vec![
            serde_json::json!({"id": 7, "event_type": "agent_spawned"}),
            serde_json::json!({"id": 12, "event_type": "chaos_triggered"}),
            serde_json::json!({"event_type": "legacy_without_id"}),
        ];
        assert_eq!(max_event_log_id(&events), Some(12));
        assert_eq!(max_event_log_id(&[]), None);
    }

    #[tokio::test]
    async fn push_event_log_cas_tick_sends_sync_trigger() {
        let state = crate::AppState::new(crate::Config::from_env()).unwrap();
        state
            .event_cas
            .lock()
            .unwrap()
            .ingest_events(&[serde_json::json!({
                "id": 42,
                "event_id": "evt-42",
                "event_type": "agent_action_received",
                "payload": "{\"content\":\"Hallo\"}"
            })])
            .unwrap();
        let mut rx = state.broadcast_tx.subscribe();

        push_event_log_cas_tick(&state);

        let frame = rx
            .try_recv()
            .expect("event_log_cas_tick muss einen Broadcast-Frame pushen");
        let (topic, value): (String, serde_json::Value) =
            crate::codec::decode_frame_as(&frame).expect("decode frame");
        assert_eq!(topic, "event_log_cas_tick");
        assert_eq!(value["stats"]["event_count"], 1);
        assert_eq!(value["stats"]["max_event_id"], 42);
    }

    /// Integrationstest (Maßgabe Hauptsession): ein `config_applied`-Event laeuft durch classify
    /// (-> dirty) und der Resync-Push (`push_agent_live`) legt tatsaechlich einen `agent_live`-Frame
    /// auf den Broadcast-Kanal. Belegt den genannten config_applied->Resync-Pfad end-to-end (die
    /// Live-Optik O3 zeigt denselben Effekt via Schichtwechsel).
    #[tokio::test]
    async fn config_applied_event_pushes_agent_live_frame() {
        // Temp projection.db mit dem agent_live_view-Schema + einem Agenten.
        let dir = std::env::temp_dir().join(format!("evsub-cfgapply-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("projection.db");
        {
            let c = rusqlite::Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE agent_live_view (agent_id INTEGER PRIMARY KEY,name TEXT NOT NULL,role TEXT NOT NULL,\
                 shift_set INTEGER NOT NULL,status TEXT NOT NULL,current_room TEXT,in_transit INTEGER NOT NULL,\
                 transit_target TEXT,last_action TEXT,last_action_tick INTEGER,hunger REAL NOT NULL,energy REAL NOT NULL,\
                 stress REAL NOT NULL,bladder REAL NOT NULL,social_need REAL NOT NULL,caffeine_mg REAL NOT NULL,mood TEXT,\
                 last_event_id INTEGER NOT NULL,updated_at INTEGER NOT NULL);\
                 INSERT INTO agent_live_view VALUES (1,'Thomas','CEO',1,'active','buero-ceo',0,NULL,NULL,NULL,\
                 0.2,0.8,0.1,0.0,0.0,0.0,'fokussiert',5,100);",
            )
            .unwrap();
        }
        let mut config = crate::Config::from_env();
        config.projection_db = db.to_string_lossy().into_owned();
        let state = crate::AppState::new(config).unwrap();
        let mut rx = state.broadcast_tx.subscribe();

        // config_applied ist push-relevant -> Resync-Push ausloesen (wie der Tick es taete).
        assert!(
            classify("config_applied").agents,
            "config_applied muss dirty setzen"
        );
        push_agent_live(&state);

        // Ein agent_live-Frame muss auf dem Broadcast-Kanal liegen.
        let frame = rx
            .try_recv()
            .expect("config_applied muss einen agent_live-Frame pushen");
        let (topic, value): (String, serde_json::Value) =
            crate::codec::decode_frame_as(&frame).expect("decode frame");
        assert_eq!(topic, "agent_live");
        assert_eq!(value["agents"][0]["name"], "Thomas");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn dirty_room_and_kpi_pushes_topic_frames() {
        let dir = std::env::temp_dir().join(format!("evsub-room-kpi-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("projection.db");
        {
            let c = rusqlite::Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE room_live_view (room_id TEXT PRIMARY KEY,occupant_count INTEGER NOT NULL,\
                 transit_count INTEGER NOT NULL,active_chaos TEXT,active_smells TEXT,temperature REAL,\
                 co2_ppm REAL,noise_db REAL,last_event_tick INTEGER,last_event_id INTEGER NOT NULL,\
                 updated_at INTEGER NOT NULL);\
                 INSERT INTO room_live_view VALUES ('kueche',2,0,'[]','[]',22.0,650.0,40.0,7,11,1200);\
                 CREATE TABLE kpi_1m (bucket_start INTEGER PRIMARY KEY,active_agents INTEGER NOT NULL,\
                 total_actions INTEGER NOT NULL,total_transits INTEGER NOT NULL,chaos_events INTEGER NOT NULL,\
                 tick_count INTEGER NOT NULL,shift_changes INTEGER NOT NULL,nightrun_events INTEGER NOT NULL,\
                 updated_at INTEGER NOT NULL);\
                 INSERT INTO kpi_1m VALUES (1000,12,30,4,1,60,0,0,1100);",
            )
            .unwrap();
        }
        let mut config = crate::Config::from_env();
        config.projection_db = db.to_string_lossy().into_owned();
        let state = crate::AppState::new(config).unwrap();
        let mut rx = state.broadcast_tx.subscribe();

        push_dirty(
            &state,
            DirtyModels {
                agents: false,
                rooms: true,
                kpi: true,
            },
        );

        let mut seen = Vec::new();
        for _ in 0..2 {
            let frame = rx
                .try_recv()
                .expect("room_live und kpi muessen Frames senden");
            let (topic, value): (String, serde_json::Value) =
                crate::codec::decode_frame_as(&frame).expect("decode frame");
            seen.push((topic, value));
        }
        seen.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(seen[0].0, "kpi");
        assert_eq!(seen[0].1["kpi"]["active_agents"], 12);
        assert_eq!(seen[1].0, "room_live");
        assert_eq!(seen[1].1["rooms"][0]["room_id"], "kueche");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
