//! Token-bounded read-only wake-up rehydration for Gaia Console Memory.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::hippocampus_source::{read_hippocampus, HippocampusReadRequest, HippocampusWakeMemory};
use crate::MEMORY_FILE_NAME;

pub const EVENTS_DB_FILE_NAME: &str = "events.db";
pub const PROJECTION_DB_FILE_NAME: &str = "projection.db";
pub const HIPPOCAMPUS_DB_FILE_NAME: &str = "hippocampus.redb";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RehydrateRequest {
    pub data_dir: PathBuf,
    pub agent_name: Option<String>,
    pub fact_keys: Vec<String>,
    pub max_memory_bytes: usize,
    pub max_agents: usize,
    pub max_episodes: usize,
}

impl RehydrateRequest {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
            agent_name: None,
            fact_keys: Vec::new(),
            max_memory_bytes: 16_384,
            max_agents: 16,
            max_episodes: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventStoreWakeSummary {
    pub status: String,
    pub path: PathBuf,
    pub latest_event_id: Option<i64>,
    pub event_count: Option<i64>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionAgentSummary {
    pub agent_id: i64,
    pub name: String,
    pub role: String,
    pub status: String,
    pub current_room: Option<String>,
    pub mood: Option<String>,
    pub last_event_id: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionWakeSummary {
    pub status: String,
    pub path: PathBuf,
    pub active_agent_count: Option<i64>,
    pub active_agents: Vec<ProjectionAgentSummary>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFileWakeSummary {
    pub status: String,
    pub path: PathBuf,
    pub bytes_returned: usize,
    pub contents: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RehydrationContext {
    pub data_dir: PathBuf,
    pub event_store: EventStoreWakeSummary,
    pub projection: ProjectionWakeSummary,
    pub memory_file: MemoryFileWakeSummary,
    pub hippocampus: HippocampusWakeMemory,
    pub events_replayed: u64,
    pub event_rows_loaded: u64,
    pub event_copy_count: u64,
    pub notes: Vec<String>,
}

pub fn rehydrate_from_data_dir(request: &RehydrateRequest) -> anyhow::Result<RehydrationContext> {
    let event_store = read_event_store(&request.data_dir.join(EVENTS_DB_FILE_NAME));
    let projection = read_projection(
        &request.data_dir.join(PROJECTION_DB_FILE_NAME),
        request.max_agents,
    );
    let memory_file = read_memory_file(
        &request.data_dir.join(MEMORY_FILE_NAME),
        request.max_memory_bytes,
    );
    let hippocampus = read_hippocampus(&HippocampusReadRequest {
        path: request.data_dir.join(HIPPOCAMPUS_DB_FILE_NAME),
        agent_name: request.agent_name.clone(),
        fact_keys: request.fact_keys.clone(),
        max_episodes: request.max_episodes,
    })?;

    Ok(RehydrationContext {
        data_dir: request.data_dir.clone(),
        event_store,
        projection,
        memory_file,
        hippocampus,
        events_replayed: 0,
        event_rows_loaded: 0,
        event_copy_count: 0,
        notes: vec![
            "read-only rehydration: metadata/read-model reads only; no event replay".to_string(),
            "events_replayed=0 and event_rows_loaded=0 by design".to_string(),
            "event store rows are referenced through source metadata; they are not copied into Gaia Console Memory".to_string(),
            "task_kanban open-task context is skipped in this crate: no public projection read API exists yet, so #438 data remains optional and graceful".to_string(),
        ],
    })
}

fn read_event_store(path: &Path) -> EventStoreWakeSummary {
    if !path.exists() {
        return EventStoreWakeSummary {
            status: "missing".to_string(),
            path: path.to_path_buf(),
            latest_event_id: None,
            event_count: None,
            notes: vec![format!("{} is missing", path.display())],
        };
    }

    match open_immutable_readonly(path) {
        Ok(conn) => {
            let mut notes = Vec::new();
            let event_count = match conn.query_row("SELECT COUNT(*) FROM events", [], |row| {
                row.get::<_, i64>(0)
            }) {
                Ok(value) => Some(value),
                Err(error) => {
                    notes.push(format!("event count read failed: {error}"));
                    None
                }
            };
            let latest_event_id =
                match conn.query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |row| {
                    row.get::<_, i64>(0)
                }) {
                    Ok(value) => Some(value),
                    Err(error) => {
                        notes.push(format!("latest event id read failed: {error}"));
                        None
                    }
                };
            notes.push(
                "metadata-only immutable SQLite read; no event rows loaded or replayed".to_string(),
            );
            notes.push(
                "schema assumption: events(id) exists; failure degrades to unavailable notes"
                    .to_string(),
            );
            EventStoreWakeSummary {
                status: "ok".to_string(),
                path: path.to_path_buf(),
                latest_event_id,
                event_count,
                notes,
            }
        }
        Err(error) => EventStoreWakeSummary {
            status: "unavailable".to_string(),
            path: path.to_path_buf(),
            latest_event_id: None,
            event_count: None,
            notes: vec![format!("read-only open failed: {error}")],
        },
    }
}

fn read_projection(path: &Path, max_agents: usize) -> ProjectionWakeSummary {
    if !path.exists() {
        return ProjectionWakeSummary {
            status: "missing".to_string(),
            path: path.to_path_buf(),
            active_agent_count: None,
            active_agents: Vec::new(),
            notes: vec![format!("{} is missing", path.display())],
        };
    }

    match open_immutable_readonly(path) {
        Ok(conn) => {
            let mut notes = Vec::new();
            let active_agent_count = match conn.query_row(
                "SELECT count(*) FROM agent_live_view WHERE status = 'active'",
                [],
                |row| row.get::<_, i64>(0),
            ) {
                Ok(value) => Some(value),
                Err(error) => {
                    notes.push(format!("active agent count read failed: {error}"));
                    None
                }
            };
            let active_agents = match read_active_projection_agents(&conn, max_agents) {
                Ok(agents) => agents,
                Err(error) => {
                    notes.push(format!("active agent read failed: {error}"));
                    Vec::new()
                }
            };
            notes.push(
                "projection read used immutable SQLite to avoid WAL side-file writes".to_string(),
            );
            notes.push(
                "schema assumption: agent_live_view active-agent columns exist; failure degrades to unavailable notes"
                    .to_string(),
            );
            ProjectionWakeSummary {
                status: "ok".to_string(),
                path: path.to_path_buf(),
                active_agent_count,
                active_agents,
                notes,
            }
        }
        Err(error) => ProjectionWakeSummary {
            status: "unavailable".to_string(),
            path: path.to_path_buf(),
            active_agent_count: None,
            active_agents: Vec::new(),
            notes: vec![format!("read-only open failed: {error}")],
        },
    }
}

fn open_immutable_readonly(path: &Path) -> anyhow::Result<Connection> {
    let path = path
        .to_str()
        .with_context(|| format!("{} is not valid UTF-8", path.display()))?;
    let uri = format!("file:{path}?mode=ro&immutable=1");
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open immutable read-only SQLite at {path}"))
}

fn read_active_projection_agents(
    conn: &Connection,
    max_agents: usize,
) -> anyhow::Result<Vec<ProjectionAgentSummary>> {
    let mut stmt = conn.prepare(
        "SELECT agent_id, name, role, status, current_room, mood, last_event_id
         FROM agent_live_view
         WHERE status = 'active'
         ORDER BY agent_id
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![max_agents as i64], |row| {
        Ok(ProjectionAgentSummary {
            agent_id: row.get(0)?,
            name: row.get(1)?,
            role: row.get(2)?,
            status: row.get(3)?,
            current_room: row.get(4)?,
            mood: row.get(5)?,
            last_event_id: row.get(6)?,
        })
    })?;

    let mut agents = Vec::new();
    for row in rows {
        agents.push(row?);
    }
    Ok(agents)
}

fn read_memory_file(path: &Path, max_bytes: usize) -> MemoryFileWakeSummary {
    if !path.exists() {
        return MemoryFileWakeSummary {
            status: "missing".to_string(),
            path: path.to_path_buf(),
            bytes_returned: 0,
            contents: String::new(),
            notes: vec![format!("{} is missing", path.display())],
        };
    }

    match fs::read_to_string(path) {
        Ok(contents) => {
            let condensed = take_utf8_prefix(&contents, max_bytes);
            MemoryFileWakeSummary {
                status: "ok".to_string(),
                path: path.to_path_buf(),
                bytes_returned: condensed.len(),
                contents: condensed,
                notes: vec!["read-only Markdown read; file was not created or modified".to_string()],
            }
        }
        Err(error) => MemoryFileWakeSummary {
            status: "unavailable".to_string(),
            path: path.to_path_buf(),
            bytes_returned: 0,
            contents: String::new(),
            notes: vec![format!("read failed: {error}")],
        },
    }
}

fn take_utf8_prefix(contents: &str, max_bytes: usize) -> String {
    if contents.len() <= max_bytes {
        return contents.to_string();
    }

    let mut end = max_bytes;
    while end > 0 && !contents.is_char_boundary(end) {
        end -= 1;
    }
    contents[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_file::{GaiaConsoleMemoryFile, MemorySection};
    use sentinel_hippocampus::{Episode, HippocampusStore, NarrativeState};

    fn episode(id: u64, summary: &str) -> Episode {
        Episode {
            id,
            agent_name: "Thomas".to_string(),
            summary: summary.to_string(),
            relevance: 1.0,
            emotion: 0.5,
            repetitions: 1,
            hours_ago: 0.0,
            participants: Vec::new(),
            tags: Vec::new(),
        }
    }

    #[test]
    fn rehydrate_missing_sources_degrades_without_replay() {
        let dir = tempfile::tempdir().unwrap();
        let request = RehydrateRequest::new(dir.path());

        let context = rehydrate_from_data_dir(&request).unwrap();

        assert_eq!(context.event_store.status, "missing");
        assert_eq!(context.projection.status, "missing");
        assert_eq!(context.memory_file.status, "missing");
        assert_eq!(context.hippocampus.status, "unavailable");
        assert_eq!(context.events_replayed, 0);
        assert_eq!(context.event_rows_loaded, 0);
        assert_eq!(context.event_copy_count, 0);
    }

    #[test]
    fn rehydrate_reads_existing_sources_without_event_rows() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path();

        let event_store_path = data_dir.join(EVENTS_DB_FILE_NAME);
        {
            let conn = rusqlite::Connection::open(&event_store_path).unwrap();
            conn.execute(
                "CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT)",
                [],
            )
            .unwrap();
        }

        let projection_path = data_dir.join(PROJECTION_DB_FILE_NAME);
        {
            let conn = rusqlite::Connection::open(&projection_path).unwrap();
            conn.execute(
                "CREATE TABLE agent_live_view (
                    agent_id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL,
                    role TEXT NOT NULL,
                    status TEXT NOT NULL,
                    current_room TEXT,
                    mood TEXT,
                    last_event_id INTEGER NOT NULL DEFAULT 0
                )",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO agent_live_view
                    (agent_id, name, role, status, current_room, mood, last_event_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    7,
                    "Thomas",
                    "Engineer",
                    "active",
                    "buero-dev-1",
                    "Focused",
                    11
                ],
            )
            .unwrap();
        }

        let memory_file = GaiaConsoleMemoryFile::open_or_create(data_dir).unwrap();
        memory_file
            .append_entry(
                MemorySection::Notes,
                12,
                "wake-up boundary remains read-only",
            )
            .unwrap();

        let hippocampus_path = data_dir.join(HIPPOCAMPUS_DB_FILE_NAME);
        {
            let store = HippocampusStore::open(&hippocampus_path.to_string_lossy()).unwrap();
            store
                .store_narrative(
                    "Thomas",
                    &NarrativeState {
                        agent_name: "Thomas".to_string(),
                        summary: "Knows the no-replay rule".to_string(),
                        episode_count: 1,
                    },
                )
                .unwrap();
            store
                .store_episodes("Thomas", &[episode(1, "Console memory restored")])
                .unwrap();
            store
                .store_fact("facts/projects/aurora", "Aurora is active")
                .unwrap();
        }

        let mut request = RehydrateRequest::new(data_dir);
        request.agent_name = Some("Thomas".to_string());
        request.fact_keys = vec!["facts/projects/aurora".to_string()];
        request.max_memory_bytes = 1024;

        let context = rehydrate_from_data_dir(&request).unwrap();

        assert_eq!(context.event_store.status, "ok");
        assert_eq!(context.event_store.latest_event_id, Some(0));
        assert_eq!(context.event_store.event_count, Some(0));
        assert_eq!(context.projection.status, "ok");
        assert_eq!(context.projection.active_agent_count, Some(1));
        assert_eq!(context.projection.active_agents[0].name, "Thomas");
        assert_eq!(
            context.projection.active_agents[0].current_room.as_deref(),
            Some("buero-dev-1")
        );
        assert_eq!(context.memory_file.status, "ok");
        assert!(context
            .memory_file
            .contents
            .contains("wake-up boundary remains read-only"));
        assert_eq!(context.hippocampus.status, "ok");
        assert_eq!(
            context.hippocampus.narrative_summary.as_deref(),
            Some("Knows the no-replay rule")
        );
        assert_eq!(context.hippocampus.facts[0].value, "Aurora is active");
        assert_eq!(context.events_replayed, 0);
        assert_eq!(context.event_rows_loaded, 0);
        assert_eq!(context.event_copy_count, 0);
    }
}
