//! Read-only EventStore surfaces for the SolidJS console migration (#433).
//!
//! The dashboard backend is a read consumer: every SQLite connection is opened
//! with `SQLITE_OPEN_READ_ONLY` and `PRAGMA query_only = ON`.

use std::collections::HashMap;
use std::time::Duration;

use axum::{
    extract::{Query, State},
    response::{IntoResponse, Response},
    Json,
};
use rusqlite::{
    params, params_from_iter, types::Value as SqlValue, Connection, OpenFlags, OptionalExtension,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

pub(crate) const ROOM_REACTION_WINDOW_TICKS: i64 = 60;

#[derive(Debug, Clone)]
pub(crate) struct EventRow {
    pub id: i64,
    pub event_id: String,
    pub event_type: String,
    pub aggregate_id: String,
    pub payload: String,
    pub correlation_id: String,
    pub causation_id: Option<String>,
    pub tick: i64,
    pub timestamp_ms: i64,
    pub compensation_type: String,
}

pub(crate) fn open_events_ro(path: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(Duration::from_millis(3000))?;
    conn.execute_batch("PRAGMA query_only = ON;")?;
    Ok(conn)
}

pub(crate) fn parse_payload(raw: &str) -> Value {
    serde_json::from_str::<Value>(raw).unwrap_or(Value::Null)
}

fn event_column_exists(conn: &Connection, column: &str) -> bool {
    let Ok(mut stmt) = conn.prepare("PRAGMA table_info(events)") else {
        return false;
    };
    let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(1)) else {
        return false;
    };
    let mut exists = false;
    for name in rows.filter_map(Result::ok) {
        if name == column {
            exists = true;
            break;
        }
    }
    exists
}

pub(crate) fn event_select_columns(conn: &Connection) -> String {
    let base =
        "id,event_id,event_type,aggregate_id,payload,correlation_id,causation_id,tick,timestamp_ms";
    if event_column_exists(conn, "compensation_type") {
        format!("{base},compensation_type")
    } else {
        format!("{base},'none' AS compensation_type")
    }
}

pub(crate) fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        id: row.get(0)?,
        event_id: row.get(1)?,
        event_type: row.get(2)?,
        aggregate_id: row.get(3)?,
        payload: row.get(4)?,
        correlation_id: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
        causation_id: row.get(6)?,
        tick: row.get(7)?,
        timestamp_ms: row.get(8)?,
        compensation_type: row.get(9)?,
    })
}

pub(crate) fn event_to_json(event: &EventRow) -> Value {
    json!({
        "id": event.id,
        "event_id": event.event_id,
        "event_type": event.event_type,
        "aggregate_id": event.aggregate_id,
        "payload": event.payload,
        "correlation_id": event.correlation_id,
        "causation_id": event.causation_id,
        "tick": event.tick,
        "timestamp_ms": event.timestamp_ms,
        "compensation_type": event.compensation_type,
    })
}

fn offline_events_payload(limit: usize, offset: usize) -> Value {
    json!({
        "events": [],
        "total": 0,
        "limit": limit,
        "offset": offset,
        "events_db": "offline",
    })
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    #[serde(rename = "type")]
    event_type: Option<String>,
    agent: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
    since: Option<i64>,
}

pub async fn events(State(st): State<AppState>, Query(q): Query<EventsQuery>) -> Response {
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let offset = q.offset.unwrap_or(0);
    let conn = match open_events_ro(&st.config.events_db) {
        Ok(conn) => conn,
        Err(e) => {
            tracing::warn!(error = %e, path = %st.config.events_db, "events route degraded: events.db unavailable");
            return Json(offline_events_payload(limit, offset)).into_response();
        }
    };

    let columns = event_select_columns(&conn);
    let mut conditions = Vec::new();
    let mut params = Vec::<SqlValue>::new();
    if let Some(event_type) = q.event_type.filter(|s| !s.is_empty()) {
        conditions.push("event_type = ?");
        params.push(SqlValue::Text(event_type));
    }
    if let Some(agent) = q.agent.filter(|s| !s.is_empty()) {
        conditions.push("aggregate_id = ?");
        params.push(SqlValue::Text(agent));
    }
    if let Some(since) = q.since {
        conditions.push("timestamp_ms > ?");
        params.push(SqlValue::Integer(since));
    }
    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    let count_sql = format!("SELECT COUNT(*) FROM events {where_clause}");
    let total = conn
        .query_row(&count_sql, params_from_iter(params.iter()), |row| {
            row.get::<_, i64>(0)
        })
        .unwrap_or(0);

    let mut query_params = params;
    query_params.push(SqlValue::Integer(limit as i64));
    query_params.push(SqlValue::Integer(offset as i64));
    let sql =
        format!("SELECT {columns} FROM events {where_clause} ORDER BY id DESC LIMIT ? OFFSET ?");
    let events = match query_event_rows(&conn, &sql, &query_params) {
        Ok(rows) => rows.iter().map(event_to_json).collect::<Vec<_>>(),
        Err(e) => {
            tracing::warn!(error = %e, "events query failed");
            Vec::new()
        }
    };

    Json(json!({
        "events": events,
        "total": total,
        "limit": limit,
        "offset": offset,
        "events_db": "ok",
    }))
    .into_response()
}

