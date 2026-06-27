//! Gaia Console API (#442).
//!
//! These routes are explicit operator surfaces. Read routes inspect the Gaia Console
//! JSONL files. POST routes run one bounded Claude Code session through the
//! `sentinel-gaia-loop` library; the readiness service path remains token-free.

use std::fs;
use std::path::{Path, PathBuf};

use axum::{
    extract::{Path as AxumPath, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use sentinel_gaia_loop::config::GaiaLoopConfig;
use sentinel_gaia_loop::session::{ClaudeSessionRunner, GaiaSessionRequest};
use sentinel_gaia_loop::types::{GaiaAlert, GaiaSessionIndexEntry};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct StartSessionRequest {
    prompt: String,
    #[serde(default)]
    resume: Option<String>,
}

/// GET /api/gaia/alerts — persisted readiness alerts from the token-free Gaia loop.
pub async fn alerts(State(st): State<AppState>) -> Response {
    let path =
        PathBuf::from(&st.config.gaia_console_dir).join(sentinel_gaia_loop::ALERTS_FILE_NAME);
    match read_jsonl::<GaiaAlert>(&path) {
        Ok(alerts) => Json(json!({
            "alerts": alerts,
            "count": alerts.len(),
            "source": path,
        }))
        .into_response(),
        Err(error) => internal_error("read Gaia alerts", error),
    }
}

/// GET /api/gaia/sessions — append-only Gaia Console session index.
pub async fn sessions(State(st): State<AppState>) -> Response {
    let path = PathBuf::from(&st.config.gaia_console_dir)
        .join(sentinel_gaia_loop::SESSIONS_DIR_NAME)
        .join(sentinel_gaia_loop::SESSION_INDEX_FILE_NAME);
    match read_jsonl::<GaiaSessionIndexEntry>(&path) {
        Ok(sessions) => Json(json!({
            "sessions": sessions,
            "count": sessions.len(),
            "source": path,
        }))
        .into_response(),
        Err(error) => internal_error("read Gaia sessions", error),
    }
}

/// GET /api/gaia/sessions/{id}/stream — raw Claude Code stream-json for Deep Mode.
pub async fn session_stream(
    AxumPath(id): AxumPath<String>,
    State(st): State<AppState>,
) -> Response {
    let Some(id) = safe_session_id(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid Gaia session id"})),
        )
            .into_response();
    };
    let path = PathBuf::from(&st.config.gaia_console_dir)
        .join(sentinel_gaia_loop::SESSIONS_DIR_NAME)
        .join(id)
        .join("stream.jsonl");
    match fs::read_to_string(&path) {
        Ok(stream) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/x-ndjson")],
            stream,
        )
            .into_response(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Gaia session stream not found"})),
        )
            .into_response(),
        Err(error) => internal_error("read Gaia session stream", error),
    }
}

/// POST /api/gaia/deep — explicit one-turn Claude Code deep session.
pub async fn deep(State(st): State<AppState>, Json(req): Json<StartSessionRequest>) -> Response {
    run_session(st, GaiaSessionRequest::deep(req.prompt, req.resume)).await
}

/// POST /api/gaia/setup-interview — explicit setup interview Claude Code session.
pub async fn setup_interview(
    State(st): State<AppState>,
    Json(req): Json<StartSessionRequest>,
) -> Response {
    run_session(
        st,
        GaiaSessionRequest::setup_interview(req.prompt, req.resume),
    )
    .await
}

async fn run_session(st: AppState, request: GaiaSessionRequest) -> Response {
    let config = match gaia_loop_config(&st) {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(error = %error, "Gaia Console config invalid");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Gaia Console config invalid: {error}")})),
            )
                .into_response();
        }
    };
    let runner = ClaudeSessionRunner::new(config);
    match runner.run(request).await {
        Ok(run) => Json(run).into_response(),
        Err(error) => {
            tracing::warn!(error = %error, "Gaia Console session failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("Gaia Console session failed: {error}")})),
            )
                .into_response()
        }
    }
}

fn gaia_loop_config(st: &AppState) -> anyhow::Result<GaiaLoopConfig> {
    let config = GaiaLoopConfig {
        console_dir: PathBuf::from(&st.config.gaia_console_dir),
        events_db: PathBuf::from(&st.config.events_db),
        nats_url: st.config.nats_url.clone(),
        http_bind: sentinel_gaia_loop::DEFAULT_HTTP_BIND.to_string(),
        claude_bin: PathBuf::from(&st.config.gaia_claude_bin),
        sentinel_ctl_bin: PathBuf::from(&st.config.gaia_sentinel_ctl_bin),
        sentinel_gaia_bin: PathBuf::from(&st.config.gaia_sentinel_gaia_bin),
        model: st.config.gaia_model.clone(),
        max_budget_usd: st.config.gaia_max_budget_usd,
        session_timeout_secs: st.config.gaia_session_timeout_secs,
        max_turns: st.config.gaia_max_turns,
    };
    config.validate()?;
    Ok(config)
}

fn read_jsonl<T>(path: &Path) -> anyhow::Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path)?;
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<T>(line).map_err(Into::into))
        .collect()
}

fn safe_session_id(id: &str) -> Option<&str> {
    let valid = id.starts_with("gaia-")
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    valid.then_some(id)
}

fn internal_error(action: &str, error: impl std::fmt::Display) -> Response {
    tracing::warn!(%error, action, "Gaia Console API error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": format!("{action}: {error}")})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_session_ids() {
        assert_eq!(
            safe_session_id("gaia-deep-abc_123"),
            Some("gaia-deep-abc_123")
        );
        assert_eq!(safe_session_id("../gaia-deep-abc"), None);
        assert_eq!(safe_session_id("gaia/abc"), None);
        assert_eq!(safe_session_id("deep-abc"), None);
    }
}
