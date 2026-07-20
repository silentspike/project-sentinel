//! Append-only Event Store mit Outbox-Pattern fuer Zenoh-Publish.
//!
//! Drei Tabellen:
//! - `events`: Append-only Event Log (KEIN UPDATE/DELETE - Application-Layer Enforcement)
//! - `outbox`: Pending Zenoh-Publishes nach Commit
//! - `projection_offsets`: CQRS Projection Bookmark
//!
//! Append-Only wird im Code erzwungen (kein UPDATE/DELETE auf events).
//! rusqlite unterstuetzt keine INSTEAD OF Trigger auf normalen Tabellen.

use rusqlite::{params, Connection, OptionalExtension};
use sentinel_common::{
    DomainEvent, FencedStore, OwnerRegistry, OwnerWriteGuard, StateTransferScope,
};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{debug, info, instrument};

/// Histogram-Buckets fuer EventStore Latenzen (Mikrosekunden).
#[cfg(feature = "telemetry")]
const LATENCY_BUCKETS: &[f64] = &[50.0, 100.0, 500.0, 1000.0, 5000.0, 10000.0, 50000.0];

/// #264/#250: Immutability-Fenster fuer `world_snapshots` in Millisekunden (7 Tage). SSOT — DIESELBE
/// Konstante speist den `protect_recent_snapshots`-Trigger UND den Daemon-seitigen Retention-Skip
/// (`SnapshotManager::maintain`). Damit koennen Trigger-Block-Schwelle und Daemon-Skip-Alter nicht
/// still auseinanderdriften (Boundary-Invariant-Test in sentinel-daemon).
pub const IMMUTABLE_SNAPSHOT_MS: i64 = 7 * 86400 * 1000;

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
const CREATE_IDX_EVENTS_EVENT_ID: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_events_event_id ON events(event_id)";
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
    published_at INTEGER,
    retry_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT
)";

const CREATE_IDX_OUTBOX_PENDING: &str =
    "CREATE INDEX IF NOT EXISTS idx_outbox_pending ON outbox(status) WHERE status = 'pending'";
const CREATE_IDX_OUTBOX_EVENT_ID: &str =
    "CREATE INDEX IF NOT EXISTS idx_outbox_event_id ON outbox(event_id)";

const CREATE_LLM_COMPLETION_OUTBOX: &str = "
CREATE TABLE IF NOT EXISTS llm_completion_outbox (
    request_id TEXT PRIMARY KEY,
    request_digest TEXT NOT NULL,
    payload TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('provider_in_flight', 'pending_usage', 'ready_for_action', 'action_claimed', 'failed')),
    attempt_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
)";

const CREATE_IDX_LLM_COMPLETION_RECOVERABLE: &str =
    "CREATE INDEX IF NOT EXISTS idx_llm_completion_recoverable ON llm_completion_outbox(status, created_at) WHERE status IN ('pending_usage', 'ready_for_action')";

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

/// #493 (TM-5): side table for the restore generation epoch + the discarded "future" id-intervals.
/// `events` stays the untouched append-only SSOT (no per-row generation column); the dead branch is
/// recorded here as `restore_generation` (monotonic) + `dead_ranges` (JSON `[[from_exclusive,
/// to_inclusive], ...]`). Read guards exclude these intervals; the pruner deletes them and clears
/// the spent entry.
const CREATE_SIM_METADATA: &str = "
CREATE TABLE IF NOT EXISTS sim_metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
)";

// ──────────────────────────────────────────────
// #493 Dead Branch helpers (free fns over an existing Connection — no re-lock)
// ──────────────────────────────────────────────

