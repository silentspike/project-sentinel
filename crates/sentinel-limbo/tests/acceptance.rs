//! Acceptance tests for sentinel-limbo (Issue #8).

use sentinel_common::{AgentId, DomainEvent, Emotion, RoomId, Tick, Timestamp};
use sentinel_limbo::{ChatStore, EventStore};

// ──────────────────────────────────────────────
// ChatStore Acceptance Tests
// ──────────────────────────────────────────────

/// ChatStore: Alle 4 Tabellen werden bei open erstellt.
#[tokio::test]
async fn ac_08_chat_tables_created() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let _store = ChatStore::open(path.to_str().unwrap()).await.unwrap();

    let conn = rusqlite::Connection::open(&path).unwrap();
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap();
    let tables: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    assert!(
        tables.contains(&"messages".to_string()),
        "Table 'messages' not found in: {:?}",
        tables
    );
    assert!(
        tables.contains(&"meetings".to_string()),
        "Table 'meetings' not found in: {:?}",
        tables
    );
    assert!(
        tables.contains(&"observations".to_string()),
        "Table 'observations' not found in: {:?}",
        tables
    );
    assert!(
        tables.contains(&"chaos_events".to_string()),
        "Table 'chaos_events' not found in: {:?}",
        tables
    );
}

/// ChatStore: Performance Pragmas aktiv.
#[tokio::test]
async fn ac_08_chat_performance_pragmas() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let _store = ChatStore::open(path.to_str().unwrap()).await.unwrap();

    let conn = rusqlite::Connection::open(&path).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        mode.to_lowercase(),
        "wal",
        "journal_mode should be WAL, got: {}",
        mode
    );

    let wal_path = dir.path().join("test.db-wal");
    assert!(
        wal_path.exists(),
        "WAL file should exist at {:?}, proving WAL mode is active",
        wal_path
    );
}

/// ChatStore: Messages Insert + Query Roundtrip.
#[tokio::test]
async fn ac_08_chat_messages_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let store = ChatStore::open(path.to_str().unwrap()).await.unwrap();

    let room = RoomId::new(1).unwrap();
    let agent = AgentId::new(1).unwrap();

    let id = store
        .insert_message(
            room,
            agent,
            "Guten Morgen zusammen!",
            Some(Emotion::Happy),
            Timestamp(1000),
            Tick(42),
        )
        .await
        .unwrap();
    assert!(id > 0, "Insert should return a positive rowid");

    let messages = store.get_room_messages(room, 10).await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].agent_name, "AGENT-01");
    assert_eq!(messages[0].content, "Guten Morgen zusammen!");
    assert_eq!(messages[0].emotion, Some("happy".to_string()));
    assert_eq!(messages[0].timestamp, Timestamp(1000));
    assert_eq!(messages[0].tick, Tick(42));
}

/// ChatStore: Meeting Lifecycle (create, end, status).
#[tokio::test]
async fn ac_08_chat_meeting_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let store = ChatStore::open(path.to_str().unwrap()).await.unwrap();

    let room = RoomId::new(5).unwrap();
    let participants = vec![AgentId::new(1).unwrap(), AgentId::new(2).unwrap()];

    let meeting_id = store
        .insert_meeting(room, "Sprint Review", &participants, Timestamp(9000))
        .await
        .unwrap();
    assert!(
        meeting_id > 0,
        "Meeting insert should return positive rowid"
    );

    let conn = rusqlite::Connection::open(dir.path().join("test.db")).unwrap();
    let ended_at: Option<i64> = conn
        .query_row(
            "SELECT ended_at FROM meetings WHERE id = ?1",
            [meeting_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(ended_at.is_none(), "Meeting should be open (ended_at NULL)");

    store
        .end_meeting(meeting_id, Timestamp(10800), "Sprint goals achieved")
        .await
        .unwrap();

    let (ended_at, summary): (i64, String) = conn
        .query_row(
            "SELECT ended_at, summary FROM meetings WHERE id = ?1",
            [meeting_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(ended_at, 10800);
    assert_eq!(summary, "Sprint goals achieved");
}

// ──────────────────────────────────────────────
// EventStore Acceptance Tests
// ──────────────────────────────────────────────

fn test_event(event_type: &str, aggregate_id: &str) -> DomainEvent {
    DomainEvent::new(event_type, aggregate_id, r#"{"test":true}"#, "corr-ac", 42)
}

/// AC-1: events hat keinen Update/Delete Pfad.
#[test]
fn ac_08_01_events_append_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ac01.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    let event = test_event("agent_action_received", "AGENT-01");
    store.append_event(&event).unwrap();

    // EventStore API bietet kein update/delete — Event bleibt unveraendert
    let events = store.get_events_since(0, 10).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_id, event.event_id);
    assert_eq!(events[0].event_type, "agent_action_received");
    assert_eq!(events[0].aggregate_id, "AGENT-01");
    assert_eq!(events[0].payload, r#"{"test":true}"#);
}

/// AC-2: Event + Outbox Write sind atomar.
#[test]
fn ac_08_02_event_outbox_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ac02.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    let event = test_event("transit_started", "AGENT-02");
    store
        .append_with_outbox(&event, "sentinel/events/AGENT-02")
        .unwrap();

    // Beide muessen existieren (atomar)
    assert_eq!(store.event_count().unwrap(), 1);
    let outbox = store.poll_outbox(10).unwrap();
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0].event_id, event.event_id);
    assert_eq!(outbox[0].topic, "sentinel/events/AGENT-02");
}

