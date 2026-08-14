//! Read Model Store fuer CQRS-Projektionen.
//!
//! Eigene SQLite-Datenbank (projection.db) mit drei materialisierten Views:
//! `agent_live_view`, `room_live_view`, `kpi_1m`.
//!
//! Separate DB vom EventStore — kein shared-transaction moeglich.
//! Crash-Safety: Views committen VOR Offset-Update. Idempotente Handler
//! tolerieren Re-Processing bei Restart.

use anyhow::Context;
use rusqlite::{params, Connection, OpenFlags};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, info};

// ── SQL Schemas ──────────────────────────────────

const CREATE_AGENT_LIVE_VIEW: &str = "
CREATE TABLE IF NOT EXISTS agent_live_view (
    agent_id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    role TEXT NOT NULL,
    shift_set INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    current_room TEXT,
    in_transit INTEGER NOT NULL DEFAULT 0,
    transit_target TEXT,
    last_action TEXT,
    last_action_tick INTEGER,
    hunger REAL NOT NULL DEFAULT 0.0,
    energy REAL NOT NULL DEFAULT 1.0,
    stress REAL NOT NULL DEFAULT 0.0,
    bladder REAL NOT NULL DEFAULT 0.0,
    social_need REAL NOT NULL DEFAULT 0.0,
    caffeine_mg REAL NOT NULL DEFAULT 0.0,
    mood TEXT,
    last_event_id INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
)";

const MIGRATE_AGENT_BIO_COLUMNS: &str = "
ALTER TABLE agent_live_view ADD COLUMN hunger REAL NOT NULL DEFAULT 0.0;
ALTER TABLE agent_live_view ADD COLUMN energy REAL NOT NULL DEFAULT 1.0;
ALTER TABLE agent_live_view ADD COLUMN stress REAL NOT NULL DEFAULT 0.0;
ALTER TABLE agent_live_view ADD COLUMN bladder REAL NOT NULL DEFAULT 0.0;
ALTER TABLE agent_live_view ADD COLUMN social_need REAL NOT NULL DEFAULT 0.0;
ALTER TABLE agent_live_view ADD COLUMN caffeine_mg REAL NOT NULL DEFAULT 0.0;
ALTER TABLE agent_live_view ADD COLUMN mood TEXT;
";

const CREATE_ROOM_LIVE_VIEW: &str = "
CREATE TABLE IF NOT EXISTS room_live_view (
    room_id TEXT PRIMARY KEY,
    occupant_count INTEGER NOT NULL DEFAULT 0,
    transit_count INTEGER NOT NULL DEFAULT 0,
    active_chaos TEXT,
    last_event_tick INTEGER,
    last_event_id INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
)";

const MIGRATE_ROOM_PHYSICS_COLUMNS: &str = "
ALTER TABLE room_live_view ADD COLUMN temperature REAL;
ALTER TABLE room_live_view ADD COLUMN co2_ppm REAL;
ALTER TABLE room_live_view ADD COLUMN noise_db REAL;
";

const MIGRATE_ROOM_SMELLS_COLUMN: &str = "
ALTER TABLE room_live_view ADD COLUMN active_smells TEXT;
";

const CREATE_KPI_1M: &str = "
CREATE TABLE IF NOT EXISTS kpi_1m (
    bucket_start INTEGER PRIMARY KEY,
    active_agents INTEGER NOT NULL DEFAULT 0,
    total_actions INTEGER NOT NULL DEFAULT 0,
    total_transits INTEGER NOT NULL DEFAULT 0,
    chaos_events INTEGER NOT NULL DEFAULT 0,
    tick_count INTEGER NOT NULL DEFAULT 0,
    shift_changes INTEGER NOT NULL DEFAULT 0,
    nightrun_events INTEGER NOT NULL DEFAULT 0,
    last_event_id INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
)";

// #427: per-agent / per-tier / per-minute cost+token read-models. Aggregated from
// AgentLlmUsage events by CostHandler; the dashboard reads these read-only (1:n — the
// cost info lives once as the event sequence, the projection is its materialized view).
const CREATE_COST_BY_AGENT: &str = "
CREATE TABLE IF NOT EXISTS cost_by_agent (
    agent_id TEXT PRIMARY KEY,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read INTEGER NOT NULL DEFAULT 0,
    cache_creation INTEGER NOT NULL DEFAULT 0,
    cost_usd REAL NOT NULL DEFAULT 0.0,
    call_count INTEGER NOT NULL DEFAULT 0,
    last_event_id INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
)";

const CREATE_COST_BY_TIER: &str = "
CREATE TABLE IF NOT EXISTS cost_by_tier (
    tier TEXT PRIMARY KEY,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read INTEGER NOT NULL DEFAULT 0,
    cache_creation INTEGER NOT NULL DEFAULT 0,
    cost_usd REAL NOT NULL DEFAULT 0.0,
    call_count INTEGER NOT NULL DEFAULT 0,
    last_event_id INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
)";

// #395: additive organization-hierarchy aggregation. This table is maintained
// by an independent event-store offset and never changes cost_by_tier semantics.
const CREATE_COST_BY_HIERARCHY_TIER: &str = "
CREATE TABLE IF NOT EXISTS cost_by_hierarchy_tier (
    hierarchy_tier TEXT PRIMARY KEY,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read INTEGER NOT NULL DEFAULT 0,
    cache_creation INTEGER NOT NULL DEFAULT 0,
    cost_usd REAL NOT NULL DEFAULT 0.0,
    call_count INTEGER NOT NULL DEFAULT 0,
    last_event_id INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
)";

const CREATE_COST_HIERARCHY_PROJECTION_META: &str = "
CREATE TABLE IF NOT EXISTS cost_hierarchy_projection_meta (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    first_v2_event_id INTEGER,
    last_usage_event_id INTEGER NOT NULL DEFAULT 0,
    last_hierarchy_event_id INTEGER NOT NULL DEFAULT 0,
    unattributed_v1_usage_events INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0
);
INSERT OR IGNORE INTO cost_hierarchy_projection_meta (id) VALUES (1)";

const CREATE_COST_TIMESERIES: &str = "
CREATE TABLE IF NOT EXISTS cost_timeseries (
    bucket_start INTEGER PRIMARY KEY,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read INTEGER NOT NULL DEFAULT 0,
    cache_creation INTEGER NOT NULL DEFAULT 0,
    cost_usd REAL NOT NULL DEFAULT 0.0,
    call_count INTEGER NOT NULL DEFAULT 0,
    last_event_id INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
)";

const CREATE_TASK_KANBAN: &str = "
CREATE TABLE IF NOT EXISTS task_kanban (
    task_id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    assigned_to INTEGER NOT NULL,
    assigned_by INTEGER,
    parent_task INTEGER,
    status TEXT NOT NULL DEFAULT 'pending',
    result TEXT,
    last_event_id INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
)";

