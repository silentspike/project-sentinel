//! Control-Proxy (#431) — leitet mutierende Operator-Befehle an die Backend-Dienste weiter:
//! Operator-API (Daemon, `:8084`, Header `x-sentinel-operator-key`) und Gateway-Control (`:8081`).
//! 1:1-Aequivalent zu `dashboard/src/routes/control.ts`. Alle Routen laufen hinter `require_auth`.

use axum::{
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::AppState;

const OPERATOR_KEY_HEADER: &str = "x-sentinel-operator-key";

/// Generischer Upstream-Forward: Status + Body werden durchgereicht (JSON).
async fn forward(
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
        req = req.header(reqwest::header::CONTENT_TYPE, "application/json").body(b.to_vec());
    }
    match req.send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let text = resp.text().await.unwrap_or_default();
            (status, [(reqwest::header::CONTENT_TYPE.as_str(), "application/json")], text).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, url, "control proxy upstream error");
            (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("upstream: {e}")}))).into_response()
        }
    }
}

// ── Operator-API (:8084) ──

/// POST /api/control/chaos → Operator-API `/operator/chaos`.
pub async fn chaos(State(st): State<AppState>, body: Bytes) -> Response {
    forward(&st, reqwest::Method::POST, format!("{}/operator/chaos", st.config.operator_url), true, Some(body)).await
}

/// POST /api/control/stimulus → Operator-API `/operator/stimulus`.
pub async fn stimulus(State(st): State<AppState>, body: Bytes) -> Response {
    forward(&st, reqwest::Method::POST, format!("{}/operator/stimulus", st.config.operator_url), true, Some(body)).await
}

/// POST /api/control/nightrun → Operator-API `/operator/nightrun`.
pub async fn nightrun(State(st): State<AppState>, body: Bytes) -> Response {
    forward(&st, reqwest::Method::POST, format!("{}/operator/nightrun", st.config.operator_url), true, Some(body)).await
}

// ── Gateway-Control (:8081) ──

/// GET /api/control/config → Gateway `/control/config`.
pub async fn get_config(State(st): State<AppState>) -> Response {
    forward(&st, reqwest::Method::GET, format!("{}/control/config", st.config.gateway_url), false, None).await
}

/// PATCH /api/control/config → Gateway `/control/config`.
pub async fn patch_config(State(st): State<AppState>, body: Bytes) -> Response {
    forward(&st, reqwest::Method::PATCH, format!("{}/control/config", st.config.gateway_url), false, Some(body)).await
}

/// POST /api/control/provider → Gateway `/control/provider` (Provider-Wechsel).
pub async fn provider(State(st): State<AppState>, body: Bytes) -> Response {
    forward(&st, reqwest::Method::POST, format!("{}/control/provider", st.config.gateway_url), false, Some(body)).await
}
