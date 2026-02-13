//! Append-only Event Store mit Outbox-Pattern fuer Zenoh-Publish.
//!
//! Drei Tabellen:
//! - `events`: Append-only Event Log (KEIN UPDATE/DELETE - Application-Layer Enforcement)
//! - `outbox`: Pending Zenoh-Publishes nach Commit
//! - `projection_offsets`: CQRS Projection Bookmark
//!
//! Append-Only wird im Code erzwungen (kein UPDATE/DELETE auf events).
//! rusqlite unterstuetzt keine INSTEAD OF Trigger auf normalen Tabellen.

use rusqlite::{params, Connection};
use sentinel_common::DomainEvent;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, instrument};

// ──────────────────────────────────────────────
// SQL Schema
// ──────────────────────────────────────────────

const CREATE_EVENTS: &str = "
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    event_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    correlation_id TEXT NOT NULL,
    causation_id TEXT,
    operation_id TEXT NOT NULL,
    tick INTEGER NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    schema_version INTEGER NOT NULL DEFAULT 1
)";

const CREATE_IDX_EVENTS_AGGREGATE: &str =
    "CREATE INDEX IF NOT EXISTS idx_events_aggregate ON events(aggregate_id, id)";
const CREATE_IDX_EVENTS_TYPE: &str =
    "CREATE INDEX IF NOT EXISTS idx_events_type ON events(event_type, id)";
const CREATE_IDX_EVENTS_CORRELATION: &str =
    "CREATE INDEX IF NOT EXISTS idx_events_correlation ON events(correlation_id)";
const CREATE_IDX_EVENTS_OPERATION: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_events_operation ON events(operation_id)";

const CREATE_OUTBOX: &str = "
CREATE TABLE IF NOT EXISTS outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL REFERENCES events(event_id),
    topic TEXT NOT NULL,
    payload TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at INTEGER NOT NULL,
    published_at INTEGER
)";

const CREATE_IDX_OUTBOX_PENDING: &str =
    "CREATE INDEX IF NOT EXISTS idx_outbox_pending ON outbox(status) WHERE status = 'pending'";

const CREATE_PROJECTION_OFFSETS: &str = "
CREATE TABLE IF NOT EXISTS projection_offsets (
    projection_name TEXT PRIMARY KEY,
    last_event_id INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
)";

// ──────────────────────────────────────────────
// OutboxEntry
// ──────────────────────────────────────────────

/// Zeile aus der Outbox-Tabelle fuer den Publisher.
#[derive(Debug, Clone)]
pub struct OutboxEntry {
    pub id: i64,
    pub event_id: String,
    pub topic: String,
    pub payload: String,
    pub status: String,
    pub created_at: u64,
}

// ──────────────────────────────────────────────
// EventStore
// ──────────────────────────────────────────────

/// Sync Event Store mit append-only Semantik.
///
/// Thread-safe via `Arc<Mutex<Connection>>`. Fuer async-Kontexte:
/// in tokio::task::spawn_blocking wrappen.
pub struct EventStore {
    conn: Arc<Mutex<Connection>>,
}

impl EventStore {
    /// Oeffnet oder erstellt den Event Store.
    #[instrument(level = "debug", fields(path = %path))]
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;