const CREATE_WORKBENCH_INVOCATIONS: &str = "
CREATE TABLE IF NOT EXISTS workbench_invocations (
    invocation_id TEXT PRIMARY KEY,
    agent_id INTEGER NOT NULL,
    project_id TEXT NOT NULL,
    work_item_id TEXT NOT NULL,
    tool_class TEXT NOT NULL,
    runtime_key TEXT NOT NULL,
    state TEXT NOT NULL,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    cpu_time_ms INTEGER NOT NULL DEFAULT 0,
    peak_memory_bytes INTEGER NOT NULL DEFAULT 0,
    peak_process_count INTEGER NOT NULL DEFAULT 0,
    bytes_read INTEGER NOT NULL DEFAULT 0,
    bytes_written INTEGER NOT NULL DEFAULT 0,
    artifact_bytes INTEGER NOT NULL DEFAULT 0,
    artifact_ids TEXT NOT NULL DEFAULT '[]',
    error_code TEXT,
    last_event_id INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
)";

const CREATE_PROJECTION_WATERMARKS: &str = "
CREATE TABLE IF NOT EXISTS projection_watermarks (
    projection_name TEXT PRIMARY KEY,
    last_event_id INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
)";

const CREATE_WATERMARK_INDEXES: &str = "
CREATE INDEX IF NOT EXISTS idx_agent_live_view_last_event_id ON agent_live_view(last_event_id);
CREATE INDEX IF NOT EXISTS idx_room_live_view_last_event_id ON room_live_view(last_event_id);
CREATE INDEX IF NOT EXISTS idx_kpi_1m_last_event_id ON kpi_1m(last_event_id);
CREATE INDEX IF NOT EXISTS idx_cost_by_agent_last_event_id ON cost_by_agent(last_event_id);
CREATE INDEX IF NOT EXISTS idx_cost_by_hierarchy_tier_last_event_id ON cost_by_hierarchy_tier(last_event_id);
CREATE INDEX IF NOT EXISTS idx_workbench_invocations_last_event_id ON workbench_invocations(last_event_id);
CREATE INDEX IF NOT EXISTS idx_workbench_invocations_agent_id ON workbench_invocations(agent_id);
";

const PROJECTION_NAME: &str = "sentinel-projection";

// ── ReadModelStore ───────────────────────────────

/// Store fuer materialisierte Read Models.
pub struct ReadModelStore {
    conn: Arc<Mutex<Connection>>,
}

impl ReadModelStore {
    /// Oeffnet oder erstellt die Read-Model-Datenbank.
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open projection DB: {path}"))?;
        conn.busy_timeout(Duration::from_secs(5))?;