/// Read a `sim_metadata` scalar within an existing (already-locked) connection.
fn read_sim_metadata(conn: &rusqlite::Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM sim_metadata WHERE key = ?1",
        params![key],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

/// Upsert a `sim_metadata` scalar within an existing connection.
fn set_sim_metadata_conn(
    conn: &rusqlite::Connection,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    conn.execute(
        "INSERT INTO sim_metadata (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
        params![key, value, now],
    )?;
    Ok(())
}

/// Read the discarded-future id-intervals `(from_exclusive, to_inclusive]` (dead branches).
fn read_dead_ranges(conn: &rusqlite::Connection) -> Vec<(i64, i64)> {
    match read_sim_metadata(conn, "dead_ranges") {
        Some(json) => serde_json::from_str::<Vec<(i64, i64)>>(&json).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// #493: the single shared dead-range EXCLUSION fragment used in EVERY event-read path so a
/// discarded "future" is never served. Returns ` AND NOT (id_expr > ?N AND id_expr <= ?M) …` with
/// numbered placeholders starting at `next_param`, plus the param values (from, to, from, to, …).
/// Empty ranges → empty fragment (zero-cost fast path). Read paths with no other WHERE append it to
/// a `WHERE 1=1`. A per-read-path test seeds a dead interval and asserts each read excludes it.
fn dead_range_exclusion(
    ranges: &[(i64, i64)],
    id_expr: &str,
    next_param: usize,
) -> (String, Vec<i64>) {
    let mut sql = String::new();
    let mut p = Vec::with_capacity(ranges.len() * 2);
    let mut idx = next_param;
    for (from, to) in ranges {
        sql.push_str(&format!(
            " AND NOT ({id_expr} > ?{idx} AND {id_expr} <= ?{})",
            idx + 1
        ));
        p.push(*from);
        p.push(*to);
        idx += 2;
    }
    (sql, p)
}

/// #493: the positive counterpart of [`dead_range_exclusion`], used ONLY by the pruner to SELECT the
/// dead-interval events for deletion (even when they sit above the retention cutoff). Returns
/// ` OR ({id_expr} > ?N AND {id_expr} <= ?M) …` with numbered placeholders from `next_param`. Empty
/// ranges → empty fragment.
fn dead_range_inclusion(
    ranges: &[(i64, i64)],
    id_expr: &str,
    next_param: usize,
) -> (String, Vec<i64>) {
    let mut sql = String::new();
    let mut p = Vec::with_capacity(ranges.len() * 2);
    let mut idx = next_param;
    for (from, to) in ranges {
        sql.push_str(&format!(
            " OR ({id_expr} > ?{idx} AND {id_expr} <= ?{})",
            idx + 1
        ));
        p.push(*from);
        p.push(*to);
        idx += 2;
    }
    (sql, p)
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetUpdateDecision {
    InsertOrAdvance,
    Noop,
    Reject,
}

pub fn classify_offset_update(current: Option<i64>, attempted: i64) -> OffsetUpdateDecision {
    match current {
        Some(current) if attempted < current => OffsetUpdateDecision::Reject,
        Some(current) if attempted == current => OffsetUpdateDecision::Noop,
        _ => OffsetUpdateDecision::InsertOrAdvance,
    }
}

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

/// Durable provider result awaiting local usage persistence and action delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmCompletionEntry {
    pub request_id: String,
    pub request_digest: String,
    pub payload: String,
    pub status: String,
    pub attempt_count: u32,
    pub last_error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

fn llm_completion_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LlmCompletionEntry> {
    Ok(LlmCompletionEntry {
        request_id: row.get(0)?,
        request_digest: row.get(1)?,
        payload: row.get(2)?,
        status: row.get(3)?,
        attempt_count: row.get::<_, i64>(4)? as u32,
        last_error: row.get(5)?,
        created_at: row.get::<_, i64>(6)? as u64,
        updated_at: row.get::<_, i64>(7)? as u64,
    })
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
    path: PathBuf,
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
             PRAGMA page_size = 8192;
             PRAGMA busy_timeout = 5000;",
        )?;

        // Schema erstellen
        conn.execute_batch(CREATE_EVENTS)?;
        conn.execute(CREATE_IDX_EVENTS_AGGREGATE, [])?;
        conn.execute(CREATE_IDX_EVENTS_TYPE, [])?;
        conn.execute(CREATE_IDX_EVENTS_CORRELATION, [])?;
        conn.execute(CREATE_IDX_EVENTS_CAUSATION, [])?;
        conn.execute(CREATE_IDX_EVENTS_EVENT_ID, [])?;
        conn.execute(CREATE_IDX_EVENTS_OPERATION, [])?;
        conn.execute_batch(CREATE_OUTBOX)?;
        Self::ensure_outbox_migrations(&conn)?;
        conn.execute(CREATE_IDX_OUTBOX_PENDING, [])?;
        conn.execute(CREATE_IDX_OUTBOX_EVENT_ID, [])?;
        conn.execute_batch(CREATE_LLM_COMPLETION_OUTBOX)?;
        conn.execute(CREATE_IDX_LLM_COMPLETION_RECOVERABLE, [])?;
        conn.execute_batch(CREATE_SNAPSHOTS)?;
        conn.execute(CREATE_IDX_SNAPSHOTS_AGGREGATE, [])?;
        conn.execute_batch(CREATE_WORLD_SNAPSHOTS)?;
        conn.execute(CREATE_IDX_WORLD_SNAPSHOTS_TIER, [])?;
        conn.execute_batch(CREATE_PROJECTION_OFFSETS)?;
        conn.execute_batch(CREATE_SIM_METADATA)?;

        // Security: Immutable Snapshots — Schutz vor Loeschung junger Snapshots.
        // #250: dieselbe SSOT-Konstante wie der Daemon-Retention-Skip (siehe IMMUTABLE_SNAPSHOT_MS).
        let immutable_ms: i64 = IMMUTABLE_SNAPSHOT_MS;
        conn.execute_batch(&format!(
            "DROP TRIGGER IF EXISTS protect_recent_snapshots;
             CREATE TRIGGER protect_recent_snapshots
             BEFORE DELETE ON world_snapshots
             WHEN (CAST(strftime('%s','now') AS INTEGER) * 1000 - OLD.created_at) < {immutable_ms}
             BEGIN
                 SELECT RAISE(ABORT, 'Cannot delete snapshot younger than 7 days');
             END;"
        ))?;

        info!("EventStore opened at {path}");
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: PathBuf::from(path),
        })
    }

    fn ensure_outbox_migrations(conn: &Connection) -> anyhow::Result<()> {
        if !Self::table_has_column(conn, "outbox", "retry_count")? {
            conn.execute(
                "ALTER TABLE outbox ADD COLUMN retry_count INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !Self::table_has_column(conn, "outbox", "last_error")? {
            conn.execute("ALTER TABLE outbox ADD COLUMN last_error TEXT", [])?;
        }
        Ok(())
    }

    fn table_has_column(conn: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Oeffnet den Event Store **read-only** — fuer reine Consumer (z.B. Dashboard/CAS-Pusher),
    /// die unter `systemd ReadOnlyPaths=` auf einem read-only gemounteten Datenverzeichnis laufen.
    /// Kein Schema-DDL, kein WAL-Mode-Write — `open()` wuerde sonst mit
    /// "attempt to write a readonly database" scheitern. Liest die Live-WAL-DB read-only
    /// (SQLite read-only shm-Fallback); sieht neu committete Events des Writers.
    #[instrument(level = "debug", fields(path = %path))]
    pub fn open_readonly(path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )?;
        // Nur verbindungslokale, lesende Pragmas (kein DB-Write, kein Schema, kein WAL-Wechsel).
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        conn.execute_batch("PRAGMA query_only = ON;")?;
        info!("EventStore opened READ-ONLY at {path}");
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: PathBuf::from(path),
        })
    }

    // The single fenced write entry (#496 V3/V19) is `impl FencedStore for EventStore`
    // below. It opens an explicit SQLite transaction and rechecks the complete owner
    // term immediately before COMMIT; dropping the wrapper rolls the transaction back.

    /// Append-only: Fuegt ein Event ein. Gibt die interne Row-ID zurueck.
    pub fn append_event(&self, event: &DomainEvent) -> anyhow::Result<i64> {
        let _telemetry_start = std::time::Instant::now();
        let conn = self.begin_fenced_write(
            &OwnerRegistry::global()
                .issue(StateTransferScope::for_aggregate(&event.aggregate_id))?,
        )?;
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
        conn.commit()?;
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.limbo.event.append.count").increment();
            reg.histogram("sentinel.limbo.event.append.duration_us", LATENCY_BUCKETS)
                .observe(_telemetry_start.elapsed().as_micros() as f64);
        }
        Ok(row_id)
    }

    /// Store a completed provider response before attempting the local usage append.
    /// Reusing a request ID with different request bytes or response bytes fails closed.
    pub fn enqueue_llm_completion(
        &self,
        request_id: &str,
        request_digest: &str,
        payload: &str,
    ) -> anyhow::Result<()> {
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let existing = conn
            .query_row(
                "SELECT request_digest, payload, status FROM llm_completion_outbox WHERE request_id = ?1",
                params![request_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        match existing {
            None => {
                conn.execute(
                    "INSERT INTO llm_completion_outbox
                     (request_id, request_digest, payload, status, attempt_count, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 'pending_usage', 0, ?4, ?4)",
                    params![request_id, request_digest, payload, now_ms],
                )?;
            }
            Some((existing_digest, _, status))
                if existing_digest == request_digest && status == "provider_in_flight" =>
            {
                conn.execute(
                    "UPDATE llm_completion_outbox
                     SET payload = ?3, status = 'pending_usage', updated_at = ?4
                     WHERE request_id = ?1 AND request_digest = ?2 AND status = 'provider_in_flight'",
                    params![request_id, request_digest, payload, now_ms],
                )?;
            }
            Some((existing_digest, existing_payload, _)) => anyhow::ensure!(
                existing_digest == request_digest && existing_payload == payload,
                "LLM completion request_id conflict for {request_id}"
            ),
        }
        conn.commit()?;
        Ok(())
    }

    /// Reserve the stable request ID immediately before network execution. If the
    /// process dies after the provider may have run but before its response is
    /// durable, the reservation remains fail-closed and prevents a paid replay.
    pub fn reserve_llm_request(
        &self,
        request_id: &str,
        request_digest: &str,
    ) -> anyhow::Result<bool> {
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO llm_completion_outbox
             (request_id, request_digest, payload, status, attempt_count, created_at, updated_at)
             VALUES (?1, ?2, '', 'provider_in_flight', 0, ?3, ?3)",
            params![request_id, request_digest, now_ms],
        )?;
        if inserted == 0 {
            let existing_digest: String = conn.query_row(
                "SELECT request_digest FROM llm_completion_outbox WHERE request_id = ?1",
                params![request_id],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                existing_digest == request_digest,
                "LLM request reservation digest conflict for {request_id}"
            );
        }
        conn.commit()?;
        Ok(inserted == 1)
    }

    /// Return one durable completion by its stable request ID.
    pub fn get_llm_completion(
        &self,
        request_id: &str,
    ) -> anyhow::Result<Option<LlmCompletionEntry>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        conn.query_row(
            "SELECT request_id, request_digest, payload, status, attempt_count, last_error,
                    created_at, updated_at
             FROM llm_completion_outbox WHERE request_id = ?1",
            params![request_id],
            llm_completion_from_row,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Poll only records that can make automatic progress. Claimed and terminal
    /// records are deliberately excluded to prevent ambiguous action redelivery.
    pub fn poll_llm_completions(&self, limit: usize) -> anyhow::Result<Vec<LlmCompletionEntry>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT request_id, request_digest, payload, status, attempt_count, last_error,
                    created_at, updated_at
             FROM llm_completion_outbox
             WHERE status IN ('pending_usage', 'ready_for_action')
             ORDER BY created_at ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], llm_completion_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Atomically append the usage event and advance the durable response to the
    /// action-ready state. Retrying this operation is idempotent by operation_id.
    pub fn persist_llm_completion_usage(
        &self,
        request_id: &str,
        request_digest: &str,
        event: &DomainEvent,
    ) -> anyhow::Result<()> {
        let conn = self.begin_fenced_write(
            &OwnerRegistry::global()
                .issue(StateTransferScope::for_aggregate(&event.aggregate_id))?,
        )?;
        let state = conn
            .query_row(
                "SELECT request_digest, status FROM llm_completion_outbox WHERE request_id = ?1",
                params![request_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("missing LLM completion {request_id}"))?;
        anyhow::ensure!(
            state.0 == request_digest,
            "LLM completion digest conflict for {request_id}"
        );
        if state.1 == "ready_for_action" || state.1 == "action_claimed" {
            conn.commit()?;
            return Ok(());
        }
        anyhow::ensure!(
            state.1 == "pending_usage",
            "LLM completion {request_id} is not recoverable from status {}",
            state.1
        );
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
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        conn.execute(
            "UPDATE llm_completion_outbox
             SET status = 'ready_for_action', last_error = NULL, updated_at = ?3
             WHERE request_id = ?1 AND request_digest = ?2 AND status = 'pending_usage'",
            params![request_id, request_digest, now_ms],
        )?;
        conn.commit()?;
        Ok(())
    }

    /// Count one failed recovery attempt and stop automatic recovery at the
    /// supplied finite limit. Both pre-usage and pre-action failures use the same
    /// durable budget so no corrupt or unavailable record spins forever.
    pub fn record_llm_completion_failure(
        &self,
        request_id: &str,
        request_digest: &str,
        error: &str,
        max_attempts: u32,
    ) -> anyhow::Result<(u32, bool)> {
        anyhow::ensure!(max_attempts > 0, "max_attempts must be positive");
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let (attempts, current_status): (i64, String) = conn
            .query_row(
                "SELECT attempt_count, status FROM llm_completion_outbox
                 WHERE request_id = ?1 AND request_digest = ?2
                   AND status IN ('pending_usage', 'ready_for_action')",
                params![request_id, request_digest],
                |row| Ok((row.get::<_, i64>(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("LLM completion {request_id} is not pending"))?;
        let attempts: u32 = attempts.try_into().unwrap_or(u32::MAX);
        let attempts = attempts.saturating_add(1);
        let terminal = attempts >= max_attempts;
        let status = if terminal {
            "failed"
        } else {
            current_status.as_str()
        };
        conn.execute(
            "UPDATE llm_completion_outbox
             SET attempt_count = ?3, last_error = ?4, status = ?5, updated_at = ?6
             WHERE request_id = ?1 AND request_digest = ?2
               AND status IN ('pending_usage', 'ready_for_action')",
            params![request_id, request_digest, attempts, error, status, now_ms],
        )?;
        conn.commit()?;
        Ok((attempts, terminal))
    }

    /// Claim actions before sending them. A crash after this transition is
    /// intentionally at-most-once/fail-closed: claimed actions are never replayed.
    pub fn claim_llm_completion_actions(
        &self,
        request_id: &str,
        request_digest: &str,
    ) -> anyhow::Result<bool> {
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let changed = conn.execute(
            "UPDATE llm_completion_outbox
             SET status = 'action_claimed', updated_at = ?3
             WHERE request_id = ?1 AND request_digest = ?2 AND status = 'ready_for_action'",
            params![request_id, request_digest, now_ms],
        )?;
        conn.commit()?;
        Ok(changed == 1)
    }

    /// Remove a claimed completion after all actions were accepted by the local
    /// channel. The durable usage event remains the long-term idempotency marker.
    pub fn complete_llm_completion_actions(
        &self,
        request_id: &str,
        request_digest: &str,
    ) -> anyhow::Result<bool> {
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let changed = conn.execute(
            "DELETE FROM llm_completion_outbox
             WHERE request_id = ?1 AND request_digest = ?2 AND status = 'action_claimed'",
            params![request_id, request_digest],
        )?;
        conn.commit()?;
        Ok(changed == 1)
    }

    /// Check the durable operation id before issuing a provider call for a
    /// perception that has already completed in an earlier process.
    pub fn has_event_operation_id(&self, operation_id: &str) -> anyhow::Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let exists = conn
            .query_row(
                "SELECT 1 FROM events WHERE operation_id = ?1 LIMIT 1",
                params![operation_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        Ok(exists)
    }

    /// Atomar: Event + Outbox-Eintrag in einer Transaktion (AC1, AC3).
    ///
    /// Nutzt operation_id als Idempotenz-Key (UNIQUE INDEX).
    /// Bei Duplikat (gleiche operation_id) wird kein neuer Eintrag erstellt.
    pub fn append_with_outbox(&self, event: &DomainEvent, topic: &str) -> anyhow::Result<i64> {
        let _telemetry_start = std::time::Instant::now();
        let conn = self.begin_fenced_write(
            &OwnerRegistry::global()
                .issue(StateTransferScope::for_aggregate(&event.aggregate_id))?,
        )?;

        // INSERT OR IGNORE: Idempotenz via operation_id UNIQUE INDEX
        let inserted = conn.execute(
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

            conn.execute(
                "INSERT INTO outbox (event_id, topic, payload, status, created_at) VALUES (?1, ?2, ?3, 'pending', ?4)",
                params![event.event_id, topic, event.payload, now_ms],
            )?;
        }

        let row_id = conn.last_insert_rowid();
        conn.commit()?;

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

    /// Atomar: mehrere Events + Outbox-Eintraege in einer Transaktion.
    ///
    /// Nutzt dieselbe operation_id-Idempotenz wie `append_with_outbox`.
    /// Duplikate werden ignoriert und erzeugen keinen Outbox-Eintrag.
    pub fn append_with_outbox_batch<'a, I>(&self, entries: I) -> anyhow::Result<usize>
    where
        I: IntoIterator<Item = (&'a DomainEvent, &'a str)>,
    {
        let _telemetry_start = std::time::Instant::now();
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let inserted_count = {
            let mut insert_event = conn.prepare(
                "INSERT OR IGNORE INTO events (event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            let mut insert_outbox = conn.prepare(
                "INSERT INTO outbox (event_id, topic, payload, status, created_at) VALUES (?1, ?2, ?3, 'pending', ?4)",
            )?;
            let mut inserted_count = 0usize;

            for (event, topic) in entries {
                let inserted = insert_event.execute(params![
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
                ])?;

                if inserted > 0 {
                    insert_outbox.execute(params![event.event_id, topic, event.payload, now_ms])?;
                    inserted_count += inserted;
                }
            }

            inserted_count
        };

        conn.commit()?;

        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.limbo.event.append_outbox_batch.count")
                .increment_by(inserted_count as u64);
            reg.histogram(
                "sentinel.limbo.event.append_outbox_batch.duration_us",
                LATENCY_BUCKETS,
            )
            .observe(_telemetry_start.elapsed().as_micros() as f64);
        }
        Ok(inserted_count)
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
        // #493: exclude any discarded "future" (dead id-intervals) from forward reads.
        let dead = read_dead_ranges(&conn);
        let (dead_sql, dead_p) = dead_range_exclusion(&dead, "id", 3);
        let mut stmt = conn.prepare(&format!(
            "SELECT event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type FROM events WHERE id > ?1{dead_sql} ORDER BY id ASC LIMIT ?2"
        ))?;
        let limit_i64 = limit as i64;
        let mut bind: Vec<&dyn rusqlite::ToSql> = vec![&after_id, &limit_i64];
        bind.extend(dead_p.iter().map(|d| d as &dyn rusqlite::ToSql));
        let rows = stmt.query_map(bind.as_slice(), |row| {
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

    /// #491 (TM-3): Liest Events im halboffenen Cursor-Intervall `(from_exclusive, to_inclusive]`
    /// in stabiler `id`-Reihenfolge — die exakte Eingabe-Sequenz fuer den Bounded Replay
    /// `(anchor, target]`. Im Gegensatz zu `get_events_since` mit oberer Schranke statt Limit.
    pub fn get_events_range(
        &self,
        from_exclusive: i64,
        to_inclusive: i64,
    ) -> anyhow::Result<Vec<DomainEvent>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        // #493 (couples #491): a bounded replay over a window that crosses a dead-interval boundary
        // must NOT feed discarded-future events back into the restored world — exclude them here.
        let dead = read_dead_ranges(&conn);
        let (dead_sql, dead_p) = dead_range_exclusion(&dead, "id", 3);
        let mut stmt = conn.prepare(&format!(
            "SELECT event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type FROM events WHERE id > ?1 AND id <= ?2{dead_sql} ORDER BY id ASC"
        ))?;
        let mut bind: Vec<&dyn rusqlite::ToSql> = vec![&from_exclusive, &to_inclusive];
        bind.extend(dead_p.iter().map(|d| d as &dyn rusqlite::ToSql));
        let rows = stmt.query_map(bind.as_slice(), |row| {
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

    /// #491 (TM-3): Anzahl Events in `(from_exclusive, to_inclusive]` (Replay-Range-Groesse fuer die
    /// Restore-Response `replay_event_count`, ohne die Events tatsaechlich zu laden).
    pub fn count_events_in_range(
        &self,
        from_exclusive: i64,
        to_inclusive: i64,
    ) -> anyhow::Result<u64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        // #493: keep the replay-count consistent with `get_events_range` (dead events never replay).
        let dead = read_dead_ranges(&conn);
        let (dead_sql, dead_p) = dead_range_exclusion(&dead, "id", 3);
        let mut bind: Vec<&dyn rusqlite::ToSql> = vec![&from_exclusive, &to_inclusive];
        bind.extend(dead_p.iter().map(|d| d as &dyn rusqlite::ToSql));
        let count: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM events WHERE id > ?1 AND id <= ?2{dead_sql}"),
            bind.as_slice(),
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    /// #491 (TM-3): groesste `events.id` mit `tick <= target_tick` — loest ein Ziel-Tick in einen
    /// Event-Cursor auf (Restore-auf-Tick = Ende dieses Ticks). `None` wenn kein Event so frueh ist
    /// (Ziel liegt vor dem ersten Event).
    pub fn max_event_id_at_tick(&self, target_tick: u64) -> anyhow::Result<Option<i64>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let id: Option<i64> = conn.query_row(
            "SELECT MAX(id) FROM events WHERE tick <= ?1",
            params![target_tick as i64],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    /// #491 (TM-3): Tick eines Events anhand seiner `id` (Anchor-Aufloesung / Ziel-Validierung).
    pub fn get_event_tick(&self, id: i64) -> anyhow::Result<Option<u64>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let tick: Option<i64> = conn
            .query_row(
                "SELECT tick FROM events WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(tick.map(|t| t as u64))
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
        // #493: projection workers must skip discarded-future events (offset jumps past dead ids).
        let dead = read_dead_ranges(&conn);
        let (dead_sql, dead_p) = dead_range_exclusion(&dead, "id", 3);
        let mut stmt = conn.prepare(&format!(
            "SELECT id, event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type FROM events WHERE id > ?1{dead_sql} ORDER BY id ASC LIMIT ?2"
        ))?;
        let limit_i64 = limit as i64;
        let mut bind: Vec<&dyn rusqlite::ToSql> = vec![&after_id, &limit_i64];
        bind.extend(dead_p.iter().map(|d| d as &dyn rusqlite::ToSql));
        let rows = stmt.query_map(bind.as_slice(), |row| {
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
        // #493: never serve discarded-future events for an aggregate read.
        let dead = read_dead_ranges(&conn);
        let (dead_sql, dead_p) = dead_range_exclusion(&dead, "id", 3);
        let mut stmt = conn.prepare(&format!(
            "SELECT event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type FROM events WHERE aggregate_id = ?1{dead_sql} ORDER BY id ASC LIMIT ?2"
        ))?;
        let limit_i64 = limit as i64;
        let mut bind: Vec<&dyn rusqlite::ToSql> = vec![&aggregate_id, &limit_i64];
        bind.extend(dead_p.iter().map(|d| d as &dyn rusqlite::ToSql));
        let rows = stmt.query_map(bind.as_slice(), |row| {
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
        // #493: never serve discarded-future events for a correlation read.
        let dead = read_dead_ranges(&conn);
        let (dead_sql, dead_p) = dead_range_exclusion(&dead, "id", 3);
        let mut stmt = conn.prepare(&format!(
            "SELECT event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type FROM events WHERE correlation_id = ?1{dead_sql} ORDER BY id ASC LIMIT ?2"
        ))?;
        let limit_i64 = limit as i64;
        let mut bind: Vec<&dyn rusqlite::ToSql> = vec![&correlation_id, &limit_i64];
        bind.extend(dead_p.iter().map(|d| d as &dyn rusqlite::ToSql));
        let rows = stmt.query_map(bind.as_slice(), |row| {
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
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;

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
        conn.execute(
            "DELETE FROM snapshots WHERE aggregate_id = ?1 AND id <> ?2",
            params![aggregate_id, row_id],
        )?;
        conn.commit()?;
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

    /// Loescht alte Snapshot-Versionen und behaelt pro Aggregate nur die neueste Version.
    ///
    /// Diese Retention betrifft die kompakte `snapshots`-Tabelle, nicht die immutable
    /// `world_snapshots` Time-Machine-Tabelle.
    pub fn retain_latest_snapshots(&self) -> anyhow::Result<u64> {
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let deleted = conn.execute(
            "DELETE FROM snapshots
             WHERE id NOT IN (
                 SELECT (
                     SELECT s2.id
                     FROM snapshots s2
                     WHERE s2.aggregate_id = aggregates.aggregate_id
                     ORDER BY s2.version DESC, s2.id DESC
                     LIMIT 1
                 )
                 FROM (SELECT DISTINCT aggregate_id FROM snapshots) aggregates
             )",
            [],
        )? as u64;
        conn.commit()?;
        Ok(deleted)
    }

    // ── Outbox ──────────────────────────────────

    /// Pollt pending Outbox-Eintraege fuer den Zenoh-Publisher.
    pub fn poll_outbox(&self, limit: usize) -> anyhow::Result<Vec<OutboxEntry>> {
        let _telemetry_start = std::time::Instant::now();
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        // #493: never publish a discarded-future event. The dead intervals are id-ranges in `events`,
        // so join the outbox to its event and exclude dead `e.id`s with the shared guard. The join is
        // INNER, which is safe here: `can_prune` forbids pruning while an outbox entry is pending, so
        // a pending entry's event row always exists (both are written in one transaction). The
        // matching `idx_outbox_event_id` / unique `idx_events_event_id` keep the join cheap.
        let dead = read_dead_ranges(&conn);
        let (dead_sql, dead_p) = dead_range_exclusion(&dead, "e.id", 2);
        let limit_i64 = limit as i64;
        let mut bind: Vec<&dyn rusqlite::ToSql> = vec![&limit_i64];
        bind.extend(dead_p.iter().map(|d| d as &dyn rusqlite::ToSql));
        let mut stmt = conn.prepare(&format!(
            "SELECT o.id, o.event_id, o.topic, o.payload, o.status, o.created_at FROM outbox o JOIN events e ON o.event_id = e.event_id WHERE o.status = 'pending'{dead_sql} ORDER BY o.id ASC LIMIT ?1"
        ))?;
        let rows = stmt.query_map(bind.as_slice(), |row| {
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
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        conn.execute(
            "UPDATE outbox SET status = 'published', published_at = ?1 WHERE event_id = ?2",
            params![now_ms, event_id],
        )?;
        conn.commit()?;
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

    // ── #493 Restore Generation / Dead Branch ──────────────────────────────

    /// Current restore generation epoch (monotonic; 0 before the first restore).
    pub fn get_restore_generation(&self) -> anyhow::Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        Ok(read_sim_metadata(&conn, "restore_generation")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0))
    }

    /// Bump the restore generation epoch (called once per restore). Returns the new generation.
    pub fn increment_restore_generation(&self) -> anyhow::Result<i64> {
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let next = read_sim_metadata(&conn, "restore_generation")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0)
            + 1;
        set_sim_metadata_conn(&conn, "restore_generation", &next.to_string())?;
        conn.commit()?;
        Ok(next)
    }

    /// The discarded-future id-intervals `(from_exclusive, to_inclusive]` (dead branches).
    pub fn dead_ranges(&self) -> anyhow::Result<Vec<(i64, i64)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        Ok(read_dead_ranges(&conn))
    }

    /// Mark `(from_exclusive, to_inclusive]` as a dead branch (the future discarded by a restore).
    pub fn push_dead_range(&self, from_exclusive: i64, to_inclusive: i64) -> anyhow::Result<()> {
        if to_inclusive <= from_exclusive {
            return Ok(()); // nothing discarded
        }
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let mut ranges = read_dead_ranges(&conn);
        ranges.push((from_exclusive, to_inclusive));
        set_sim_metadata_conn(&conn, "dead_ranges", &serde_json::to_string(&ranges)?)?;
        conn.commit()?;
        Ok(())
    }

    /// Drop a dead-range entry once its events have been pruned (keeps the list bounded).
    pub fn remove_dead_range(&self, from_exclusive: i64) -> anyhow::Result<()> {
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let mut ranges = read_dead_ranges(&conn);
        let before = ranges.len();
        ranges.retain(|(from, _)| *from != from_exclusive);
        if ranges.len() != before {
            set_sim_metadata_conn(&conn, "dead_ranges", &serde_json::to_string(&ranges)?)?;
        }
        conn.commit()?;
        Ok(())
    }

    /// Prueft ob Pruning sicher ist (Projection-Offsets, Outbox-Backlog).
    /// Gibt `Ok(true)` zurueck wenn Pruning erlaubt, `Ok(false)` bei Safety-Guard.
    pub fn can_prune(&self, cutoff_event_id: i64) -> anyhow::Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;

        let min_offset: Option<i64> = conn
            .query_row(
                "SELECT MIN(last_event_id) FROM projection_offsets",
                [],
                |row| row.get(0),
            )
            .ok();
        if let Some(min) = min_offset {
            if min < cutoff_event_id {
                return Ok(false);
            }
        }

        // #493: only LIVE pending outbox entries gate pruning. A discarded future leaves its outbox
        // entries 'pending' forever (poll_outbox skips them via the same guard), so counting them
        // would deadlock pruning permanently. Exclude dead `e.id`s with the shared guard.
        let dead = read_dead_ranges(&conn);
        let (dead_sql, dead_p) = dead_range_exclusion(&dead, "e.id", 1);
        let bind: Vec<&dyn rusqlite::ToSql> =
            dead_p.iter().map(|d| d as &dyn rusqlite::ToSql).collect();
        let pending_outbox: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM outbox o JOIN events e ON o.event_id = e.event_id WHERE o.status = 'pending'{dead_sql}"
                ),
                bind.as_slice(),
                |row| row.get(0),
            )
            .unwrap_or(0);
        if pending_outbox > 0 {
            return Ok(false);
        }

        Ok(true)
    }

    /// Loescht einen einzelnen Batch von Events mit id < cutoff_event_id.
    ///
    /// Designed fuer Aufruf aus dem Tick-Loop: 1 Batch (1000 Rows) pro Tick.
    /// Nutzt die shared Connection — kein separater Thread, kein Lock-Konflikt.
    /// Gibt die Anzahl geloeschter Rows zurueck (0 = fertig).
    pub fn prune_batch(&self, cutoff_event_id: i64, batch_size: i64) -> anyhow::Result<u64> {
        if batch_size <= 0 {
            return Ok(0);
        }

        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;

        // #493: dead intervals are pruned ALONGSIDE the retention cutoff — even above it — so a
        // discarded future is physically removed in the normal retention window (not left to linger).
        let dead = read_dead_ranges(&conn);
        let (dead_incl_sql, dead_p) = dead_range_inclusion(&dead, "id", 3);

        conn.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS prune_batch_ids (
                id INTEGER PRIMARY KEY,
                event_id TEXT NOT NULL
            );
            DELETE FROM prune_batch_ids;",
        )?;

        let mut bind: Vec<&dyn rusqlite::ToSql> = vec![&cutoff_event_id, &batch_size];
        bind.extend(dead_p.iter().map(|d| d as &dyn rusqlite::ToSql));
        let selected = conn.execute(
            &format!(
                "INSERT INTO prune_batch_ids(id, event_id)
                 SELECT id, event_id
                 FROM events
                 WHERE (id < ?1{dead_incl_sql})
                 ORDER BY id
                 LIMIT ?2"
            ),
            bind.as_slice(),
        )? as u64;

        if selected > 0 {
            conn.execute(
                "DELETE FROM outbox
                 WHERE event_id IN (SELECT event_id FROM prune_batch_ids)",
                [],
            )?;
            conn.execute(
                "DELETE FROM events
                 WHERE id IN (SELECT id FROM prune_batch_ids)",
                [],
            )?;
        }
        // #493 requirement 2: drop a `dead_ranges` entry once its interval is empty (keeps the list
        // bounded over many restores). Runs on the same held connection — no re-lock, no deadlock.
        if !dead.is_empty() {
            let mut kept = Vec::with_capacity(dead.len());
            let mut changed = false;
            for (from, to) in &dead {
                let remaining: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM events WHERE id > ?1 AND id <= ?2",
                    params![from, to],
                    |row| row.get(0),
                )?;
                if remaining > 0 {
                    kept.push((*from, *to));
                } else {
                    changed = true;
                }
            }
            if changed {
                set_sim_metadata_conn(&conn, "dead_ranges", &serde_json::to_string(&kept)?)?;
            }
        }

        conn.commit()?;
        Ok(selected)
    }

    /// Loescht Outbox-Zeilen, deren Event bereits nicht mehr existiert.
    ///
    /// Fuer Live-Grossdatenbanken ist der bevorzugte Pfad die Offline-CTAS-Kompaktion.
    /// Diese Methode ist als Maintenance-/Test-Primitive gedacht.
    pub fn delete_orphan_outbox(&self) -> anyhow::Result<u64> {
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let deleted = conn.execute(
            "DELETE FROM outbox
             WHERE event_id NOT IN (SELECT event_id FROM events)",
            [],
        )? as u64;
        conn.commit()?;
        Ok(deleted)
    }

    /// Gibt den DB-Pfad zurueck (fuer Tests / Diagnostik).
    pub fn db_path(&self) -> &Path {
        &self.path
    }

    /// Setzt den Offset einer Projection (upsert, monoton steigend).
    ///
    /// Verhalten:
    /// - `offset > current` → Normal-Update (Fortschritt)
    /// - `offset == current` → No-op (idempotent, kein Fehler)
    /// - `offset < current` → `MonotonicityError` (Rueckwaerts-Drift)
    pub fn update_offset(&self, name: &str, offset: i64) -> anyhow::Result<()> {
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;

        // Aktuellen Offset pruefen
        let current: Option<i64> = conn
            .query_row(
                "SELECT last_event_id FROM projection_offsets WHERE projection_name = ?1",
                params![name],
                |row| row.get(0),
            )
            .ok();

        match classify_offset_update(current, offset) {
            OffsetUpdateDecision::Noop => return Ok(()),
            OffsetUpdateDecision::Reject => {
                return Err(MonotonicityError {
                    projection: name.to_string(),
                    current: current.expect("reject requires existing offset"),
                    attempted: offset,
                }
                .into());
            }
            OffsetUpdateDecision::InsertOrAdvance => {}
        }

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        conn.execute(
            "INSERT INTO projection_offsets (projection_name, last_event_id, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(projection_name) DO UPDATE SET last_event_id = ?2, updated_at = ?3",
            params![name, offset, now_ms],
        )?;
        conn.commit()?;
        Ok(())
    }

    /// Setzt den Offset einer Projection zurueck (fuer Rebuild-Modus).
    ///
    /// Loescht den Eintrag aus `projection_offsets`, sodass der naechste
    /// `get_offset()` Call `None` zurueckgibt. Umgeht die Monotonitaetspruefung
    /// von `update_offset()`.
    /// Erzwingt einen Offset-Wert (umgeht Monotonie-Pruefung, fuer Restore).
    pub fn force_reset_offset(&self, name: &str, offset: i64) -> anyhow::Result<()> {
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        conn.execute(
            "INSERT OR REPLACE INTO projection_offsets (projection_name, last_event_id, updated_at) VALUES (?1, ?2, ?3)",
            params![name, offset, now_ms],
        )?;
        conn.commit()?;
        Ok(())
    }

    pub fn reset_offset(&self, name: &str) -> anyhow::Result<()> {
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        conn.execute(
            "DELETE FROM projection_offsets WHERE projection_name = ?1",
            params![name],
        )?;
        conn.commit()?;
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
        // #493: a full rebuild must not replay a discarded future — exclude dead intervals here too
        // (no other WHERE, so the guard hangs off `WHERE 1=1`).
        let dead = read_dead_ranges(&conn);
        let (dead_sql, dead_p) = dead_range_exclusion(&dead, "id", 1);
        let mut stmt = conn.prepare(&format!(
            "SELECT event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type FROM events WHERE 1=1{dead_sql} ORDER BY id ASC"
        ))?;
        let bind: Vec<&dyn rusqlite::ToSql> =
            dead_p.iter().map(|d| d as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(bind.as_slice(), |row| {
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

    /// Speichert einen World Snapshot als bincode BLOB (created_at = jetzt).
    pub fn save_world_snapshot(
        &self,
        id: &str,
        tier: &str,
        tick: u64,
        sim_hour: f32,
        last_event_id: i64,
        payload: &[u8],
    ) -> anyhow::Result<()> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        self.save_world_snapshot_at(id, tier, tick, sim_hour, last_event_id, payload, now_ms)
    }

    /// Speichert einen World Snapshot mit explizitem `created_at_ms` (Epoch-Millisekunden).
    ///
    /// Fuer Pfade, die einen Snapshot zu einem bestimmten Zeitpunkt einspielen (Import, Replay,
    /// Tests, Retention-Benchmarks mit gealterter Population). Der Produktiv-Pfad nutzt
    /// `save_world_snapshot` (created_at = jetzt). Der #264-Trigger bewertet das Alter ueber die
    /// echte Uhr (`strftime('now')`), unabhaengig vom hier gesetzten `created_at_ms`.
    pub fn save_world_snapshot_at(
        &self,
        id: &str,
        tier: &str,
        tick: u64,
        sim_hour: f32,
        last_event_id: i64,
        payload: &[u8],
        created_at_ms: i64,
    ) -> anyhow::Result<()> {
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
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
                created_at_ms,
            ],
        )?;
        conn.commit()?;
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
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let deleted = conn.execute("DELETE FROM world_snapshots WHERE id = ?1", params![id])?;
        conn.commit()?;
        Ok(deleted > 0)
    }

    /// Aktualisiert den Tier eines World Snapshots (fuer Promotion).
    pub fn promote_world_snapshot(&self, id: &str, new_tier: &str) -> anyhow::Result<bool> {
        let conn =
            self.begin_fenced_write(&OwnerRegistry::global().issue(StateTransferScope::World)?)?;
        let updated = conn.execute(
            "UPDATE world_snapshots SET tier = ?1 WHERE id = ?2",
            params![new_tier, id],
        )?;
        conn.commit()?;
        Ok(updated > 0)
    }

    /// Zaehlt World Snapshots gruppiert nach Tier (`GROUP BY tier`, absteigend nach Anzahl).
    /// Fuer Retention-Verifikation/Monitoring (#250 AC-1: Tier-Verteilung) und Benchmarks.
    pub fn count_world_snapshots_by_tier(&self) -> anyhow::Result<Vec<(String, i64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT tier, count(*) FROM world_snapshots GROUP BY tier ORDER BY count(*) DESC",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

pub struct FencedSqliteWrite<'a> {
    conn: std::sync::MutexGuard<'a, Connection>,
    guard: OwnerWriteGuard,
    committed: bool,
}

impl std::ops::Deref for FencedSqliteWrite<'_> {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.conn
    }
}

impl std::ops::DerefMut for FencedSqliteWrite<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.conn
    }
}

impl FencedSqliteWrite<'_> {
    pub fn commit(mut self) -> anyhow::Result<()> {
        OwnerRegistry::global().validate(&self.guard)?;
        self.conn.execute_batch("COMMIT")?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for FencedSqliteWrite<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

impl FencedStore for EventStore {
    type Txn<'a> = FencedSqliteWrite<'a>;

    fn begin_fenced_write(&self, guard: &OwnerWriteGuard) -> anyhow::Result<FencedSqliteWrite<'_>> {
        OwnerRegistry::global().validate(guard)?;
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(FencedSqliteWrite {
            conn,
            guard: guard.clone(),
            committed: false,
        })
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

    #[test]
    fn llm_completion_outbox_is_durable_bounded_and_action_claimed_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llm-completion.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();
        let request_id = "agent-runtime-07-55";
        let digest = "request-digest";
        let payload = r#"{"completed":true}"#;
        assert!(store.reserve_llm_request(request_id, digest).unwrap());
        assert!(!store.reserve_llm_request(request_id, digest).unwrap());
        assert!(store.reserve_llm_request(request_id, "different").is_err());
        assert_eq!(
            store
                .get_llm_completion(request_id)
                .unwrap()
                .unwrap()
                .status,
            "provider_in_flight"
        );
        assert!(store.poll_llm_completions(10).unwrap().is_empty());
        drop(store);

        let store = EventStore::open(path.to_str().unwrap()).unwrap();
        assert!(!store.reserve_llm_request(request_id, digest).unwrap());
        assert_eq!(
            store
                .get_llm_completion(request_id)
                .unwrap()
                .unwrap()
                .status,
            "provider_in_flight"
        );
        store
            .enqueue_llm_completion(request_id, digest, payload)
            .unwrap();
        store
            .enqueue_llm_completion(request_id, digest, payload)
            .unwrap();
        assert!(store
            .enqueue_llm_completion(request_id, "different", payload)
            .is_err());

        let pending = store.get_llm_completion(request_id).unwrap().unwrap();
        assert_eq!(pending.status, "pending_usage");
        assert_eq!(store.poll_llm_completions(10).unwrap().len(), 1);
        assert_eq!(
            store
                .record_llm_completion_failure(request_id, digest, "injected", 3)
                .unwrap(),
            (1, false)
        );

        let mut usage = test_event("agent_llm_usage", "AGENT-07");
        usage.operation_id = format!("llm_usage_{request_id}");
        store
            .persist_llm_completion_usage(request_id, digest, &usage)
            .unwrap();
        assert!(store.has_event_operation_id(&usage.operation_id).unwrap());
        assert_eq!(
            store
                .get_llm_completion(request_id)
                .unwrap()
                .unwrap()
                .status,
            "ready_for_action"
        );
        assert!(store
            .claim_llm_completion_actions(request_id, digest)
            .unwrap());
        assert!(!store
            .claim_llm_completion_actions(request_id, digest)
            .unwrap());
        assert!(store
            .complete_llm_completion_actions(request_id, digest)
            .unwrap());
        assert!(store.get_llm_completion(request_id).unwrap().is_none());
    }

    #[test]
    fn llm_completion_failure_limit_is_terminal_and_not_polled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llm-completion-terminal.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();
        store
            .enqueue_llm_completion("request-1", "digest-1", "{}")
            .unwrap();
        assert_eq!(
            store
                .record_llm_completion_failure("request-1", "digest-1", "first", 2)
                .unwrap(),
            (1, false)
        );
        assert_eq!(
            store
                .record_llm_completion_failure("request-1", "digest-1", "second", 2)
                .unwrap(),
            (2, true)
        );
        let terminal = store.get_llm_completion("request-1").unwrap().unwrap();
        assert_eq!(terminal.status, "failed");
        assert_eq!(terminal.attempt_count, 2);
        assert!(store.poll_llm_completions(10).unwrap().is_empty());
    }

    /// #496: the fenced write entry is the single choke point every SQLite writer
    /// routes through. Both the `World` and a `NanoContainer(agent)` scope remain
    /// behavior-compatible on the single-node fast path.
    #[test]
    fn test_begin_fenced_write_is_behavior_preserving_choke_point() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-fenced.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        // A routed writer (append_event goes through begin_fenced_write) persists.
        let event = test_event("agent_action_received", "AGENT-07");
        store.append_event(&event).unwrap();
        assert_eq!(store.event_count().unwrap(), 1);

        // The choke point itself yields a usable connection under both scopes —
        // single-node the registry owns every scope, so the issued guard validates,
        // World and per-container alike.
        for scope in [
            StateTransferScope::World,
            StateTransferScope::NanoContainer("AGENT-07".to_string()),
        ] {
            let guard = OwnerRegistry::global().issue(scope).unwrap();
            let conn = store.begin_fenced_write(&guard).unwrap();
            let count: i64 = conn
                .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 1);
        }
    }

    /// V19 TOCTOU: a guard that is no longer current when SQLite reaches COMMIT is
    /// rejected and the explicit transaction is rolled back.
    #[test]
    fn commit_rechecks_owner_term_and_rolls_back_stale_sqlite_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-stale-fenced.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let conn = store.conn.lock().unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let stale = super::FencedSqliteWrite {
            conn,
            guard: OwnerWriteGuard::for_test(
                StateTransferScope::World,
                OwnerRegistry::global().this_node(),
                0,
            ),
            committed: false,
        };
        stale
            .execute(
                "INSERT INTO projection_offsets \
                 (projection_name, last_event_id, updated_at) VALUES ('stale', 1, 1)",
                [],
            )
            .unwrap();
        assert!(stale.commit().is_err());

        assert_eq!(store.get_offset("stale").unwrap(), None);
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

    #[test]
    fn test_append_with_outbox_batch_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-batch-empty.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let inserted = store
            .append_with_outbox_batch(std::iter::empty::<(&DomainEvent, &str)>())
            .unwrap();
        assert_eq!(inserted, 0);

        let conn = store.conn();
        let event_count: i64 = conn
            .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
            .unwrap();
        let outbox_count: i64 = conn
            .query_row("SELECT count(*) FROM outbox", [], |row| row.get(0))
            .unwrap();
        assert_eq!(event_count, 0);
        assert_eq!(outbox_count, 0);
    }

    #[test]
    fn test_append_with_outbox_batch_preserves_order_and_outbox() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-batch-order.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let event_a = test_event("agent_action_received", "AGENT-01");
        let event_b = test_event("transit_started", "AGENT-02");
        let event_c = test_event("transit_completed", "AGENT-03");
        let entries = [
            (&event_a, "sentinel/events/agent_action_received/AGENT-01"),
            (&event_b, "sentinel/events/transit_started/AGENT-02"),
            (&event_c, "sentinel/events/transit_completed/AGENT-03"),
        ];

        let inserted = store.append_with_outbox_batch(entries).unwrap();
        assert_eq!(inserted, 3);

        let events = store.get_events_since(0, 10).unwrap();
        let event_ids: Vec<&str> = events.iter().map(|event| event.event_id.as_str()).collect();
        assert_eq!(
            event_ids,
            vec![
                event_a.event_id.as_str(),
                event_b.event_id.as_str(),
                event_c.event_id.as_str()
            ]
        );

        let outbox = store.poll_outbox(10).unwrap();
        let outbox_event_ids: Vec<&str> =
            outbox.iter().map(|entry| entry.event_id.as_str()).collect();
        assert_eq!(outbox_event_ids, event_ids);
        assert_eq!(
            outbox[0].topic,
            "sentinel/events/agent_action_received/AGENT-01"
        );
        assert_eq!(outbox[1].topic, "sentinel/events/transit_started/AGENT-02");
        assert_eq!(
            outbox[2].topic,
            "sentinel/events/transit_completed/AGENT-03"
        );
    }

    #[test]
    fn test_append_with_outbox_batch_idempotency() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-batch-idempotency.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let mut event = test_event("agent_action_received", "AGENT-01");
        event.operation_id = "op-batch-fixed".to_string();
        let mut duplicate = test_event("agent_action_received", "AGENT-02");
        duplicate.operation_id = "op-batch-fixed".to_string();

        let inserted = store
            .append_with_outbox_batch([
                (&event, "sentinel/events/agent_action_received/AGENT-01"),
                (&duplicate, "sentinel/events/agent_action_received/AGENT-02"),
            ])
            .unwrap();
        assert_eq!(inserted, 1);

        let events = store.get_events_since(0, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_id, event.event_id);

        let outbox = store.poll_outbox(10).unwrap();
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0].event_id, event.event_id);
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
    fn test_outbox_event_id_index_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-outbox-index.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let conn = store.conn();
        let exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_events_event_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1);

        let exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_outbox_event_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1);

        let mut stmt = conn
            .prepare(
                "EXPLAIN QUERY PLAN
                 DELETE FROM outbox
                 WHERE event_id IN (
                     SELECT event_id FROM events WHERE id < 100 ORDER BY id LIMIT 10
                 )",
            )
            .unwrap();
        let details = stmt
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");
        assert!(
            details.contains("idx_outbox_event_id"),
            "outbox delete should use idx_outbox_event_id, plan:\n{details}"
        );
    }

    #[test]
    fn test_open_migrates_legacy_outbox_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-legacy-outbox.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE events (
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
                );
                CREATE TABLE outbox (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    event_id TEXT NOT NULL REFERENCES events(event_id),
                    topic TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending',
                    created_at INTEGER NOT NULL,
                    published_at INTEGER
                );",
            )
            .unwrap();
        }

        let store = EventStore::open(path.to_str().unwrap()).unwrap();
        let conn = store.conn();
        assert!(EventStore::table_has_column(&conn, "outbox", "retry_count").unwrap());
        assert!(EventStore::table_has_column(&conn, "outbox", "last_error").unwrap());
    }

    #[test]
    fn test_prune_batch_deletes_outbox_for_all_statuses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-prune-outbox.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let events = [
            test_event("agent_action_received", "AGENT-01"),
            test_event("agent_action_received", "AGENT-02"),
            test_event("agent_action_received", "AGENT-03"),
            test_event("agent_action_received", "AGENT-04"),
        ];
        for event in &events {
            store
                .append_with_outbox(event, "sentinel/events/agent_action_received/test")
                .unwrap();
        }

        {
            let conn = store.conn();
            conn.execute(
                "UPDATE outbox SET status = 'published' WHERE event_id = ?1",
                params![events[0].event_id],
            )
            .unwrap();
            conn.execute(
                "UPDATE outbox SET status = 'failed' WHERE event_id = ?1",
                params![events[1].event_id],
            )
            .unwrap();
        }

        let cutoff: i64 = {
            let conn = store.conn();
            conn.query_row(
                "SELECT id FROM events WHERE event_id = ?1",
                params![events[3].event_id],
                |row| row.get(0),
            )
            .unwrap()
        };

        let deleted = store.prune_batch(cutoff, 10).unwrap();
        assert_eq!(deleted, 3);

        let conn = store.conn();
        let event_count: i64 = conn
            .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
            .unwrap();
        let outbox_count: i64 = conn
            .query_row("SELECT count(*) FROM outbox", [], |row| row.get(0))
            .unwrap();
        let remaining_event_id: String = conn
            .query_row("SELECT event_id FROM outbox", [], |row| row.get(0))
            .unwrap();

        assert_eq!(event_count, 1);
        assert_eq!(outbox_count, 1);
        assert_eq!(remaining_event_id, events[3].event_id);
    }

    #[test]
    fn test_prune_batch_works_with_foreign_keys_on() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-prune-fk.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        {
            let conn = store.conn();
            conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
            let fk_enabled: i64 = conn
                .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
                .unwrap();
            assert_eq!(fk_enabled, 1);
        }

        let event = test_event("agent_action_received", "AGENT-01");
        store
            .append_with_outbox(&event, "sentinel/events/agent_action_received/AGENT-01")
            .unwrap();

        let deleted = store.prune_batch(i64::MAX, 10).unwrap();
        assert_eq!(deleted, 1);

        let conn = store.conn();
        let orphan_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM outbox o
                 WHERE NOT EXISTS (SELECT 1 FROM events e WHERE e.event_id = o.event_id)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphan_count, 0);
    }

    #[test]
    fn test_delete_orphan_outbox_removes_only_orphans() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-orphan-cleanup.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let event = test_event("agent_action_received", "AGENT-01");
        store
            .append_with_outbox(&event, "sentinel/events/agent_action_received/AGENT-01")
            .unwrap();

        {
            let conn = store.conn();
            conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
            conn.execute(
                "INSERT INTO outbox (event_id, topic, payload, status, created_at)
                 VALUES ('missing-event', 'sentinel/events/test/missing', '{}', 'pending', 1)",
                [],
            )
            .unwrap();
        }

        let deleted = store.delete_orphan_outbox().unwrap();
        assert_eq!(deleted, 1);

        let conn = store.conn();
        let outbox_count: i64 = conn
            .query_row("SELECT count(*) FROM outbox", [], |row| row.get(0))
            .unwrap();
        let remaining_event_id: String = conn
            .query_row("SELECT event_id FROM outbox", [], |row| row.get(0))
            .unwrap();
        assert_eq!(outbox_count, 1);
        assert_eq!(remaining_event_id, event.event_id);
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
    fn test_range_queries_for_bounded_replay() {
        // #491 (TM-3): get_events_range/count/max_event_id_at_tick/get_event_tick.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-range.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        // 5 Events, ids 1..5, ticks [1,1,2,2,3].
        for tick in [1u64, 1, 2, 2, 3] {
            let e = DomainEvent::new("agent_action_received", "AGENT-01", "{}", "corr-1", tick);
            store.append_event(&e).unwrap();
        }

        // get_events_range: (from_exclusive, to_inclusive], stabile id-Reihenfolge.
        assert_eq!(
            store.get_events_range(0, 5).unwrap().len(),
            5,
            "voller Bereich"
        );
        let mid = store.get_events_range(2, 4).unwrap();
        assert_eq!(mid.len(), 2, "ids 3,4");
        assert!(mid.iter().all(|e| e.tick == 2));
        assert!(
            store.get_events_range(5, 5).unwrap().is_empty(),
            "leerer Bereich"
        );
        assert!(
            store.get_events_range(2, 2).unwrap().is_empty(),
            "from==to leer"
        );

        // count_events_in_range
        assert_eq!(store.count_events_in_range(0, 5).unwrap(), 5);
        assert_eq!(store.count_events_in_range(2, 4).unwrap(), 2);
        assert_eq!(store.count_events_in_range(5, 5).unwrap(), 0);

        // max_event_id_at_tick: groesste id mit tick <= target.
        assert_eq!(store.max_event_id_at_tick(2).unwrap(), Some(4));
        assert_eq!(store.max_event_id_at_tick(1).unwrap(), Some(2));
        assert_eq!(store.max_event_id_at_tick(3).unwrap(), Some(5));
        assert_eq!(
            store.max_event_id_at_tick(0).unwrap(),
            None,
            "vor erstem Event"
        );

        // get_event_tick
        assert_eq!(store.get_event_tick(3).unwrap(), Some(2));
        assert_eq!(store.get_event_tick(5).unwrap(), Some(3));
        assert_eq!(store.get_event_tick(99).unwrap(), None, "unbekannte id");
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

    #[test]
    fn test_retain_latest_snapshots_keeps_latest_per_aggregate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-snap-retention.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        store
            .save_snapshot("AGENT-01", "bio_state", r#"{"hunger":50}"#, 5)
            .unwrap();
        store
            .save_snapshot("AGENT-01", "bio_state", r#"{"hunger":80}"#, 10)
            .unwrap();
        store
            .save_snapshot("AGENT-02", "bio_state", r#"{"hunger":30}"#, 7)
            .unwrap();

        let deleted = store.retain_latest_snapshots().unwrap();
        assert_eq!(deleted, 0);

        let conn = store.conn();
        let snapshot_count: i64 = conn
            .query_row("SELECT count(*) FROM snapshots", [], |row| row.get(0))
            .unwrap();
        assert_eq!(snapshot_count, 2);
        let agent_01_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM snapshots WHERE aggregate_id = 'AGENT-01'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(agent_01_count, 1);
        drop(conn);

        let snap = store.get_latest_snapshot("AGENT-01").unwrap().unwrap();
        assert_eq!(snap.version, 2);
        assert_eq!(snap.payload, r#"{"hunger":80}"#);
        let snap = store.get_latest_snapshot("AGENT-02").unwrap().unwrap();
        assert_eq!(snap.version, 1);
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

    /// #252: db_path() gibt den korrekten Pfad zurueck.
    #[test]
    fn test_db_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-path.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();
        assert_eq!(store.db_path(), path);
    }

    /// #264: Immutable Snapshots — DELETE auf jungen Snapshot wird blockiert.
    #[test]
    fn test_snapshot_delete_blocked_within_7_days() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-immutable.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        // Snapshot speichern (created_at = jetzt → innerhalb 7 Tage)
        store
            .save_world_snapshot("snap-1", "hourly", 100, 8.0, 50, b"test-payload")
            .unwrap();

        // DELETE muss fehlschlagen (Trigger blockiert)
        let result = store.delete_world_snapshot("snap-1");
        assert!(
            result.is_err(),
            "DELETE auf jungen Snapshot sollte vom Trigger blockiert werden"
        );
    }

    #[test]
    fn test_snapshot_immutable_trigger_installed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-immutable-trigger.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let conn = store.conn();
        let trigger_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master
                 WHERE type = 'trigger' AND name = 'protect_recent_snapshots'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(trigger_sql.contains("BEFORE DELETE ON world_snapshots"));
        // #250: SSOT-Linkage — der Trigger MUSS exakt die geteilte Konstante einbetten, damit
        // Trigger-Block-Schwelle und Daemon-Retention-Skip (sentinel-daemon nutzt
        // IMMUTABLE_SNAPSHOT_MS) nicht auseinanderdriften koennen. Value-Lock haelt die 7 Tage fest.
        assert_eq!(IMMUTABLE_SNAPSHOT_MS, 7 * 86400 * 1000, "7 Tage in ms");
        assert_eq!(IMMUTABLE_SNAPSHOT_MS, 604_800_000);
        assert!(
            trigger_sql.contains(&IMMUTABLE_SNAPSHOT_MS.to_string()),
            "Trigger-SQL muss die geteilte Konstante {IMMUTABLE_SNAPSHOT_MS} einbetten: {trigger_sql}"
        );
    }

    /// #250/AC-7: Promotion (UPDATE tier) darf den Restore-Anker NICHT veraendern — Payload-Blob,
    /// `last_event_id` und `tick` bleiben byte-/wert-identisch, nur der Tier aendert sich. Damit ist
    /// jeder Restore aus einem promoteten Snapshot identisch zu einem Restore vor der Promotion (die
    /// End-to-End-Bio/Position/Tick-Restore-Korrektheit selbst liegt im #491/#529-Replay-Pfad).
    #[test]
    fn test_promote_world_snapshot_preserves_restore_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-promote-anchor.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        let payload = b"restore-anchor-blob-\x00\x01\x02".as_slice();
        store
            .save_world_snapshot("snap-x", "hourly", 4242, 13.5, 999, payload)
            .unwrap();
        let before = store.load_world_snapshot("snap-x").unwrap().unwrap();
        let meta_before = store
            .list_world_snapshots()
            .unwrap()
            .into_iter()
            .find(|s| s.id == "snap-x")
            .unwrap();

        // Promotion = reiner Tier-UPDATE (trigger-immun, keine neue Zeile).
        assert!(store.promote_world_snapshot("snap-x", "daily").unwrap());

        let after = store.load_world_snapshot("snap-x").unwrap().unwrap();
        let meta_after = store
            .list_world_snapshots()
            .unwrap()
            .into_iter()
            .find(|s| s.id == "snap-x")
            .unwrap();

        assert_eq!(
            after, before,
            "Payload-Blob unveraendert (Restore-Anker intakt)"
        );
        assert_eq!(after, payload, "Blob == Original");
        assert_eq!(
            meta_after.last_event_id, meta_before.last_event_id,
            "last_event_id stabil"
        );
        assert_eq!(meta_after.tick, meta_before.tick, "tick stabil");
        assert_eq!(
            meta_after.tier,
            sentinel_common::SnapshotTier::Daily,
            "nur der Tier aendert sich"
        );
        assert_eq!(meta_before.tier, sentinel_common::SnapshotTier::Hourly);
        let count: i64 = store
            .conn()
            .query_row("SELECT count(*) FROM world_snapshots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 1,
            "Promotion erzeugt KEINE zweite Zeile (1:n, kein Kopieren)"
        );
    }

    /// #264: Immutable Snapshots — DELETE auf alten Snapshot funktioniert.
    #[test]
    fn test_snapshot_delete_allowed_after_7_days() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-immutable-old.db");
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        // Snapshot mit altem created_at direkt einfuegen (> 7 Tage alt)
        let old_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            - (8 * 86400 * 1000); // 8 Tage alt

        let conn = store.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO world_snapshots (id, tier, tick, sim_hour, last_event_id, payload_size, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params!["snap-old", "hourly", 50, 4.0, 25, 4, b"old", old_ts],
        )
        .unwrap();

        // DELETE auf alten Snapshot muss funktionieren
        let deleted = conn
            .execute("DELETE FROM world_snapshots WHERE id = 'snap-old'", [])
            .unwrap();
        assert_eq!(deleted, 1, "DELETE auf alten Snapshot sollte funktionieren");
    }

    // ──────────────────────────────────────────────
    // #493 Dead Branch GC
    // ──────────────────────────────────────────────

    /// Seeds `n` events (ids 1..=n), each with a pending outbox entry, and returns `(events.id,
    /// event_id-uuid)` in id order so a test can map ids ↔ uuids for content assertions.
    fn seed_events_with_outbox(store: &EventStore, n: i64) -> Vec<(i64, String)> {
        let mut out = Vec::new();
        for i in 1..=n {
            let e = DomainEvent::new(
                "agent_action_received",
                "AGENT-01",
                "{}",
                "corr-1",
                i as u64,
            );
            let id = store.append_with_outbox(&e, "sentinel.events").unwrap();
            out.push((id, e.event_id.clone()));
        }
        out
    }

    /// #493 AC-1: a restore bumps the persistent generation and records the discarded id-interval;
    /// a no-op restore (anchor == head, or anchor > head) records nothing.
    #[test]
    fn dead_branch_marking_sets_generation_and_range() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("d.db").to_str().unwrap()).unwrap();
        assert_eq!(store.get_restore_generation().unwrap(), 0);
        assert!(store.dead_ranges().unwrap().is_empty());

        assert_eq!(store.increment_restore_generation().unwrap(), 1);
        store.push_dead_range(5, 10).unwrap();
        assert_eq!(store.get_restore_generation().unwrap(), 1);
        assert_eq!(store.dead_ranges().unwrap(), vec![(5, 10)]);

        assert_eq!(store.increment_restore_generation().unwrap(), 2);
        store.push_dead_range(12, 20).unwrap();
        assert_eq!(store.dead_ranges().unwrap(), vec![(5, 10), (12, 20)]);

        // nothing discarded → no new interval
        store.push_dead_range(20, 20).unwrap();
        store.push_dead_range(25, 20).unwrap();
        assert_eq!(store.dead_ranges().unwrap(), vec![(5, 10), (12, 20)]);
    }

    /// #493 AC-2 / requirement 1: EVERY event read path applies the shared dead-range guard. If a new
    /// read method is added without the guard (or one is dropped), this test goes red.
    #[test]
    fn every_read_path_excludes_dead_interval() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("d.db").to_str().unwrap()).unwrap();
        let seeded = seed_events_with_outbox(&store, 10); // ids 1..=10

        // discard ids 5,6,7 via interval (4, 7]
        store.push_dead_range(4, 7).unwrap();
        let dead_uuids: std::collections::HashSet<String> = seeded
            .iter()
            .filter(|(id, _)| (5..=7).contains(id))
            .map(|(_, u)| u.clone())
            .collect();
        let assert_clean = |uuids: Vec<String>, ctx: &str| {
            assert_eq!(uuids.len(), 7, "{ctx}: 3 dead events excluded");
            for u in &uuids {
                assert!(!dead_uuids.contains(u), "{ctx} leaked a dead event");
            }
        };

        assert_clean(
            store
                .get_events_since(0, 100)
                .unwrap()
                .into_iter()
                .map(|e| e.event_id)
                .collect(),
            "get_events_since",
        );
        assert_clean(
            store
                .get_events_since_with_id(0, 100)
                .unwrap()
                .into_iter()
                .map(|(_, e)| e.event_id)
                .collect(),
            "get_events_since_with_id",
        );
        assert_clean(
            store
                .get_events_by_aggregate("AGENT-01", 100)
                .unwrap()
                .into_iter()
                .map(|e| e.event_id)
                .collect(),
            "get_events_by_aggregate",
        );
        assert_clean(
            store
                .get_events_by_correlation("corr-1", 100)
                .unwrap()
                .into_iter()
                .map(|e| e.event_id)
                .collect(),
            "get_events_by_correlation",
        );
        assert_clean(
            store
                .get_all_events()
                .unwrap()
                .into_iter()
                .map(|e| e.event_id)
                .collect(),
            "get_all_events",
        );
        assert_clean(
            store
                .poll_outbox(100)
                .unwrap()
                .into_iter()
                .map(|o| o.event_id)
                .collect(),
            "poll_outbox",
        );

        // range-based reads: window (2, 9] = 3,4,5,6,7,8,9 minus dead 5,6,7 = 3,4,8,9
        let range: Vec<String> = store
            .get_events_range(2, 9)
            .unwrap()
            .into_iter()
            .map(|e| e.event_id)
            .collect();
        assert_eq!(range.len(), 4, "get_events_range excludes dead in window");
        for u in &range {
            assert!(
                !dead_uuids.contains(u),
                "get_events_range leaked a dead event"
            );
        }
        assert_eq!(
            store.count_events_in_range(2, 9).unwrap(),
            4,
            "count matches range"
        );
        assert_eq!(
            store.count_events_in_range(0, 10).unwrap(),
            7,
            "count excludes all dead"
        );
    }

    /// #493 AC-5 (couples #491): a bounded-replay window that crosses a dead-interval boundary feeds
    /// ONLY the live timeline into the replay (stable id order, dead excluded) and the reported count
    /// equals the replayed length — a discarded future can never replay back into the restored world.
    #[test]
    fn replay_input_excludes_dead_branch_at_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("d.db").to_str().unwrap()).unwrap();
        let seeded = seed_events_with_outbox(&store, 12);

        // restore to anchor id=6 → discard old future (6, 9] = ids 7,8,9; ids 10,11,12 are live.
        store.increment_restore_generation().unwrap();
        store.push_dead_range(6, 9).unwrap();

        // replay window (5, 11] crosses the boundary: 6(live),7,8,9(dead),10,11(live).
        let replay = store.get_events_range(5, 11).unwrap();
        let got: Vec<&str> = replay.iter().map(|e| e.event_id.as_str()).collect();
        let expected: Vec<&str> = seeded
            .iter()
            .filter(|(id, _)| [6i64, 10, 11].contains(id))
            .map(|(_, u)| u.as_str())
            .collect();
        assert_eq!(got, expected, "stable id order, dead excluded");
        assert_eq!(
            store.count_events_in_range(5, 11).unwrap() as usize,
            replay.len(),
            "STRICT/CORE: reported replay count equals replayed length"
        );
        for (id, uuid) in &seeded {
            if (7..=9).contains(id) {
                assert!(
                    !replay.iter().any(|e| &e.event_id == uuid),
                    "dead event id={id} replayed into restored world"
                );
            }
        }
    }

    /// #493 AC-3 / requirement 2: the pruner deletes dead-interval events even ABOVE the retention
    /// cutoff and clears the spent `dead_ranges` entry (the list stays bounded).
    #[test]
    fn pruner_removes_dead_interval_and_clears_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("d.db").to_str().unwrap()).unwrap();
        seed_events_with_outbox(&store, 10); // ids 1..=10

        // discard ids 6,7,8 — ABOVE the retention cutoff we use below.
        store.push_dead_range(5, 8).unwrap();

        // prune with cutoff=3 (retention removes ids < 3: 1,2). The dead interval (5,8] sits above it
        // and must STILL be removed.
        let mut total = 0u64;
        loop {
            let n = store.prune_batch(3, 500).unwrap();
            total += n;
            if n == 0 {
                break;
            }
        }
        assert_eq!(total, 5, "ids 1,2 (cutoff) + 6,7,8 (dead) removed");

        let conn = store.conn();
        // raw count (bypasses the read guard) proves the dead events are physically gone (AC-3 verify).
        let dead_remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE id > 5 AND id <= 8",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dead_remaining, 0, "dead events physically deleted");
        let total_remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total_remaining, 5, "remaining: ids 3,4,5,9,10");
        // the dead-event outbox entries are gone too (no leak).
        let dead_outbox: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM outbox WHERE event_id NOT IN (SELECT event_id FROM events)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dead_outbox, 0, "no orphaned dead outbox entries");
        drop(conn);

        assert!(
            store.dead_ranges().unwrap().is_empty(),
            "spent dead_ranges entry removed (list bounded)"
        );
    }

    /// #493: a discarded future's stuck-pending outbox entries must NOT permanently block pruning
    /// (`can_prune` counts only LIVE pending entries).
    #[test]
    fn can_prune_ignores_dead_pending_outbox() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("d.db").to_str().unwrap()).unwrap();
        seed_events_with_outbox(&store, 6); // ids 1..=6, all pending
                                            // advance the projection past head so the offset check passes
        store.force_reset_offset("p", 6).unwrap();

        // all 6 pending → blocked
        assert!(
            !store.can_prune(4).unwrap(),
            "live pending entries block pruning"
        );

        // mark the whole tail dead → those pending entries no longer count
        store.push_dead_range(0, 6).unwrap();
        assert!(
            store.can_prune(4).unwrap(),
            "dead pending entries do not block pruning"
        );
    }

    /// #493 AC-4: the guard reads `dead_ranges` fresh per query; concurrent readers under contention
    /// never observe a discarded future, even while it is being marked.
    #[test]
    fn guard_reads_dead_ranges_fresh_under_contention() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(EventStore::open(dir.path().join("d.db").to_str().unwrap()).unwrap());
        seed_events_with_outbox(&store, 6); // ids 1..=6

        assert_eq!(
            store.get_events_since(0, 100).unwrap().len(),
            6,
            "all visible before"
        );

        // mark (3, 5] dead from another thread; a fresh read reflects it immediately.
        let s2 = Arc::clone(&store);
        std::thread::spawn(move || s2.push_dead_range(3, 5).unwrap())
            .join()
            .unwrap();
        assert_eq!(
            store.get_events_since(0, 100).unwrap().len(),
            4,
            "fresh dead_ranges applied: ids 4,5 excluded"
        );

        let mut handles = Vec::new();
        for _ in 0..8 {
            let s = Arc::clone(&store);
            handles.push(std::thread::spawn(move || {
                for _ in 0..50 {
                    assert_eq!(s.get_events_range(0, 6).unwrap().len(), 4);
                    assert_eq!(s.get_all_events().unwrap().len(), 4);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
    }
}