/// AC-3: operation_id bleibt bei Retry identisch (kein Duplikat).
#[test]
fn ac_08_03_operation_id_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ac03.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    let event1 = test_event("agent_action_received", "AGENT-01").with_operation_id("op-retry-abc");
    store
        .append_with_outbox(&event1, "sentinel/events/AGENT-01")
        .unwrap();

    // Retry: gleiche operation_id, anderer event_id
    let event2 = test_event("agent_action_received", "AGENT-01").with_operation_id("op-retry-abc");
    store
        .append_with_outbox(&event2, "sentinel/events/AGENT-01")
        .unwrap();

    // Nur 1 Event, nicht 2
    assert_eq!(store.event_count().unwrap(), 1);
    let outbox = store.poll_outbox(10).unwrap();
    assert_eq!(outbox.len(), 1, "Duplicate should not create outbox entry");
}

/// AC-4: projection_offsets steigt monoton.
#[test]
fn ac_08_04_offsets_monotonic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ac04.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    // Erster Offset
    store.update_offset("ac-test", 5).unwrap();
    assert_eq!(store.get_offset("ac-test").unwrap(), Some(5));

    // Erhoehung: ok
    store.update_offset("ac-test", 10).unwrap();
    assert_eq!(store.get_offset("ac-test").unwrap(), Some(10));

    // Verringerung: muss fehlschlagen
    let result = store.update_offset("ac-test", 3);
    assert!(result.is_err(), "decreasing offset must fail");

    // Gleicher Wert: muss fehlschlagen
    let result = store.update_offset("ac-test", 10);
    assert!(result.is_err(), "same offset must fail");

    // Offset ist unveraendert
    assert_eq!(store.get_offset("ac-test").unwrap(), Some(10));
}

/// AC-5: Rebuild aus events ist reproduzierbar.
#[test]
fn ac_08_05_rebuild_reproducible() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ac05.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    // 10 Events mit deterministischen Daten einfuegen
    for i in 0..10u64 {
        let event = DomainEvent::new(
            "transit_started",
            &format!("AGENT-{:02}", (i % 5) + 1),
            &format!(r#"{{"step":{i}}}"#),
            "corr-rebuild",
            i * 100,
        );
        store.append_event(&event).unwrap();
    }

    assert_eq!(store.event_count().unwrap(), 10);

    // Erstes Lesen
    let read1 = store.get_all_events().unwrap();
    assert_eq!(read1.len(), 10);

    // Zweites Lesen (Reproduzierbarkeit)
    let read2 = store.get_all_events().unwrap();
    assert_eq!(read2.len(), 10);

    // Reihenfolge und Daten identisch
    for (a, b) in read1.iter().zip(read2.iter()) {
        assert_eq!(a.event_id, b.event_id);
        assert_eq!(a.event_type, b.event_type);
        assert_eq!(a.aggregate_id, b.aggregate_id);
        assert_eq!(a.payload, b.payload);
        assert_eq!(a.tick, b.tick);
        assert_eq!(a.correlation_id, b.correlation_id);
        assert_eq!(a.operation_id, b.operation_id);
        assert_eq!(a.compensation_type, b.compensation_type);
    }

    // Reihenfolge = aufsteigend nach Tick (= Insertion Order)
    for i in 1..read1.len() {
        assert!(
            read1[i].tick >= read1[i - 1].tick,
            "Events should be in insertion order"
        );
    }
}