pub async fn event_types(State(st): State<AppState>) -> Response {
    let conn = match open_events_ro(&st.config.events_db) {
        Ok(conn) => conn,
        Err(e) => {
            tracing::warn!(error = %e, path = %st.config.events_db, "event-types route degraded");
            return Json(json!({"types": [], "events_db": "offline"})).into_response();
        }
    };
    let mut stmt = match conn.prepare(
        "SELECT event_type, COUNT(*) as cnt FROM events GROUP BY event_type ORDER BY cnt DESC",
    ) {
        Ok(stmt) => stmt,
        Err(e) => {
            tracing::warn!(error = %e, "event-types query failed");
            return Json(json!({"types": [], "events_db": "offline"})).into_response();
        }
    };
    let rows = stmt
        .query_map([], |row| {
            Ok(json!({
                "event_type": row.get::<_, String>(0)?,
                "cnt": row.get::<_, i64>(1)?,
            }))
        })
        .map(|rows| rows.filter_map(Result::ok).collect::<Vec<_>>())
        .unwrap_or_default();
    Json(json!({"types": rows, "events_db": "ok"})).into_response()
}

pub(crate) fn query_event_rows(
    conn: &Connection,
    sql: &str,
    params: &[SqlValue],
) -> rusqlite::Result<Vec<EventRow>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params_from_iter(params.iter()), row_to_event)?;
    rows.collect()
}

pub(crate) fn room_history(db_path: &str, room_id: &str) -> Value {
    let conn = match open_events_ro(db_path) {
        Ok(conn) => conn,
        Err(e) => {
            tracing::warn!(error = %e, room = %room_id, "room history degraded: events.db unavailable");
            return json!({
                "physics_history": [],
                "chaos_history": [],
                "stimulus_history": [],
                "recent_reactions": [],
                "reaction_window_ticks": ROOM_REACTION_WINDOW_TICKS,
                "events_db": "offline",
            });
        }
    };
    json!({
        "physics_history": room_physics_history(&conn, room_id),
        "chaos_history": chaos_history(&conn, room_id, 25),
        "stimulus_history": stimulus_history(&conn, room_id, 25),
        "recent_reactions": recent_room_reactions(&conn, room_id, 20),
        "reaction_window_ticks": ROOM_REACTION_WINDOW_TICKS,
        "events_db": "ok",
    })
}

fn room_physics_history(conn: &Connection, room_id: &str) -> Vec<Value> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT payload,tick,timestamp_ms FROM events \
         WHERE event_type = 'room_physics_updated' AND aggregate_id = ?1 \
         ORDER BY id DESC LIMIT 30",
    ) else {
        return Vec::new();
    };
    let rows = stmt
        .query_map([room_id], |row| {
            let payload = parse_payload(&row.get::<_, String>(0)?);
            Ok(json!({
                "tick": row.get::<_, i64>(1)?,
                "timestamp_ms": row.get::<_, i64>(2)?,
                "temperature": payload.get("temperature").cloned().unwrap_or(Value::Null),
                "co2_ppm": payload.get("co2_ppm").cloned().unwrap_or(Value::Null),
                "noise_db": payload.get("noise_db").cloned().unwrap_or(Value::Null),
                "occupant_count": payload.get("occupant_count").cloned().unwrap_or(json!(0)),
            }))
        })
        .map(|rows| rows.filter_map(Result::ok).collect::<Vec<_>>())
        .unwrap_or_default();
    rows.into_iter().rev().collect()
}

