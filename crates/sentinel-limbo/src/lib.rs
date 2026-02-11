//! Async SQLite for cold storage: chat archive, observations.
//!
//! Cold storage tier for Project Sentinel using rusqlite (SQLite binding).
//! Stores chat messages, meeting logs, observation data, and chaos events.
//!
//! Note: Uses rusqlite as fallback for limbo (v0.0.22) which lacks pragma setter
//! support in its Rust binding. Same tables, same pragmas, sync API wrapped
//! with tokio::task::spawn_blocking for async compatibility.

use rusqlite::{params, Connection};
use sentinel_common::{AgentId, Emotion, EventType, RoomId, Tick, Timestamp};
use std::sync::{Arc, Mutex};
use tracing::{info, instrument};

// ──────────────────────────────────────────────
// SQL Constants
// ──────────────────────────────────────────────

const CREATE_MESSAGES: &str = "
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY,
    room_id TEXT NOT NULL,
    agent_name TEXT NOT NULL,
    content TEXT NOT NULL,
    emotion TEXT,
    timestamp_ms INTEGER NOT NULL,
    tick INTEGER NOT NULL
)";

const CREATE_IDX_MESSAGES_ROOM: &str =
    "CREATE INDEX IF NOT EXISTS idx_messages_room ON messages(room_id, timestamp_ms)";

const CREATE_MEETINGS: &str = "
CREATE TABLE IF NOT EXISTS meetings (
    id INTEGER PRIMARY KEY,
    room_id TEXT NOT NULL,
    title TEXT NOT NULL,
    participants TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    summary TEXT
)";

const CREATE_OBSERVATIONS: &str = "
CREATE TABLE IF NOT EXISTS observations (
    id INTEGER PRIMARY KEY,
    agent_name TEXT NOT NULL,
    model TEXT NOT NULL,
    metric TEXT NOT NULL,
    value REAL NOT NULL,
    context TEXT,
    timestamp_ms INTEGER NOT NULL
)";

const CREATE_IDX_OBSERVATIONS_AGENT: &str =
    "CREATE INDEX IF NOT EXISTS idx_observations_agent ON observations(agent_name, metric, timestamp_ms)";

const CREATE_CHAOS_EVENTS: &str = "
CREATE TABLE IF NOT EXISTS chaos_events (
    id INTEGER PRIMARY KEY,
    event_type TEXT NOT NULL,
    target_room TEXT,
    target_agent TEXT,
    description TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL
)";

// ──────────────────────────────────────────────
// ChatStore
// ──────────────────────────────────────────────

/// Cold storage for chat messages, meetings, observations, and chaos events.
///
/// Uses rusqlite (sync SQLite) wrapped with Arc<Mutex<>> for thread-safe
/// access from async contexts via spawn_blocking.
pub struct ChatStore {
    conn: Arc<Mutex<Connection>>,
}

impl ChatStore {
    /// Open or create the chat store at the given path.
    /// Runs performance pragmas and creates all tables.
    #[instrument(fields(path = %path))]
    pub async fn open(path: &str) -> anyhow::Result<Self> {
        let path = path.to_string();
        let conn = tokio::task::spawn_blocking(move || -> anyhow::Result<Connection> {
            let conn = Connection::open(&path)?;

            // Performance pragmas (PFLICHT laut Issue-Spec)
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA mmap_size = 268435456;
                 PRAGMA page_size = 8192;",
            )?;

            // Create tables
            conn.execute_batch(CREATE_MESSAGES)?;
            conn.execute(CREATE_IDX_MESSAGES_ROOM, [])?;
            conn.execute_batch(CREATE_MEETINGS)?;
            conn.execute_batch(CREATE_OBSERVATIONS)?;
            conn.execute(CREATE_IDX_OBSERVATIONS_AGENT, [])?;
            conn.execute_batch(CREATE_CHAOS_EVENTS)?;

            info!("ChatStore opened at {path}");
            Ok(conn)
        })
        .await??;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    // === MESSAGES ===

