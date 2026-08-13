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
    config.operator_key = Some("public-test-operator-authority".into());
    let credential_directory = dir.path().join("credentials");
    fs::create_dir_all(&credential_directory).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&credential_directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    config.operator_credential_directory = Some(credential_directory.display().to_string());
    let sentinel_ctl = dir.path().join("sentinel-ctl");
    fs::write(&sentinel_ctl, "#!/usr/bin/env bash\nexit 0\n").unwrap();
    make_executable(&sentinel_ctl);
    config.gaia_sentinel_ctl_bin = sentinel_ctl.display().to_string();
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
    request_with_idempotency(
        state,
        method,
        path,
        body,
        authed,
        Some("test-idempotency-0001"),
    )
    .await
}

async fn request_with_idempotency(
    state: AppState,
    method: Method,
    path: &str,
    body: impl Into<Body>,
    authed: bool,
    idempotency_key: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let token = state.sessions.create();
    let app = build_app(state);
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(idempotency_key) = idempotency_key {
        builder = builder.header("Idempotency-Key", idempotency_key);
    }
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
            resumed_from_gaia_session_id: None,
            idempotency_key: None,
            request_fingerprint: None,
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
        Body::from(r#"{"prompt":"create task evidence"}"#),
        true,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entry"]["status"], "succeeded");
    assert!(body["entry"]["claude_session_id"].as_str().is_some());
    assert_eq!(body["entry"]["usage"]["input_tokens"], 2);
    let stream_path = body["entry"]["stream_path"].as_str().unwrap();
    assert!(Path::new(stream_path).exists());

    let argv = fs::read_to_string(dir.path().join("argv.txt")).unwrap();
    assert!(argv.contains("--max-budget-usd\n0.05"));
    assert!(argv.contains("--session-id"));
    assert!(!argv.contains("--max-turns"));

    let (replay_status, replay) = request(
        state,
        Method::POST,
        "/api/gaia/deep",
        Body::from(r#"{"prompt":"create task evidence"}"#),
        true,
    )
    .await;
    assert_eq!(replay_status, StatusCode::OK);
    assert_eq!(
        replay["entry"]["gaia_session_id"],
        body["entry"]["gaia_session_id"]
    );
}

#[tokio::test]
async fn gaia_write_admission_maps_missing_key_conflict_busy_budget_and_foreign_resume() {
    let dir = tempfile::tempdir().unwrap();
    let base_state = test_state(&dir);
    let mut config = (*base_state.config).clone();
    config.gaia_budget_window_usd = 0.05;
    let state = AppState::new(config).unwrap();
    let fake_claude = PathBuf::from(&state.config.gaia_claude_bin);
    fs::write(
        &fake_claude,
        r#"#!/usr/bin/env bash
echo '{"type":"message","usage":{"input_tokens":1,"output_tokens":1,"cost_usd":0.001}}'
"#,
    )
    .unwrap();
    make_executable(&fake_claude);

    let (missing, _) = request_with_idempotency(
        state.clone(),
        Method::POST,
        "/api/gaia/deep",
        Body::from(r#"{"prompt":"one"}"#),
        true,
        None,
    )
    .await;
    assert_eq!(missing, StatusCode::BAD_REQUEST);

    let (foreign, _) = request_with_idempotency(
        state.clone(),
        Method::POST,
        "/api/gaia/deep",
        Body::from(
            r#"{"prompt":"one","resume_session_id":"11111111-1111-1111-1111-111111111111"}"#,
        ),
        true,
        Some("test-idempotency-foreign"),
    )
    .await;
    assert_eq!(foreign, StatusCode::BAD_REQUEST);

    let lock_path = PathBuf::from(&state.config.gaia_console_dir)
        .join("sessions")
        .join(sentinel_gaia_loop::SESSION_ACTIVE_LOCK_FILE_NAME);
    let active_lock = sentinel_gaia_loop::storage::try_exclusive_file_lock(&lock_path)
        .unwrap()
        .unwrap();
    let (busy, _) = request_with_idempotency(
        state.clone(),
        Method::POST,
        "/api/gaia/deep",
        Body::from(r#"{"prompt":"one"}"#),
        true,
        Some("test-idempotency-busy"),
    )
    .await;
    drop(active_lock);
    assert_eq!(busy, StatusCode::TOO_MANY_REQUESTS);

    let (first, _) = request_with_idempotency(
        state.clone(),
        Method::POST,
        "/api/gaia/deep",
        Body::from(r#"{"prompt":"one"}"#),
        true,
        Some("test-idempotency-first"),
    )
    .await;
    assert_eq!(first, StatusCode::OK);

    let (conflict, _) = request_with_idempotency(
        state.clone(),
        Method::POST,
        "/api/gaia/deep",
        Body::from(r#"{"prompt":"different"}"#),
        true,
        Some("test-idempotency-first"),
    )
    .await;
    assert_eq!(conflict, StatusCode::CONFLICT);

    let (budget, _) = request_with_idempotency(
        state,
        Method::POST,
        "/api/gaia/deep",
        Body::from(r#"{"prompt":"two"}"#),
        true,
        Some("test-idempotency-second"),
    )
    .await;
    assert_eq!(budget, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn gaia_rate_limit_counts_new_operations_and_allows_idempotent_retry() {
    let dir = tempfile::tempdir().unwrap();
    let base_state = test_state(&dir);
    let mut config = (*base_state.config).clone();
    config.gaia_rate_limit_requests = 1;
    let state = AppState::new(config).unwrap();
    let fake_claude = PathBuf::from(&state.config.gaia_claude_bin);
    fs::write(
        &fake_claude,
        r#"#!/usr/bin/env bash
echo '{"type":"message","usage":{"input_tokens":1,"output_tokens":1,"cost_usd":0.001}}'
"#,
    )
    .unwrap();
    make_executable(&fake_claude);

    for _ in 0..2 {
        let (status, _) = request_with_idempotency(
            state.clone(),
            Method::POST,
            "/api/gaia/deep",
            Body::from(r#"{"prompt":"one"}"#),
            true,
            Some("rate-operation-one"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (limited, _) = request_with_idempotency(
        state,
        Method::POST,
        "/api/gaia/deep",
        Body::from(r#"{"prompt":"two"}"#),
        true,
        Some("rate-operation-two"),
    )
    .await;
    assert_eq!(limited, StatusCode::TOO_MANY_REQUESTS);
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
