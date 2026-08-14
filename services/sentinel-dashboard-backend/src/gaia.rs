//! Gaia Console API (#442).
//!
//! These routes are explicit operator surfaces. Read routes inspect the Gaia Console
//! JSONL files. POST routes run one bounded Claude Code session through the
//! `sentinel-gaia-loop` library; the readiness service path remains token-free.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
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
use sentinel_gaia_loop::session::{
    ClaudeSessionRunner, GaiaAdmissionError, GaiaOperatorBrokerCapability,
    GaiaOperatorBrokerProcessAuthority, GaiaSessionRequest,
};
use sentinel_gaia_loop::storage::{ensure_private_dir, read_jsonl_locked};
use sentinel_gaia_loop::types::{GaiaAlert, GaiaSessionIndexEntry, GaiaSessionStatus};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;

use crate::AppState;

const BROKER_SCHEMA_VERSION: u8 = 1;
const BROKER_MAX_REQUEST_BYTES: u64 = 64 * 1024;
const BROKER_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const BROKER_MAX_CONNECTIONS: usize = 16;
const BROKER_MAX_OPERATIONS: usize = 256;
const BROKER_IO_TIMEOUT: Duration = Duration::from_secs(15);
const OPERATOR_KEY_HEADER: &str = "x-sentinel-operator-key";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BrokerRequest {
    schema_version: u8,
    session_id: String,
    capability: String,
    operation_id: String,
    method: String,
    gateway: bool,
    path: String,
    body: Option<serde_json::Value>,
    risk: String,
    confirmed: bool,
}

#[derive(Serialize)]
struct BrokerResponse {
    ok: bool,
    value: Option<serde_json::Value>,
    error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ExecutableIdentity {
    device: u64,
    inode: u64,
}

impl ExecutableIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

struct GaiaOperatorBroker {
    capability: GaiaOperatorBrokerCapability,
    process_authority: GaiaOperatorBrokerProcessAuthority,
    socket_path: PathBuf,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl GaiaOperatorBroker {
    async fn start(st: &AppState, config: &GaiaLoopConfig) -> anyhow::Result<Self> {
        let operator_key = st
            .config
            .operator_key
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Gaia operator broker authority is unavailable"))?;
        let credential_directory = st
            .config
            .operator_credential_directory
            .as_ref()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("Gaia credential isolation is unavailable"))?;
        let expected_executable =
            ExecutableIdentity::from_metadata(&fs::metadata(&config.sentinel_ctl_bin).map_err(
                |error| anyhow::anyhow!("trusted sentinel-ctl executable is unavailable: {error}"),
            )?);
        let broker_dir = config.console_dir.join("operator-brokers");
        ensure_private_dir(&broker_dir)?;
        let session_id = format!("gaia-broker-{}", uuid::Uuid::new_v4().simple());
        let capability_value = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let socket_path = broker_dir.join(format!("{session_id}.sock"));
        let listener = UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        let capability = GaiaOperatorBrokerCapability::new(
            socket_path.clone(),
            session_id.clone(),
            capability_value.clone(),
            credential_directory,
        );
        let process_authority = capability.process_authority();
        let http = st.http.clone();
        let operator_url = st.config.operator_url.clone();
        let replay = Arc::new(tokio::sync::Mutex::new(HashSet::new()));
        let (shutdown, mut shutdown_rx) = oneshot::channel();
        let broker_process_authority = process_authority.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        if connections.len() >= BROKER_MAX_CONNECTIONS {
                            drop(stream);
                            continue;
                        }
                        connections.spawn(handle_broker_connection(
                            stream,
                            expected_executable,
                            broker_process_authority.clone(),
                            session_id.clone(),
                            capability_value.clone(),
                            replay.clone(),
                            http.clone(),
                            operator_url.clone(),
                            operator_key.clone(),
                        ));
                    }
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Ok(Self {
            capability,
            process_authority,
            socket_path,
            shutdown: Some(shutdown),
            task: Some(task),
        })
    }

    fn capability(&self) -> GaiaOperatorBrokerCapability {
        self.capability.clone()
    }

