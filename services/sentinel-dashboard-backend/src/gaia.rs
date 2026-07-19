//! Gaia Console API (#442).
//!
//! These routes are explicit operator surfaces. Read routes inspect the Gaia Console
//! JSONL files. POST routes run one bounded Claude Code session through the
//! `sentinel-gaia-loop` library; the readiness service path remains token-free.

use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::{Path as AxumPath, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use sentinel_gaia_loop::config::GaiaLoopConfig;
use sentinel_gaia_loop::session::{ClaudeSessionRunner, GaiaAdmissionError, GaiaSessionRequest};
use sentinel_gaia_loop::storage::read_jsonl_locked;
use sentinel_gaia_loop::types::{GaiaAlert, GaiaSessionIndexEntry, GaiaSessionStatus};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct StartSessionRequest {
    prompt: String,
    #[serde(default)]
    resume_session_id: Option<String>,
}

#[derive(Clone)]
pub struct GaiaRequestLimiter {
    attempts: Arc<Mutex<VecDeque<(Instant, String)>>>,
    max_requests: usize,
    window: Duration,
}

impl GaiaRequestLimiter {
    pub fn new(max_requests: usize, window: Duration) -> Self {
        Self {
            attempts: Arc::new(Mutex::new(VecDeque::new())),
            max_requests,
            window,
        }
    }

    fn allow(&self, idempotency_key: &str) -> bool {
        let now = Instant::now();
        let mut attempts = self.attempts.lock().unwrap_or_else(|e| e.into_inner());
        while attempts
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) >= self.window)
        {
            attempts.pop_front();
        }
        if attempts.iter().any(|(_, key)| key == idempotency_key) {
            return true;
        }
        if attempts.len() >= self.max_requests {
            return false;
        }
        attempts.push_back((now, idempotency_key.to_string()));
        true
    }
}

/// GET /api/gaia/alerts — persisted readiness alerts from the token-free Gaia loop.
pub async fn alerts(State(st): State<AppState>) -> Response {
    let path =
        PathBuf::from(&st.config.gaia_console_dir).join(sentinel_gaia_loop::ALERTS_FILE_NAME);
    match read_jsonl_locked::<GaiaAlert>(&path) {
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
    match read_jsonl_locked::<GaiaSessionIndexEntry>(&path) {
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
pub async fn deep(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<StartSessionRequest>,
) -> Response {
    let Some(idempotency_key) = idempotency_key(&headers) else {
        return missing_idempotency_key();
    };
    if !st.gaia_request_limiter.allow(&idempotency_key) {
        return request_rate_limited();
    }
    run_session(
        st,
        GaiaSessionRequest::deep_idempotent(req.prompt, req.resume_session_id, idempotency_key),
    )
    .await
}

/// POST /api/gaia/setup-interview — explicit setup interview Claude Code session.
pub async fn setup_interview(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<StartSessionRequest>,
) -> Response {
    let Some(idempotency_key) = idempotency_key(&headers) else {
        return missing_idempotency_key();
    };
    if !st.gaia_request_limiter.allow(&idempotency_key) {
        return request_rate_limited();
    }
    run_session(
        st,
        GaiaSessionRequest::setup_interview_idempotent(
            req.prompt,
            req.resume_session_id,
            idempotency_key,
        ),
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
        Ok(run) if run.entry.status == GaiaSessionStatus::Succeeded => Json(run).into_response(),
        Ok(run) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": format!(
                    "Claude Code session ended with status {:?}",
                    run.entry.status
                ),
                "run": run,
            })),
        )
            .into_response(),
        Err(error) if error.downcast_ref::<GaiaAdmissionError>().is_some() => {
            let admission = error.downcast_ref::<GaiaAdmissionError>().expect("checked");
            let status = match admission {
                GaiaAdmissionError::Busy | GaiaAdmissionError::BudgetExceeded { .. } => {
                    StatusCode::TOO_MANY_REQUESTS
                }
                GaiaAdmissionError::IdempotencyConflict => StatusCode::CONFLICT,
                GaiaAdmissionError::InvalidIdempotencyKey
                | GaiaAdmissionError::InvalidResume(_) => StatusCode::BAD_REQUEST,
            };
            (status, Json(json!({"error": admission.to_string()}))).into_response()
        }
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
        company_context_path: PathBuf::from(&st.config.gaia_company_context_path),
        model: st.config.gaia_model.clone(),
        max_budget_usd: st.config.gaia_max_budget_usd,
        budget_window_secs: st.config.gaia_budget_window_secs,
        budget_window_usd: st.config.gaia_budget_window_usd,
        session_timeout_secs: st.config.gaia_session_timeout_secs,
        readiness_scan_interval_secs: sentinel_gaia_loop::DEFAULT_READINESS_SCAN_INTERVAL_SECS,
    };
    config.validate()?;
    Ok(config)
}

fn idempotency_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn missing_idempotency_key() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": "Idempotency-Key header is required"})),
    )
        .into_response()
}

fn request_rate_limited() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({"error": "Gaia request rate limit exceeded"})),
    )
        .into_response()
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

    #[test]
    fn limiter_counts_distinct_operations_but_allows_safe_retries() {
        let limiter = GaiaRequestLimiter::new(2, Duration::from_secs(60));
        assert!(limiter.allow("operation-1"));
        assert!(limiter.allow("operation-1"));
        assert!(limiter.allow("operation-2"));
        assert!(!limiter.allow("operation-3"));
    }
}