fn chaos_history(conn: &Connection, room_id: &str, limit: usize) -> Vec<Value> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT id,event_id,aggregate_id,payload,tick,timestamp_ms FROM events \
         WHERE event_type = 'chaos_triggered' AND (aggregate_id = ?1 OR payload LIKE ?2) \
         ORDER BY id DESC LIMIT ?3",
    ) else {
        return Vec::new();
    };
    let like = format!("%\"target_room\":\"{room_id}\"%");
    stmt.query_map(params![room_id, like, limit as i64], |row| {
        let payload = parse_payload(&row.get::<_, String>(3)?);
        Ok(json!({
            "id": row.get::<_, i64>(0)?,
            "event_id": row.get::<_, String>(1)?,
            "chaos_type": payload.get("event_type").and_then(Value::as_str).unwrap_or("unknown"),
            "room_id": room_id,
            "description": payload.get("description").and_then(Value::as_str).unwrap_or(""),
            "tick": row.get::<_, i64>(4)?,
            "timestamp_ms": row.get::<_, i64>(5)?,
        }))
    })
    .map(|rows| rows.filter_map(Result::ok).collect())
    .unwrap_or_default()
}

fn stimulus_history(conn: &Connection, room_id: &str, limit: usize) -> Vec<Value> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT event_id,aggregate_id,payload,tick,timestamp_ms FROM events \
         WHERE event_type = 'room_stimulus_applied' AND aggregate_id = ?1 \
         ORDER BY id DESC LIMIT ?2",
    ) else {
        return Vec::new();
    };
    let rows = stmt
        .query_map(params![room_id, limit as i64], |row| {
            let payload = parse_payload(&row.get::<_, String>(2)?);
            Ok(json!({
                "event_id": row.get::<_, String>(0)?,
                "room_id": row.get::<_, String>(1)?,
                "stimulus_type": payload.get("stimulus_type").and_then(Value::as_str).unwrap_or("unknown"),
                "delta": payload.get("delta").cloned().unwrap_or(json!(0)),
                "description": payload.get("description").and_then(Value::as_str).unwrap_or(""),
                "tick": row.get::<_, i64>(3)?,
                "timestamp_ms": row.get::<_, i64>(4)?,
            }))
        })
        .map(|rows| rows.filter_map(Result::ok).collect::<Vec<_>>())
        .unwrap_or_default();
    rows.into_iter().rev().collect()
}

fn recent_room_reactions(conn: &Connection, room_id: &str, limit: usize) -> Vec<Value> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT event_id,aggregate_id,payload,correlation_id,tick,timestamp_ms FROM events \
         WHERE event_type = 'agent_action_received' AND payload LIKE ?1 \
         ORDER BY id DESC LIMIT ?2",
    ) else {
        return Vec::new();
    };
    let like = format!("%\"target_room\":\"{room_id}\"%");
    stmt.query_map(params![like, limit as i64], |row| {
        let payload = parse_payload(&row.get::<_, String>(2)?);
        Ok(json!({
            "event_id": row.get::<_, String>(0)?,
            "agent_id": payload.get("agent_id").cloned().unwrap_or(Value::String(row.get::<_, String>(1)?)),
            "agent_name": payload.get("name").and_then(Value::as_str).unwrap_or(""),
            "action_type": payload.get("action_type").and_then(Value::as_str).unwrap_or(""),
            "content": payload.get("content").cloned().unwrap_or(Value::Null),
            "target_room": payload.get("target_room").cloned().unwrap_or(Value::Null),
            "tick": row.get::<_, i64>(4)?,
            "timestamp_ms": row.get::<_, i64>(5)?,
            "correlation_id": row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            "chaos_event_id": Value::Null,
            "chaos_type": Value::Null,
            "chaos_description": Value::Null,
            "chaos_tick": Value::Null,
            "stimulus_event_id": Value::Null,
            "stimulus_type": Value::Null,
            "stimulus_description": Value::Null,
            "stimulus_tick": Value::Null,
        }))
    })
    .map(|rows| rows.filter_map(Result::ok).collect())
    .unwrap_or_default()
}