        // WAL-Modus + Performance-Pragmas (wie EventStore)
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "mmap_size", 268_435_456i64)?;
        conn.pragma_update(None, "page_size", 8192)?;

        conn.execute_batch(CREATE_AGENT_LIVE_VIEW)?;
        conn.execute_batch(CREATE_ROOM_LIVE_VIEW)?;
        conn.execute_batch(CREATE_KPI_1M)?;
        conn.execute_batch(CREATE_COST_BY_AGENT)?;
        conn.execute_batch(CREATE_COST_BY_TIER)?;
        conn.execute_batch(CREATE_COST_BY_HIERARCHY_TIER)?;
        conn.execute_batch(CREATE_COST_HIERARCHY_PROJECTION_META)?;
        conn.execute_batch(CREATE_COST_TIMESERIES)?;
        conn.execute_batch(CREATE_TASK_KANBAN)?;
        conn.execute_batch(CREATE_WORKBENCH_INVOCATIONS)?;
        conn.execute_batch(CREATE_PROJECTION_WATERMARKS)?;
        conn.execute_batch(CREATE_WATERMARK_INDEXES)?;

        // Migration: Bio-Spalten hinzufuegen (idempotent, ignoriert "duplicate column" Fehler)
        for line in MIGRATE_AGENT_BIO_COLUMNS.lines() {
            let line = line.trim();
            if line.starts_with("ALTER") {
                let _ = conn.execute_batch(line);
            }
        }

        // Migration: Room-Physics-Spalten hinzufuegen (idempotent)
        for line in MIGRATE_ROOM_PHYSICS_COLUMNS.lines() {
            let line = line.trim();
            if line.starts_with("ALTER") {
                let _ = conn.execute_batch(line);
            }
        }

        // Migration: Room-Smells-Spalte hinzufuegen (idempotent)
        for line in MIGRATE_ROOM_SMELLS_COLUMN.lines() {
            let line = line.trim();
            if line.starts_with("ALTER") {
                let _ = conn.execute_batch(line);
            }
        }

        // Startup-Cleanup: Stale Transits zuruecksetzen.
        // Nach Daemon-Crash/-Restart kann die Projection in_transit=1 Zeilen enthalten,
        // fuer die kein TransitCompleted Event geschrieben wurde. Der ECS-State (SSOT)
        // hat nach Restore keine stale Transits — die Projection muss nachziehen.
        let stale_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_live_view WHERE in_transit = 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if stale_count > 0 {
            conn.execute(
                "UPDATE agent_live_view SET in_transit = 0, transit_target = NULL WHERE in_transit = 1",
                [],
            )?;
            info!(stale_count, "Stale Transits in Projection zurueckgesetzt");
        }

        bootstrap_projection_watermark(&conn)?;

        info!(path, "ReadModelStore opened");
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Oeffnet eine existierende Read-Model-Datenbank schreibgeschuetzt.
    ///
    /// Verwendet keinen Startup-Cleanup und keine Schema-Migrationen. Dieser
    /// Pfad ist fuer daemon-seitige Runtime-Health-Diagnostik bestimmt, damit
    /// kein zweiter Writer auf `projection.db` entsteht.
    pub fn open_readonly(path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("Failed to open projection DB readonly: {path}"))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        debug!(path, readonly = true, "ReadModelStore opened");
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Initialisiert Room-Rows fuer alle bekannten Raeume.
    /// INSERT OR IGNORE — existierende Rows werden nicht ueberschrieben.
    pub fn initialize_rooms(&self, room_ids: &[&str]) -> anyhow::Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let now_ms = now_ms();
        for room_id in room_ids {
            conn.execute(
                "INSERT OR IGNORE INTO room_live_view (room_id, occupant_count, transit_count, updated_at) VALUES (?1, 0, 0, ?2)",
                params![room_id, now_ms],
            )?;
        }
        info!(rooms = room_ids.len(), "Rooms initialized");
        Ok(())
    }

    /// Entfernt abgelaufene Smells aus room_live_view.
    ///
    /// Prueft alle Rooms mit active_smells und setzt auf NULL wenn
    /// `current_tick > smell.tick + smell.duration_ticks`.
    pub fn cleanup_expired_smells(&self, current_tick: u64) -> anyhow::Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;

        let mut stmt = conn.prepare(
            "SELECT room_id, active_smells FROM room_live_view WHERE active_smells IS NOT NULL",
        )?;
        let expired: Vec<String> = stmt
            .query_map([], |row| {
                let room_id: String = row.get(0)?;
                let json: String = row.get(1)?;
                Ok((room_id, json))
            })?
            .filter_map(|r| r.ok())
            .filter(|(_, json)| {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
                    let tick = v["tick"].as_u64().unwrap_or(0);
                    let duration = v["duration_ticks"].as_u64().unwrap_or(0);
                    current_tick > tick + duration
                } else {
                    true // Malformed JSON → cleanup
                }
            })
            .map(|(room_id, _)| room_id)
            .collect();

        if !expired.is_empty() {
            let mut update =
                conn.prepare("UPDATE room_live_view SET active_smells = NULL WHERE room_id = ?1")?;
            for room_id in &expired {
                update.execute(params![room_id])?;
            }
            tracing::debug!(count = expired.len(), "Expired smells cleaned up");
        }

        Ok(())
    }

    /// Loescht alle Daten aus allen Projection-Tabellen (fuer Rebuild).
    pub fn clear_all(&self) -> anyhow::Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        conn.execute_batch(
            "DELETE FROM agent_live_view;
             DELETE FROM room_live_view;
             DELETE FROM kpi_1m;
             DELETE FROM cost_by_agent;
             DELETE FROM cost_by_tier;
             DELETE FROM cost_by_hierarchy_tier;
             UPDATE cost_hierarchy_projection_meta SET
               first_v2_event_id = NULL,
               last_usage_event_id = 0,
               last_hierarchy_event_id = 0,
               unattributed_v1_usage_events = 0,
               updated_at = 0
             WHERE id = 1;
             DELETE FROM cost_timeseries;
             DELETE FROM workbench_invocations;
             DELETE FROM projection_watermarks;",
        )?;
        info!("All read model tables cleared");
        Ok(())
    }

    /// Recomputes occupant_count from agent_live_view (post-rebuild consistency).
    ///
    /// Delta-based counting drifts when the event stream has gaps (daemon restarts
    /// without despawn events). This sets occupant_count = COUNT of active agents
    /// currently assigned to each room — the ground truth.
    pub fn recompute_occupant_counts(&self) -> anyhow::Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        conn.execute_batch(
            "UPDATE room_live_view SET occupant_count = (
                SELECT COUNT(*) FROM agent_live_view
                WHERE agent_live_view.current_room = room_live_view.room_id
                  AND agent_live_view.status = 'active'
            )",
        )?;
        info!("Occupant counts recomputed from agent_live_view");
        Ok(())
    }

    /// Startet eine Transaktion fuer Batch-Verarbeitung.
    ///
    /// Der Caller erhaelt ein `ReadModelTransaction` das typed Methoden
    /// fuer Updates auf den drei Tabellen bietet.
    pub fn begin_transaction(&self) -> anyhow::Result<ReadModelTransaction<'_>> {
        let guard = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        Ok(ReadModelTransaction { guard })
    }

    // ── Query-Methoden (fuer Tests und Dashboard) ──

    /// Liest einen Agent aus der Live-View.
    pub fn get_agent(&self, agent_id: u16) -> anyhow::Result<Option<AgentView>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let result = conn.query_row(
            "SELECT agent_id, name, role, shift_set, status, current_room, in_transit, transit_target, last_action, last_action_tick, hunger, energy, stress, bladder, social_need, caffeine_mg, mood, last_event_id, updated_at FROM agent_live_view WHERE agent_id = ?1",
            params![agent_id],
            |row| {
                Ok(AgentView {
                    agent_id: row.get(0)?,
                    name: row.get(1)?,
                    role: row.get(2)?,
                    shift_set: row.get(3)?,
                    status: row.get(4)?,
                    current_room: row.get(5)?,
                    in_transit: row.get::<_, i32>(6)? != 0,
                    transit_target: row.get(7)?,
                    last_action: row.get(8)?,
                    last_action_tick: row.get(9)?,
                    hunger: row.get(10)?,
                    energy: row.get(11)?,
                    stress: row.get(12)?,
                    bladder: row.get(13)?,
                    social_need: row.get(14)?,
                    caffeine_mg: row.get(15)?,
                    mood: row.get(16)?,
                    last_event_id: row.get(17)?,
                    updated_at: row.get(18)?,
                })
            },
        );
        match result {
            Ok(view) => Ok(Some(view)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Liest einen Raum aus der Live-View.
    pub fn get_room(&self, room_id: &str) -> anyhow::Result<Option<RoomView>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let result = conn.query_row(
            "SELECT room_id, occupant_count, transit_count, active_chaos, active_smells, temperature, co2_ppm, noise_db, last_event_tick, last_event_id, updated_at FROM room_live_view WHERE room_id = ?1",
            params![room_id],
            |row| {
                Ok(RoomView {
                    room_id: row.get(0)?,
                    occupant_count: row.get(1)?,
                    transit_count: row.get(2)?,
                    active_chaos: row.get(3)?,
                    active_smells: row.get(4)?,
                    temperature: row.get(5)?,
                    co2_ppm: row.get(6)?,
                    noise_db: row.get(7)?,
                    last_event_tick: row.get(8)?,
                    last_event_id: row.get(9)?,
                    updated_at: row.get(10)?,
                })
            },
        );
        match result {
            Ok(view) => Ok(Some(view)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Zaehlt aktive Agents in der Live-View.
    pub fn active_agent_count(&self) -> anyhow::Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let count = conn.query_row(
            "SELECT count(*) FROM agent_live_view WHERE status = 'active'",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Listet aktive Agents in der Live-View.
    ///
    /// Dieser Read-Pfad wird vom Daemon fuer Runtime-Health genutzt. Er bleibt
    /// read-only und macht Projection-only Ghost-Agents explizit sichtbar.
    pub fn active_agents(&self) -> anyhow::Result<Vec<AgentView>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT agent_id, name, role, shift_set, status, current_room, in_transit,
                    transit_target, last_action, last_action_tick, hunger, energy,
                    stress, bladder, social_need, caffeine_mg, mood, last_event_id,
                    updated_at
             FROM agent_live_view
             WHERE status = 'active'
             ORDER BY agent_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(AgentView {
                agent_id: row.get(0)?,
                name: row.get(1)?,
                role: row.get(2)?,
                shift_set: row.get(3)?,
                status: row.get(4)?,
                current_room: row.get(5)?,
                in_transit: row.get::<_, i32>(6)? != 0,
                transit_target: row.get(7)?,
                last_action: row.get(8)?,
                last_action_tick: row.get(9)?,
                hunger: row.get(10)?,
                energy: row.get(11)?,
                stress: row.get(12)?,
                bladder: row.get(13)?,
                social_need: row.get(14)?,
                caffeine_mg: row.get(15)?,
                mood: row.get(16)?,
                last_event_id: row.get(17)?,
                updated_at: row.get(18)?,
            })
        })?;

        let mut agents = Vec::new();
        for row in rows {
            agents.push(row?);
        }
        Ok(agents)
    }

    pub fn get_workbench_invocation(
        &self,
        invocation_id: &str,
    ) -> anyhow::Result<Option<WorkbenchInvocationView>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let result = conn.query_row(
            "SELECT invocation_id, agent_id, project_id, work_item_id, tool_class,
                    runtime_key, state, duration_ms, cpu_time_ms, peak_memory_bytes,
                    peak_process_count, bytes_read, bytes_written, artifact_bytes,
                    artifact_ids, error_code, last_event_id
             FROM workbench_invocations WHERE invocation_id = ?1",
            params![invocation_id],
            |row| {
                let artifact_ids: String = row.get(14)?;
                Ok(WorkbenchInvocationView {
                    invocation_id: row.get(0)?,
                    agent_id: row.get(1)?,
                    project_id: row.get(2)?,
                    work_item_id: row.get(3)?,
                    tool_class: row.get(4)?,
                    runtime_key: row.get(5)?,
                    state: row.get(6)?,
                    duration_ms: row.get(7)?,
                    cpu_time_ms: row.get(8)?,
                    peak_memory_bytes: row.get(9)?,
                    peak_process_count: row.get(10)?,
                    bytes_read: row.get(11)?,
                    bytes_written: row.get(12)?,
                    artifact_bytes: row.get(13)?,
                    artifact_ids: serde_json::from_str(&artifact_ids).unwrap_or_default(),
                    error_code: row.get(15)?,
                    last_event_id: row.get(16)?,
                })
            },
        );
        match result {
            Ok(view) => Ok(Some(view)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

/// #427: eine Zeile aus einer der drei Cost-Read-Models. `key` ist die agent_id
/// ("AGENT-NN"), der Tier-Name oder der Minuten-Bucket-Start (als String).
#[derive(Debug, Clone)]
pub struct CostRow {
    pub key: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub cost_usd: f64,
    pub call_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HierarchyProjectionMeta {
    pub first_v2_event_id: Option<i64>,
    pub last_usage_event_id: i64,
    pub last_hierarchy_event_id: i64,
    pub unattributed_v1_usage_events: i64,
}

impl ReadModelStore {
    fn read_cost_table(&self, table: &str, key_col: &str) -> anyhow::Result<Vec<CostRow>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        let sql = format!(
            "SELECT CAST({key_col} AS TEXT), input_tokens, output_tokens, cache_read, \
             cache_creation, cost_usd, call_count FROM {table} ORDER BY {key_col}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(CostRow {
                key: row.get(0)?,
                input_tokens: row.get(1)?,
                output_tokens: row.get(2)?,
                cache_read: row.get(3)?,
                cache_creation: row.get(4)?,
                cost_usd: row.get(5)?,
                call_count: row.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Kosten/Tokens aggregiert pro Agent ("AGENT-NN").
    pub fn cost_by_agent(&self) -> anyhow::Result<Vec<CostRow>> {
        self.read_cost_table("cost_by_agent", "agent_id")
    }

    /// Kosten/Tokens aggregiert pro Model-Tier.
    pub fn cost_by_tier(&self) -> anyhow::Result<Vec<CostRow>> {
        self.read_cost_table("cost_by_tier", "tier")
    }

    /// Kosten/Tokens aggregiert pro explizitem Organisationstier. Legacy-v1
    /// usage events are coverage-only and never enter this table.
    pub fn cost_by_hierarchy_tier(&self) -> anyhow::Result<Vec<CostRow>> {
        self.read_cost_table("cost_by_hierarchy_tier", "hierarchy_tier")
    }

    pub fn hierarchy_projection_meta(&self) -> anyhow::Result<HierarchyProjectionMeta> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        conn.query_row(
            "SELECT first_v2_event_id,last_usage_event_id,last_hierarchy_event_id,\
             unattributed_v1_usage_events FROM cost_hierarchy_projection_meta WHERE id = 1",
            [],
            |row| {
                Ok(HierarchyProjectionMeta {
                    first_v2_event_id: row.get(0)?,
                    last_usage_event_id: row.get(1)?,
                    last_hierarchy_event_id: row.get(2)?,
                    unattributed_v1_usage_events: row.get(3)?,
                })
            },
        )
        .map_err(Into::into)
    }

    /// Kosten/Tokens als Minuten-Zeitreihe (aufsteigend nach Bucket-Start).
    pub fn cost_timeseries(&self) -> anyhow::Result<Vec<CostRow>> {
        self.read_cost_table("cost_timeseries", "bucket_start")
    }
}

// ── View-Structs ─────────────────────────────────

/// Projizierter Agent-Zustand.
#[derive(Debug, Clone)]
pub struct AgentView {
    pub agent_id: i64,
    pub name: String,
    pub role: String,
    pub shift_set: i64,
    pub status: String,
    pub current_room: Option<String>,
    pub in_transit: bool,
    pub transit_target: Option<String>,
    pub last_action: Option<String>,
    pub last_action_tick: Option<i64>,
    pub hunger: f64,
    pub energy: f64,
    pub stress: f64,
    pub bladder: f64,
    pub social_need: f64,
    pub caffeine_mg: f64,
    pub mood: Option<String>,
    pub last_event_id: i64,
    pub updated_at: i64,
}

/// Projizierter Raum-Zustand.
#[derive(Debug, Clone)]
pub struct RoomView {
    pub room_id: String,
    pub occupant_count: i64,
    pub transit_count: i64,
    pub active_chaos: Option<String>,
    pub active_smells: Option<String>,
    pub temperature: Option<f64>,
    pub co2_ppm: Option<f64>,
    pub noise_db: Option<f64>,
    pub last_event_tick: Option<i64>,
    pub last_event_id: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbenchInvocationView {
    pub invocation_id: String,
    pub agent_id: i64,
    pub project_id: String,
    pub work_item_id: String,
    pub tool_class: String,
    pub runtime_key: String,
    pub state: String,
    pub duration_ms: i64,
    pub cpu_time_ms: i64,
    pub peak_memory_bytes: i64,
    pub peak_process_count: i64,
    pub bytes_read: i64,
    pub bytes_written: i64,
    pub artifact_bytes: i64,
    pub artifact_ids: Vec<String>,
    pub error_code: Option<String>,
    pub last_event_id: i64,
}

// ── ReadModelTransaction ─────────────────────────

/// Transaktions-Wrapper fuer Batch-Updates auf den Read Models.
///
/// Haelt den Mutex-Lock auf die Connection. Der Caller ruft typed
/// Methoden auf und committed am Ende mit `commit()`.
pub struct ReadModelTransaction<'a> {
    guard: std::sync::MutexGuard<'a, Connection>,
}

impl<'a> ReadModelTransaction<'a> {
    /// Startet die SQLite-Transaktion.
    pub fn begin(&self) -> anyhow::Result<()> {
        self.guard.execute_batch("BEGIN")?;
        Ok(())
    }

    /// Committed die SQLite-Transaktion.
    pub fn commit(&self) -> anyhow::Result<()> {
        self.guard.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Rollback fuer fail-closed Batch-Fehler.
    pub fn rollback(&self) -> anyhow::Result<()> {
        self.guard.execute_batch("ROLLBACK")?;
        Ok(())
    }

    /// Globaler Dashboard-Watermark: ein Read aus dieser Tabelle ersetzt die
    /// frueheren per-view MAX-Queries im WebSocket-Poll.
    pub fn update_projection_watermark(
        &self,
        projection_name: &str,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "INSERT INTO projection_watermarks (projection_name, last_event_id, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(projection_name) DO UPDATE SET
               last_event_id = MAX(projection_watermarks.last_event_id, ?2),
               updated_at = ?3",
            params![projection_name, row_id, now_ms()],
        )?;
        Ok(())
    }

    /// Upserts the latest safe state of one workbench invocation. Private tool
    /// output is structurally absent from this read model.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_workbench_invocation(
        &self,
        invocation_id: &str,
        agent_id: u16,
        project_id: &str,
        work_item_id: &str,
        tool_class: &str,
        runtime_key: &str,
        state: &str,
        resources: &sentinel_common::WorkbenchResourceUsage,
        artifact_ids: &[String],
        error_code: Option<&str>,
        row_id: i64,
    ) -> anyhow::Result<()> {
        let artifact_ids = serde_json::to_string(artifact_ids)?;
        self.guard.execute(
            "INSERT INTO workbench_invocations (
               invocation_id, agent_id, project_id, work_item_id, tool_class,
               runtime_key, state, duration_ms, cpu_time_ms, peak_memory_bytes,
               peak_process_count, bytes_read, bytes_written, artifact_bytes,
               artifact_ids, error_code, last_event_id, updated_at
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
               ?15, ?16, ?17, ?18
             )
             ON CONFLICT(invocation_id) DO UPDATE SET
               agent_id = excluded.agent_id,
               project_id = excluded.project_id,
               work_item_id = excluded.work_item_id,
               tool_class = excluded.tool_class,
               runtime_key = excluded.runtime_key,
               state = excluded.state,
               duration_ms = excluded.duration_ms,
               cpu_time_ms = excluded.cpu_time_ms,
               peak_memory_bytes = excluded.peak_memory_bytes,
               peak_process_count = excluded.peak_process_count,
               bytes_read = excluded.bytes_read,
               bytes_written = excluded.bytes_written,
               artifact_bytes = excluded.artifact_bytes,
               artifact_ids = excluded.artifact_ids,
               error_code = excluded.error_code,
               last_event_id = excluded.last_event_id,
               updated_at = excluded.updated_at
             WHERE excluded.last_event_id > workbench_invocations.last_event_id",
            params![
                invocation_id,
                agent_id,
                project_id,
                work_item_id,
                tool_class,
                runtime_key,
                state,
                i64::try_from(resources.duration_ms)?,
                i64::try_from(resources.cpu_time_ms)?,
                i64::try_from(resources.peak_memory_bytes)?,
                resources.peak_process_count,
                i64::try_from(resources.bytes_read)?,
                i64::try_from(resources.bytes_written)?,
                i64::try_from(resources.artifact_bytes)?,
                artifact_ids,
                error_code,
                row_id,
                now_ms(),
            ],
        )?;
        Ok(())
    }

    // ── task_kanban (#438) ──

    /// UPSERT: Task erstellt. Idempotent via row_id > last_event_id.
    pub fn upsert_task(
        &self,
        task_id: u32,
        title: &str,
        assigned_to: u16,
        assigned_by: Option<u16>,
        parent_task: Option<u32>,
        status: &str,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "INSERT INTO task_kanban (task_id, title, assigned_to, assigned_by, parent_task, status, last_event_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(task_id) DO UPDATE SET
               title = excluded.title, assigned_to = excluded.assigned_to,
               assigned_by = excluded.assigned_by, parent_task = excluded.parent_task,
               status = excluded.status,
               last_event_id = excluded.last_event_id, updated_at = excluded.updated_at
             WHERE excluded.last_event_id > task_kanban.last_event_id",
            params![task_id, title, assigned_to, assigned_by, parent_task, status, row_id, now_ms()],
        )?;
        Ok(())
    }

    /// UPDATE: Task neu zugewiesen / delegiert.
    pub fn update_task_assignee(
        &self,
        task_id: u32,
        assigned_to: u16,
        assigned_by: Option<u16>,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE task_kanban SET assigned_to = ?1, assigned_by = ?2, last_event_id = ?3, updated_at = ?4
             WHERE task_id = ?5 AND ?3 > last_event_id",
            params![assigned_to, assigned_by, row_id, now_ms(), task_id],
        )?;
        Ok(())
    }

    /// UPDATE: Task-Status aendern.
    pub fn update_task_status(
        &self,
        task_id: u32,
        status: &str,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE task_kanban SET status = ?1, last_event_id = ?2, updated_at = ?3
             WHERE task_id = ?4 AND ?2 > last_event_id",
            params![status, row_id, now_ms(), task_id],
        )?;
        Ok(())
    }

    /// UPDATE: Task abgeschlossen (status=done + Ergebnis).
    pub fn complete_task(
        &self,
        task_id: u32,
        result: Option<&str>,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE task_kanban SET status = 'done', result = ?1, last_event_id = ?2, updated_at = ?3
             WHERE task_id = ?4 AND ?2 > last_event_id",
            params![result, row_id, now_ms(), task_id],
        )?;
        Ok(())
    }

    // ── agent_live_view ──

    /// UPSERT: Agent spawned oder aktualisiert.
    /// Idempotent: nur wenn row_id > last_event_id.
    pub fn upsert_agent(
        &self,
        agent_id: u16,
        name: &str,
        role: &str,
        shift_set: u8,
        status: &str,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "INSERT INTO agent_live_view (agent_id, name, role, shift_set, status, last_event_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(agent_id) DO UPDATE SET
               name = excluded.name, role = excluded.role,
               shift_set = excluded.shift_set, status = excluded.status,
               last_event_id = excluded.last_event_id, updated_at = excluded.updated_at
             WHERE excluded.last_event_id > agent_live_view.last_event_id",
            params![agent_id, name, role, shift_set, status, row_id, now_ms()],
        )?;
        Ok(())
    }

    /// UPDATE: Agent-Status aendern (z.B. despawned, paused).
    /// Idempotent: nur wenn row_id > last_event_id.
    pub fn update_agent_status(
        &self,
        agent_id: u16,
        status: &str,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE agent_live_view SET status = ?1, last_event_id = ?2, updated_at = ?3
             WHERE agent_id = ?4 AND ?2 > last_event_id",
            params![status, row_id, now_ms(), agent_id],
        )?;
        Ok(())
    }

    /// UPDATE: Agent Transit gestartet.
    pub fn update_agent_transit_start(
        &self,
        agent_id: u16,
        from_room: &str,
        to_room: &str,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE agent_live_view SET
               current_room = ?1, in_transit = 1, transit_target = ?2,
               last_event_id = ?3, updated_at = ?4
             WHERE agent_id = ?5 AND ?3 > last_event_id",
            params![from_room, to_room, row_id, now_ms(), agent_id],
        )?;
        Ok(())
    }

    /// UPDATE: Agent Transit abgeschlossen.
    pub fn update_agent_transit_complete(
        &self,
        agent_id: u16,
        room_id: &str,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE agent_live_view SET
               current_room = ?1, in_transit = 0, transit_target = NULL,
               last_event_id = ?2, updated_at = ?3
             WHERE agent_id = ?4 AND ?2 > last_event_id",
            params![room_id, row_id, now_ms(), agent_id],
        )?;
        Ok(())
    }

    /// UPDATE: Letzte Aktion eines Agenten.
    pub fn update_agent_last_action(
        &self,
        agent_id: u16,
        action: &str,
        tick: u64,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE agent_live_view SET
               last_action = ?1, last_action_tick = ?2,
               last_event_id = ?3, updated_at = ?4
             WHERE agent_id = ?5 AND ?3 > last_event_id",
            params![action, tick as i64, row_id, now_ms(), agent_id],
        )?;
        Ok(())
    }

    /// Liest den status eines Agenten (fuer Re-Spawn-Erkennung).
    pub fn get_agent_status(&self, agent_id: u16) -> anyhow::Result<Option<String>> {
        let result = self.guard.query_row(
            "SELECT status FROM agent_live_view WHERE agent_id = ?1",
            params![agent_id],
            |row| row.get(0),
        );
        match result {
            Ok(status) => Ok(status),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Liest die current_room eines Agenten (fuer ShiftTransition).
    pub fn get_agent_room(&self, agent_id: u16) -> anyhow::Result<Option<String>> {
        let result = self.guard.query_row(
            "SELECT current_room FROM agent_live_view WHERE agent_id = ?1",
            params![agent_id],
            |row| row.get(0),
        );
        match result {
            Ok(room) => Ok(room),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Liest das aktuell projizierte Chaos-JSON eines Raums.
    pub fn get_room_active_chaos(&self, room_id: &str) -> anyhow::Result<Option<String>> {
        let result = self.guard.query_row(
            "SELECT active_chaos FROM room_live_view WHERE room_id = ?1",
            params![room_id],
            |row| row.get(0),
        );
        match result {
            Ok(chaos) => Ok(chaos),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// UPDATE: Agent initial room (aus AgentSpawned Events).
    pub fn update_agent_room(
        &self,
        agent_id: u16,
        room_id: &str,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE agent_live_view SET
               current_room = ?1, last_event_id = ?2, updated_at = ?3
             WHERE agent_id = ?4 AND ?2 > last_event_id",
            params![room_id, row_id, now_ms(), agent_id],
        )?;
        Ok(())
    }

    /// UPDATE: Agent Bio-State aktualisieren (aus BioStateUpdated Events).
    pub fn update_agent_bio(&self, bio: &BioUpdate<'_>, row_id: i64) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE agent_live_view SET
               hunger = ?1, energy = ?2, stress = ?3, bladder = ?4,
               social_need = ?5, caffeine_mg = ?6, mood = ?7, current_room = ?8,
               last_event_id = ?9, updated_at = ?10
             WHERE agent_id = ?11 AND ?9 > last_event_id",
            params![
                bio.hunger,
                bio.energy,
                bio.stress,
                bio.bladder,
                bio.social_need,
                bio.caffeine_mg,
                bio.mood,
                bio.room_id,
                row_id,
                now_ms(),
                bio.agent_id,
            ],
        )?;
        Ok(())
    }

    // ── room_live_view ──

    /// Raum-Belegung aendern (delta: +1 oder -1).
    pub fn update_room_occupancy(
        &self,
        room_id: &str,
        delta: i64,
        tick: u64,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE room_live_view SET
               occupant_count = MAX(0, occupant_count + ?1),
               last_event_tick = ?2, last_event_id = ?3, updated_at = ?4
             WHERE room_id = ?5 AND ?3 > last_event_id",
            params![delta, tick as i64, row_id, now_ms(), room_id],
        )?;
        Ok(())
    }

    /// Transit-Zaehler aendern (delta: +1 oder -1).
    pub fn update_room_transit(
        &self,
        room_id: &str,
        delta: i64,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE room_live_view SET
               transit_count = MAX(0, transit_count + ?1),
               last_event_id = ?2, updated_at = ?3
             WHERE room_id = ?4 AND ?2 > last_event_id",
            params![delta, row_id, now_ms(), room_id],
        )?;
        Ok(())
    }

    /// Aktive Chaos-Events aktualisieren (JSON-Text).
    pub fn update_room_chaos(
        &self,
        room_id: &str,
        chaos_json: &str,
        tick: u64,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE room_live_view SET
               active_chaos = ?1, last_event_tick = ?2,
               last_event_id = ?3, updated_at = ?4
             WHERE room_id = ?5 AND ?3 > last_event_id",
            params![chaos_json, tick as i64, row_id, now_ms(), room_id],
        )?;
        Ok(())
    }

    /// Aktive Smells aktualisieren (JSON-Text).
    pub fn update_room_smells(
        &self,
        room_id: &str,
        smells_json: &str,
        tick: u64,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE room_live_view SET
               active_smells = ?1, last_event_tick = ?2,
               last_event_id = ?3, updated_at = ?4
             WHERE room_id = ?5 AND ?3 > last_event_id",
            params![smells_json, tick as i64, row_id, now_ms(), room_id],
        )?;
        Ok(())
    }

    /// Room-Physik aktualisieren (Temperatur, CO2, Laerm).
    pub fn update_room_physics(
        &self,
        room_id: &str,
        temperature: f64,
        co2_ppm: f64,
        noise_db: f64,
        clear_active_chaos: bool,
        tick: u64,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE room_live_view SET
               temperature = ?1, co2_ppm = ?2, noise_db = ?3,
               active_chaos = CASE WHEN ?4 = 1 THEN NULL ELSE active_chaos END,
               last_event_tick = ?5, last_event_id = ?6, updated_at = ?7
             WHERE room_id = ?8 AND ?6 > last_event_id",
            params![
                temperature,
                co2_ppm,
                noise_db,
                if clear_active_chaos { 1 } else { 0 },
                tick as i64,
                row_id,
                now_ms(),
                room_id
            ],
        )?;
        Ok(())
    }

    // ── kpi_1m ──

    /// KPI-Bucket UPSERT mit Inkrement-Feldern.
    ///
    /// KEIN per-row Idempotenz-Guard — ein Event kann mehrere Felder im
    /// selben Bucket inkrementieren (z.B. ShiftTransition: ShiftChanges
    /// UND ActiveAgents). `last_event_id` trackt den hoechsten gesehenen
    /// Wert (MAX), wird aber nicht als Gate verwendet.
    ///
    /// Idempotenz wird stattdessen auf Batch-Ebene via Projection-Offset
    /// sichergestellt. Bei Crash-Recovery koennten KPI-Werte minimal
    /// abweichen (akzeptabel fuer Monitoring). `rebuild()` ist immer korrekt.
    pub fn increment_kpi(
        &self,
        timestamp_ms: u64,
        field: KpiField,
        row_id: i64,
    ) -> anyhow::Result<()> {
        let bucket_start = (timestamp_ms / 60_000) * 60_000;
        let (col, inc_sql, initial_val) = match field {
            KpiField::ActiveAgents(delta) => {
                ("active_agents", format!("active_agents + {delta}"), delta)
            }
            KpiField::TotalActions => ("total_actions", "total_actions + 1".to_string(), 1i64),
            KpiField::TotalTransits => ("total_transits", "total_transits + 1".to_string(), 1),
            KpiField::ChaosEvents => ("chaos_events", "chaos_events + 1".to_string(), 1),
            KpiField::TickCount => ("tick_count", "tick_count + 1".to_string(), 1),
            KpiField::ShiftChanges => ("shift_changes", "shift_changes + 1".to_string(), 1),
            KpiField::NightrunEvents => ("nightrun_events", "nightrun_events + 1".to_string(), 1),
        };
        let sql = format!(
            "INSERT INTO kpi_1m (bucket_start, {col}, last_event_id, updated_at)
             VALUES (?1, {initial_val}, ?2, ?3)
             ON CONFLICT(bucket_start) DO UPDATE SET
               {col} = {inc_sql},
               last_event_id = MAX(kpi_1m.last_event_id, ?2),
               updated_at = ?3"
        );
        self.guard
            .execute(&sql, params![bucket_start as i64, row_id, now_ms()])?;
        Ok(())
    }

    /// #427: aggregiert einen LLM-Call (cache-aware) in die drei Cost-Read-Models
    /// (cost_by_agent / cost_by_tier / cost_timeseries). Idempotent via
    /// `WHERE excluded.last_event_id > <table>.last_event_id` — ein erneut verarbeitetes
    /// Event (z.B. nach Restart) zaehlt NICHT doppelt.
    pub fn record_llm_cost(&self, u: &LlmCostUpdate<'_>, row_id: i64) -> anyhow::Result<()> {
        let now = now_ms();
        let bucket_start = ((u.bucket_ms / 60_000) * 60_000) as i64;
        self.guard.execute(
            &cost_upsert_sql("cost_by_agent", "agent_id"),
            params![
                u.agent_id,
                u.input_tokens,
                u.output_tokens,
                u.cache_read,
                u.cache_creation,
                u.cost_usd,
                row_id,
                now
            ],
        )?;
        self.guard.execute(
            &cost_upsert_sql("cost_by_tier", "tier"),
            params![
                u.tier,
                u.input_tokens,
                u.output_tokens,
                u.cache_read,
                u.cache_creation,
                u.cost_usd,
                row_id,
                now
            ],
        )?;
        self.guard.execute(
            &cost_upsert_sql("cost_timeseries", "bucket_start"),
            params![
                bucket_start,
                u.input_tokens,
                u.output_tokens,
                u.cache_read,
                u.cache_creation,
                u.cost_usd,
                row_id,
                now
            ],
        )?;
        Ok(())
    }

    /// #395: independently aggregates one usage event by organization hierarchy.
    pub fn record_hierarchy_cost(
        &self,
        u: &LlmHierarchyCostUpdate<'_>,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            &cost_upsert_sql("cost_by_hierarchy_tier", "hierarchy_tier"),
            params![
                u.hierarchy_tier,
                u.input_tokens,
                u.output_tokens,
                u.cache_read,
                u.cache_creation,
                u.cost_usd,
                row_id,
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// Records coverage for each usage event exactly once. V1 usage is counted
    /// as unattributed but is never inserted into the hierarchy aggregate.
    pub fn record_hierarchy_usage_meta(
        &self,
        row_id: i64,
        attributed_v2: bool,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE cost_hierarchy_projection_meta SET
               first_v2_event_id = CASE
                 WHEN ?2 THEN COALESCE(first_v2_event_id, ?1)
                 ELSE first_v2_event_id
               END,
               last_usage_event_id = ?1,
               last_hierarchy_event_id = CASE WHEN ?2 THEN ?1 ELSE last_hierarchy_event_id END,
               unattributed_v1_usage_events = unattributed_v1_usage_events + CASE WHEN ?2 THEN 0 ELSE 1 END,
               updated_at = ?3
             WHERE id = 1 AND ?1 > last_usage_event_id",
            params![row_id, attributed_v2, now_ms()],
        )?;
        Ok(())
    }
}