    /// Insert a chat message. Returns the rowid of the inserted row.
    #[instrument(skip(self, content), fields(room_id = %room_id, agent_id = %agent_id, tick = %tick))]
    pub async fn insert_message(
        &self,
        room_id: RoomId,
        agent_id: AgentId,
        content: &str,
        emotion: Option<Emotion>,
        timestamp: Timestamp,
        tick: Tick,
    ) -> anyhow::Result<i64> {
        let conn = self.conn.clone();
        let room_str = room_id.to_string();
        let agent_str = agent_id.to_string();
        let content = content.to_string();
        let emotion_str = emotion.map(emotion_to_str);
        let ts = timestamp.0 as i64;
        let t = tick.0 as i64;

        tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
            let conn = conn.lock().map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
            conn.execute(
                "INSERT INTO messages (room_id, agent_name, content, emotion, timestamp_ms, tick) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![room_str, agent_str, content, emotion_str, ts, t],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await?
    }

    /// Get messages for a room, ordered by timestamp descending.
    #[instrument(skip(self), fields(room_id = %room_id, limit = %limit))]
    pub async fn get_room_messages(
        &self,
        room_id: RoomId,
        limit: u32,
    ) -> anyhow::Result<Vec<MessageRow>> {
        let conn = self.conn.clone();
        let room_str = room_id.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MessageRow>> {
            let conn = conn.lock().map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
            let mut stmt = conn.prepare(
                "SELECT id, room_id, agent_name, content, emotion, timestamp_ms, tick FROM messages WHERE room_id = ?1 ORDER BY timestamp_ms DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![room_str, limit], |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    room_id: row.get(1)?,
                    agent_name: row.get(2)?,
                    content: row.get(3)?,
                    emotion: row.get(4)?,
                    timestamp: Timestamp(row.get::<_, i64>(5)? as u64),
                    tick: Tick(row.get::<_, i64>(6)? as u64),
                })
            })?;
            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok(results)
        })
        .await?
    }

    // === MEETINGS ===

    /// Insert a meeting record. Participants are serialized as JSON array.
    /// Returns the rowid of the inserted row.
    #[instrument(skip(self, participants), fields(room_id = %room_id, title = %title))]
    pub async fn insert_meeting(
        &self,
        room_id: RoomId,
        title: &str,
        participants: &[AgentId],
        started_at: Timestamp,
    ) -> anyhow::Result<i64> {
        let conn = self.conn.clone();
        let room_str = room_id.to_string();
        let title = title.to_string();
        let participant_strings: Vec<String> = participants.iter().map(|a| a.to_string()).collect();
        let participants_json = serde_json::to_string(&participant_strings)?;
        let started = started_at.0 as i64;

        tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
            let conn = conn.lock().map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
            conn.execute(
                "INSERT INTO meetings (room_id, title, participants, started_at) VALUES (?1, ?2, ?3, ?4)",
                params![room_str, title, participants_json, started],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await?
    }

    /// End a meeting by setting ended_at and summary.
    #[instrument(skip(self, summary), fields(meeting_id = %meeting_id))]
    pub async fn end_meeting(
        &self,
        meeting_id: i64,
        ended_at: Timestamp,
        summary: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        let ended = ended_at.0 as i64;
        let summary = summary.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = conn.lock().map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
            conn.execute(
                "UPDATE meetings SET ended_at = ?1, summary = ?2 WHERE id = ?3",
                params![ended, summary, meeting_id],
            )?;
            Ok(())
        })
        .await?
    }

    // === OBSERVATIONS ===

    /// Insert an observation data point. Returns the rowid of the inserted row.
    #[instrument(skip(self, context), fields(agent_id = %agent_id, model = %model, metric = %metric))]
    pub async fn insert_observation(
        &self,
        agent_id: AgentId,
        model: &str,
        metric: &str,
        value: f64,
        context: Option<&str>,
        timestamp: Timestamp,
    ) -> anyhow::Result<i64> {
        let conn = self.conn.clone();
        let agent_str = agent_id.to_string();
        let model = model.to_string();
        let metric = metric.to_string();
        let context = context.map(|c| c.to_string());
        let ts = timestamp.0 as i64;

        tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
            let conn = conn.lock().map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
            conn.execute(
                "INSERT INTO observations (agent_name, model, metric, value, context, timestamp_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![agent_str, model, metric, value, context, ts],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await?
    }

    // === CHAOS EVENTS ===

    /// Insert a chaos event. Returns the rowid of the inserted row.
    #[instrument(skip(self, description), fields(event_type = ?event_type, target_room = ?target_room, target_agent = ?target_agent))]
    pub async fn insert_chaos_event(
        &self,
        event_type: EventType,
        target_room: Option<RoomId>,
        target_agent: Option<AgentId>,
        description: &str,
        timestamp: Timestamp,
    ) -> anyhow::Result<i64> {
        let conn = self.conn.clone();
        let event_str = event_type_to_str(event_type);
        let room_str = target_room.map(|r| r.to_string());
        let agent_str = target_agent.map(|a| a.to_string());
        let description = description.to_string();
        let ts = timestamp.0 as i64;

        tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
            let conn = conn.lock().map_err(|e| anyhow::anyhow!("Lock poisoned: {e}"))?;
            conn.execute(
                "INSERT INTO chaos_events (event_type, target_room, target_agent, description, timestamp_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![event_str, room_str, agent_str, description, ts],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await?
    }

    /// Get a reference to the inner connection for testing.
    #[cfg(test)]
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }
}

