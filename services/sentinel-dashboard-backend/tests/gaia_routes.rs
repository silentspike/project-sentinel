//! Integration tests for `/api/gaia/*` (#442). These use `build_app` directly
//! and fake Claude Code binaries, so they do not spend tokens.

use std::fs;
use std::path::{Path, PathBuf};

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use sentinel_dashboard_backend::{auth, build_app, AppState, Config};
use sentinel_gaia_loop::types::{
    ClaudeUsageSummary, GaiaAlert, GaiaSessionIndexEntry, GaiaSessionKind, GaiaSessionStatus,
};
use tempfile::TempDir;
use tower::ServiceExt;

fn test_state(dir: &TempDir) -> AppState {
    let mut config = Config::from_env();
    config.dashboard_api_key = Some("test-key".into());
    config.projection_db = "/nonexistent/dashboard-gaia-test-projection.db".into();
    config.events_db = "/nonexistent/dashboard-gaia-test-events.db".into();
    config.gateway_proxy_url = "http://127.0.0.1:1".into();
    config.prometheus_url = "http://127.0.0.1:1".into();
    config.gaia_console_dir = dir.path().join("gaia-console").display().to_string();
    config.gaia_claude_bin = dir.path().join("fake-claude.sh").display().to_string();
    config.gaia_session_timeout_secs = 5;
    AppState::new(config).unwrap()
}

async fn request(
    state: AppState,
    method: Method,
    path: &str,
    body: impl Into<Body>,
    authed: bool,
) -> (StatusCode, serde_json::Value) {
    let token = state.sessions.create();
    let app = build_app(state);
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if authed {
        builder = builder.header(header::COOKIE, format!("{}={token}", auth::SESSION_COOKIE));
    }
    let resp = app
        .oneshot(builder.body(body.into()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn gaia_routes_require_auth() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir);
    for path in [
        "/api/gaia/alerts",
        "/api/gaia/sessions",
        "/api/gaia/sessions/gaia-deep-test/stream",
    ] {
        let (status, _) = request(state.clone(), Method::GET, path, Body::empty(), false).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} must require auth");
    }
    for path in ["/api/gaia/deep", "/api/gaia/setup-interview"] {
        let (status, _) = request(
            state.clone(),
            Method::POST,
            path,
            Body::from(r#"{"prompt":"hello"}"#),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} must require auth");
    }
}

#[tokio::test]
async fn gaia_read_routes_return_console_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir);
    let console_dir = PathBuf::from(&state.config.gaia_console_dir);
    fs::create_dir_all(console_dir.join("sessions/gaia-deep-test")).unwrap();
    append_jsonl(
        &console_dir.join("alerts.jsonl"),
        &GaiaAlert {
            alert_id: "gaia-alert-event-1".into(),
            source_event_id: "event-1".into(),
            tick: 7,
            timestamp_ms: 42,
            trigger: "unresolved_escalation".into(),
            severity: "warning".into(),
            target: "system".into(),
            summary: "projection lag".into(),
            recommendation: "inspect".into(),
            unresolved_keys: vec!["projection".into()],
        },
    );
    append_jsonl(
        &console_dir.join("sessions/index.jsonl"),
        &GaiaSessionIndexEntry {
            gaia_session_id: "gaia-deep-test".into(),
            claude_session_id: Some("claude-test".into()),
            kind: GaiaSessionKind::Deep,
            status: GaiaSessionStatus::Succeeded,
            stream_path: console_dir
                .join("sessions/gaia-deep-test/stream.jsonl")
                .display()
                .to_string(),
            started_at_ms: 1,
            finished_at_ms: Some(2),
            exit_code: Some(0),
            usage: ClaudeUsageSummary {
                input_tokens: 1,
                output_tokens: 2,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                total_cost_usd: Some(0.001),
            },
        },
    );
    fs::write(
        console_dir.join("sessions/gaia-deep-test/stream.jsonl"),
        "{\"type\":\"message\"}\n",
    )
    .unwrap();

    let (status, alerts) = request(
        state.clone(),
        Method::GET,
        "/api/gaia/alerts",
        Body::empty(),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(alerts["count"], 1);
    assert_eq!(alerts["alerts"][0]["summary"], "projection lag");

    let (status, sessions) = request(
        state.clone(),
        Method::GET,
        "/api/gaia/sessions",
        Body::empty(),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sessions["count"], 1);
    assert_eq!(sessions["sessions"][0]["gaia_session_id"], "gaia-deep-test");

    let app = build_app(state.clone());
    let token = state.sessions.create();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/gaia/sessions/gaia-deep-test/stream")
                .header(header::COOKIE, format!("{}={token}", auth::SESSION_COOKIE))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/x-ndjson"
    );
}

#[tokio::test]
async fn gaia_deep_route_runs_fake_claude_with_budget_cap() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir);
    let fake_claude = PathBuf::from(&state.config.gaia_claude_bin);
    fs::write(
        &fake_claude,
        r#"#!/usr/bin/env bash
script_dir="$(cd "$(dirname "$0")" && pwd)"
printf '%s\n' "$@" > "$script_dir/argv.txt"
echo '{"type":"message","usage":{"input_tokens":2,"output_tokens":3,"cost_usd":0.0005}}'
"#,
    )
    .unwrap();
    make_executable(&fake_claude);

    let (status, body) = request(
        state.clone(),
        Method::POST,
        "/api/gaia/deep",
        Body::from(r#"{"prompt":"create task evidence","resume":"resume-test"}"#),
        true,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entry"]["status"], "succeeded");
    assert_eq!(body["entry"]["claude_session_id"], "resume-test");
    assert_eq!(body["entry"]["usage"]["input_tokens"], 2);
    let stream_path = body["entry"]["stream_path"].as_str().unwrap();
    assert!(Path::new(stream_path).exists());

    let argv = fs::read_to_string(dir.path().join("argv.txt")).unwrap();
    assert!(argv.contains("--max-budget-usd\n0.05"));
    assert!(argv.contains("--resume\nresume-test"));
    assert!(!argv.contains("--max-turns"));
}

fn append_jsonl<T: serde::Serialize>(path: &Path, value: &T) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut raw = serde_json::to_string(value).unwrap();
    raw.push('\n');
    fs::write(path, raw).unwrap();
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}
