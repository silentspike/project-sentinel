//! Read-only Projection-Zugriff (#431) — liest `projection.db` (CQRS Read-Models von
//! `sentinel-projection`) read-only und liefert die ersten Dashboard-Views als JSON.
//!
//! **Read-only Pflicht** (Lehre aus #439-F3): unter systemd `ReadOnlyPaths=` scheitert ein
//! read-write-Open mit „attempt to write a readonly database". Wir oeffnen pro Request mit
//! `SQLITE_OPEN_READ_ONLY` (kein WAL-Write, kein DDL) — liest die Live-WAL-DB des Daemons.
//!
//! Tabellen (SSOT `sentinel-projection/src/store.rs`): `agent_live_view`, `room_live_view`,
//! `kpi_1m`, `task_kanban`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

use crate::AppState;

/// Oeffnet die Projection-DB strikt read-only (siehe Modul-Doku).
fn open_ro(path: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(std::time::Duration::from_millis(3000))?;
    Ok(conn)
}

/// Mappt einen DB-Fehler auf 503 (Projection nicht lesbar) statt zu panicken.
fn db_unavailable(e: rusqlite::Error) -> Response {
    tracing::warn!(error = %e, "projection read failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": format!("projection unavailable: {e}")})),
    )
        .into_response()
}

fn parse_optional_json(raw: Option<String>) -> Value {
    raw.and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null)
}

/// Fuehrt eine SELECT-Query aus und mappt jede Zeile via `row_to_json` zu einem JSON-Objekt.
fn query_json<F>(path: &str, sql: &str, row_to_json: F) -> Result<Vec<Value>, rusqlite::Error>
where
    F: Fn(&rusqlite::Row) -> rusqlite::Result<Value>,
{
    let conn = open_ro(path)?;
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| row_to_json(row))?;
    rows.collect()
}

/// GET /api/agents — aktueller Zustand aller Agenten (Bio + Position).
/// Liest die Agent-Read-Models als JSON-Zeilen (read-only). Wiederverwendet von der HTTP-Route
/// `agents` **und** vom WebTransport-Connect-Snapshot (`wt.rs`).
pub fn agents_rows(db_path: &str) -> Result<Vec<Value>, rusqlite::Error> {
    let sql = "SELECT agent_id,name,role,shift_set,status,current_room,in_transit,transit_target,\
               last_action,last_action_tick,hunger,energy,stress,bladder,social_need,caffeine_mg,mood,\
               last_event_id,updated_at FROM agent_live_view ORDER BY agent_id";
    query_json(db_path, sql, |r| {
        Ok(json!({
            "agent_id": r.get::<_, i64>(0)?,
            "name": r.get::<_, String>(1)?,
            "role": r.get::<_, String>(2)?,
            "shift_set": r.get::<_, i64>(3)?,
            "status": r.get::<_, String>(4)?,
            "current_room": r.get::<_, Option<String>>(5)?,
            "in_transit": r.get::<_, i64>(6)? != 0,
            "transit_target": r.get::<_, Option<String>>(7)?,
            "last_action": r.get::<_, Option<String>>(8)?,
            "last_action_tick": r.get::<_, Option<i64>>(9)?,
            "hunger": r.get::<_, f64>(10)?,
            "energy": r.get::<_, f64>(11)?,
            "stress": r.get::<_, f64>(12)?,
            "bladder": r.get::<_, f64>(13)?,
            "social_need": r.get::<_, f64>(14)?,
            "caffeine_mg": r.get::<_, f64>(15)?,
            "mood": r.get::<_, Option<String>>(16)?,
            "last_event_id": r.get::<_, i64>(17)?,
            "updated_at": r.get::<_, i64>(18)?,
        }))
    })
}

pub async fn agents(State(st): State<AppState>) -> Response {
    match agents_rows(&st.config.projection_db) {
        Ok(rows) => Json(json!({ "agents": rows })).into_response(),
        Err(e) => db_unavailable(e),
    }
}

/// GET /api/rooms — wiederverwendbarer voller `room_live_view`-Satz.
pub fn rooms_rows(db_path: &str) -> Result<Vec<Value>, rusqlite::Error> {
    let sql = "SELECT room_id,occupant_count,transit_count,active_chaos,active_smells,temperature,\
               co2_ppm,noise_db,last_event_tick,last_event_id,updated_at FROM room_live_view ORDER BY room_id";
    query_json(db_path, sql, |r| {
        Ok(json!({
            "room_id": r.get::<_, String>(0)?,
            "occupant_count": r.get::<_, i64>(1)?,
            "transit_count": r.get::<_, i64>(2)?,
            "active_chaos": parse_optional_json(r.get::<_, Option<String>>(3)?),
            "active_smells": parse_optional_json(r.get::<_, Option<String>>(4)?),
            "temperature": r.get::<_, Option<f64>>(5)?,
            "co2_ppm": r.get::<_, Option<f64>>(6)?,
            "noise_db": r.get::<_, Option<f64>>(7)?,
            "last_event_tick": r.get::<_, Option<i64>>(8)?,
            "last_event_id": r.get::<_, i64>(9)?,
            "updated_at": r.get::<_, i64>(10)?,
        }))
    })
}

