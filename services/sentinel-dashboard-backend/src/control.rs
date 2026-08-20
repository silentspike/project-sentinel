//! Control-Proxy (#431) — leitet mutierende Operator-Befehle an die Backend-Dienste weiter:
//! Operator-API (Daemon, `:8084`, Header `x-sentinel-operator-key`) und Gateway-Control (`:8081`).
//! Control- und Operator-Proxies fuer die SolidJS-Konsole. Alle Routen laufen hinter `require_auth`.

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

const OPERATOR_KEY_HEADER: &str = "x-sentinel-operator-key";
const MAX_DELIVERY_LINEAGE_BYTES: u64 = 256 * 1024;

/// Generischer Upstream-Forward: Status + Body werden durchgereicht (JSON).
/// `pub(crate)`: auch von `config::apply` (#420) fuer den Daemon-Config-Apply-Proxy genutzt.
pub(crate) async fn forward(
    st: &AppState,
    method: reqwest::Method,
    url: String,
    operator_auth: bool,
    body: Option<Bytes>,
) -> Response {
    let mut req = st.http.request(method, &url);
    if operator_auth {
        if let Some(key) = &st.config.operator_key {
            req = req.header(OPERATOR_KEY_HEADER, key);
        }
    }
    if let Some(b) = body {
        req = req
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(b.to_vec());
    }
    match req.send().await {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let text = resp.text().await.unwrap_or_default();
            (
                status,
                [(reqwest::header::CONTENT_TYPE.as_str(), "application/json")],
                text,
            )
                .into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, url, "control proxy upstream error");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("upstream: {e}")})),
            )
                .into_response()
        }
    }
}

// ── Operator-API (:8084) ──

/// POST /api/control/chaos → Operator-API `/operator/chaos`.
pub async fn chaos(State(st): State<AppState>, body: Bytes) -> Response {
    forward(
        &st,
        reqwest::Method::POST,
        format!("{}/operator/chaos", st.config.operator_url),
        true,
        Some(body),
    )
    .await
}

/// POST /api/control/stimulus → Operator-API `/operator/stimulus`.
pub async fn stimulus(State(st): State<AppState>, body: Bytes) -> Response {
    forward(
        &st,
        reqwest::Method::POST,
        format!("{}/operator/stimulus", st.config.operator_url),
        true,
        Some(body),
    )
    .await
}

/// POST /api/control/nightrun → Operator-API `/operator/nightrun`.
pub async fn nightrun(State(st): State<AppState>, body: Bytes) -> Response {
    forward(
        &st,
        reqwest::Method::POST,
        format!("{}/operator/nightrun", st.config.operator_url),
        true,
        Some(body),
    )
    .await
}

/// GET /api/control/snapshots → Operator-API `/operator/snapshots`.
pub async fn snapshots(State(st): State<AppState>) -> Response {
    forward(
        &st,
        reqwest::Method::GET,
        format!("{}/operator/snapshots", st.config.operator_url),
        true,
        None,
    )
    .await
}

/// POST /api/control/snapshot → Operator-API `/operator/snapshot`.
pub async fn snapshot(State(st): State<AppState>, body: Bytes) -> Response {
    forward(
        &st,
        reqwest::Method::POST,
        format!("{}/operator/snapshot", st.config.operator_url),
        true,
        Some(body),
    )
    .await
}

#[derive(Debug, Deserialize)]
pub struct SnapshotStateQuery {
    snapshot_id: Option<String>,
}

/// GET /api/control/snapshot-state?snapshot_id=... — lokaler read-only EventStore-Replay.
pub async fn snapshot_state(
    State(st): State<AppState>,
    Query(q): Query<SnapshotStateQuery>,
) -> Response {
    let Some(snapshot_id) = q.snapshot_id.filter(|s| !s.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "snapshot_id required"})),
        )
            .into_response();
    };
    match crate::events::snapshot_state_json(&st.config.events_db, &snapshot_id) {
        Ok(Some(state)) => Json(state).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "snapshot not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "snapshot-state degraded");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "events db unavailable"})),
            )
                .into_response()
        }
    }
}