        // Performance Pragmas
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA mmap_size = 268435456;
             PRAGMA page_size = 8192;",
        )?;

        // Schema erstellen
        conn.execute_batch(CREATE_EVENTS)?;
        conn.execute(CREATE_IDX_EVENTS_AGGREGATE, [])?;
        conn.execute(CREATE_IDX_EVENTS_TYPE, [])?;
        conn.execute(CREATE_IDX_EVENTS_CORRELATION, [])?;
        conn.execute(CREATE_IDX_EVENTS_OPERATION, [])?;
        conn.execute_batch(CREATE_OUTBOX)?;
        conn.execute(CREATE_IDX_OUTBOX_PENDING, [])?;
        conn.execute_batch(CREATE_PROJECTION_OFFSETS)?;

        info!("EventStore opened at {path}");
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Append-only: Fuegt ein Event ein. Gibt die interne Row-ID zurueck.
    pub fn append_event(&self, event: &DomainEvent) -> anyhow::Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        conn.execute(
            "INSERT OR IGNORE INTO events (event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                event.event_id,
                event.event_type,
                event.aggregate_id,
                event.payload,
                event.correlation_id,
                event.causation_id,
                event.operation_id,
                event.tick as i64,
                event.timestamp_ms as i64,
                event.schema_version,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Atomar: Event + Outbox-Eintrag in einer Transaktion (AC1, AC3).
    ///
    /// Nutzt operation_id als Idempotenz-Key (UNIQUE INDEX).
    /// Bei Duplikat (gleiche operation_id) wird kein neuer Eintrag erstellt.
    pub fn append_with_outbox(&self, event: &DomainEvent, topic: &str) -> anyhow::Result<i64> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let tx = conn.transaction()?;

        // INSERT OR IGNORE: Idempotenz via operation_id UNIQUE INDEX
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO events (event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                event.event_id,
                event.event_type,
                event.aggregate_id,
                event.payload,
                event.correlation_id,
                event.causation_id,
                event.operation_id,
                event.tick as i64,
                event.timestamp_ms as i64,
                event.schema_version,
            ],
        )?;

        // Nur Outbox-Eintrag wenn Event tatsaechlich eingefuegt wurde
        if inserted > 0 {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;

            tx.execute(
                "INSERT INTO outbox (event_id, topic, payload, status, created_at) VALUES (?1, ?2, ?3, 'pending', ?4)",
                params![event.event_id, topic, event.payload, now_ms],
            )?;
        }

        let row_id = tx.last_insert_rowid();
        tx.commit()?;

        debug!(event_id = %event.event_id, event_type = %event.event_type, "event appended");
        Ok(row_id)
    }

    /// Liest Events nach einer bestimmten internen ID (Cursor-basiert).
    pub fn get_events_since(
        &self,
        after_id: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<DomainEvent>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version FROM events WHERE id > ?1 ORDER BY id ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![after_id, limit as i64], |row| {
            Ok(DomainEvent {
                event_id: row.get(0)?,
                event_type: row.get(1)?,
                aggregate_id: row.get(2)?,
                payload: row.get(3)?,
                correlation_id: row.get(4)?,
                causation_id: row.get(5)?,
                operation_id: row.get(6)?,
                tick: row.get::<_, i64>(7)? as u64,
                timestamp_ms: row.get::<_, i64>(8)? as u64,
                schema_version: row.get::<_, i32>(9)? as u32,
            })
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Liest Events fuer ein bestimmtes Aggregate (z.B. Agent oder Raum).
    pub fn get_events_by_aggregate(
        &self,
        aggregate_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<DomainEvent>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version FROM events WHERE aggregate_id = ?1 ORDER BY id ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![aggregate_id, limit as i64], |row| {
            Ok(DomainEvent {
                event_id: row.get(0)?,
                event_type: row.get(1)?,
                aggregate_id: row.get(2)?,
                payload: row.get(3)?,
                correlation_id: row.get(4)?,
                causation_id: row.get(5)?,
                operation_id: row.get(6)?,
                tick: row.get::<_, i64>(7)? as u64,
                timestamp_ms: row.get::<_, i64>(8)? as u64,
                schema_version: row.get::<_, i32>(9)? as u32,
            })
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    // ── Outbox ──────────────────────────────────

    /// Pollt pending Outbox-Eintraege fuer den Zenoh-Publisher.
    pub fn poll_outbox(&self, limit: usize) -> anyhow::Result<Vec<OutboxEntry>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT id, event_id, topic, payload, status, created_at FROM outbox WHERE status = 'pending' ORDER BY id ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(OutboxEntry {
                id: row.get(0)?,
                event_id: row.get(1)?,
                topic: row.get(2)?,
                payload: row.get(3)?,
                status: row.get(4)?,
                created_at: row.get::<_, i64>(5)? as u64,
            })
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Markiert einen Outbox-Eintrag als publiziert.
    pub fn mark_published(&self, event_id: &str) -> anyhow::Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        conn.execute(
            "UPDATE outbox SET status = 'published', published_at = ?1 WHERE event_id = ?2",
            params![now_ms, event_id],
        )?;
        Ok(())
    }

    // ── Projection Offsets ──────────────────────

    /// Liest den aktuellen Offset einer Projection.
    pub fn get_offset(&self, name: &str) -> anyhow::Result<Option<i64>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let result = conn.query_row(
            "SELECT last_event_id FROM projection_offsets WHERE projection_name = ?1",
            params![name],
            |row| row.get(0),
        );
        match result {
            Ok(offset) => Ok(Some(offset)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Setzt den Offset einer Projection (upsert).
    pub fn update_offset(&self, name: &str, offset: i64) -> anyhow::Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        conn.execute(
            "INSERT INTO projection_offsets (projection_name, last_event_id, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(projection_name) DO UPDATE SET last_event_id = ?2, updated_at = ?3",
            params![name, offset, now_ms],
        )?;
        Ok(())
    }

    /// Zugriff auf Connection fuer Tests.
    #[cfg(test)]
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }
}

// ──────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::DomainEvent;

    fn test_event(event_type: &str, aggregate_id: &str) -> DomainEvent {
        DomainEvent::new(event_type, aggregate_id, r#"{"test":true}"#, "corr-1", 42)
    }

    #[test]
    fn test_open_creates_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-events.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let conn = store.conn();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = conn
            .query_row("SELECT count(*) FROM outbox", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = conn
            .query_row("SELECT count(*) FROM projection_offsets", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    /// AC1: Event+Outbox atomar in einer Transaktion
    #[test]
    fn test_append_with_outbox_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-atomic.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let event = test_event("agent_action_received", "AGENT-01");
        let row_id = store
            .append_with_outbox(&event, "sentinel/events/AGENT-01")
            .unwrap();
        assert!(row_id > 0);

        // Event und Outbox muessen beide existieren
        let conn = store.conn();
        let event_count: i64 = conn
            .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(event_count, 1);

        let outbox_count: i64 = conn
            .query_row("SELECT count(*) FROM outbox", [], |row| row.get(0))
            .unwrap();
        assert_eq!(outbox_count, 1);

        // Outbox referenziert das Event
        let outbox_event_id: String = conn
            .query_row("SELECT event_id FROM outbox LIMIT 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(outbox_event_id, event.event_id);
    }

    /// AC2: Append-only - UPDATE/DELETE auf events sollte nicht passieren
    /// (Application-Layer Enforcement - wir testen dass unsere API es nicht tut)
    #[test]
    fn test_append_only_no_update_api() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-append-only.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let event = test_event("transit_started", "AGENT-01");
        store.append_event(&event).unwrap();

        // EventStore API bietet kein update/delete fuer events
        // Verifiziere: Event ist unveraendert lesbar
        let events = store.get_events_since(0, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_id, event.event_id);
        assert_eq!(events[0].event_type, "transit_started");
    }

    /// AC3: operation_id Idempotenz - gleiche operation_id = kein Duplikat
    #[test]
    fn test_operation_id_idempotency() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-idempotency.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let mut event = test_event("agent_action_received", "AGENT-01");
        event.operation_id = "op-fixed-123".to_string();

        // Erstes Insert
        store
            .append_with_outbox(&event, "sentinel/events/AGENT-01")
            .unwrap();

        // Zweites Insert mit gleicher operation_id (anderer event_id)
        let mut event2 = test_event("agent_action_received", "AGENT-01");
        event2.operation_id = "op-fixed-123".to_string();
        store
            .append_with_outbox(&event2, "sentinel/events/AGENT-01")
            .unwrap();

        // Nur 1 Event, nicht 2
        let events = store.get_events_since(0, 10).unwrap();
        assert_eq!(events.len(), 1, "Duplicate operation_id should be ignored");

        // Nur 1 Outbox-Eintrag
        let outbox = store.poll_outbox(10).unwrap();
        assert_eq!(outbox.len(), 1, "Duplicate should not create outbox entry");
    }

    /// AC4: causation_id Kette - Event B.causation_id == Event A.event_id
    #[test]
    fn test_causation_chain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-causation.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let event_a = test_event("agent_action_received", "AGENT-01");
        store.append_event(&event_a).unwrap();

        let event_b = test_event("transit_started", "AGENT-01").with_causation(&event_a.event_id);
        store.append_event(&event_b).unwrap();

        // Verifiziere Kette
        let events = store.get_events_since(0, 10).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events[0].causation_id.is_none());
        assert_eq!(
            events[1].causation_id.as_deref(),
            Some(events[0].event_id.as_str())
        );
    }

    /// AC5: projection_offsets monoton steigend
    #[test]
    fn test_projection_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-offsets.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        // Initial: kein Offset
        assert_eq!(store.get_offset("dashboard").unwrap(), None);

        // Offset setzen
        store.update_offset("dashboard", 5).unwrap();
        assert_eq!(store.get_offset("dashboard").unwrap(), Some(5));

        // Offset erhoehen (monoton)
        store.update_offset("dashboard", 10).unwrap();
        assert_eq!(store.get_offset("dashboard").unwrap(), Some(10));
    }

    #[test]
    fn test_outbox_poll_and_mark_published() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-outbox.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let event = test_event("chaos_triggered", "buero-dev-1");
        store
            .append_with_outbox(&event, "sentinel/chaos/buero-dev-1")
            .unwrap();

        // Poll: 1 pending
        let pending = store.poll_outbox(10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event_id, event.event_id);
        assert_eq!(pending[0].topic, "sentinel/chaos/buero-dev-1");

        // Markiere als publiziert
        store.mark_published(&event.event_id).unwrap();

        // Poll: 0 pending
        let pending = store.poll_outbox(10).unwrap();
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn test_get_events_by_aggregate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-aggregate.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        store
            .append_event(&test_event("transit_started", "AGENT-01"))
            .unwrap();
        store
            .append_event(&test_event("transit_completed", "AGENT-01"))
            .unwrap();
        store
            .append_event(&test_event("transit_started", "AGENT-02"))
            .unwrap();

        let agent1_events = store.get_events_by_aggregate("AGENT-01", 10).unwrap();
        assert_eq!(agent1_events.len(), 2);

        let agent2_events = store.get_events_by_aggregate("AGENT-02", 10).unwrap();
        assert_eq!(agent2_events.len(), 1);
    }

    #[test]
    fn test_wal_mode_active() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-wal.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let conn = store.conn();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }
}