pub(crate) fn platform_analyses_json(db_path: &str, limit: usize) -> Value {
    let conn = match open_events_ro(db_path) {
        Ok(conn) => conn,
        Err(e) => {
            tracing::warn!(error = %e, "platform analyses degraded");
            return json!([]);
        }
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT event_id,aggregate_id,payload,tick,timestamp_ms FROM events \
         WHERE event_type = 'platform_analysis' ORDER BY id DESC LIMIT ?1",
    ) else {
        return json!([]);
    };
    let rows = stmt
        .query_map([limit as i64], |row| {
            let payload = parse_payload(&row.get::<_, String>(2)?);
            Ok(json!({
                "event_id": row.get::<_, String>(0)?,
                "aggregate_id": row.get::<_, String>(1)?,
                "trigger": payload.get("trigger").and_then(Value::as_str).unwrap_or("unknown"),
                "severity": payload.get("severity").and_then(Value::as_str).unwrap_or("info"),
                "summary": payload.get("summary").and_then(Value::as_str).unwrap_or(""),
                "recommendation": payload.get("recommendation").and_then(Value::as_str).unwrap_or(""),
                "suggested_action": payload.get("suggested_action").cloned().unwrap_or(Value::Null),
                "target": payload.get("target").and_then(Value::as_str).unwrap_or(""),
                "provider": payload.get("provider").cloned().unwrap_or(Value::Null),
                "model": payload.get("model").cloned().unwrap_or(Value::Null),
                "unresolved_keys": payload.get("unresolved_keys").cloned().unwrap_or(json!([])),
                "parameters": payload.get("parameters").cloned().unwrap_or(json!({})),
                "tick": row.get::<_, i64>(3)?,
                "timestamp_ms": row.get::<_, i64>(4)?,
            }))
        })
        .map(|rows| rows.filter_map(Result::ok).collect::<Vec<_>>())
        .unwrap_or_default();
    Value::Array(rows)
}

pub(crate) fn snapshot_state_json(
    db_path: &str,
    snapshot_id: &str,
) -> rusqlite::Result<Option<Value>> {
    let conn = open_events_ro(db_path)?;
    let meta = conn
        .query_row(
            "SELECT id,tier,tick,sim_hour,last_event_id,payload_size,created_at \
             FROM world_snapshots WHERE id = ?1",
            [snapshot_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((id, tier, tick, sim_hour, last_event_id, payload_size, created_at)) = meta else {
        return Ok(None);
    };

    let mut agent_room: HashMap<i64, String> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT event_type,payload FROM events \
         WHERE id <= ?1 AND event_type IN ('agent_spawned','transit_completed','agent_despawned') \
         ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([last_event_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows.filter_map(Result::ok) {
        let payload = parse_payload(&row.1);
        let Some(agent_id) = payload.get("agent_id").and_then(Value::as_i64) else {
            continue;
        };
        if row.0 == "agent_despawned" {
            agent_room.remove(&agent_id);
        } else if let Some(room_id) = payload.get("room_id").and_then(Value::as_str) {
            agent_room.insert(agent_id, room_id.to_string());
        }
    }

    let mut room_counts: HashMap<String, i64> = HashMap::new();
    for room in agent_room.values() {
        *room_counts.entry(room.clone()).or_insert(0) += 1;
    }
    let mut rooms = room_counts
        .into_iter()
        .map(|(room_id, count)| {
            json!({
                "room_id": room_id,
                "name": room_id,
                "occupant_count": count,
            })
        })
        .collect::<Vec<_>>();
    rooms.sort_by(|a, b| {
        b["occupant_count"]
            .as_i64()
            .cmp(&a["occupant_count"].as_i64())
            .then_with(|| a["room_id"].as_str().cmp(&b["room_id"].as_str()))
    });

    Ok(Some(json!({
        "snapshot_id": id,
        "tier": tier,
        "tick": tick,
        "sim_hour": sim_hour,
        "last_event_id": last_event_id,
        "payload_size": payload_size,
        "created_at_ms": created_at,
        "active_agent_count": Value::Null,
        "present_agent_count": agent_room.len(),
        "room_count": rooms.len(),
        "rooms": rooms,
    })))
}
