//! Operator cockpit aggregation (#433): incidents + SLO state from events.db.

use axum::{
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
    Json,
};
use rusqlite::{params_from_iter, types::Value as SqlValue, Connection};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{events, AppState};

const SLO_LAG_THRESHOLD: i64 = 100;
const SLO_NIGHTRUN_FAILURE_RATE: f64 = 0.1;
const SLO_CHAOS_PER_HOUR: i64 = 3;
const SLO_DESPAWN_PER_HOUR: i64 = 2;
const PROXIMITY_WINDOW_TICKS: i64 = 200;
const INCIDENT_EVENT_TYPES: &[&str] = &[
    "chaos_triggered",
    "agent_consolidation_failed",
    "agent_despawned",
    "nightrun_completed",
    "platform_analysis",
    "platform_intervention",
];

#[derive(Debug, Deserialize)]
pub struct CockpitQuery {
    hours: Option<i64>,
    limit: Option<usize>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn severity_rank(severity: &str) -> i64 {
    match severity {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        _ => 4,
    }
}

fn event_severity(event_type: &str, payload: &Value) -> &'static str {
    match event_type {
        "chaos_triggered" | "agent_consolidation_failed" => "high",
        "agent_despawned" => "medium",
        "platform_analysis" => match payload
            .get("severity")
            .and_then(Value::as_str)
            .unwrap_or("medium")
            .to_ascii_lowercase()
            .as_str()
        {
            "critical" => "critical",
            "high" | "warning" => "high",
            "low" | "info" => "low",
            _ => "medium",
        },
        "platform_intervention" => {
            let action = payload
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            if action.contains("restart") {
                "high"
            } else {
                "medium"
            }
        }
        "nightrun_completed" => {
            if payload
                .get("agents_failed")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                > 0
            {
                "medium"
            } else {
                "low"
            }
        }
        _ => "medium",
    }
}

fn summarize_event(event_type: &str, payload: &Value, aggregate_id: &str) -> String {
    match event_type {
        "chaos_triggered" => format!(
            "Chaos: {} in {}",
            payload
                .get("event_type")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            aggregate_id
        ),
        "agent_consolidation_failed" => format!(
            "Konsolidierung fehlgeschlagen: {} - {}",
            payload
                .get("agent_name")
                .and_then(Value::as_str)
                .unwrap_or(aggregate_id),
            payload
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ),
        "agent_despawned" => format!(
            "Agent despawned: {} ({})",
            aggregate_id,
            payload
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ),
        "nightrun_completed" => format!(
            "Nightrun: {} konsolidiert, {} fehlgeschlagen",
            payload
                .get("agents_consolidated")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            payload
                .get("agents_failed")
                .and_then(Value::as_i64)
                .unwrap_or(0)
        ),
        "platform_analysis" => format!(
            "Platform Analyse {}: {} ({})",
            payload
                .get("severity")
                .and_then(Value::as_str)
                .unwrap_or("info")
                .to_ascii_uppercase(),
            payload
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or("ohne Summary"),
            payload
                .get("target")
                .and_then(Value::as_str)
                .unwrap_or(aggregate_id)
        ),
        "platform_intervention" => format!(
            "Platform Intervention: {} fuer {} - {}",
            payload
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            payload
                .get("target")
                .and_then(Value::as_str)
                .unwrap_or(aggregate_id),
            payload
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
        ),
        _ => format!("{event_type} on {aggregate_id}"),
    }
}

fn to_action(event: &events::EventRow) -> Value {
    let payload = events::parse_payload(&event.payload);
    json!({
        "event_id": event.event_id,
        "event_type": event.event_type,
        "agent_id": event.aggregate_id,
        "summary": summarize_event(&event.event_type, &payload, &event.aggregate_id),
        "tick": event.tick,
    })
}

