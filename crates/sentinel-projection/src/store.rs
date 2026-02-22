//! Read Model Store fuer CQRS-Projektionen.
//!
//! Eigene SQLite-Datenbank (projection.db) mit drei materialisierten Views:
//! `agent_live_view`, `room_live_view`, `kpi_1m`.
//!
//! Separate DB vom EventStore — kein shared-transaction moeglich.
//! Crash-Safety: Views committen VOR Offset-Update. Idempotente Handler
//! tolerieren Re-Processing bei Restart.

use anyhow::Context;
use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};
use tracing::info;

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

        // WAL-Modus + Performance-Pragmas (wie EventStore)
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "mmap_size", 268_435_456i64)?;
        conn.pragma_update(None, "page_size", 8192)?;

        conn.execute_batch(CREATE_AGENT_LIVE_VIEW)?;
        conn.execute_batch(CREATE_ROOM_LIVE_VIEW)?;
        conn.execute_batch(CREATE_KPI_1M)?;

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

        info!(path, "ReadModelStore opened");
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

    /// Loescht alle Daten aus allen drei View-Tabellen (fuer Rebuild).
    pub fn clear_all(&self) -> anyhow::Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
        conn.execute_batch(
            "DELETE FROM agent_live_view; DELETE FROM room_live_view; DELETE FROM kpi_1m;",
        )?;
        info!("All read model tables cleared");
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
            "SELECT room_id, occupant_count, transit_count, active_chaos, temperature, co2_ppm, noise_db, last_event_tick, last_event_id, updated_at FROM room_live_view WHERE room_id = ?1",
            params![room_id],
            |row| {
                Ok(RoomView {
                    room_id: row.get(0)?,
                    occupant_count: row.get(1)?,
                    transit_count: row.get(2)?,
                    active_chaos: row.get(3)?,
                    temperature: row.get(4)?,
                    co2_ppm: row.get(5)?,
                    noise_db: row.get(6)?,
                    last_event_tick: row.get(7)?,
                    last_event_id: row.get(8)?,
                    updated_at: row.get(9)?,
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
    pub temperature: Option<f64>,
    pub co2_ppm: Option<f64>,
    pub noise_db: Option<f64>,
    pub last_event_tick: Option<i64>,
    pub last_event_id: i64,
    pub updated_at: i64,
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

    /// Room-Physik aktualisieren (Temperatur, CO2, Laerm).
    pub fn update_room_physics(
        &self,
        room_id: &str,
        temperature: f64,
        co2_ppm: f64,
        noise_db: f64,
        tick: u64,
        row_id: i64,
    ) -> anyhow::Result<()> {
        self.guard.execute(
            "UPDATE room_live_view SET
               temperature = ?1, co2_ppm = ?2, noise_db = ?3,
               last_event_tick = ?4, last_event_id = ?5, updated_at = ?6
             WHERE room_id = ?7 AND ?5 > last_event_id",
            params![
                temperature,
                co2_ppm,
                noise_db,
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

        store.clear_all().unwrap();
        assert!(store.get_room("empfang").unwrap().is_none());
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
