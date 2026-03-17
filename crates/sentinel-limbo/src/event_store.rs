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
use std::fmt;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, instrument};

/// Histogram-Buckets fuer EventStore Latenzen (Mikrosekunden).
#[cfg(feature = "telemetry")]
const LATENCY_BUCKETS: &[f64] = &[50.0, 100.0, 500.0, 1000.0, 5000.0, 10000.0, 50000.0];

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
    schema_version INTEGER NOT NULL DEFAULT 1,
    compensation_type TEXT NOT NULL DEFAULT 'none'
)";

const CREATE_IDX_EVENTS_AGGREGATE: &str =
    "CREATE INDEX IF NOT EXISTS idx_events_aggregate ON events(aggregate_id, id)";
const CREATE_IDX_EVENTS_TYPE: &str =
    "CREATE INDEX IF NOT EXISTS idx_events_type ON events(event_type, id)";
const CREATE_IDX_EVENTS_CORRELATION: &str =
    "CREATE INDEX IF NOT EXISTS idx_events_correlation ON events(correlation_id)";
const CREATE_IDX_EVENTS_CAUSATION: &str =
    "CREATE INDEX IF NOT EXISTS idx_events_causation ON events(causation_id)";
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

const CREATE_SNAPSHOTS: &str = "
CREATE TABLE IF NOT EXISTS snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    aggregate_id TEXT NOT NULL,
    snapshot_type TEXT NOT NULL,
    payload TEXT NOT NULL,
    last_event_id INTEGER NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
)";

const CREATE_IDX_SNAPSHOTS_AGGREGATE: &str =
    "CREATE INDEX IF NOT EXISTS idx_snapshots_aggregate ON snapshots(aggregate_id, version DESC)";

const CREATE_WORLD_SNAPSHOTS: &str = "
CREATE TABLE IF NOT EXISTS world_snapshots (
    id TEXT PRIMARY KEY,
    tier TEXT NOT NULL,
    tick INTEGER NOT NULL,
    sim_hour REAL NOT NULL,
    last_event_id INTEGER NOT NULL,
    payload_size INTEGER NOT NULL DEFAULT 0,
    payload BLOB NOT NULL,
    created_at INTEGER NOT NULL
)";

const CREATE_IDX_WORLD_SNAPSHOTS_TIER: &str =
    "CREATE INDEX IF NOT EXISTS idx_world_snapshots_tier ON world_snapshots(tier, tick DESC)";

const CREATE_PROJECTION_OFFSETS: &str = "
CREATE TABLE IF NOT EXISTS projection_offsets (
    projection_name TEXT PRIMARY KEY,
    last_event_id INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
)";

// ──────────────────────────────────────────────
// OutboxEntry
// ──────────────────────────────────────────────

/// Fehler bei Monotonie-Verletzung von projection_offsets.
#[derive(Debug)]
pub struct MonotonicityError {
    pub projection: String,
    pub current: i64,
    pub attempted: i64,
}

impl fmt::Display for MonotonicityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "monotonicity violation for projection '{}': current={}, attempted={}",
            self.projection, self.current, self.attempted
        )
    }
}

impl std::error::Error for MonotonicityError {}

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

/// Zeile aus der Snapshots-Tabelle.
#[derive(Debug, Clone)]
pub struct SnapshotRow {
    pub id: i64,
    pub aggregate_id: String,
    pub snapshot_type: String,
    pub payload: String,
    pub last_event_id: i64,
    pub version: i32,
    pub created_at: u64,
}

// ──────────────────────────────────────────────
// EventStore
// ──────────────────────────────────────────────