fn query_related_actions(conn: &Connection, event: &events::EventRow) -> Vec<Value> {
    if !event.correlation_id.is_empty() {
        let columns = events::event_select_columns(conn);
        let sql = format!("SELECT {columns} FROM events WHERE correlation_id = ?1 ORDER BY id ASC");
        if let Ok(rows) =
            events::query_event_rows(conn, &sql, &[SqlValue::Text(event.correlation_id.clone())])
        {
            let actions = rows
                .iter()
                .filter(|row| row.event_id != event.event_id)
                .map(to_action)
                .collect::<Vec<_>>();
            if !actions.is_empty() {
                return actions;
            }
        }
    }

    let columns = events::event_select_columns(conn);
    let sql = format!("SELECT {columns} FROM events WHERE causation_id = ?1 ORDER BY id ASC");
    if let Ok(rows) =
        events::query_event_rows(conn, &sql, &[SqlValue::Text(event.event_id.clone())])
    {
        let actions = rows.iter().map(to_action).collect::<Vec<_>>();
        if !actions.is_empty() {
            return actions;
        }
    }

    let payload = events::parse_payload(&event.payload);
    let room_id = if event.event_type == "chaos_triggered" {
        payload
            .get("target_room")
            .and_then(Value::as_str)
            .unwrap_or(&event.aggregate_id)
            .to_string()
    } else {
        return Vec::new();
    };
    let sql = format!(
        "SELECT {columns} FROM events \
         WHERE tick BETWEEN ?1 AND ?2 AND event_type = 'agent_action_received' AND payload LIKE ?3 \
         ORDER BY tick ASC LIMIT 20"
    );
    let like = format!("%\"target_room\":\"{room_id}\"%");
    events::query_event_rows(
        conn,
        &sql,
        &[
            SqlValue::Integer(event.tick),
            SqlValue::Integer(event.tick + PROXIMITY_WINDOW_TICKS),
            SqlValue::Text(like),
        ],
    )
    .map(|rows| rows.iter().map(to_action).collect())
    .unwrap_or_default()
}