/// #427: Aggregations-Eingabe fuer einen einzelnen LLM-Call (cache-aware).
pub struct LlmCostUpdate<'a> {
    pub agent_id: &'a str,
    pub tier: &'a str,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read: u32,
    pub cache_creation: u32,
    pub cost_usd: f64,
    pub bucket_ms: u64,
}

/// #395: aggregation input for the independent hierarchy projection.
pub struct LlmHierarchyCostUpdate<'a> {
    pub hierarchy_tier: &'a str,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read: u32,
    pub cache_creation: u32,
    pub cost_usd: f64,
}

/// Baut das idempotente Cost-Upsert-SQL fuer eine der drei Cost-Tabellen. Die
/// Wert-Spalten sind identisch; nur Tabellenname + Key-Spalte unterscheiden sich.
fn cost_upsert_sql(table: &str, key_col: &str) -> String {
    format!(
        "INSERT INTO {table} ({key_col}, input_tokens, output_tokens, cache_read, cache_creation, cost_usd, call_count, last_event_id, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8)
         ON CONFLICT({key_col}) DO UPDATE SET
           input_tokens = input_tokens + excluded.input_tokens,
           output_tokens = output_tokens + excluded.output_tokens,
           cache_read = cache_read + excluded.cache_read,
           cache_creation = cache_creation + excluded.cache_creation,
           cost_usd = cost_usd + excluded.cost_usd,
           call_count = call_count + 1,
           last_event_id = excluded.last_event_id,
           updated_at = excluded.updated_at
         WHERE excluded.last_event_id > {table}.last_event_id"
    )
}