    async fn shutdown(mut self) {
        self.process_authority.revoke();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

impl Drop for GaiaOperatorBroker {
    fn drop(&mut self) {
        self.process_authority.revoke();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

async fn handle_broker_connection(
    mut stream: UnixStream,
    expected_executable: ExecutableIdentity,
    process_authority: GaiaOperatorBrokerProcessAuthority,
    session_id: String,
    capability: String,
    replay: Arc<tokio::sync::Mutex<HashSet<String>>>,
    http: reqwest::Client,
    operator_url: String,
    operator_key: String,
) {
    let response = match timeout(
        BROKER_IO_TIMEOUT,
        broker_request(
            &mut stream,
            expected_executable,
            &process_authority,
            &session_id,
            &capability,
            replay,
            http,
            &operator_url,
            &operator_key,
        ),
    )
    .await
    {
        Ok(Ok(value)) => BrokerResponse {
            ok: true,
            value: Some(value),
            error: None,
        },
        Ok(Err(error)) => BrokerResponse {
            ok: false,
            value: None,
            error: Some(error),
        },
        Err(_) => BrokerResponse {
            ok: false,
            value: None,
            error: Some("Gaia operator broker request timed out".to_string()),
        },
    };
    if let Ok(wire) = serde_json::to_vec(&response) {
        let _ = stream.write_all(&wire).await;
        let _ = stream.shutdown().await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn broker_request(
    stream: &mut UnixStream,
    expected_executable: ExecutableIdentity,
    process_authority: &GaiaOperatorBrokerProcessAuthority,
    expected_session_id: &str,
    expected_capability: &str,
    replay: Arc<tokio::sync::Mutex<HashSet<String>>>,
    http: reqwest::Client,
    operator_url: &str,
    operator_key: &str,
) -> Result<serde_json::Value, String> {
    verify_broker_peer(stream, expected_executable, process_authority)?;
    let mut wire = Vec::new();
    stream
        .take(BROKER_MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut wire)
        .await
        .map_err(|_| "Gaia operator broker request read failed".to_string())?;
    if wire.len() > BROKER_MAX_REQUEST_BYTES as usize {
        return Err("Gaia operator broker request is too large".into());
    }
    let request: BrokerRequest = serde_json::from_slice(&wire)
        .map_err(|_| "Gaia operator broker request is invalid".to_string())?;
    process_broker_request(
        request,
        expected_session_id,
        expected_capability,
        replay,
        http,
        operator_url,
        operator_key,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn process_broker_request(
    request: BrokerRequest,
    expected_session_id: &str,
    expected_capability: &str,
    replay: Arc<tokio::sync::Mutex<HashSet<String>>>,
    http: reqwest::Client,
    operator_url: &str,
    operator_key: &str,
) -> Result<serde_json::Value, String> {
    validate_broker_request(&request, expected_session_id, expected_capability)?;
    claim_broker_operation(&replay, &request.operation_id).await?;

    let url = format!("{operator_url}{}", request.path);
    let mut upstream = http
        .get(url)
        .header(OPERATOR_KEY_HEADER, operator_key)
        .send()
        .await
        .map_err(|_| "Gaia operator broker upstream request failed".to_string())?;
    if upstream
        .content_length()
        .is_some_and(|length| length > BROKER_MAX_RESPONSE_BYTES as u64)
    {
        return Err("Gaia operator broker upstream response is too large".into());
    }
    let status = upstream.status().as_u16();
    let mut response_body = Vec::new();
    while let Some(chunk) = upstream
        .chunk()
        .await
        .map_err(|_| "Gaia operator broker upstream response read failed".to_string())?
    {
        if response_body.len().saturating_add(chunk.len()) > BROKER_MAX_RESPONSE_BYTES {
            return Err("Gaia operator broker upstream response is too large".into());
        }
        response_body.extend_from_slice(&chunk);
    }
    let body = serde_json::from_slice(&response_body).unwrap_or_else(|_| {
        serde_json::Value::String(String::from_utf8_lossy(&response_body).into_owned())
    });
    if (200..300).contains(&status) {
        Ok(json!({"status": status, "body": body}))
    } else {
        Err(format!("operator request failed with HTTP {status}"))
    }
}

async fn claim_broker_operation(
    replay: &tokio::sync::Mutex<HashSet<String>>,
    operation_id: &str,
) -> Result<(), String> {
    let mut operations = replay.lock().await;
    if operations.len() >= BROKER_MAX_OPERATIONS {
        return Err("Gaia operator broker operation limit reached".into());
    }
    if !operations.insert(operation_id.to_string()) {
        return Err("Gaia operator broker replay rejected".into());
    }
    Ok(())
}

fn verify_broker_peer(
    stream: &UnixStream,
    expected_executable: ExecutableIdentity,
    process_authority: &GaiaOperatorBrokerProcessAuthority,
) -> Result<(), String> {
    let pid = stream
        .peer_cred()
        .map_err(|_| "Gaia operator broker peer credentials unavailable".to_string())?
        .pid()
        .ok_or_else(|| "Gaia operator broker peer pid unavailable".to_string())?;
    let pid =
        u32::try_from(pid).map_err(|_| "Gaia operator broker peer pid is invalid".to_string())?;
    let metadata = fs::metadata(format!("/proc/{pid}/exe"))
        .map_err(|_| "Gaia operator broker peer executable unavailable".to_string())?;
    let executable = ExecutableIdentity::from_metadata(&metadata);
    let process_group = process_group_for_pid(pid)?;
    verify_broker_peer_authority(
        executable,
        process_group,
        expected_executable,
        process_authority.expected_pgid(),
    )
}

fn process_group_for_pid(pid: u32) -> Result<i32, String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|_| "Gaia operator broker peer process group unavailable".to_string())?;
    let suffix = stat
        .rsplit_once(')')
        .map(|(_, suffix)| suffix)
        .ok_or_else(|| "Gaia operator broker peer process stat is invalid".to_string())?;
    suffix
        .split_whitespace()
        .nth(2)
        .ok_or_else(|| "Gaia operator broker peer process stat is invalid".to_string())?
        .parse::<i32>()
        .map_err(|_| "Gaia operator broker peer process group is invalid".to_string())
}

fn verify_broker_peer_authority(
    executable: ExecutableIdentity,
    process_group: i32,
    expected_executable: ExecutableIdentity,
    expected_process_group: i32,
) -> Result<(), String> {
    if executable != expected_executable {
        return Err("Gaia operator broker peer executable is not sentinel-ctl".into());
    }
    if expected_process_group <= 0 {
        return Err("Gaia operator broker process authority is unbound".into());
    }
    if process_group != expected_process_group {
        return Err("Gaia operator broker peer is outside the active session process group".into());
    }
    Ok(())
}

fn validate_broker_request(
    request: &BrokerRequest,
    expected_session_id: &str,
    expected_capability: &str,
) -> Result<(), String> {
    if request.schema_version != BROKER_SCHEMA_VERSION
        || request.session_id != expected_session_id
        || request.capability != expected_capability
    {
        return Err("Gaia operator broker capability rejected".into());
    }
    if !(8..=160).contains(&request.operation_id.len())
        || !request
            .operation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("Gaia operator broker operation id is invalid".into());
    }
    if request.method != "GET"
        || request.gateway
        || request.body.is_some()
        || request.risk != "read"
        || request.confirmed
        || !broker_read_path_allowed(&request.path)
    {
        return Err("Gaia operator broker permits only bodyless read observations".into());
    }
    Ok(())
}

fn broker_read_path_allowed(path: &str) -> bool {
    matches!(
        path,
        "/operator/platform-state"
            | "/operator/runtime-health"
            | "/operator/snapshots"
            | "/operator/security/fs-stats"
    )
}

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
    let broker = match GaiaOperatorBroker::start(&st, &config).await {
        Ok(broker) => broker,
        Err(error) => {
            tracing::warn!(error = %error, "Gaia operator broker unavailable");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "Gaia operator broker unavailable"})),
            )
                .into_response();
        }
    };
    let runner = ClaudeSessionRunner::new(config).with_operator_broker(broker.capability());
    let result = runner.run(request).await;
    broker.shutdown().await;
    match result {
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

    fn broker_request_fixture() -> BrokerRequest {
        BrokerRequest {
            schema_version: BROKER_SCHEMA_VERSION,
            session_id: "gaia-broker-session-1".to_string(),
            capability: "opaque-capability-0123456789abcdef".to_string(),
            operation_id: "operation-1".to_string(),
            method: "GET".to_string(),
            gateway: false,
            path: "/operator/runtime-health".to_string(),
            body: None,
            risk: "read".to_string(),
            confirmed: false,
        }
    }

    #[test]
    fn broker_peer_requires_bound_exact_session_process_group() {
        let executable =
            ExecutableIdentity::from_metadata(&fs::metadata("/proc/self/exe").unwrap());
        let current_group = process_group_for_pid(std::process::id()).unwrap();
        assert!(current_group > 0);
        assert!(
            verify_broker_peer_authority(executable, current_group, executable, 0)
                .unwrap_err()
                .contains("unbound")
        );
        assert!(verify_broker_peer_authority(
            executable,
            current_group,
            executable,
            current_group + 1,
        )
        .unwrap_err()
        .contains("outside"));
        assert!(
            verify_broker_peer_authority(executable, current_group, executable, current_group,)
                .is_ok()
        );
    }

    #[tokio::test]
    async fn broker_rejects_foreign_session_replay_and_malformed_reads() {
        let request = broker_request_fixture();
        assert!(validate_broker_request(
            &request,
            "gaia-broker-session-1",
            "opaque-capability-0123456789abcdef"
        )
        .is_ok());

        let mut foreign = broker_request_fixture();
        foreign.session_id = "gaia-broker-foreign".to_string();
        assert!(validate_broker_request(
            &foreign,
            "gaia-broker-session-1",
            "opaque-capability-0123456789abcdef"
        )
        .is_err());

        let mut body = broker_request_fixture();
        body.body = Some(json!({"not": "allowed"}));
        assert!(validate_broker_request(
            &body,
            "gaia-broker-session-1",
            "opaque-capability-0123456789abcdef"
        )
        .is_err());

        let mut wrong_risk = broker_request_fixture();
        wrong_risk.risk = "mutate".to_string();
        assert!(validate_broker_request(
            &wrong_risk,
            "gaia-broker-session-1",
            "opaque-capability-0123456789abcdef"
        )
        .is_err());

        let replay = tokio::sync::Mutex::new(HashSet::new());
        assert!(claim_broker_operation(&replay, "operation-1").await.is_ok());
        assert!(claim_broker_operation(&replay, "operation-1")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn rejected_mutation_classes_and_malformed_reads_make_zero_upstream_calls() {
        let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let operator_url = format!("http://{}", http_listener.local_addr().unwrap());
        let replay = Arc::new(tokio::sync::Mutex::new(HashSet::new()));
        let mut rejected = Vec::new();

        let mut mutate = broker_request_fixture();
        mutate.operation_id = "operation-mutate".to_string();
        mutate.method = "POST".to_string();
        mutate.path = "/operator/snapshot".to_string();
        mutate.risk = "mutate".to_string();
        mutate.confirmed = true;
        mutate.body = Some(json!({"tier": "manual"}));
        rejected.push(mutate);

        let mut high_risk = broker_request_fixture();
        high_risk.operation_id = "operation-high-risk".to_string();
        high_risk.method = "POST".to_string();
        high_risk.path = "/operator/runtime/reconcile".to_string();
        high_risk.risk = "high-risk".to_string();
        high_risk.confirmed = true;
        rejected.push(high_risk);

        let mut gateway = broker_request_fixture();
        gateway.operation_id = "operation-gateway".to_string();
        gateway.method = "POST".to_string();
        gateway.gateway = true;
        gateway.path = "/control/reload".to_string();
        gateway.risk = "mutate".to_string();
        gateway.confirmed = true;
        rejected.push(gateway);

        let mut read_body = broker_request_fixture();
        read_body.operation_id = "operation-read-body".to_string();
        read_body.body = Some(json!({"unexpected": true}));
        rejected.push(read_body);

        let mut unknown_read = broker_request_fixture();
        unknown_read.operation_id = "operation-unknown-read".to_string();
        unknown_read.path = "/operator/private".to_string();
        rejected.push(unknown_read);

        let mut wrong_risk = broker_request_fixture();
        wrong_risk.operation_id = "operation-wrong-risk".to_string();
        wrong_risk.risk = "mutate".to_string();
        rejected.push(wrong_risk);

        let mut confirmed_read = broker_request_fixture();
        confirmed_read.operation_id = "operation-confirmed-read".to_string();
        confirmed_read.confirmed = true;
        rejected.push(confirmed_read);

        let mut malformed_method = broker_request_fixture();
        malformed_method.operation_id = "operation-malformed-method".to_string();
        malformed_method.method = "get".to_string();
        rejected.push(malformed_method);

        for request in rejected {
            assert!(process_broker_request(
                request,
                "gaia-broker-session-1",
                "opaque-capability-0123456789abcdef",
                replay.clone(),
                reqwest::Client::new(),
                &operator_url,
                "secret-never-used",
            )
            .await
            .is_err());
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(25), http_listener.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn trusted_broker_attaches_operator_header_once() {
        let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_address = http_listener.local_addr().unwrap();
        let upstream = tokio::spawn(async move {
            let (mut stream, _) = http_listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&chunk[..count]);
                assert!(request.len() <= 4096);
            }
            let request = String::from_utf8_lossy(&request);
            assert!(request.contains("x-sentinel-operator-key: 0123456789abcdef0123456789abcdef"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
                )
                .await
                .unwrap();
        });

        let mut request = broker_request_fixture();
        request.operation_id = "operation-trusted-read".to_string();
        let value = process_broker_request(
            request,
            "gaia-broker-session-1",
            "opaque-capability-0123456789abcdef",
            Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            reqwest::Client::new(),
            &format!("http://{http_address}"),
            "0123456789abcdef0123456789abcdef",
        )
        .await
        .unwrap();
        assert_eq!(value["status"], 200);
        upstream.await.unwrap();
    }
}