fn room_row(db_path: &str, room_id: &str) -> Result<Option<Value>, rusqlite::Error> {
    let conn = open_ro(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT room_id,occupant_count,transit_count,active_chaos,active_smells,temperature,\
         co2_ppm,noise_db,last_event_tick,last_event_id,updated_at FROM room_live_view WHERE room_id = ?1",
    )?;
    let mut rows = stmt.query([room_id])?;
    let Some(r) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(json!({
        "room_id": r.get::<_, String>(0)?,
        "occupant_count": r.get::<_, i64>(1)?,
        "transit_count": r.get::<_, i64>(2)?,
        "active_chaos": parse_optional_json(r.get::<_, Option<String>>(3)?),
        "active_smells": parse_optional_json(r.get::<_, Option<String>>(4)?),
        "temperature": r.get::<_, Option<f64>>(5)?,
        "co2_ppm": r.get::<_, Option<f64>>(6)?,
        "noise_db": r.get::<_, Option<f64>>(7)?,
        "last_event_tick": r.get::<_, Option<i64>>(8)?,
        "last_event_id": r.get::<_, i64>(9)?,
        "updated_at": r.get::<_, i64>(10)?,
    })))
}

fn room_occupants(db_path: &str, room_id: &str) -> Result<Vec<Value>, rusqlite::Error> {
    let conn = open_ro(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT agent_id,name,status FROM agent_live_view \
         WHERE current_room = ?1 AND status != 'despawned' ORDER BY agent_id",
    )?;
    let rows = stmt.query_map([room_id], |r| {
        Ok(json!({
            "agent_id": r.get::<_, i64>(0)?,
            "name": r.get::<_, String>(1)?,
            "status": r.get::<_, String>(2)?,
        }))
    })?;
    rows.collect()
}

/// GET /api/rooms — Belegung + Physik je Raum.
pub async fn rooms(State(st): State<AppState>) -> Response {
    match rooms_rows(&st.config.projection_db) {
        Ok(rows) => Json(json!({ "rooms": rows })).into_response(),
        Err(e) => db_unavailable(e),
    }
}

/// GET /api/rooms/:id/detail — room_live + EventStore-Historien.
pub async fn room_detail(Path(room_id): Path<String>, State(st): State<AppState>) -> Response {
    let mut room = match room_row(&st.config.projection_db, &room_id) {
        Ok(Some(room)) => room,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "room not found"})),
            )
                .into_response()
        }
        Err(e) => return db_unavailable(e),
    };

    let occupants = match room_occupants(&st.config.projection_db, &room_id) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, room = %room_id, "room occupants unavailable");
            Vec::new()
        }
    };
    let history = crate::events::room_history(&st.config.events_db, &room_id);
    let obj = room.as_object_mut().expect("room row is object");
    obj.insert("id".into(), Value::String(room_id.clone()));
    obj.insert("occupants".into(), Value::Array(occupants));
    if let Some(history) = history.as_object() {
        for (key, value) in history {
            obj.insert(key.clone(), value.clone());
        }
    }
    Json(room).into_response()
}

/// Letzter KPI-1m-Bucket als JSON-Wert. Wiederverwendet von Route und Push.
pub fn metrics_row(db_path: &str) -> Result<Value, rusqlite::Error> {
    let sql = "SELECT bucket_start,active_agents,total_actions,total_transits,chaos_events,tick_count,\
               shift_changes,nightrun_events,updated_at FROM kpi_1m ORDER BY bucket_start DESC LIMIT 1";
    let mut rows = query_json(db_path, sql, |r| {
        Ok(json!({
            "bucket_start": r.get::<_, i64>(0)?,
            "active_agents": r.get::<_, i64>(1)?,
            "total_actions": r.get::<_, i64>(2)?,
            "total_transits": r.get::<_, i64>(3)?,
            "chaos_events": r.get::<_, i64>(4)?,
            "tick_count": r.get::<_, i64>(5)?,
            "shift_changes": r.get::<_, i64>(6)?,
            "nightrun_events": r.get::<_, i64>(7)?,
            "updated_at": r.get::<_, i64>(8)?,
        }))
    })?;
    Ok(rows.pop().unwrap_or(Value::Null))
}

/// GET /api/metrics — letzter KPI-1m-Bucket.
pub async fn metrics(State(st): State<AppState>) -> Response {
    match metrics_row(&st.config.projection_db) {
        Ok(kpi) => Json(json!({ "kpi": kpi })).into_response(),
        Err(e) => db_unavailable(e),
    }
}