/// POST /api/control/snapshot-restore → Operator-API `/operator/restore`.
pub async fn snapshot_restore(State(st): State<AppState>, body: Bytes) -> Response {
    forward(
        &st,
        reqwest::Method::POST,
        format!("{}/operator/restore", st.config.operator_url),
        true,
        Some(body),
    )
    .await
}

/// POST /api/operator/chat → Operator-API `/operator/chat`.
pub async fn operator_chat(State(st): State<AppState>, body: Bytes) -> Response {
    forward(
        &st,
        reqwest::Method::POST,
        format!("{}/operator/chat", st.config.operator_url),
        true,
        Some(body),
    )
    .await
}

#[derive(Debug, Deserialize)]
pub struct DeliveryLineageQuery {
    tenant_id: String,
    project_id: String,
}

/// GET /api/v1/delivery/lineage -> authenticated, server-redacted daemon lineage.
pub async fn delivery_lineage(
    State(st): State<AppState>,
    Query(query): Query<DeliveryLineageQuery>,
) -> Response {
    if !safe_delivery_scope(&query.tenant_id) || !safe_delivery_scope(&query.project_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid delivery scope"})),
        )
            .into_response();
    }
    let Some(credential) = st.config.workflow_read_credential.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "delivery lineage credential unavailable"})),
        )
            .into_response();
    };
    let response = st
        .http
        .get(format!(
            "{}/company/delivery/lineage",
            st.config.operator_url
        ))
        .query(&[
            ("tenant_id", query.tenant_id.as_str()),
            ("project_id", query.project_id.as_str()),
        ])
        .bearer_auth(credential)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await;
    let mut response = match response {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(error = %error, "delivery lineage upstream unavailable");
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "delivery lineage unavailable"})),
            )
                .into_response();
        }
    };
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type_is_json = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("application/json")
                || value.to_ascii_lowercase().starts_with("application/json;")
        });
    if !content_type_is_json
        || response
            .content_length()
            .is_some_and(|length| length > MAX_DELIVERY_LINEAGE_BYTES)
    {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": "invalid delivery lineage response"})),
        )
            .into_response();
    }
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk))
                if body.len().saturating_add(chunk.len())
                    <= MAX_DELIVERY_LINEAGE_BYTES as usize =>
            {
                body.extend_from_slice(&chunk);
            }
            Ok(Some(_)) | Err(_) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({"error": "invalid delivery lineage response"})),
                )
                    .into_response();
            }
            Ok(None) => break,
        }
    }
    (
        status,
        [(reqwest::header::CONTENT_TYPE.as_str(), "application/json")],
        body,
    )
        .into_response()
}

fn safe_delivery_scope(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

// ── Gateway-Control (:8081) ──

/// GET /api/control/config → Gateway `/control/config`.
pub async fn get_config(State(st): State<AppState>) -> Response {
    forward(
        &st,
        reqwest::Method::GET,
        format!("{}/control/config", st.config.gateway_url),
        false,
        None,
    )
    .await
}

/// PATCH /api/control/config → Gateway `/control/config`.
pub async fn patch_config(State(st): State<AppState>, body: Bytes) -> Response {
    forward(
        &st,
        reqwest::Method::PATCH,
        format!("{}/control/config", st.config.gateway_url),
        false,
        Some(body),
    )
    .await
}

/// POST /api/control/provider → Gateway `/control/provider` (Provider-Wechsel).
pub async fn provider(State(st): State<AppState>, body: Bytes) -> Response {
    forward(
        &st,
        reqwest::Method::POST,
        format!("{}/control/provider", st.config.gateway_url),
        false,
        Some(body),
    )
    .await
}

/// POST /api/control/pause — setzt Gateway `rate_limit_rps` auf 0 und merkt den alten Wert.
pub async fn pause(State(st): State<AppState>) -> Response {
    let config_url = format!("{}/control/config", st.config.gateway_url);
    let current_rate = match st.http.get(&config_url).send().await {
        Ok(resp) if resp.status().is_success() => resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("rate_limit_rps").and_then(serde_json::Value::as_f64))
            .unwrap_or(0.0),
        Ok(resp) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("config read failed: {}", resp.status())})),
            )
                .into_response()
        }
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("Cortex Gateway nicht erreichbar: {e}")})),
            )
                .into_response()
        }
    };
    if current_rate > 0.0 {
        *st.saved_rate_limit.lock().await = Some(current_rate);
    }
    let body = Bytes::from(serde_json::to_vec(&json!({"rate_limit_rps": 0})).unwrap_or_default());
    forward(&st, reqwest::Method::PATCH, config_url, false, Some(body)).await
}