/// Scope: snapshots Tabelle mit save + get Roundtrip.
#[test]
fn ac_08_06_snapshots_table() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ac06.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    // Kein Snapshot vorhanden
    assert!(store.get_latest_snapshot("AGENT-01").unwrap().is_none());

    // Events einfuegen
    for i in 0..3 {
        let event = test_event("bio_action_performed", "AGENT-01")
            .with_operation_id(&format!("op-snap-{i}"));
        store.append_event(&event).unwrap();
    }

    // Snapshot speichern
    store
        .save_snapshot("AGENT-01", "bio_state", r#"{"hunger":42}"#, 3)
        .unwrap();

    let snap = store.get_latest_snapshot("AGENT-01").unwrap().unwrap();
    assert_eq!(snap.aggregate_id, "AGENT-01");
    assert_eq!(snap.snapshot_type, "bio_state");
    assert_eq!(snap.payload, r#"{"hunger":42}"#);
    assert_eq!(snap.last_event_id, 3);
    assert_eq!(snap.version, 1);

    // Zweiter Snapshot = hoehere Version
    store
        .save_snapshot("AGENT-01", "bio_state", r#"{"hunger":80}"#, 6)
        .unwrap();
    let snap2 = store.get_latest_snapshot("AGENT-01").unwrap().unwrap();
    assert_eq!(snap2.version, 2);
    assert_eq!(snap2.last_event_id, 6);
}

/// Scope: compensation_type Feld persistieren und lesen.
#[test]
fn ac_08_07_compensation_type() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ac07.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    // Default compensation_type
    let event1 = test_event("transit_started", "AGENT-01");
    store.append_event(&event1).unwrap();

    // Expliziter compensation_type
    let event2 = test_event("transit_started", "AGENT-02").with_compensation_type("rollback");
    store.append_event(&event2).unwrap();

    let events = store.get_events_since(0, 10).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].compensation_type, "none");
    assert_eq!(events[1].compensation_type, "rollback");
}

// ──────────────────────────────────────────────
// Issue #57: OutboxPublisher Acceptance Tests
// ──────────────────────────────────────────────

type PublishedPayloads = Vec<(String, Vec<u8>)>;

/// Mock-Transport fuer Acceptance Tests.
struct AcceptanceMockTransport {
    published: std::sync::Arc<std::sync::Mutex<PublishedPayloads>>,
}

impl AcceptanceMockTransport {
    fn new() -> Self {
        Self {
            published: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

impl sentinel_limbo::OutboxTransport for AcceptanceMockTransport {
    async fn publish(&self, topic: &str, payload: &[u8]) -> anyhow::Result<()> {
        self.published
            .lock()
            .unwrap()
            .push((topic.to_string(), payload.to_vec()));
        Ok(())
    }
}

/// AC-6 (Issue #57): E2E Flow — Event append → Outbox poll → Publish → Mark published → Offset update.
#[tokio::test]
async fn ac_57_06_e2e_outbox_publish_flow() {
    use sentinel_limbo::{OutboxPublisher, OutboxPublisherConfig};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ac57_06.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();
    let transport = AcceptanceMockTransport::new();
    let published = transport.published.clone();

    // 1. Agent-Action als Event mit Outbox persistieren
    let event = test_event("agent_action_received", "AGENT-05");
    let row_id = store
        .append_with_outbox(&event, "sentinel/agent/AGENT-05/action")
        .unwrap();
    assert!(row_id > 0, "event should be persisted");

    // 2. OutboxPublisher verarbeitet pending entries
    let publisher =
        OutboxPublisher::new(store.clone(), transport, OutboxPublisherConfig::default());
    let stats = publisher.process_batch().await;
    assert_eq!(stats.published, 1, "one entry should be published");
    assert_eq!(stats.failed, 0, "no failures expected");

    // 3. Transport hat das Event empfangen
    let msgs = published.lock().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].0, "sentinel/agent/AGENT-05/action");

    // 4. Outbox ist leer (alle published)
    let pending = store.poll_outbox(10).unwrap();
    assert!(pending.is_empty(), "outbox should be drained");

    // 5. Projection-Offset aktualisieren (monoton)
    store.update_offset("agent_projection", row_id).unwrap();
    let offset = store.get_offset("agent_projection").unwrap();
    assert_eq!(offset, Some(row_id), "offset should match row_id");

    // 6. Monotonie-Verletzung wird abgefangen
    let result = store.update_offset("agent_projection", row_id - 1);
    assert!(result.is_err(), "backward offset should fail");
}