/// GET /api/tasks — Task-Kanban (#438).
pub async fn tasks(State(st): State<AppState>) -> Response {
    let sql = "SELECT task_id,title,assigned_to,assigned_by,parent_task,status,result \
               FROM task_kanban ORDER BY task_id";
    match query_json(&st.config.projection_db, sql, |r| {
        Ok(json!({
            "task_id": r.get::<_, i64>(0)?,
            "title": r.get::<_, String>(1)?,
            "assigned_to": r.get::<_, i64>(2)?,
            "assigned_by": r.get::<_, Option<i64>>(3)?,
            "parent_task": r.get::<_, Option<i64>>(4)?,
            "status": r.get::<_, String>(5)?,
            "result": r.get::<_, Option<String>>(6)?,
        }))
    }) {
        Ok(rows) => Json(json!({ "tasks": rows })).into_response(),
        Err(e) => db_unavailable(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Baut eine Test-DB mit dem Read-Model-Schema + einem Agenten und prueft den read-only Pfad.
    #[test]
    fn agents_query_reads_readonly() {
        let dir = std::env::temp_dir().join(format!("pdb-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("projection.db");
        {
            let c = Connection::open(&db).unwrap();
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
        // read-only Open + Query funktioniert
        let rows = query_json(db.to_str().unwrap(),
            "SELECT agent_id,name,role,shift_set,status,current_room,in_transit,transit_target,last_action,\
             last_action_tick,hunger,energy,stress,bladder,social_need,caffeine_mg,mood,last_event_id,updated_at \
             FROM agent_live_view ORDER BY agent_id",
            |r| Ok(json!({"agent_id": r.get::<_,i64>(0)?, "name": r.get::<_,String>(1)?})),
        ).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "Thomas");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rooms_rows_reads_room_live_view() {
        let dir = std::env::temp_dir().join(format!("pdb-rooms-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("projection.db");
        {
            let c = Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE room_live_view (room_id TEXT PRIMARY KEY,occupant_count INTEGER NOT NULL,\
                 transit_count INTEGER NOT NULL,active_chaos TEXT,active_smells TEXT,temperature REAL,\
                 co2_ppm REAL,noise_db REAL,last_event_tick INTEGER,last_event_id INTEGER NOT NULL,\
                 updated_at INTEGER NOT NULL);\
                 INSERT INTO room_live_view VALUES ('kueche',3,1,'[\"coffee_spill\"]','[\"espresso\"]',22.5,\
                 710.0,48.0,42,99,1234);",
            )
            .unwrap();
        }

        let rows = rooms_rows(db.to_str().unwrap()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["room_id"], "kueche");
        assert_eq!(rows[0]["occupant_count"], 3);
        assert_eq!(rows[0]["active_chaos"][0], "coffee_spill");
        assert_eq!(rows[0]["active_smells"][0], "espresso");
        assert_eq!(rows[0]["last_event_id"], 99);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn metrics_row_reads_latest_kpi_bucket() {
        let dir = std::env::temp_dir().join(format!("pdb-kpi-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("projection.db");
        {
            let c = Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE kpi_1m (bucket_start INTEGER PRIMARY KEY,active_agents INTEGER NOT NULL,\
                 total_actions INTEGER NOT NULL,total_transits INTEGER NOT NULL,chaos_events INTEGER NOT NULL,\
                 tick_count INTEGER NOT NULL,shift_changes INTEGER NOT NULL,nightrun_events INTEGER NOT NULL,\
                 updated_at INTEGER NOT NULL);\
                 INSERT INTO kpi_1m VALUES (1000,12,30,4,1,60,0,0,1100);\
                 INSERT INTO kpi_1m VALUES (2000,13,44,5,2,61,1,1,2100);",
            )
            .unwrap();
        }

        let kpi = metrics_row(db.to_str().unwrap()).unwrap();
        assert_eq!(kpi["bucket_start"], 2000);
        assert_eq!(kpi["active_agents"], 13);
        assert_eq!(kpi["total_actions"], 44);
        assert_eq!(kpi["chaos_events"], 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn metrics_row_returns_null_when_no_bucket_exists() {
        let dir = std::env::temp_dir().join(format!("pdb-empty-kpi-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("projection.db");
        {
            let c = Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE kpi_1m (bucket_start INTEGER PRIMARY KEY,active_agents INTEGER NOT NULL,\
                 total_actions INTEGER NOT NULL,total_transits INTEGER NOT NULL,chaos_events INTEGER NOT NULL,\
                 tick_count INTEGER NOT NULL,shift_changes INTEGER NOT NULL,nightrun_events INTEGER NOT NULL,\
                 updated_at INTEGER NOT NULL);",
            )
            .unwrap();
        }

        assert!(metrics_row(db.to_str().unwrap()).unwrap().is_null());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
