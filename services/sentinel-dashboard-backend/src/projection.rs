//! Read-only Projection-Zugriff (#431) — liest `projection.db` (CQRS Read-Models von
//! `sentinel-projection`) read-only und liefert die ersten Dashboard-Views als JSON.
//!
//! **Read-only Pflicht** (Lehre aus #439-F3): unter systemd `ReadOnlyPaths=` scheitert ein
//! read-write-Open mit „attempt to write a readonly database". Wir oeffnen pro Request mit
//! `SQLITE_OPEN_READ_ONLY` (kein WAL-Write, kein DDL) — liest die Live-WAL-DB des Daemons.
//!
//! Tabellen (SSOT `sentinel-projection/src/store.rs`): `agent_live_view`, `room_live_view`,
//! `kpi_1m`, `task_kanban`.

use axum::{extract::State, http::StatusCode, response::{IntoResponse, Response}, Json};
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
pub async fn agents(State(st): State<AppState>) -> Response {
    let sql = "SELECT agent_id,name,role,shift_set,status,current_room,in_transit,transit_target,\
               last_action,last_action_tick,hunger,energy,stress,bladder,social_need,caffeine_mg,mood,\
               last_event_id,updated_at FROM agent_live_view ORDER BY agent_id";
    match query_json(&st.config.projection_db, sql, |r| {
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
    }) {
        Ok(rows) => Json(json!({ "agents": rows })).into_response(),
        Err(e) => db_unavailable(e),
    }
}

/// GET /api/rooms — Belegung + Physik je Raum.
pub async fn rooms(State(st): State<AppState>) -> Response {
    let sql = "SELECT room_id,occupant_count,transit_count,active_chaos,active_smells,temperature,\
               co2_ppm,noise_db,last_event_tick,last_event_id,updated_at FROM room_live_view ORDER BY room_id";
    match query_json(&st.config.projection_db, sql, |r| {
        Ok(json!({
            "room_id": r.get::<_, String>(0)?,
            "occupant_count": r.get::<_, i64>(1)?,
            "transit_count": r.get::<_, i64>(2)?,
            "active_chaos": r.get::<_, Option<String>>(3)?,
            "active_smells": r.get::<_, Option<String>>(4)?,
            "temperature": r.get::<_, Option<f64>>(5)?,
            "co2_ppm": r.get::<_, Option<f64>>(6)?,
            "noise_db": r.get::<_, Option<f64>>(7)?,
            "last_event_tick": r.get::<_, Option<i64>>(8)?,
            "last_event_id": r.get::<_, i64>(9)?,
            "updated_at": r.get::<_, i64>(10)?,
        }))
    }) {
        Ok(rows) => Json(json!({ "rooms": rows })).into_response(),
        Err(e) => db_unavailable(e),
    }
}

/// GET /api/metrics — letzter KPI-1m-Bucket.
pub async fn metrics(State(st): State<AppState>) -> Response {
    let sql = "SELECT bucket_start,active_agents,total_actions,total_transits,chaos_events,tick_count,\
               shift_changes,nightrun_events,updated_at FROM kpi_1m ORDER BY bucket_start DESC LIMIT 1";
    match query_json(&st.config.projection_db, sql, |r| {
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
    }) {
        Ok(mut rows) => Json(json!({ "kpi": rows.pop().unwrap_or(Value::Null) })).into_response(),
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
}