/// POST /api/control/resume — restauriert den zuvor gemerkten `rate_limit_rps`.
pub async fn resume(State(st): State<AppState>) -> Response {
    let restore_rate = st.saved_rate_limit.lock().await.take().unwrap_or(10.0);
    let body = Bytes::from(
        serde_json::to_vec(&json!({"rate_limit_rps": restore_rate})).unwrap_or_default(),
    );
    forward(
        &st,
        reqwest::Method::PATCH,
        format!("{}/control/config", st.config.gateway_url),
        false,
        Some(body),
    )
    .await
}

/// POST /api/control/agent-provider → Gateway `/control/agent-provider`.
pub async fn agent_provider(State(st): State<AppState>, body: Bytes) -> Response {
    forward(
        &st,
        reqwest::Method::POST,
        format!("{}/control/agent-provider", st.config.gateway_url),
        false,
        Some(body),
    )
    .await
}

/// DELETE /api/control/agent-provider → Gateway `/control/agent-provider`.
pub async fn delete_agent_provider(State(st): State<AppState>, body: Bytes) -> Response {
    forward(
        &st,
        reqwest::Method::DELETE,
        format!("{}/control/agent-provider", st.config.gateway_url),
        false,
        Some(body),
    )
    .await
}

/// GET /api/control/traffic-stats → Gateway `/control/traffic-stats`.
pub async fn traffic_stats(State(st): State<AppState>) -> Response {
    forward(
        &st,
        reqwest::Method::GET,
        format!("{}/control/traffic-stats", st.config.gateway_url),
        false,
        None,
    )
    .await
}

/// GET /api/control/synthesis-rules → Gateway `/control/synthesis/rules` (#429 Rules Editor).
pub async fn synthesis_rules(State(st): State<AppState>) -> Response {
    forward(
        &st,
        reqwest::Method::GET,
        format!("{}/control/synthesis/rules", st.config.gateway_url),
        false,
        None,
    )
    .await
}

/// POST /api/control/synthesis-rules/{name} → Gateway `/control/synthesis/rules/{name}` (#429 toggle).
pub async fn set_synthesis_rule(
    State(st): State<AppState>,
    Path(name): Path<String>,
    body: Bytes,
) -> Response {
    forward(
        &st,
        reqwest::Method::POST,
        format!("{}/control/synthesis/rules/{name}", st.config.gateway_url),
        false,
        Some(body),
    )
    .await
}

/// GET /api/control/traffic-responses → Gateway `/control/traffic-responses` (#429 Request Inspector).
pub async fn traffic_responses(State(st): State<AppState>) -> Response {
    forward(
        &st,
        reqwest::Method::GET,
        format!("{}/control/traffic-responses", st.config.gateway_url),
        false,
        None,
    )
    .await
}

/// GET /api/control/platform-state → Operator-API `/operator/platform-state`.
pub async fn platform_state(State(st): State<AppState>) -> Response {
    forward(
        &st,
        reqwest::Method::GET,
        format!("{}/operator/platform-state", st.config.operator_url),
        true,
        None,
    )
    .await
}

#[derive(Debug, Deserialize)]
pub struct LimitQuery {
    limit: Option<usize>,
}

/// GET /api/control/platform-analyses — local read-only events.db, empty when Judge never wrote.
pub async fn platform_analyses(
    State(st): State<AppState>,
    Query(q): Query<LimitQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    Json(crate::events::platform_analyses_json(
        &st.config.events_db,
        limit,
    ))
    .into_response()
}