// ──────────────────────────────────────────────
// Row Types
// ──────────────────────────────────────────────

/// Row type for message queries.
#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: i64,
    pub room_id: String,
    pub agent_name: String,
    pub content: String,
    pub emotion: Option<String>,
    pub timestamp: Timestamp,
    pub tick: Tick,
}

// ──────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────

/// Convert Emotion enum to lowercase string for DB storage.
fn emotion_to_str(e: Emotion) -> String {
    match e {
        Emotion::Neutral => "neutral",
        Emotion::Happy => "happy",
        Emotion::Frustrated => "frustrated",
        Emotion::Stressed => "stressed",
        Emotion::Relaxed => "relaxed",
        Emotion::Excited => "excited",
        Emotion::Bored => "bored",
        Emotion::Anxious => "anxious",
        Emotion::Focused => "focused",
        Emotion::Tired => "tired",
    }
    .to_string()
}

/// Convert EventType enum to snake_case string for DB storage.
fn event_type_to_str(e: EventType) -> String {
    match e {
        EventType::PhoneRing => "phone_ring",
        EventType::PrinterBroken => "printer_broken",
        EventType::PackageDelivery => "package_delivery",
        EventType::SBahnDelay => "sbahn_delay",
        EventType::FireAlarmDrill => "fire_alarm_drill",
        EventType::CakeInKitchen => "cake_in_kitchen",
        EventType::AirConBroken => "air_con_broken",
        EventType::InternetOutage => "internet_outage",
    }
    .to_string()
}

// ──────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_open_creates_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = ChatStore::open(path.to_str().unwrap()).await.unwrap();

        // Verify all 4 tables exist by querying them
        let conn = store.conn();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = conn
            .query_row("SELECT count(*) FROM meetings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = conn
            .query_row("SELECT count(*) FROM observations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = conn
            .query_row("SELECT count(*) FROM chaos_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_message_insert_and_query() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = ChatStore::open(path.to_str().unwrap()).await.unwrap();

        let room = RoomId::new(1).unwrap();
        let agent = AgentId::new(1).unwrap();

        let id = store
            .insert_message(
                room,
                agent,
                "Guten Morgen!",
                Some(Emotion::Happy),
                Timestamp(1000),
                Tick(42),
            )
            .await
            .unwrap();
        assert!(id > 0);

        let messages = store.get_room_messages(room, 10).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent_name, "AGENT-01");
        assert_eq!(messages[0].content, "Guten Morgen!");
        assert_eq!(messages[0].emotion, Some("happy".to_string()));
        assert_eq!(messages[0].timestamp, Timestamp(1000));
        assert_eq!(messages[0].tick, Tick(42));
    }

    #[tokio::test]
    async fn test_meeting_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = ChatStore::open(path.to_str().unwrap()).await.unwrap();

        let room = RoomId::new(5).unwrap();
        let participants = vec![AgentId::new(1).unwrap(), AgentId::new(2).unwrap()];
        let id = store
            .insert_meeting(room, "Sprint Review", &participants, Timestamp(9000))
            .await
            .unwrap();
        assert!(id > 0);

        store
            .end_meeting(id, Timestamp(10800), "Sprint goals achieved")
            .await
            .unwrap();

        // Verify the meeting was updated
        let conn = store.conn();
        let (ended_at, summary): (i64, String) = conn
            .query_row(
                "SELECT ended_at, summary FROM meetings WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(ended_at, 10800);
        assert_eq!(summary, "Sprint goals achieved");
    }

    #[tokio::test]
    async fn test_wal_mode_active() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = ChatStore::open(path.to_str().unwrap()).await.unwrap();

        // Insert something to trigger WAL file creation
        let room = RoomId::new(1).unwrap();
        let agent = AgentId::new(1).unwrap();
        store
            .insert_message(room, agent, "test", None, Timestamp(1), Tick(1))
            .await
            .unwrap();

        // Verify WAL mode via pragma
        let conn = store.conn();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }
}