fn determine_outcome(actions: &[Value], event: &events::EventRow) -> (&'static str, Value) {
    if actions.is_empty() {
        if event.compensation_type != "none" {
            return (
                "resolved",
                Value::String(format!("Kompensation: {}", event.compensation_type)),
            );
        }
        return ("pending", Value::Null);
    }
    let last_type = actions
        .last()
        .and_then(|a| a.get("event_type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let last_summary = actions
        .last()
        .and_then(|a| a.get("summary"))
        .cloned()
        .unwrap_or(Value::Null);
    if [
        "transit_completed",
        "agent_consolidated",
        "bio_action_performed",
        "agent_spawned",
        "agent_action_received",
        "resource_profile_changed",
    ]
    .contains(&last_type)
    {
        return ("resolved", last_summary);
    }
    if ["agent_consolidation_failed", "agent_despawned"].contains(&last_type) {
        return ("failed", last_summary);
    }
    ("active", last_summary)
}

fn build_event_incident(conn: &Connection, event: &events::EventRow) -> Value {
    let payload = events::parse_payload(&event.payload);
    let severity = event_severity(&event.event_type, &payload);
    let actions = query_related_actions(conn, event);
    let (mut status, mut outcome) = determine_outcome(&actions, event);
    if status == "pending" && now_ms() - event.timestamp_ms > 30 * 60 * 1000 {
        status = "resolved";
        if outcome.is_null() {
            outcome = Value::String("Automatisch abgeschlossen".into());
        }
    }
    json!({
        "id": event.event_id,
        "source": "event",
        "incident_type": event.event_type,
        "severity": severity,
        "status": status,
        "agent_id": if event.aggregate_id.starts_with("AGENT-") { Value::String(event.aggregate_id.clone()) } else { Value::Null },
        "room_id": if event.event_type == "chaos_triggered" {
            payload.get("target_room").cloned().unwrap_or(Value::String(event.aggregate_id.clone()))
        } else {
            Value::Null
        },
        "summary": summarize_event(&event.event_type, &payload, &event.aggregate_id),
        "tick": event.tick,
        "timestamp_ms": event.timestamp_ms,
        "actions": actions,
        "outcome": outcome,
    })
}

fn evolution_incidents(conn: &Connection, hours: i64) -> Vec<Value> {
    let cutoff = now_ms() - hours * 3_600_000;
    let Ok(mut stmt) = conn.prepare(
        "SELECT id,agent_id,tick,field,change_type,old_value,new_value,created_at_ms \
         FROM personality_evolution \
         WHERE change_type IN ('drift','fatigue_spike','quality_shift') AND created_at_ms > ?1 \
         ORDER BY id DESC",
    ) else {
        return Vec::new();
    };
    stmt.query_map([cutoff], |row| {
        let change_type: String = row.get(4)?;
        let old_value: Option<String> = row.get(5)?;
        let new_value: String = row.get(6)?;
        let delta = match old_value {
            Some(old) => format!(" ({old} -> {new_value})"),
            None => format!(" ({new_value})"),
        };
        let agent_id: String = row.get(1)?;
        let field: String = row.get(3)?;
        let severity = if change_type == "fatigue_spike" {
            "high"
        } else {
            "medium"
        };
        Ok(json!({
            "id": row.get::<_, i64>(0)?.to_string(),
            "source": "evolution",
            "incident_type": change_type,
            "severity": severity,
            "status": "active",
            "agent_id": agent_id,
            "room_id": Value::Null,
            "summary": format!("{change_type}: {agent_id} {field}{delta}"),
            "tick": row.get::<_, i64>(2)?,
            "timestamp_ms": row.get::<_, i64>(7)?,
            "actions": [],
            "outcome": Value::Null,
        }))
    })
    .map(|rows| rows.filter_map(Result::ok).collect())
    .unwrap_or_default()
}

fn scalar_i64(conn: &Connection, sql: &str, params: &[SqlValue]) -> i64 {
    conn.query_row(sql, params_from_iter(params.iter()), |row| {
        row.get::<_, i64>(0)
    })
    .unwrap_or(0)
}

fn slo_violations(conn: &Connection) -> Vec<Value> {
    let mut violations = Vec::new();
    let max_id = scalar_i64(conn, "SELECT COALESCE(MAX(id), 0) FROM events", &[]);
    let offset = scalar_i64(
        conn,
        "SELECT COALESCE(last_event_id, 0) FROM projection_offsets WHERE projection_name = ?1",
        &[SqlValue::Text("sentinel-projection".into())],
    );
    let lag = (max_id - offset).max(0);
    if lag > SLO_LAG_THRESHOLD {
        violations.push(json!({
            "name": "Projection Lag",
            "current_value": lag,
            "threshold": SLO_LAG_THRESHOLD,
            "severity": if lag > SLO_LAG_THRESHOLD * 5 { "critical" } else { "high" },
            "description": format!("Lag {lag} Events (Grenze: {SLO_LAG_THRESHOLD})"),
        }));
    }

    let hour_cutoff = now_ms() - 3_600_000;
    let chaos_count = scalar_i64(
        conn,
        "SELECT COUNT(*) FROM events WHERE event_type = 'chaos_triggered' AND timestamp_ms > ?1",
        &[SqlValue::Integer(hour_cutoff)],
    );
    if chaos_count > SLO_CHAOS_PER_HOUR {
        violations.push(json!({
            "name": "Chaos-Frequenz",
            "current_value": chaos_count,
            "threshold": SLO_CHAOS_PER_HOUR,
            "severity": "high",
            "description": format!("{chaos_count} Chaos-Events/h (Grenze: {SLO_CHAOS_PER_HOUR})"),
        }));
    }

    let despawn_count = scalar_i64(
        conn,
        "SELECT COUNT(*) FROM events \
         WHERE event_type = 'agent_despawned' AND payload NOT LIKE '%\"reason\":\"shift\"%' AND timestamp_ms > ?1",
        &[SqlValue::Integer(hour_cutoff)],
    );
    if despawn_count > SLO_DESPAWN_PER_HOUR {
        violations.push(json!({
            "name": "Despawn-Rate",
            "current_value": despawn_count,
            "threshold": SLO_DESPAWN_PER_HOUR,
            "severity": "medium",
            "description": format!("{despawn_count} unerwartete Despawns/h (Grenze: {SLO_DESPAWN_PER_HOUR})"),
        }));
    }

    let nightrun_payload = conn
        .query_row(
            "SELECT payload FROM events WHERE event_type = 'nightrun_completed' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .map(|raw| events::parse_payload(&raw));
    if let Some(payload) = nightrun_payload {
        let consolidated = payload
            .get("agents_consolidated")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let failed = payload
            .get("agents_failed")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let total = consolidated + failed;
        if total > 0.0 && failed / total > SLO_NIGHTRUN_FAILURE_RATE {
            violations.push(json!({
                "name": "Nightrun Failure-Rate",
                "current_value": ((failed / total) * 100.0).round() as i64,
                "threshold": (SLO_NIGHTRUN_FAILURE_RATE * 100.0).round() as i64,
                "severity": if failed / total > 0.5 { "critical" } else { "high" },
                "description": format!("{failed:.0}/{total:.0} fehlgeschlagen"),
            }));
        }
    }

    violations
}

fn build_response(conn: &Connection, hours: i64, limit: usize) -> Value {
    let cutoff = now_ms() - hours * 3_600_000;
    let columns = events::event_select_columns(conn);
    let placeholders = INCIDENT_EVENT_TYPES
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT {columns} FROM events \
         WHERE event_type IN ({placeholders}) AND timestamp_ms > ? \
         ORDER BY id DESC LIMIT ?"
    );
    let mut params = INCIDENT_EVENT_TYPES
        .iter()
        .map(|t| SqlValue::Text((*t).into()))
        .collect::<Vec<_>>();
    params.push(SqlValue::Integer(cutoff));
    params.push(SqlValue::Integer(limit as i64));

    let mut incidents = events::query_event_rows(conn, &sql, &params)
        .unwrap_or_default()
        .iter()
        .map(|event| build_event_incident(conn, event))
        .filter(|incident| incident["severity"] != "low")
        .collect::<Vec<_>>();
    incidents.extend(evolution_incidents(conn, hours));
    incidents.sort_by(|a, b| {
        let sev = severity_rank(a["severity"].as_str().unwrap_or(""))
            .cmp(&severity_rank(b["severity"].as_str().unwrap_or("")));
        if sev == std::cmp::Ordering::Equal {
            b["timestamp_ms"].as_i64().cmp(&a["timestamp_ms"].as_i64())
        } else {
            sev
        }
    });

    let total_active = incidents
        .iter()
        .filter(|incident| matches!(incident["status"].as_str(), Some("active" | "pending")))
        .count();
    let total_resolved = incidents
        .iter()
        .filter(|incident| incident["status"].as_str() == Some("resolved"))
        .count();

    json!({
        "incidents": incidents,
        "slo_violations": slo_violations(conn),
        "total_active": total_active,
        "total_resolved_24h": total_resolved,
        "events_db": "ok",
    })
}

pub async fn cockpit(State(st): State<AppState>, Query(q): Query<CockpitQuery>) -> Response {
    let hours = q.hours.unwrap_or(24).clamp(1, 168);
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let conn = match events::open_events_ro(&st.config.events_db) {
        Ok(conn) => conn,
        Err(e) => {
            tracing::warn!(error = %e, path = %st.config.events_db, "cockpit degraded: events.db unavailable");
            return Json(json!({
                "incidents": [],
                "slo_violations": [],
                "total_active": 0,
                "total_resolved_24h": 0,
                "events_db": "offline",
            }))
            .into_response();
        }
    };
    Json(build_response(&conn, hours, limit)).into_response()
}

pub async fn incident(Path(id): Path<String>, State(st): State<AppState>) -> Response {
    let conn = match events::open_events_ro(&st.config.events_db) {
        Ok(conn) => conn,
        Err(e) => {
            tracing::warn!(error = %e, path = %st.config.events_db, "incident degraded: events.db unavailable");
            return Json(json!({"error": "events db offline"})).into_response();
        }
    };
    let columns = events::event_select_columns(&conn);
    let sql = format!("SELECT {columns} FROM events WHERE event_id = ?1");
    let rows = events::query_event_rows(&conn, &sql, &[SqlValue::Text(id)]).unwrap_or_default();
    match rows.first() {
        Some(event) => Json(build_event_incident(&conn, event)).into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({"error": "incident not found"})),
        )
            .into_response(),
    }
}