/// Sync Event Store mit append-only Semantik.
///
/// Thread-safe via `Arc<Mutex<Connection>>`. Fuer async-Kontexte:
/// in tokio::task::spawn_blocking wrappen.
#[derive(Clone)]
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
        conn.execute(CREATE_IDX_EVENTS_CAUSATION, [])?;
        conn.execute(CREATE_IDX_EVENTS_OPERATION, [])?;
        conn.execute_batch(CREATE_OUTBOX)?;
        conn.execute(CREATE_IDX_OUTBOX_PENDING, [])?;
        conn.execute_batch(CREATE_SNAPSHOTS)?;
        conn.execute(CREATE_IDX_SNAPSHOTS_AGGREGATE, [])?;
        conn.execute_batch(CREATE_WORLD_SNAPSHOTS)?;
        conn.execute(CREATE_IDX_WORLD_SNAPSHOTS_TIER, [])?;
        conn.execute_batch(CREATE_PROJECTION_OFFSETS)?;

        info!("EventStore opened at {path}");
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Append-only: Fuegt ein Event ein. Gibt die interne Row-ID zurueck.
    pub fn append_event(&self, event: &DomainEvent) -> anyhow::Result<i64> {
        let _telemetry_start = std::time::Instant::now();
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        conn.execute(
            "INSERT OR IGNORE INTO events (event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                event.compensation_type,
            ],
        )?;
        let row_id = conn.last_insert_rowid();
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.limbo.event.append.count").increment();
            reg.histogram("sentinel.limbo.event.append.duration_us", LATENCY_BUCKETS)
                .observe(_telemetry_start.elapsed().as_micros() as f64);
        }
        Ok(row_id)
    }

    /// Atomar: Event + Outbox-Eintrag in einer Transaktion (AC1, AC3).
    ///
    /// Nutzt operation_id als Idempotenz-Key (UNIQUE INDEX).
    /// Bei Duplikat (gleiche operation_id) wird kein neuer Eintrag erstellt.
    pub fn append_with_outbox(&self, event: &DomainEvent, topic: &str) -> anyhow::Result<i64> {
        let _telemetry_start = std::time::Instant::now();
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let tx = conn.transaction()?;

        // INSERT OR IGNORE: Idempotenz via operation_id UNIQUE INDEX
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO events (event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                event.compensation_type,
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
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.limbo.event.append_outbox.count")
                .increment();
            reg.histogram(
                "sentinel.limbo.event.append_outbox.duration_us",
                LATENCY_BUCKETS,
            )
            .observe(_telemetry_start.elapsed().as_micros() as f64);
        }
        Ok(row_id)
    }

    /// Liest Events nach einer bestimmten internen ID (Cursor-basiert).
    pub fn get_events_since(
        &self,
        after_id: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<DomainEvent>> {
        let _telemetry_start = std::time::Instant::now();
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type FROM events WHERE id > ?1 ORDER BY id ASC LIMIT ?2",
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
                compensation_type: row.get(10)?,
            })
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.limbo.event.query.count").increment();
            reg.histogram("sentinel.limbo.event.query.duration_us", LATENCY_BUCKETS)
                .observe(_telemetry_start.elapsed().as_micros() as f64);
        }
        Ok(results)
    }

    /// Liest Events mit interner Row-ID (fuer Projection-Cursor-Tracking).
    ///
    /// Wie `get_events_since`, gibt aber zusaetzlich die SQLite `id`-Spalte
    /// zurueck, die Projection-Worker fuer `update_offset()` benoetigen.
    pub fn get_events_since_with_id(
        &self,
        after_id: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<(i64, DomainEvent)>> {
        let _telemetry_start = std::time::Instant::now();
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT id, event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type FROM events WHERE id > ?1 ORDER BY id ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![after_id, limit as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                DomainEvent {
                    event_id: row.get(1)?,
                    event_type: row.get(2)?,
                    aggregate_id: row.get(3)?,
                    payload: row.get(4)?,
                    correlation_id: row.get(5)?,
                    causation_id: row.get(6)?,
                    operation_id: row.get(7)?,
                    tick: row.get::<_, i64>(8)? as u64,
                    timestamp_ms: row.get::<_, i64>(9)? as u64,
                    schema_version: row.get::<_, i32>(10)? as u32,
                    compensation_type: row.get(11)?,
                },
            ))
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.limbo.event.query_with_id.count")
                .increment();
            reg.histogram(
                "sentinel.limbo.event.query_with_id.duration_us",
                LATENCY_BUCKETS,
            )
            .observe(_telemetry_start.elapsed().as_micros() as f64);
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
            "SELECT event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type FROM events WHERE aggregate_id = ?1 ORDER BY id ASC LIMIT ?2",
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
                compensation_type: row.get(10)?,
            })
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Liest Events mit einer bestimmten correlation_id (z.B. run_id fuer Replay).
    pub fn get_events_by_correlation(
        &self,
        correlation_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<DomainEvent>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type FROM events WHERE correlation_id = ?1 ORDER BY id ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![correlation_id, limit as i64], |row| {
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
                compensation_type: row.get(10)?,
            })
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    // ── Snapshots ──────────────────────────────────

    /// Speichert einen Snapshot fuer ein Aggregate. Version wird automatisch inkrementiert.
    pub fn save_snapshot(
        &self,
        aggregate_id: &str,
        snapshot_type: &str,
        payload: &str,
        last_event_id: i64,
    ) -> anyhow::Result<i64> {
        let _telemetry_start = std::time::Instant::now();
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;

        // Aktuelle Version ermitteln
        let current_version: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM snapshots WHERE aggregate_id = ?1",
                params![aggregate_id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        conn.execute(
            "INSERT INTO snapshots (aggregate_id, snapshot_type, payload, last_event_id, version, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                aggregate_id,
                snapshot_type,
                payload,
                last_event_id,
                current_version + 1,
                now_ms,
            ],
        )?;
        let row_id = conn.last_insert_rowid();
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.limbo.snapshot.save.count")
                .increment();
            reg.histogram("sentinel.limbo.snapshot.save.duration_us", LATENCY_BUCKETS)
                .observe(_telemetry_start.elapsed().as_micros() as f64);
        }
        Ok(row_id)
    }

    /// Liest den neuesten Snapshot fuer ein Aggregate.
    pub fn get_latest_snapshot(&self, aggregate_id: &str) -> anyhow::Result<Option<SnapshotRow>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let result = conn.query_row(
            "SELECT id, aggregate_id, snapshot_type, payload, last_event_id, version, created_at FROM snapshots WHERE aggregate_id = ?1 ORDER BY version DESC LIMIT 1",
            params![aggregate_id],
            |row| {
                Ok(SnapshotRow {
                    id: row.get(0)?,
                    aggregate_id: row.get(1)?,
                    snapshot_type: row.get(2)?,
                    payload: row.get(3)?,
                    last_event_id: row.get(4)?,
                    version: row.get(5)?,
                    created_at: row.get::<_, i64>(6)? as u64,
                })
            },
        );
        match result {
            Ok(snap) => Ok(Some(snap)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // ── Outbox ──────────────────────────────────

    /// Pollt pending Outbox-Eintraege fuer den Zenoh-Publisher.
    pub fn poll_outbox(&self, limit: usize) -> anyhow::Result<Vec<OutboxEntry>> {
        let _telemetry_start = std::time::Instant::now();
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
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.limbo.outbox.poll.count").increment();
            reg.histogram("sentinel.limbo.outbox.poll.duration_us", LATENCY_BUCKETS)
                .observe(_telemetry_start.elapsed().as_micros() as f64);
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

    /// Gibt alle Projection-Offsets zurueck (fuer World Snapshot).
    pub fn get_all_offsets(&self) -> anyhow::Result<Vec<(String, i64)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT projection_name, last_event_id FROM projection_offsets ORDER BY projection_name",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Gibt die hoechste Event-ID zurueck (fuer Snapshot cursor).
    pub fn get_latest_event_id(&self) -> anyhow::Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let result = conn.query_row("SELECT MAX(id) FROM events", [], |row| row.get(0));
        match result {
            Ok(id) => Ok(id),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    /// Loescht alle Events mit id < cutoff_event_id.
    /// Safety: Prueft ob alle Projection-Offsets >= cutoff_event_id sind.
    pub fn prune_events_before(&self, cutoff_event_id: i64) -> anyhow::Result<u64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;

        // Safety Guard: Pruefe ob alle Projections ueber dem Cutoff sind
        let min_offset: Option<i64> = conn
            .query_row(
                "SELECT MIN(last_event_id) FROM projection_offsets",
                [],
                |row| row.get(0),
            )
            .ok();
        if let Some(min) = min_offset {
            if min < cutoff_event_id {
                return Err(anyhow::anyhow!(
                    "Prune blockiert: Projection-Offset ({min}) < Cutoff ({cutoff_event_id})"
                ));
            }
        }

        // Safety Guard: Pruefe Outbox-Backlog
        let pending_outbox: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM outbox WHERE status = 'pending'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if pending_outbox > 0 {
            return Err(anyhow::anyhow!(
                "Prune blockiert: {pending_outbox} ausstehende Outbox-Eintraege"
            ));
        }

        let deleted = conn.execute("DELETE FROM events WHERE id < ?1", params![cutoff_event_id])?;
        Ok(deleted as u64)
    }

    /// Fuehrt SQLite VACUUM aus um Speicherplatz freizugeben.
    pub fn vacuum(&self) -> anyhow::Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        conn.execute_batch("VACUUM")?;
        Ok(())
    }

    /// Setzt den Offset einer Projection (upsert, monoton steigend).
    ///
    /// Verhalten:
    /// - `offset > current` → Normal-Update (Fortschritt)
    /// - `offset == current` → No-op (idempotent, kein Fehler)
    /// - `offset < current` → `MonotonicityError` (Rueckwaerts-Drift)
    pub fn update_offset(&self, name: &str, offset: i64) -> anyhow::Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;

        // Aktuellen Offset pruefen
        let current: Option<i64> = conn
            .query_row(
                "SELECT last_event_id FROM projection_offsets WHERE projection_name = ?1",
                params![name],
                |row| row.get(0),
            )
            .ok();

        if let Some(current_val) = current {
            if offset == current_val {
                // Idempotent: gleicher Offset = kein Update noetig
                return Ok(());
            }
            if offset < current_val {
                return Err(MonotonicityError {
                    projection: name.to_string(),
                    current: current_val,
                    attempted: offset,
                }
                .into());
            }
        }

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

    /// Setzt den Offset einer Projection zurueck (fuer Rebuild-Modus).
    ///
    /// Loescht den Eintrag aus `projection_offsets`, sodass der naechste
    /// `get_offset()` Call `None` zurueckgibt. Umgeht die Monotonitaetspruefung
    /// von `update_offset()`.
    /// Erzwingt einen Offset-Wert (umgeht Monotonie-Pruefung, fuer Restore).
    pub fn force_reset_offset(&self, name: &str, offset: i64) -> anyhow::Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        conn.execute(
            "INSERT OR REPLACE INTO projection_offsets (projection_name, last_event_id, updated_at) VALUES (?1, ?2, ?3)",
            params![name, offset, now_ms],
        )?;
        Ok(())
    }

    pub fn reset_offset(&self, name: &str) -> anyhow::Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        conn.execute(
            "DELETE FROM projection_offsets WHERE projection_name = ?1",
            params![name],
        )?;
        Ok(())
    }

    // ── Rebuild / Recovery ──────────────────────

    /// Liefert die maximale interne Row-ID (SQLite rowid) im Event Store.
    /// Fuer Cursor-Initialisierung bei neuen Consumern (z.B. EpisodeProducer).
    pub fn max_event_rowid(&self) -> anyhow::Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let max: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |row| {
            row.get(0)
        })?;
        Ok(max)
    }

    /// Zaehlt alle Events im Store.
    pub fn event_count(&self) -> anyhow::Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let count: i64 = conn.query_row("SELECT count(*) FROM events", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Liest ALLE Events geordnet nach interner ID (fuer Rebuild/Recovery).
    ///
    /// Achtung: Nur fuer Rebuild/Recovery verwenden, nicht fuer normalen Betrieb.
    pub fn get_all_events(&self) -> anyhow::Result<Vec<DomainEvent>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type FROM events ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
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
                compensation_type: row.get(10)?,
            })
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Zugriff auf Connection fuer Tests.
    #[cfg(test)]
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }

    // ── World Snapshots (Time Machine) ──

    /// Speichert einen World Snapshot als bincode BLOB.
    pub fn save_world_snapshot(
        &self,
        id: &str,
        tier: &str,
        tick: u64,
        sim_hour: f32,
        last_event_id: i64,
        payload: &[u8],
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        conn.execute(
            "INSERT INTO world_snapshots (id, tier, tick, sim_hour, last_event_id, payload_size, payload, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                tier,
                tick as i64,
                sim_hour as f64,
                last_event_id,
                payload.len() as i64,
                payload,
                now_ms,
            ],
        )?;
        Ok(())
    }

    /// Laedt einen World Snapshot (bincode BLOB) anhand seiner ID.
    pub fn load_world_snapshot(&self, id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT payload FROM world_snapshots WHERE id = ?1")?;
        let result = stmt.query_row(params![id], |row| row.get::<_, Vec<u8>>(0));
        match result {
            Ok(payload) => Ok(Some(payload)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Listet alle World Snapshots (Metadaten ohne Payload), sortiert nach Tick DESC.
    pub fn list_world_snapshots(&self) -> anyhow::Result<Vec<sentinel_common::SnapshotMeta>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, tier, tick, sim_hour, last_event_id, payload_size, created_at \
             FROM world_snapshots ORDER BY tick DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let tier_str: String = row.get(1)?;
            let tier = match tier_str.as_str() {
                "hourly" => sentinel_common::SnapshotTier::Hourly,
                "daily" => sentinel_common::SnapshotTier::Daily,
                "weekly" => sentinel_common::SnapshotTier::Weekly,
                "monthly" => sentinel_common::SnapshotTier::Monthly,
                _ => sentinel_common::SnapshotTier::Hourly,
            };
            Ok(sentinel_common::SnapshotMeta {
                id: row.get(0)?,
                tier,
                tick: row.get::<_, i64>(2)? as u64,
                sim_hour: row.get::<_, f64>(3)? as f32,
                last_event_id: row.get(4)?,
                payload_size_bytes: row.get::<_, i64>(5)? as u64,
                created_at_ms: row.get(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Loescht einen World Snapshot anhand seiner ID.
    pub fn delete_world_snapshot(&self, id: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute("DELETE FROM world_snapshots WHERE id = ?1", params![id])?;
        Ok(deleted > 0)
    }

    /// Aktualisiert den Tier eines World Snapshots (fuer Promotion).
    pub fn promote_world_snapshot(&self, id: &str, new_tier: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE world_snapshots SET tier = ?1 WHERE id = ?2",
            params![new_tier, id],
        )?;
        Ok(updated > 0)
    }
}

// ──────────────────────────────────────────────
// OutboxTransport Trait
// ──────────────────────────────────────────────

/// Transport backend for outbox event publishing.
///
/// Implementiert von Zenoh-Adapter (sentinel-runtime) oder Mock (Tests).
/// Generisch gehalten damit sentinel-limbo NICHT von sentinel-zenoh abhaengt.
pub trait OutboxTransport: Send + Sync + 'static {
    /// Publiziert ein Event an den angegebenen Topic.
    fn publish<'a>(
        &'a self,
        topic: &'a str,
        payload: &'a [u8],
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send + 'a;
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
            .query_row("SELECT count(*) FROM snapshots", [], |row| row.get(0))
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

    /// AC4: Monotonie-Enforcement fuer projection_offsets.
    #[test]
    fn test_monotonic_offset_enforcement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-monotonic.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        // Offset setzen
        store.update_offset("test-proj", 10).unwrap();
        assert_eq!(store.get_offset("test-proj").unwrap(), Some(10));

        // Gleicher Wert ist idempotent (no-op, kein Fehler)
        store.update_offset("test-proj", 10).unwrap();
        assert_eq!(store.get_offset("test-proj").unwrap(), Some(10));

        // Kleinerer Wert muss fehlschlagen
        let result = store.update_offset("test-proj", 5);
        assert!(
            result.is_err(),
            "smaller offset should fail monotonicity check"
        );

        // Groesserer Wert muss funktionieren
        store.update_offset("test-proj", 20).unwrap();
        assert_eq!(store.get_offset("test-proj").unwrap(), Some(20));
    }

    /// Snapshot save + get Roundtrip.
    #[test]
    fn test_save_and_get_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-snap.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        // Kein Snapshot vorhanden
        assert!(store.get_latest_snapshot("AGENT-01").unwrap().is_none());

        // Snapshot speichern
        let id = store
            .save_snapshot("AGENT-01", "bio_state", r#"{"hunger":50}"#, 5)
            .unwrap();
        assert!(id > 0);

        // Snapshot lesen
        let snap = store.get_latest_snapshot("AGENT-01").unwrap().unwrap();
        assert_eq!(snap.aggregate_id, "AGENT-01");
        assert_eq!(snap.snapshot_type, "bio_state");
        assert_eq!(snap.payload, r#"{"hunger":50}"#);
        assert_eq!(snap.last_event_id, 5);
        assert_eq!(snap.version, 1);

        // Zweiter Snapshot = Version 2
        store
            .save_snapshot("AGENT-01", "bio_state", r#"{"hunger":80}"#, 10)
            .unwrap();
        let snap2 = store.get_latest_snapshot("AGENT-01").unwrap().unwrap();
        assert_eq!(snap2.version, 2);
        assert_eq!(snap2.last_event_id, 10);
        assert_eq!(snap2.payload, r#"{"hunger":80}"#);
    }

    /// AC5: Rebuild aus Events - Reihenfolge und Daten bleiben erhalten.
    #[test]
    fn test_get_all_events_ordered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-rebuild.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        // 5 Events einfuegen
        for i in 0..5u64 {
            let event = DomainEvent::new(
                "transit_started",
                &format!("AGENT-{:02}", i + 1),
                &format!(r#"{{"step":{i}}}"#),
                "corr-rebuild",
                i * 10,
            );
            store.append_event(&event).unwrap();
        }

        assert_eq!(store.event_count().unwrap(), 5);

        // Alle Events lesen
        let all = store.get_all_events().unwrap();
        assert_eq!(all.len(), 5);

        // Reihenfolge = Insertion Order (aufsteigende Ticks)
        for (idx, event) in all.iter().enumerate() {
            assert_eq!(event.tick, (idx as u64) * 10);
            assert_eq!(event.aggregate_id, format!("AGENT-{:02}", idx + 1));
        }

        // Zweites Lesen = identisch (Reproduzierbarkeit)
        let all2 = store.get_all_events().unwrap();
        assert_eq!(all.len(), all2.len());
        for (a, b) in all.iter().zip(all2.iter()) {
            assert_eq!(a.event_id, b.event_id);
            assert_eq!(a.tick, b.tick);
            assert_eq!(a.payload, b.payload);
        }
    }

    /// compensation_type Persistierung und Roundtrip.
    #[test]
    fn test_compensation_type_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-compensation.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        // Default compensation_type = "none"
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

    /// get_events_since_with_id gibt Row-IDs mit zurueck.
    #[test]
    fn test_get_events_since_with_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-with-id.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        // 5 Events einfuegen
        for i in 0..5u64 {
            let event = DomainEvent::new(
                "transit_started",
                &format!("AGENT-{:02}", i + 1),
                &format!(r#"{{"step":{i}}}"#),
                "corr-with-id",
                i * 10,
            );
            store.append_event(&event).unwrap();
        }

        // Alle Events mit IDs lesen
        let results = store.get_events_since_with_id(0, 100).unwrap();
        assert_eq!(results.len(), 5);

        // Row-IDs muessen aufsteigend und >0 sein
        for (idx, (row_id, event)) in results.iter().enumerate() {
            assert!(*row_id > 0, "row_id must be positive");
            assert_eq!(event.tick, (idx as u64) * 10);
            assert_eq!(event.aggregate_id, format!("AGENT-{:02}", idx + 1));
        }

        // Row-IDs muessen strikt aufsteigend sein
        for window in results.windows(2) {
            assert!(
                window[1].0 > window[0].0,
                "row_ids must be strictly ascending"
            );
        }

        // Cursor: ab letzter ID lesen ergibt leer
        let last_id = results.last().unwrap().0;
        let empty = store.get_events_since_with_id(last_id, 100).unwrap();
        assert!(empty.is_empty());

        // Cursor: ab Mitte lesen ergibt Rest
        let mid_id = results[2].0;
        let rest = store.get_events_since_with_id(mid_id, 100).unwrap();
        assert_eq!(rest.len(), 2);
        assert_eq!(rest[0].1.aggregate_id, "AGENT-04");
        assert_eq!(rest[1].1.aggregate_id, "AGENT-05");
    }

    /// Konsistenz: get_events_since und get_events_since_with_id liefern gleiche Events.
    #[test]
    fn test_with_id_consistent_with_without() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-consistent.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        for i in 0..3u64 {
            let event = test_event("agent_spawned", &format!("AGENT-{:02}", i + 1));
            store.append_event(&event).unwrap();
        }

        let without_id = store.get_events_since(0, 100).unwrap();
        let with_id = store.get_events_since_with_id(0, 100).unwrap();

        assert_eq!(without_id.len(), with_id.len());
        for (a, (_, b)) in without_id.iter().zip(with_id.iter()) {
            assert_eq!(a.event_id, b.event_id);
            assert_eq!(a.event_type, b.event_type);
            assert_eq!(a.aggregate_id, b.aggregate_id);
            assert_eq!(a.payload, b.payload);
            assert_eq!(a.tick, b.tick);
        }
    }

    /// reset_offset loescht den Offset und ermoeglicht Neustart bei 0.
    #[test]
    fn test_reset_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-reset.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        // Offset setzen
        store.update_offset("test-proj", 42).unwrap();
        assert_eq!(store.get_offset("test-proj").unwrap(), Some(42));

        // Offset zuruecksetzen
        store.reset_offset("test-proj").unwrap();
        assert_eq!(store.get_offset("test-proj").unwrap(), None);

        // Nach Reset kann ab 1 neu gestartet werden (kein MonotonicityError)
        store.update_offset("test-proj", 1).unwrap();
        assert_eq!(store.get_offset("test-proj").unwrap(), Some(1));
    }

    /// reset_offset auf nicht-existierenden Namen ist kein Fehler.
    #[test]
    fn test_reset_offset_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-reset-nonexist.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        // Reset auf nicht-existierenden Namen — kein Fehler
        store.reset_offset("does-not-exist").unwrap();
        assert_eq!(store.get_offset("does-not-exist").unwrap(), None);
    }
}