/// POST /api/control/platform-analyze → Operator-API `/operator/platform-analyze`.
pub async fn platform_analyze(State(st): State<AppState>, body: Bytes) -> Response {
    forward(
        &st,
        reqwest::Method::POST,
        format!("{}/operator/platform-analyze", st.config.operator_url),
        true,
        Some(body),
    )
    .await
}

// ── #428 Agent Deep View: per-Agent FS-Browse (read-only) + Pause/Resume/Despawn ──

/// #428: optionaler `inode`-Query fuer FS-Browse/Read (Default Root inode 1 beim Browse).
#[derive(Debug, Deserialize)]
pub struct AgentFsQuery {
    inode: Option<u64>,
}

/// GET /api/control/agent/{id}/fs?inode=N → Operator-API `/operator/security/agent-fs` (read-only).
pub async fn agent_fs(
    State(st): State<AppState>,
    Path(id): Path<u32>,
    Query(q): Query<AgentFsQuery>,
) -> Response {
    let inode = q.inode.unwrap_or(1);
    forward(
        &st,
        reqwest::Method::GET,
        format!(
            "{}/operator/security/agent-fs?agent_id={id}&inode={inode}",
            st.config.operator_url
        ),
        true,
        None,
    )
    .await
}

/// GET /api/control/agent/{id}/fs/read?inode=N → Operator-API `/operator/security/agent-fs-read`.
pub async fn agent_fs_read(
    State(st): State<AppState>,
    Path(id): Path<u32>,
    Query(q): Query<AgentFsQuery>,
) -> Response {
    let inode = q.inode.unwrap_or(0);
    forward(
        &st,
        reqwest::Method::GET,
        format!(
            "{}/operator/security/agent-fs-read?agent_id={id}&inode={inode}",
            st.config.operator_url
        ),
        true,
        None,
    )
    .await
}

/// POST /api/control/agent/{id}/stop → Operator-API `/operator/runtime/pause` (Pause, nicht destruktiv).
pub async fn agent_stop(State(st): State<AppState>, Path(id): Path<u32>) -> Response {
    forward(
        &st,
        reqwest::Method::POST,
        format!("{}/operator/runtime/pause", st.config.operator_url),
        true,
        Some(Bytes::from(format!("{{\"agent_id\":{id}}}"))),
    )
    .await
}

/// POST /api/control/agent/{id}/start → Operator-API `/operator/runtime/resume`.
pub async fn agent_start(State(st): State<AppState>, Path(id): Path<u32>) -> Response {
    forward(
        &st,
        reqwest::Method::POST,
        format!("{}/operator/runtime/resume", st.config.operator_url),
        true,
        Some(Bytes::from(format!("{{\"agent_id\":{id}}}"))),
    )
    .await
}

/// POST /api/control/agent/{id}/remove → Operator-API `/operator/runtime/despawn` (destruktiv, confirm-gated).
pub async fn agent_remove(State(st): State<AppState>, Path(id): Path<u32>) -> Response {
    forward(
        &st,
        reqwest::Method::POST,
        format!("{}/operator/runtime/despawn", st.config.operator_url),
        true,
        Some(Bytes::from(format!("{{\"agent_id\":{id}}}"))),
    )
    .await
}

/// GET /api/control/status — aggregierter Gateway-Status, 200 auch bei offline Gateway.
pub async fn status(State(st): State<AppState>) -> Response {
    let config = match st
        .http
        .get(format!("{}/control/config", st.config.gateway_url))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => resp.json::<serde_json::Value>().await.ok(),
        _ => None,
    };
    let health = match st
        .http
        .get(format!("{}/health", st.config.gateway_proxy_url))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => resp.json::<serde_json::Value>().await.ok(),
        _ => None,
    };
    let rate_limit = config
        .as_ref()
        .and_then(|v| v.get("rate_limit_rps"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(-1.0);
    Json(json!({
        "connected": config.is_some() || health.is_some(),
        "paused": rate_limit == 0.0,
        "config": config,
        "health": health,
        "saved_rate_limit": *st.saved_rate_limit.lock().await,
        "gateway": if rate_limit >= 0.0 { "ok" } else { "offline" },
    }))
    .into_response()
}