/// Bio-State Update fuer einen Agenten.
pub struct BioUpdate<'a> {
    pub agent_id: u16,
    pub hunger: f64,
    pub energy: f64,
    pub stress: f64,
    pub bladder: f64,
    pub social_need: f64,
    pub caffeine_mg: f64,
    pub mood: &'a str,
    pub room_id: &'a str,
}

/// KPI-Felder fuer Inkrement-Operationen.
pub enum KpiField {
    ActiveAgents(i64),
    TotalActions,
    TotalTransits,
    ChaosEvents,
    TickCount,
    ShiftChanges,
    NightrunEvents,
}

fn bootstrap_projection_watermark(conn: &Connection) -> anyhow::Result<()> {
    let source_max: i64 = conn.query_row(
        "SELECT COALESCE(MAX(m), 0)
         FROM (
           SELECT MAX(last_event_id) as m FROM agent_live_view
           UNION ALL
           SELECT MAX(last_event_id) as m FROM room_live_view
           UNION ALL
           SELECT MAX(last_event_id) as m FROM kpi_1m
           UNION ALL
           SELECT MAX(last_event_id) as m FROM cost_by_agent
         )",
        [],
        |row| row.get(0),
    )?;

    conn.execute(
        "INSERT INTO projection_watermarks (projection_name, last_event_id, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(projection_name) DO UPDATE SET
           last_event_id = MAX(projection_watermarks.last_event_id, excluded.last_event_id),
           updated_at = excluded.updated_at",
        params![PROJECTION_NAME, source_max, now_ms()],
    )?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_creates_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-proj.db");
        let store = ReadModelStore::open(path.to_str().unwrap()).unwrap();

        let conn = store.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM agent_live_view", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = conn
            .query_row("SELECT count(*) FROM room_live_view", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = conn
            .query_row("SELECT count(*) FROM kpi_1m", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = conn
            .query_row("SELECT count(*) FROM projection_watermarks", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);

        let watermark: i64 = conn
            .query_row(
                "SELECT last_event_id FROM projection_watermarks WHERE projection_name = ?1",
                params![PROJECTION_NAME],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(watermark, 0);

        let index_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type = 'index'
                   AND name IN (
                     'idx_agent_live_view_last_event_id',
                     'idx_room_live_view_last_event_id',
                     'idx_kpi_1m_last_event_id'
                   )",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 3);
    }

    #[test]
    fn test_open_bootstraps_projection_watermark_from_existing_views() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-bootstrap.db");
        let store = ReadModelStore::open(path.to_str().unwrap()).unwrap();

        {
            let conn = store.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO kpi_1m (bucket_start, last_event_id, updated_at)
                 VALUES (?1, ?2, ?3)",
                params![60_000_i64, 321_i64, now_ms()],
            )
            .unwrap();
        }
        drop(store);

        let store = ReadModelStore::open(path.to_str().unwrap()).unwrap();
        let conn = store.conn.lock().unwrap();
        let watermark: i64 = conn
            .query_row(
                "SELECT last_event_id FROM projection_watermarks WHERE projection_name = ?1",
                params![PROJECTION_NAME],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(watermark, 321);
    }

    #[test]
    fn test_open_readonly_reads_existing_projection_without_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-readonly.db");
        let store = ReadModelStore::open(path.to_str().unwrap()).unwrap();

        {
            let txn = store.begin_transaction().unwrap();
            txn.begin().unwrap();
            txn.upsert_agent(7, "Readonly Agent", "QA", 1, "active", 10)
                .unwrap();
            txn.update_agent_room(7, "empfang", 11).unwrap();
            txn.commit().unwrap();
        }
        drop(store);

        let readonly = ReadModelStore::open_readonly(path.to_str().unwrap()).unwrap();
        let agent = readonly.get_agent(7).unwrap().unwrap();
        assert_eq!(agent.name, "Readonly Agent");
        assert_eq!(agent.current_room.as_deref(), Some("empfang"));
    }

    #[test]
    fn test_initialize_rooms() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-rooms.db");
        let store = ReadModelStore::open(path.to_str().unwrap()).unwrap();

        store
            .initialize_rooms(&["empfang", "kueche", "buero-dev-1"])
            .unwrap();

        let room = store.get_room("empfang").unwrap().unwrap();
        assert_eq!(room.occupant_count, 0);

        let room = store.get_room("kueche").unwrap().unwrap();
        assert_eq!(room.occupant_count, 0);

        // Idempotent: nochmal initialisieren aendert nichts
        store.initialize_rooms(&["empfang", "kueche"]).unwrap();
    }

    #[test]
    fn test_clear_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-clear.db");
        let store = ReadModelStore::open(path.to_str().unwrap()).unwrap();

        store.initialize_rooms(&["empfang"]).unwrap();
        assert!(store.get_room("empfang").unwrap().is_some());

        {
            let txn = store.begin_transaction().unwrap();
            txn.begin().unwrap();
            txn.update_projection_watermark("sentinel-projection", 42)
                .unwrap();
            txn.commit().unwrap();
        }

        store.clear_all().unwrap();
        assert!(store.get_room("empfang").unwrap().is_none());
        let conn = store.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM projection_watermarks", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_upsert_agent_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-idem.db");
        let store = ReadModelStore::open(path.to_str().unwrap()).unwrap();

        {
            let txn = store.begin_transaction().unwrap();
            txn.begin().unwrap();
            txn.upsert_agent(1, "Klaus", "Developer", 1, "active", 10)
                .unwrap();
            txn.commit().unwrap();
        } // txn dropped -> Mutex released

        let agent = store.get_agent(1).unwrap().unwrap();
        assert_eq!(agent.name, "Klaus");
        assert_eq!(agent.last_event_id, 10);

        // Gleiche row_id: kein Update (idempotent)
        {
            let txn = store.begin_transaction().unwrap();
            txn.begin().unwrap();
            txn.upsert_agent(1, "KlausNeu", "Designer", 2, "paused", 10)
                .unwrap();
            txn.commit().unwrap();
        }

        let agent = store.get_agent(1).unwrap().unwrap();
        assert_eq!(agent.name, "Klaus"); // Unveraendert!
        assert_eq!(agent.role, "Developer");

        // Hoehere row_id: Update
        {
            let txn = store.begin_transaction().unwrap();
            txn.begin().unwrap();
            txn.upsert_agent(1, "KlausNeu", "Designer", 2, "paused", 20)
                .unwrap();
            txn.commit().unwrap();
        }

        let agent = store.get_agent(1).unwrap().unwrap();
        assert_eq!(agent.name, "KlausNeu");
        assert_eq!(agent.role, "Designer");
        assert_eq!(agent.last_event_id, 20);
    }
}
