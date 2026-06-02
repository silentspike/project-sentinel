//! Lokale Operator-API fuer manuelle Chaos-Trigger.
//!
//! Dashboard schreibt nicht direkt in EventStore/Projection, sondern spricht
//! diese Loopback-API an. Die API validiert Raum und Payload und leitet das
//! Kommando via std::sync::mpsc in den laufenden ECS-Thread.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result as AnyResult};
use sentinel_fs::{
    cas::CasStore,
    layer::LayerManager,
    metadata::{FileKind, MetadataStore},
};
use sentinel_redb::{ApiCpSnapshot, StateStore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};

use sentinel_common::{
    DomainEvent, DomainEventPayload, EventType, OperatorBroadcastCommand, OperatorChaosCommand,
    OperatorChatCommand, OperatorCommand, OperatorDmCommand, OperatorGaiaCommand,
    OperatorNightrunCommand, OperatorRoomStimulusCommand, OperatorTaskCommand, RoomStimulusType,
};

use crate::config::OperatorApiConfig;
use crate::platform_controlplane::{
    PlatformAnalysisCommand, PlatformControlCommand, PlatformStateSnapshot,
    PlatformTriggerTestCommand,
};
use crate::runtime_control::{
    RuntimeAnalysisFloodTestRequest, RuntimeAnalysisFloodTestResponse, RuntimeControlCommand,
    RuntimePanicTestRequest, RuntimePanicTestResponse, RuntimeReconcileRequest,
    RuntimeReconcileResponse, RuntimeStallRestartTestRequest, RuntimeStallRestartTestResponse,
};

const OPERATOR_CHAOS_PATH: &str = "/operator/chaos";
const OPERATOR_STIMULUS_PATH: &str = "/operator/stimulus";
const OPERATOR_NIGHTRUN_PATH: &str = "/operator/nightrun";
const OPERATOR_SNAPSHOTS_PATH: &str = "/operator/snapshots";
const OPERATOR_SNAPSHOT_PATH: &str = "/operator/snapshot";
const OPERATOR_RESTORE_PATH: &str = "/operator/restore";
const OPERATOR_CONFIG_APPLY_PATH: &str = "/operator/config/apply";
const OPERATOR_MIGRATE_PATH: &str = "/operator/migrate";
const OPERATOR_PRUNE_PATH: &str = "/operator/prune";
const OPERATOR_CHAT_PATH: &str = "/operator/chat";
const OPERATOR_GAIA_PATH: &str = "/operator/gaia";
const OPERATOR_BROADCAST_PATH: &str = "/operator/broadcast";
const OPERATOR_TASK_PATH: &str = "/operator/task";
const OPERATOR_DM_PATH: &str = "/operator/dm";
const OPERATOR_PLATFORM_ANALYZE_PATH: &str = "/operator/platform-analyze";
const OPERATOR_PLATFORM_TRIGGER_TEST_PATH: &str = "/operator/platform-trigger-test";
const OPERATOR_PLATFORM_ANALYSIS_TEST_PATH: &str = "/operator/platform-analysis-test";
const OPERATOR_PLATFORM_STATE_PATH: &str = "/operator/platform-state";
const OPERATOR_RUNTIME_HEALTH_PATH: &str = "/operator/runtime-health";
const OPERATOR_RUNTIME_RECONCILE_PATH: &str = "/operator/runtime/reconcile";
const OPERATOR_RUNTIME_ANALYSIS_FLOOD_TEST_PATH: &str = "/operator/runtime/analysis-flood-test";
const OPERATOR_RUNTIME_PANIC_TEST_PATH: &str = "/operator/runtime/panic-test";
const OPERATOR_RUNTIME_STALL_RESTART_TEST_PATH: &str = "/operator/runtime/stall-restart-test";
const OPERATOR_APICP_SNAPSHOT_PATH: &str = "/operator/apicp/snapshot";
const OPERATOR_SECURITY_FS_TRASH_PATH: &str = "/operator/security/fs-trash";
const OPERATOR_SECURITY_FS_TRASH_FIXTURE_PATH: &str = "/operator/security/fs-trash-fixture";
const OPERATOR_SECURITY_FS_TRASH_AGE_PATH: &str = "/operator/security/fs-trash-age";
const OPERATOR_SECURITY_FS_TRASH_GC_PATH: &str = "/operator/security/fs-trash-gc";
const OPERATOR_SECURITY_FS_STATS_PATH: &str = "/operator/security/fs-stats";
const OPERATOR_SECURITY_FS_DEDUP_BENCHMARK_PATH: &str = "/operator/security/fs-dedup-benchmark";
const OPERATOR_SECURITY_FS_RANSOMWARE_TEST_PATH: &str = "/operator/security/fs-ransomware-test";
const OPERATOR_SECURITY_AGENT_RUNTIME_STATE_PATH: &str = "/operator/security/agent-runtime-state";
const OPERATOR_SECURITY_WRITE_ANOMALY_TEST_PATH: &str = "/operator/security/write-anomaly-test";
const OPERATOR_SECURITY_LANDLOCK_TEST_PATH: &str = "/operator/security/landlock-test";
const MAX_REQUEST_BYTES: usize = 32 * 1024;
const MAX_BODY_BYTES: usize = 8 * 1024;
const MAX_APICP_SNAPSHOT_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Config-Apply (#425) traegt eine ganze Firma inline (agents[] + building) — eine 60er-Firma
/// ist ~64 KB, Gaia-Firmen koennen deutlich groesser sein. Grosszuegiges Limit (4 MB).
const MAX_CONFIG_APPLY_BODY_BYTES: usize = 4 * 1024 * 1024;
const OPERATOR_KEY_HEADER: &str = "x-sentinel-operator-key";

#[derive(Debug, Clone, Deserialize)]
pub struct TriggerChaosRequest {
    pub room_id: String,
    pub chaos_type: EventType,
    #[serde(default)]
    pub duration_ticks: Option<u64>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TriggerChaosResponse {
    pub accepted: bool,
    pub event_id: String,
    pub room_id: String,
    pub chaos_type: EventType,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TriggerStimulusRequest {
    pub room_id: String,
    pub stimulus_type: RoomStimulusType,
    pub delta: f32,
    #[serde(default)]
    pub duration_ticks: Option<u64>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TriggerStimulusResponse {
    pub accepted: bool,
    pub event_id: String,
    pub room_id: String,
    pub stimulus_type: RoomStimulusType,
    pub delta: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TriggerNightrunRequest {
    /// Optionale Schicht-Nummer (1-3). None = letzte abgelaufene Schicht.
    #[serde(default)]
    pub shift_set: Option<u8>,
    /// Nur simulieren, nicht persistieren.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TriggerNightrunResponse {
    pub accepted: bool,
    #[serde(default)]
    pub agents_queued: u32,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct NightrunAgentCounts {
    total: u32,
    by_shift: HashMap<u8, u32>,
}

impl NightrunAgentCounts {
    pub fn from_shift_sets(shift_sets: impl IntoIterator<Item = u8>) -> Self {
        let mut total = 0u32;
        let mut by_shift = HashMap::new();
        for shift_set in shift_sets {
            if shift_set == 0 {
                continue;
            }
            total += 1;
            *by_shift.entry(shift_set).or_insert(0) += 1;
        }
        Self { total, by_shift }
    }

    fn agents_queued(&self, shift_set: Option<u8>) -> u32 {
        match shift_set {
            Some(shift) => self.by_shift.get(&shift).copied().unwrap_or(0),
            None => self.total,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecurityAgentRuntimeSnapshot {
    pub agent_id: u16,
    pub aggregate_id: String,
    pub agent_name: String,
    pub bwrap_pid: Option<u32>,
    pub home_host_path: String,
    #[serde(default)]
    pub fs_mount: Option<String>,
}

pub type SharedSecurityRuntimeState =
    Arc<std::sync::RwLock<HashMap<u16, SecurityAgentRuntimeSnapshot>>>;

#[derive(Debug, Clone, Deserialize)]
struct FsTrashFixtureRequest {
    agent_name: String,
    relative_path: String,
    content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FsTrashFixtureResponse {
    accepted: bool,
    agent_name: String,
    relative_path: String,
    object_id: u64,
    chunk_hashes: Vec<String>,
    trashed_chunks: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FsTrashInspectResponse {
    found: bool,
    chunk_hash: String,
    trashed_at_ms: Option<u64>,
    age_ms: Option<u64>,
    in_chunk_index: bool,
    refcount: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct FsTrashAgeRequest {
    chunk_hash: String,
    hours_ago: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FsTrashAgeResponse {
    accepted: bool,
    chunk_hash: String,
    trashed_at_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct FsTrashGcRequest {
    grace_period_hours: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FsTrashGcResponse {
    accepted: bool,
    grace_period_hours: u64,
    freed_from_trash: u64,
    freed_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct FsRansomwareTestRequest {
    agent_name: String,
    relative_path: String,
    snapshot_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FsRansomwareTestResponse {
    accepted: bool,
    hook_version: u32,
    agent_name: String,
    relative_path: String,
    snapshot_label: String,
    host_path: String,
    snapshot_id: String,
    bytes_written: usize,
    before_sha256: String,
    mutated_sha256: String,
    restored_sha256: String,
    restored: bool,
    snapshot_wait_ms: u64,
    restore_wait_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AgentRuntimeStateResponse {
    found: bool,
    agent_id: u16,
    aggregate_id: String,
    agent_name: String,
    bwrap_pid: Option<u32>,
    cgroup_path: String,
    current_profile: String,
    home_host_path: String,
    fs_mount: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct FsStorageStatsResponse {
    accepted: bool,
    fs_mount: Option<String>,
    cas_blob_count: u64,
    cas_bytes_on_disk: u64,
    regular_file_count: u64,
    logical_regular_file_bytes: u64,
    dedup_savings_bytes: u64,
    dedup_ratio_percent: f64,
    unreadable_inode_rows: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct FsDedupBenchmarkRequest {
    agent_name: String,
    #[serde(default = "default_fs_dedup_benchmark_writes")]
    writes: u32,
    #[serde(default = "default_fs_dedup_benchmark_bytes")]
    bytes_per_write: usize,
    #[serde(default)]
    file_prefix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct FsLatencySummary {
    min_us: u64,
    p50_us: u64,
    p95_us: u64,
    max_us: u64,
    mean_us: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct FsDedupBenchmarkResponse {
    accepted: bool,
    agent_name: String,
    fs_agent_dir: String,
    file_prefix: String,
    bytes_per_write: usize,
    seed_write_us: u64,
    dedup_hit_writes: u32,
    dedup_hits: u32,
    logical_bytes_written: u64,
    cas_blob_count_before: u64,
    cas_blob_count_after: u64,
    cas_bytes_before: u64,
    cas_bytes_after: u64,
    cas_bytes_delta: u64,
    dedup_ratio_percent: f64,
    target_87_percent_met: bool,
    dedup_hit_latency_us_min: u64,
    dedup_hit_latency_us_p50: u64,
    dedup_hit_latency_us_p95: u64,
    dedup_hit_latency_us_max: u64,
    dedup_hit_latency_us_mean: f64,
    dedup_hit_cas_check_latency: FsLatencySummary,
    dedup_hit_write_file_latency: FsLatencySummary,
    dedup_hit_loop_latency: FsLatencySummary,
    storage_stats_before_us: u64,
    storage_stats_after_us: u64,
    target_100us_met: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct WriteAnomalyTestRequest {
    agent_name: String,
    mode: String,
    bytes_per_sec: u64,
    #[serde(default)]
    duration_secs: Option<u64>,
    #[serde(default = "default_write_anomaly_alignment")]
    align_to_observation_window: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WriteAnomalyTestResponse {
    accepted: bool,
    agent_name: String,
    mode: String,
    bytes_per_sec: u64,
    duration_secs: u64,
    align_to_observation_window: bool,
    start_delay_secs: u64,
    scheduled_start_tick: u64,
    bwrap_pid: u32,
    helper_pid: u32,
    host_path: String,
}

#[derive(Debug, Clone, Deserialize)]
struct LandlockTestRequest {
    agent_name: String,
    scenario: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct LandlockTestResponse {
    accepted: bool,
    agent_name: String,
    scenario: String,
    helper_pid: u32,
    exit_code: i32,
    blocked: bool,
    attempted_path: Option<String>,
    audit_event_id: Option<String>,
    stdout: String,
    stderr: String,
}

#[derive(Clone)]
struct AppState {
    allowed_rooms: Arc<HashSet<String>>,
    shared_secret: Option<String>,
    data_dir: PathBuf,
    fs_mount: Option<String>,
    fs_layer: Option<Arc<LayerManager>>,
    command_tx: mpsc::Sender<OperatorCommand>,
    platform_tx: mpsc::Sender<PlatformControlCommand>,
    runtime_tx: mpsc::Sender<RuntimeControlCommand>,
    nightrun_tx: mpsc::Sender<OperatorNightrunCommand>,
    nightrun_agent_counts: NightrunAgentCounts,
    snapshot_tx: mpsc::Sender<sentinel_common::OperatorSnapshotCommand>,
    restore_tx: mpsc::Sender<sentinel_common::OperatorRestoreCommand>,
    config_apply_tx: mpsc::Sender<sentinel_common::OperatorConfigApplyCommand>,
    migrate_tx: mpsc::Sender<sentinel_common::OperatorMigrateCommand>,
    config_apply_max_agents: usize,
    config_apply_validation: sentinel_common::agent_config::AgentConfigValidation,
    event_store: Arc<sentinel_limbo::EventStore>,
    prune_tx: mpsc::Sender<i64>,
    state_store: Arc<StateStore>,
    platform_state: Arc<std::sync::RwLock<PlatformStateSnapshot>>,
    runtime_health: crate::runtime_health::SharedRuntimeHealthState,
    security_runtime_state: SharedSecurityRuntimeState,
}

#[derive(Debug, PartialEq, Eq)]
struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

#[derive(Debug, Serialize)]
struct ErrorResponse<'a> {
    error: &'a str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ApiCpSnapshotResponse {
    accepted: bool,
    patterns: usize,
}

#[derive(Debug, PartialEq, Eq)]
enum ApiError {
    BadRequest(&'static str),
    Unauthorized,
    NotFound(&'static str),
    MethodNotAllowed,
    PayloadTooLarge,
    ServiceUnavailable(&'static str),
}

fn default_write_anomaly_alignment() -> bool {
    true
}

fn default_fs_dedup_benchmark_writes() -> u32 {
    64
}

fn default_fs_dedup_benchmark_bytes() -> usize {
    4096
}

impl ApiError {
    fn to_response(&self) -> HttpResponse {
        match self {
            Self::BadRequest(msg) => json_response(400, ErrorResponse { error: msg }),
            Self::Unauthorized => json_response(
                401,
                ErrorResponse {
                    error: "Operator-Authentifizierung fehlgeschlagen",
                },
            ),
            Self::NotFound(msg) => json_response(404, ErrorResponse { error: msg }),
            Self::MethodNotAllowed => json_response(
                405,
                ErrorResponse {
                    error: "Nur POST /operator/chaos oder /operator/stimulus ist erlaubt",
                },
            ),
            Self::PayloadTooLarge => json_response(
                413,
                ErrorResponse {
                    error: "Request zu gross",
                },
            ),
            Self::ServiceUnavailable(msg) => json_response(503, ErrorResponse { error: msg }),
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn start_server(
    config: OperatorApiConfig,
    data_dir: PathBuf,
    fs_mount: Option<String>,
    fs_layer: Option<Arc<LayerManager>>,
    allowed_rooms: Vec<String>,
    command_tx: mpsc::Sender<OperatorCommand>,
    platform_tx: mpsc::Sender<PlatformControlCommand>,
    runtime_tx: mpsc::Sender<RuntimeControlCommand>,
    nightrun_tx: mpsc::Sender<OperatorNightrunCommand>,
    nightrun_agent_counts: NightrunAgentCounts,
    snapshot_tx: mpsc::Sender<sentinel_common::OperatorSnapshotCommand>,
    restore_tx: mpsc::Sender<sentinel_common::OperatorRestoreCommand>,
    config_apply_tx: mpsc::Sender<sentinel_common::OperatorConfigApplyCommand>,
    migrate_tx: mpsc::Sender<sentinel_common::OperatorMigrateCommand>,
    config_apply_max_agents: usize,
    config_apply_validation: sentinel_common::agent_config::AgentConfigValidation,
    event_store: Arc<sentinel_limbo::EventStore>,
    prune_tx: mpsc::Sender<i64>,
    state_store: Arc<sentinel_redb::StateStore>,
    platform_state: Arc<std::sync::RwLock<PlatformStateSnapshot>>,
    runtime_health: crate::runtime_health::SharedRuntimeHealthState,
    security_runtime_state: SharedSecurityRuntimeState,
) -> AnyResult<tokio::task::JoinHandle<()>> {
    let listener = TcpListener::bind(&config.bind_addr)
        .await
        .with_context(|| format!("Operator-API bind fehlgeschlagen: {}", config.bind_addr))?;
    let room_count = allowed_rooms.len();
    let state = AppState {
        allowed_rooms: Arc::new(allowed_rooms.into_iter().collect()),
        shared_secret: config.shared_secret,
        data_dir,
        fs_mount,
        fs_layer,
        command_tx,
        platform_tx,
        runtime_tx,
        nightrun_tx,
        nightrun_agent_counts,
        snapshot_tx,
        restore_tx,
        config_apply_tx,
        migrate_tx,
        config_apply_max_agents,
        config_apply_validation,
        event_store,
        prune_tx,
        state_store,
        platform_state,
        runtime_health,
        security_runtime_state,
    };

    info!(
        bind_addr = %config.bind_addr,
        rooms = room_count,
        auth_enabled = state.shared_secret.is_some(),
        "Operator-API gestartet"
    );

    Ok(tokio::spawn(async move {
        if let Err(err) = server_loop(listener, state).await {
            warn!(error = %err, "Operator-API beendet");
        }
    }))
}

async fn server_loop(listener: TcpListener, state: AppState) -> AnyResult<()> {
    loop {
        let (stream, addr) = listener.accept().await.context("Operator-API accept")?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_connection(stream, state).await {
                debug!(error = %err, remote = %addr, "Operator-API Request fehlgeschlagen");
            }
        });
    }
}

async fn handle_connection(mut stream: TcpStream, state: AppState) -> AnyResult<()> {
    let response = match read_http_request(&mut stream).await {
        Ok(request) => handle_http_request(request, &state),
        Err(err) => err.to_response(),
    };
    write_http_response(&mut stream, response).await
}

fn handle_http_request(request: HttpRequest, state: &AppState) -> HttpResponse {
    let path_only = request_path(&request.path);
    let query = parse_query_params(&request.path);

    // GET-Endpoints ohne Auth (read-only)
    if request.method == "GET" {
        if is_protected_read_path(path_only)
            && !is_authorized(&request.headers, state.shared_secret.as_deref())
        {
            return ApiError::Unauthorized.to_response();
        }
        return match path_only {
            OPERATOR_SNAPSHOTS_PATH => match state.event_store.list_world_snapshots() {
                Ok(snapshots) => json_response(200, snapshots),
                Err(_e) => {
                    ApiError::ServiceUnavailable("Snapshot-Liste nicht verfuegbar").to_response()
                }
            },
            OPERATOR_APICP_SNAPSHOT_PATH => match state.state_store.get_api_patterns_snapshot() {
                Ok(Some(snapshot)) => HttpResponse {
                    status: 200,
                    body: snapshot,
                },
                Ok(None) => json_response(
                    200,
                    ApiCpSnapshot {
                        patterns: Vec::new(),
                        synth_count: 0,
                        last_evolution_versions: HashMap::new(),
                    },
                ),
                Err(_) => {
                    ApiError::ServiceUnavailable("API-CP Snapshot nicht verfuegbar").to_response()
                }
            },
            OPERATOR_PLATFORM_STATE_PATH => match state.platform_state.read() {
                Ok(snapshot) => json_response(200, snapshot.clone()),
                Err(_) => {
                    ApiError::ServiceUnavailable("Platform-State nicht verfuegbar").to_response()
                }
            },
            OPERATOR_RUNTIME_HEALTH_PATH => match state.runtime_health.read() {
                Ok(snapshot) => json_response(200, snapshot.clone()),
                Err(_) => {
                    ApiError::ServiceUnavailable("Runtime-Health nicht verfuegbar").to_response()
                }
            },
            OPERATOR_SECURITY_FS_TRASH_PATH => match inspect_fs_trash(query.get("hash"), state) {
                Ok(payload) => json_response(200, payload),
                Err(err) => err.to_response(),
            },
            OPERATOR_SECURITY_FS_STATS_PATH => match inspect_fs_storage_stats(state) {
                Ok(payload) => json_response(200, payload),
                Err(err) => err.to_response(),
            },
            OPERATOR_SECURITY_AGENT_RUNTIME_STATE_PATH => {
                match inspect_agent_runtime_state(query.get("agent_id"), state) {
                    Ok(payload) => json_response(200, payload),
                    Err(err) => err.to_response(),
                }
            }
            _ => ApiError::NotFound("Endpoint unbekannt").to_response(),
        };
    }
    if request.method != "POST" {
        return ApiError::MethodNotAllowed.to_response();
    }
    if !is_authorized(&request.headers, state.shared_secret.as_deref()) {
        return ApiError::Unauthorized.to_response();
    }

    match path_only {
        OPERATOR_CHAOS_PATH => {
            let payload: TriggerChaosRequest = match serde_json::from_slice(&request.body) {
                Ok(payload) => payload,
                Err(_) => {
                    return ApiError::BadRequest("Request-JSON ungueltig").to_response();
                }
            };

            match dispatch_chaos_trigger(payload, state) {
                Ok(response) => json_response(202, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_STIMULUS_PATH => {
            let payload: TriggerStimulusRequest = match serde_json::from_slice(&request.body) {
                Ok(payload) => payload,
                Err(_) => {
                    return ApiError::BadRequest("Request-JSON ungueltig").to_response();
                }
            };

            match dispatch_stimulus_trigger(payload, state) {
                Ok(response) => json_response(202, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_NIGHTRUN_PATH => {
            let payload: TriggerNightrunRequest =
                serde_json::from_slice(&request.body).unwrap_or(TriggerNightrunRequest {
                    shift_set: None,
                    dry_run: false,
                });

            match dispatch_nightrun_trigger(payload, state) {
                Ok(response) => json_response(202, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_SNAPSHOT_PATH => {
            let payload: sentinel_common::OperatorSnapshotCommand =
                serde_json::from_slice(&request.body)
                    .unwrap_or(sentinel_common::OperatorSnapshotCommand { tier: None });
            info!("Manueller Snapshot via Operator-API angefordert");
            match state.snapshot_tx.send(payload) {
                Ok(()) => json_response(
                    202,
                    TriggerNightrunResponse {
                        accepted: true,
                        agents_queued: 0,
                        message: "Snapshot-Erstellung gestartet".to_string(),
                    },
                ),
                Err(_) => {
                    ApiError::ServiceUnavailable("Snapshot-Channel nicht verfuegbar").to_response()
                }
            }
        }
        OPERATOR_RESTORE_PATH => {
            let payload: sentinel_common::OperatorRestoreCommand =
                match serde_json::from_slice(&request.body) {
                    Ok(p) => p,
                    Err(_) => {
                        return ApiError::BadRequest("Request-JSON ungueltig (snapshot_id fehlt)")
                            .to_response();
                    }
                };
            info!(snapshot_id = %payload.snapshot_id, "Restore via Operator-API angefordert");
            match state.restore_tx.send(payload) {
                Ok(()) => json_response(
                    202,
                    TriggerNightrunResponse {
                        accepted: true,
                        agents_queued: 0,
                        message: "Restore gestartet".to_string(),
                    },
                ),
                Err(_) => {
                    ApiError::ServiceUnavailable("Restore-Channel nicht verfuegbar").to_response()
                }
            }
        }
        OPERATOR_CONFIG_APPLY_PATH => {
            let payload: sentinel_common::OperatorConfigApplyCommand =
                match serde_json::from_slice(&request.body) {
                    Ok(p) => p,
                    Err(_) => {
                        return ApiError::BadRequest(
                            "Request-JSON ungueltig (erwartet {mode, agents[], building})",
                        )
                        .to_response();
                    }
                };
            // Validierung VOR dem Send — invalid => 4xx, KEIN Apply (#425 AC-3).
            if let Err(errors) = crate::config_apply::validate_config_apply(
                &payload,
                state.config_apply_max_agents,
                state.config_apply_validation,
            ) {
                warn!(
                    error_count = errors.len(),
                    "Config-Apply abgelehnt (Validierung)"
                );
                return json_response(
                    400,
                    serde_json::json!({
                        "accepted": false,
                        "errors": errors,
                    }),
                );
            }
            info!(
                mode = ?payload.mode,
                agents = payload.agents.len(),
                rooms = payload.building.rooms.len(),
                "Config-Apply via Operator-API angefordert"
            );
            match state.config_apply_tx.send(payload) {
                Ok(()) => json_response(
                    202,
                    serde_json::json!({
                        "accepted": true,
                        "message": "Config-Apply gestartet (tick-synchron)"
                    }),
                ),
                Err(_) => ApiError::ServiceUnavailable("Config-Apply-Channel nicht verfuegbar")
                    .to_response(),
            }
        }
        OPERATOR_MIGRATE_PATH => {
            // Nano-Container Live-Migration (#413): lokaler Snapshot->Restore-Handoff der
            // ECS-native-Instanz (tick-synchron im Daemon). reason ist serde(default) =>
            // leerer Body erlaubt. Cross-node/Netzwerk-Migration ist out-of-scope.
            let payload: sentinel_common::OperatorMigrateCommand = serde_json::from_slice(
                &request.body,
            )
            .unwrap_or(sentinel_common::OperatorMigrateCommand {
                reason: String::new(),
            });
            info!(
                reason = %payload.reason,
                "Nano-Container Live-Migration via Operator-API angefordert"
            );
            match state.migrate_tx.send(payload) {
                Ok(()) => json_response(
                    202,
                    serde_json::json!({
                        "accepted": true,
                        "message": "Live-Migration gestartet (lokal, tick-synchron)"
                    }),
                ),
                Err(_) => {
                    ApiError::ServiceUnavailable("Migrate-Channel nicht verfuegbar").to_response()
                }
            }
        }
        OPERATOR_PRUNE_PATH => {
            info!("Manuelles Pruning via Operator-API angefordert");
            let snapshots = state.event_store.list_world_snapshots().unwrap_or_default();
            if snapshots.len() < 2 {
                return json_response(
                    200,
                    serde_json::json!({"accepted": false, "message": "Zu wenige Snapshots fuer Pruning"}),
                );
            }
            let prune_point = snapshots[snapshots.len() - 2].last_event_id;
            if !state.event_store.can_prune(prune_point).unwrap_or(false) {
                return json_response(
                    200,
                    serde_json::json!({"accepted": false, "message": "Safety Guard: Projection-Offset oder Outbox blockiert Pruning"}),
                );
            }
            match state.prune_tx.send(prune_point) {
                Ok(()) => json_response(
                    202,
                    serde_json::json!({"accepted": true, "message": "Pruning gestartet (1000 Rows/Tick)"}),
                ),
                Err(_) => {
                    ApiError::ServiceUnavailable("Prune-Channel nicht verfuegbar").to_response()
                }
            }
        }
        OPERATOR_CHAT_PATH => {
            let cmd: OperatorChatCommand = match serde_json::from_slice(&request.body) {
                Ok(p) => p,
                Err(_) => {
                    return ApiError::BadRequest(
                        "JSON ungueltig (room_id, message, sender_name noetig)",
                    )
                    .to_response();
                }
            };
            info!(room = %cmd.room_id, sender = %cmd.sender_name, "Operator-Chat empfangen");
            match state.command_tx.send(OperatorCommand::Chat(cmd)) {
                Ok(()) => json_response(
                    202,
                    serde_json::json!({"accepted": true, "message": "Chat in RoomChatBuffer eingefuegt"}),
                ),
                Err(_) => {
                    ApiError::ServiceUnavailable("Command-Channel nicht verfuegbar").to_response()
                }
            }
        }
        OPERATOR_GAIA_PATH => {
            let cmd: OperatorGaiaCommand = match serde_json::from_slice(&request.body) {
                Ok(p) => p,
                Err(_) => {
                    return ApiError::BadRequest(
                        "JSON ungueltig (target_agent_id, thought noetig)",
                    )
                    .to_response();
                }
            };
            info!(agent_id = cmd.target_agent_id, "Voice of Gaia empfangen");
            match state.command_tx.send(OperatorCommand::Gaia(cmd)) {
                Ok(()) => json_response(
                    202,
                    serde_json::json!({"accepted": true, "message": "Gedanke eingepflanzt"}),
                ),
                Err(_) => {
                    ApiError::ServiceUnavailable("Command-Channel nicht verfuegbar").to_response()
                }
            }
        }
        OPERATOR_TASK_PATH => {
            let cmd: OperatorTaskCommand = match serde_json::from_slice(&request.body) {
                Ok(p) => p,
                Err(_) => {
                    return ApiError::BadRequest(
                        "JSON ungueltig (action + je nach Aktion task_id/title/assigned_to noetig)",
                    )
                    .to_response();
                }
            };
            info!(action = ?cmd.action, task_id = ?cmd.task_id, "Task-Kommando empfangen (#438)");
            match state.command_tx.send(OperatorCommand::Task(cmd)) {
                Ok(()) => json_response(
                    202,
                    serde_json::json!({"accepted": true, "message": "Task-Kommando angenommen"}),
                ),
                Err(_) => {
                    ApiError::ServiceUnavailable("Command-Channel nicht verfuegbar").to_response()
                }
            }
        }
        OPERATOR_DM_PATH => {
            let cmd: OperatorDmCommand = match serde_json::from_slice(&request.body) {
                Ok(p) => p,
                Err(_) => {
                    return ApiError::BadRequest(
                        "JSON ungueltig (target_agent_id, message, sender_name noetig)",
                    )
                    .to_response();
                }
            };
            info!(agent_id = cmd.target_agent_id, "DM empfangen (#437)");
            match state.command_tx.send(OperatorCommand::Dm(cmd)) {
                Ok(()) => json_response(
                    202,
                    serde_json::json!({"accepted": true, "message": "DM zugestellt"}),
                ),
                Err(_) => {
                    ApiError::ServiceUnavailable("Command-Channel nicht verfuegbar").to_response()
                }
            }
        }
        OPERATOR_BROADCAST_PATH => {
            let cmd: OperatorBroadcastCommand = match serde_json::from_slice(&request.body) {
                Ok(p) => p,
                Err(_) => {
                    return ApiError::BadRequest("JSON ungueltig (message noetig)").to_response();
                }
            };
            info!(broadcast_type = %cmd.broadcast_type, "Broadcast empfangen");
            match state.command_tx.send(OperatorCommand::Broadcast(cmd)) {
                Ok(()) => json_response(
                    202,
                    serde_json::json!({"accepted": true, "message": "Durchsage gesendet"}),
                ),
                Err(_) => {
                    ApiError::ServiceUnavailable("Command-Channel nicht verfuegbar").to_response()
                }
            }
        }
        OPERATOR_PLATFORM_ANALYZE_PATH => {
            if !request.body.is_empty()
                && serde_json::from_slice::<serde_json::Value>(&request.body).is_err()
            {
                return ApiError::BadRequest("Request-JSON ungueltig").to_response();
            }
            match dispatch_platform_analyze(state) {
                Ok(response) => json_response(202, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_PLATFORM_TRIGGER_TEST_PATH => {
            let payload: PlatformTriggerTestCommand = match serde_json::from_slice(&request.body) {
                Ok(p) => p,
                Err(_) => {
                    return ApiError::BadRequest("Request-JSON ungueltig").to_response();
                }
            };
            match dispatch_platform_trigger_test(payload, state) {
                Ok(response) => json_response(202, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_PLATFORM_ANALYSIS_TEST_PATH => {
            let payload: PlatformAnalysisCommand = match serde_json::from_slice(&request.body) {
                Ok(p) => p,
                Err(_) => {
                    return ApiError::BadRequest("Request-JSON ungueltig").to_response();
                }
            };
            match dispatch_platform_analysis_test(payload, state) {
                Ok(response) => json_response(202, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_RUNTIME_RECONCILE_PATH => {
            let payload: RuntimeReconcileRequest =
                serde_json::from_slice(&request.body).unwrap_or_default();
            match dispatch_runtime_reconcile(payload, state) {
                Ok(response) => json_response(200, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_RUNTIME_ANALYSIS_FLOOD_TEST_PATH => {
            let payload: RuntimeAnalysisFloodTestRequest =
                match serde_json::from_slice(&request.body) {
                    Ok(payload) => payload,
                    Err(_) => {
                        return ApiError::BadRequest("Request-JSON ungueltig").to_response();
                    }
                };
            match dispatch_runtime_analysis_flood_test(payload, state) {
                Ok(response) => json_response(200, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_RUNTIME_PANIC_TEST_PATH => {
            let payload: RuntimePanicTestRequest = match serde_json::from_slice(&request.body) {
                Ok(payload) => payload,
                Err(_) => {
                    return ApiError::BadRequest("Request-JSON ungueltig").to_response();
                }
            };
            match dispatch_runtime_panic_test(payload, state) {
                Ok(response) => json_response(200, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_RUNTIME_STALL_RESTART_TEST_PATH => {
            let payload: RuntimeStallRestartTestRequest =
                match serde_json::from_slice(&request.body) {
                    Ok(payload) => payload,
                    Err(_) => {
                        return ApiError::BadRequest("Request-JSON ungueltig").to_response();
                    }
                };
            match dispatch_runtime_stall_restart_test(payload, state) {
                Ok(response) => json_response(200, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_APICP_SNAPSHOT_PATH => {
            let payload: ApiCpSnapshot = match serde_json::from_slice(&request.body) {
                Ok(p) => p,
                Err(_) => {
                    return ApiError::BadRequest("Request-JSON ungueltig").to_response();
                }
            };
            let data = match serde_json::to_vec(&payload) {
                Ok(d) => d,
                Err(_) => return ApiError::BadRequest("Request-JSON ungueltig").to_response(),
            };
            match state.state_store.set_api_patterns_snapshot(&data) {
                Ok(()) => json_response(
                    200,
                    ApiCpSnapshotResponse {
                        accepted: true,
                        patterns: payload.patterns.len(),
                    },
                ),
                Err(_) => {
                    ApiError::ServiceUnavailable("API-CP Snapshot konnte nicht persistiert werden")
                        .to_response()
                }
            }
        }
        OPERATOR_SECURITY_FS_TRASH_FIXTURE_PATH => {
            let payload: FsTrashFixtureRequest = match serde_json::from_slice(&request.body) {
                Ok(p) => p,
                Err(_) => {
                    return ApiError::BadRequest("Request-JSON ungueltig").to_response();
                }
            };
            match create_fs_trash_fixture(payload, state) {
                Ok(response) => json_response(202, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_SECURITY_FS_TRASH_AGE_PATH => {
            let payload: FsTrashAgeRequest = match serde_json::from_slice(&request.body) {
                Ok(p) => p,
                Err(_) => {
                    return ApiError::BadRequest("Request-JSON ungueltig").to_response();
                }
            };
            match set_fs_trash_age(payload, state) {
                Ok(response) => json_response(200, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_SECURITY_FS_TRASH_GC_PATH => {
            let payload: FsTrashGcRequest = match serde_json::from_slice(&request.body) {
                Ok(p) => p,
                Err(_) => {
                    return ApiError::BadRequest("Request-JSON ungueltig").to_response();
                }
            };
            match run_fs_trash_gc(payload, state) {
                Ok(response) => json_response(200, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_SECURITY_FS_DEDUP_BENCHMARK_PATH => {
            let payload: FsDedupBenchmarkRequest = match serde_json::from_slice(&request.body) {
                Ok(p) => p,
                Err(_) => {
                    return ApiError::BadRequest("Request-JSON ungueltig").to_response();
                }
            };
            match run_fs_dedup_benchmark(payload, state) {
                Ok(response) => json_response(200, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_SECURITY_FS_RANSOMWARE_TEST_PATH => {
            let payload: FsRansomwareTestRequest = match serde_json::from_slice(&request.body) {
                Ok(p) => p,
                Err(_) => {
                    return ApiError::BadRequest("Request-JSON ungueltig").to_response();
                }
            };
            match run_fs_ransomware_test(payload, state) {
                Ok(response) => json_response(202, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_SECURITY_WRITE_ANOMALY_TEST_PATH => {
            let payload: WriteAnomalyTestRequest = match serde_json::from_slice(&request.body) {
                Ok(p) => p,
                Err(_) => {
                    return ApiError::BadRequest("Request-JSON ungueltig").to_response();
                }
            };
            match run_write_anomaly_test(payload, state) {
                Ok(response) => json_response(202, response),
                Err(err) => err.to_response(),
            }
        }
        OPERATOR_SECURITY_LANDLOCK_TEST_PATH => {
            let payload: LandlockTestRequest = match serde_json::from_slice(&request.body) {
                Ok(p) => p,
                Err(_) => {
                    return ApiError::BadRequest("Request-JSON ungueltig").to_response();
                }
            };
            match run_landlock_test(payload, state) {
                Ok(response) => json_response(200, response),
                Err(err) => err.to_response(),
            }
        }
        _ => ApiError::NotFound("Endpoint unbekannt").to_response(),
    }
}

fn dispatch_chaos_trigger(
    payload: TriggerChaosRequest,
    state: &AppState,
) -> std::result::Result<TriggerChaosResponse, ApiError> {
    let room_id = payload.room_id.trim();
    if room_id.is_empty() {
        return Err(ApiError::BadRequest("room_id fehlt"));
    }
    if !state.allowed_rooms.contains(room_id) {
        return Err(ApiError::NotFound("room_id unbekannt"));
    }
    if matches!(payload.duration_ticks, Some(0)) {
        return Err(ApiError::BadRequest("duration_ticks muss > 0 sein"));
    }

    let event_id = uuid::Uuid::new_v4().to_string();
    let command = OperatorChaosCommand {
        event_id: event_id.clone(),
        correlation_id: uuid::Uuid::new_v4().to_string(),
        operation_id: uuid::Uuid::new_v4().to_string(),
        room_id: room_id.to_string(),
        chaos_type: payload.chaos_type,
        description: payload
            .description
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(payload.chaos_type.default_description())
            .to_string(),
        duration_ticks: payload.duration_ticks,
    };

    state
        .command_tx
        .send(OperatorCommand::Chaos(command))
        .map_err(|_| ApiError::ServiceUnavailable("ECS-Channel nicht verfuegbar"))?;

    Ok(TriggerChaosResponse {
        accepted: true,
        event_id,
        room_id: room_id.to_string(),
        chaos_type: payload.chaos_type,
    })
}

fn dispatch_stimulus_trigger(
    payload: TriggerStimulusRequest,
    state: &AppState,
) -> std::result::Result<TriggerStimulusResponse, ApiError> {
    let room_id = payload.room_id.trim();
    if room_id.is_empty() {
        return Err(ApiError::BadRequest("room_id fehlt"));
    }
    if !state.allowed_rooms.contains(room_id) {
        return Err(ApiError::NotFound("room_id unbekannt"));
    }
    if matches!(payload.duration_ticks, Some(0)) {
        return Err(ApiError::BadRequest("duration_ticks muss > 0 sein"));
    }
    if !payload.delta.is_finite() || payload.delta.abs() < f32::EPSILON {
        return Err(ApiError::BadRequest(
            "delta muss ungleich 0 und endlich sein",
        ));
    }

    let event_id = uuid::Uuid::new_v4().to_string();
    let description = payload
        .description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| payload.stimulus_type.default_description(payload.delta));
    let command = OperatorRoomStimulusCommand {
        event_id: event_id.clone(),
        correlation_id: uuid::Uuid::new_v4().to_string(),
        operation_id: uuid::Uuid::new_v4().to_string(),
        room_id: room_id.to_string(),
        stimulus_type: payload.stimulus_type,
        delta: payload.delta,
        description,
        duration_ticks: payload.duration_ticks,
    };

    state
        .command_tx
        .send(OperatorCommand::RoomStimulus(command))
        .map_err(|_| ApiError::ServiceUnavailable("ECS-Channel nicht verfuegbar"))?;

    Ok(TriggerStimulusResponse {
        accepted: true,
        event_id,
        room_id: room_id.to_string(),
        stimulus_type: payload.stimulus_type,
        delta: payload.delta,
    })
}

fn dispatch_nightrun_trigger(
    payload: TriggerNightrunRequest,
    state: &AppState,
) -> std::result::Result<TriggerNightrunResponse, ApiError> {
    if let Some(shift) = payload.shift_set {
        if !(1..=3).contains(&shift) {
            return Err(ApiError::BadRequest("shift_set muss 1, 2 oder 3 sein"));
        }
    }

    let command = OperatorNightrunCommand {
        shift_set: payload.shift_set,
        dry_run: payload.dry_run,
    };

    info!(
        shift_set = ?command.shift_set,
        dry_run = command.dry_run,
        "Nightrun-Trigger via Operator-API empfangen"
    );

    let agents_queued = state.nightrun_agent_counts.agents_queued(payload.shift_set);

    state
        .nightrun_tx
        .send(command)
        .map_err(|_| ApiError::ServiceUnavailable("Nightrun-Channel nicht verfuegbar"))?;

    Ok(TriggerNightrunResponse {
        accepted: true,
        agents_queued,
        message: "Nightrun-Konsolidierung gestartet".to_string(),
    })
}

fn dispatch_platform_analyze(
    state: &AppState,
) -> std::result::Result<TriggerNightrunResponse, ApiError> {
    state
        .platform_tx
        .send(PlatformControlCommand::AnalyzeNow)
        .map_err(|_| ApiError::ServiceUnavailable("Platform-Channel nicht verfuegbar"))?;

    Ok(TriggerNightrunResponse {
        accepted: true,
        agents_queued: 0,
        message: "Platform-Analyse eingeplant".to_string(),
    })
}

fn dispatch_platform_trigger_test(
    payload: PlatformTriggerTestCommand,
    state: &AppState,
) -> std::result::Result<TriggerNightrunResponse, ApiError> {
    let trigger = payload.trigger.trim();
    if trigger.is_empty() {
        return Err(ApiError::BadRequest("trigger fehlt"));
    }
    if trigger != "scheduled" && trigger != "unresolved_escalation" {
        return Err(ApiError::BadRequest(
            "trigger muss scheduled oder unresolved_escalation sein",
        ));
    }
    if trigger == "unresolved_escalation" {
        let missing_rule = payload
            .rule_name
            .as_deref()
            .map(str::trim)
            .map(|value| value.is_empty())
            .unwrap_or(true);
        let missing_target = payload
            .target
            .as_deref()
            .map(str::trim)
            .map(|value| value.is_empty())
            .unwrap_or(true);
        if missing_rule || missing_target {
            return Err(ApiError::BadRequest(
                "rule_name und target sind fuer unresolved_escalation Pflicht",
            ));
        }
        if matches!(payload.count, Some(0)) {
            return Err(ApiError::BadRequest("count muss > 0 sein"));
        }
    }

    state
        .platform_tx
        .send(PlatformControlCommand::TriggerTest(payload.clone()))
        .map_err(|_| ApiError::ServiceUnavailable("Platform-Channel nicht verfuegbar"))?;

    Ok(TriggerNightrunResponse {
        accepted: true,
        agents_queued: 0,
        message: format!("Platform-Testtrigger {trigger} eingeplant"),
    })
}

fn dispatch_platform_analysis_test(
    mut payload: PlatformAnalysisCommand,
    state: &AppState,
) -> std::result::Result<TriggerNightrunResponse, ApiError> {
    payload.trigger = payload.trigger.trim().to_string();
    payload.severity = payload.severity.trim().to_string();
    payload.summary = payload.summary.trim().to_string();
    payload.recommendation = payload.recommendation.trim().to_string();
    payload.target = payload.normalized_target();
    payload.suggested_action = payload
        .suggested_action
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    if payload.trigger.is_empty() {
        return Err(ApiError::BadRequest("trigger fehlt"));
    }
    if payload.severity.is_empty() {
        return Err(ApiError::BadRequest("severity fehlt"));
    }
    if payload.summary.is_empty() {
        return Err(ApiError::BadRequest("summary fehlt"));
    }
    if payload.recommendation.is_empty() {
        return Err(ApiError::BadRequest("recommendation fehlt"));
    }

    if let Some(action) = payload.suggested_action.as_deref() {
        match action {
            "force_profile" => {
                if payload.target == "system" {
                    return Err(ApiError::BadRequest(
                        "force_profile braucht einen Agent-Target",
                    ));
                }
                let profile = payload
                    .parameters
                    .get("profile")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_ascii_lowercase())
                    .ok_or(ApiError::BadRequest(
                        "force_profile braucht parameters.profile",
                    ))?;
                if !matches!(profile.as_str(), "idle" | "normal" | "heavy" | "suspended") {
                    return Err(ApiError::BadRequest("parameters.profile ungueltig"));
                }
            }
            "adjust_threshold" => {
                let key = payload
                    .parameters
                    .get("key")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or(ApiError::BadRequest(
                        "adjust_threshold braucht parameters.key",
                    ))?;
                if !payload.parameters.contains_key("value") {
                    return Err(ApiError::BadRequest(
                        "adjust_threshold braucht parameters.value",
                    ));
                }
                let _ = key;
            }
            "escalate_to_operator" => {}
            _ => {
                return Err(ApiError::BadRequest(
                    "suggested_action muss force_profile, adjust_threshold oder escalate_to_operator sein",
                ));
            }
        }
    }

    state
        .platform_tx
        .send(PlatformControlCommand::ApplyAnalysis(payload))
        .map_err(|_| ApiError::ServiceUnavailable("Platform-Channel nicht verfuegbar"))?;

    Ok(TriggerNightrunResponse {
        accepted: true,
        agents_queued: 0,
        message: "Platform-Analyse-Test eingeplant".to_string(),
    })
}

fn dispatch_runtime_reconcile(
    payload: RuntimeReconcileRequest,
    state: &AppState,
) -> std::result::Result<RuntimeReconcileResponse, ApiError> {
    let (response_tx, response_rx) = mpsc::sync_channel(1);
    state
        .runtime_tx
        .send(RuntimeControlCommand::Reconcile {
            request: payload,
            response_tx,
        })
        .map_err(|_| ApiError::ServiceUnavailable("Runtime-Control-Channel nicht verfuegbar"))?;
    response_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| ApiError::ServiceUnavailable("Runtime-Reconcile Timeout"))
}

fn dispatch_runtime_analysis_flood_test(
    payload: RuntimeAnalysisFloodTestRequest,
    state: &AppState,
) -> std::result::Result<RuntimeAnalysisFloodTestResponse, ApiError> {
    if payload.count == 0 {
        return Err(ApiError::BadRequest("count muss > 0 sein"));
    }

    let (response_tx, response_rx) = mpsc::sync_channel(1);
    state
        .runtime_tx
        .send(RuntimeControlCommand::AnalysisFloodTest {
            request: payload,
            response_tx,
        })
        .map_err(|_| ApiError::ServiceUnavailable("Runtime-Control-Channel nicht verfuegbar"))?;
    response_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| ApiError::ServiceUnavailable("Runtime-Analysis-Flood-Test Timeout"))
}

fn dispatch_runtime_panic_test(
    mut payload: RuntimePanicTestRequest,
    state: &AppState,
) -> std::result::Result<RuntimePanicTestResponse, ApiError> {
    payload.worker = payload.worker.trim().to_ascii_lowercase();
    if payload.worker.is_empty() {
        return Err(ApiError::BadRequest("worker fehlt"));
    }
    if payload.worker != "service_health" {
        return Err(ApiError::BadRequest("worker muss service_health sein"));
    }

    let (response_tx, response_rx) = mpsc::sync_channel(1);
    state
        .runtime_tx
        .send(RuntimeControlCommand::PanicTest {
            request: payload,
            response_tx,
        })
        .map_err(|_| ApiError::ServiceUnavailable("Runtime-Control-Channel nicht verfuegbar"))?;
    response_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| ApiError::ServiceUnavailable("Runtime-Panic-Test Timeout"))
}

fn dispatch_runtime_stall_restart_test(
    mut payload: RuntimeStallRestartTestRequest,
    state: &AppState,
) -> std::result::Result<RuntimeStallRestartTestResponse, ApiError> {
    payload.mode = payload.mode.trim().to_ascii_lowercase();
    if payload.agent_id == 0 {
        return Err(ApiError::BadRequest("agent_id fehlt"));
    }
    if payload.mode.is_empty() {
        return Err(ApiError::BadRequest("mode fehlt"));
    }
    if !matches!(payload.mode.as_str(), "sigstop" | "direct") {
        return Err(ApiError::BadRequest("mode muss sigstop oder direct sein"));
    }
    if payload.stall_secs == 0 {
        return Err(ApiError::BadRequest("stall_secs muss > 0 sein"));
    }

    let (response_tx, response_rx) = mpsc::sync_channel(1);
    state
        .runtime_tx
        .send(RuntimeControlCommand::StallRestartTest {
            request: payload,
            response_tx,
        })
        .map_err(|_| ApiError::ServiceUnavailable("Runtime-Control-Channel nicht verfuegbar"))?;
    response_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| ApiError::ServiceUnavailable("Stall-Restart-Test Timeout"))
}

fn request_path(path: &str) -> &str {
    path.split_once('?').map(|(base, _)| base).unwrap_or(path)
}

fn parse_query_params(path: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    let Some((_, query)) = path.split_once('?') else {
        return params;
    };
    for pair in query.split('&').filter(|entry| !entry.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(key.to_string(), value.to_string());
    }
    params
}

fn is_security_path(path: &str) -> bool {
    path.starts_with("/operator/security/")
}

fn is_protected_read_path(path: &str) -> bool {
    path == OPERATOR_APICP_SNAPSHOT_PATH
        || path == OPERATOR_RUNTIME_HEALTH_PATH
        || is_security_path(path)
}

fn open_fs_layer(state: &AppState) -> std::result::Result<Arc<LayerManager>, ApiError> {
    if let Some(layer) = &state.fs_layer {
        return Ok(Arc::clone(layer));
    }
    let cas = CasStore::open(&state.data_dir)
        .map_err(|_| ApiError::ServiceUnavailable("sentinel-fs CAS nicht verfuegbar"))?;
    let meta = MetadataStore::open(state.data_dir.join("metadata.redb"))
        .map_err(|_| ApiError::ServiceUnavailable("sentinel-fs Metadata nicht verfuegbar"))?;
    let layer = Arc::new(LayerManager::new(cas, meta));
    layer
        .init_base_root()
        .map_err(|_| ApiError::ServiceUnavailable("sentinel-fs Base-Root nicht initialisierbar"))?;
    Ok(layer)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn decode_chunk_hash(hex: &str) -> std::result::Result<[u8; 32], ApiError> {
    let trimmed = hex.trim();
    if trimmed.len() != 64 {
        return Err(ApiError::BadRequest(
            "hash muss 64 hex Zeichen (32 Byte) haben",
        ));
    }
    let mut out = [0u8; 32];
    for (idx, chunk) in trimmed.as_bytes().chunks(2).enumerate() {
        let part = std::str::from_utf8(chunk)
            .map_err(|_| ApiError::BadRequest("hash enthaelt ungueltige Bytes"))?;
        out[idx] = u8::from_str_radix(part, 16)
            .map_err(|_| ApiError::BadRequest("hash ist kein gueltiger Hex-String"))?;
    }
    Ok(out)
}

fn fs_parent_and_name(
    layer: &LayerManager,
    agent_name: &str,
    relative_path: &str,
) -> std::result::Result<(u64, String), ApiError> {
    let mut parent_inode = 1u64;
    let mut components = Path::new(relative_path).components().peekable();
    while let Some(component) = components.next() {
        let name = component.as_os_str().to_str().ok_or(ApiError::BadRequest(
            "relative_path enthaelt ungueltige UTF-8-Segmente",
        ))?;
        if components.peek().is_none() {
            return Ok((parent_inode, name.to_string()));
        }
        parent_inode = match layer
            .lookup_dirent(agent_name, parent_inode, name)
            .map_err(|_| ApiError::ServiceUnavailable("sentinel-fs Dirent-Lookup fehlgeschlagen"))?
        {
            Some(existing) => existing,
            None => layer
                .mkdir(agent_name, parent_inode, name, 0o755)
                .map_err(|_| {
                    ApiError::ServiceUnavailable("sentinel-fs Unterverzeichnis nicht anlegbar")
                })?,
        };
    }
    Err(ApiError::BadRequest(
        "relative_path enthaelt keinen Dateinamen",
    ))
}

fn validate_relative_path(path: &str) -> std::result::Result<(), ApiError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::BadRequest("relative_path fehlt"));
    }
    let candidate = Path::new(trimmed);
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(ApiError::BadRequest(
            "relative_path muss relativ und ohne '..' sein",
        ));
    }
    Ok(())
}

fn current_runtime_snapshot_for_agent_name(
    state: &AppState,
    agent_name: &str,
) -> std::result::Result<SecurityAgentRuntimeSnapshot, ApiError> {
    let runtime_state = state
        .security_runtime_state
        .read()
        .map_err(|_| ApiError::ServiceUnavailable("Runtime-State nicht verfuegbar"))?;
    runtime_state
        .values()
        .find(|snapshot| snapshot.agent_name == agent_name)
        .cloned()
        .ok_or(ApiError::NotFound("agent_name unbekannt"))
}

fn platform_agent_snapshot_for_id(
    state: &AppState,
    agent_id: u16,
) -> std::result::Result<Option<crate::platform_controlplane::PlatformAgentSnapshot>, ApiError> {
    let platform_state = state
        .platform_state
        .read()
        .map_err(|_| ApiError::ServiceUnavailable("Platform-State nicht verfuegbar"))?;
    Ok(platform_state
        .agents
        .iter()
        .find(|snapshot| snapshot.agent_id == agent_id)
        .cloned())
}

fn fs_agent_dir_for_name(
    state: &AppState,
    agent_name: &str,
) -> std::result::Result<String, ApiError> {
    if let Some(platform) = platform_agent_snapshot_for_name(state, agent_name)? {
        return Ok(platform.aggregate_id);
    }
    Ok(current_runtime_snapshot_for_agent_name(state, agent_name)?.aggregate_id)
}

fn platform_agent_snapshot_for_name(
    state: &AppState,
    agent_name: &str,
) -> std::result::Result<Option<crate::platform_controlplane::PlatformAgentSnapshot>, ApiError> {
    let platform_state = state
        .platform_state
        .read()
        .map_err(|_| ApiError::ServiceUnavailable("Platform-State nicht verfuegbar"))?;
    Ok(platform_state
        .agents
        .iter()
        .find(|snapshot| snapshot.name == agent_name)
        .cloned())
}

fn inspect_fs_trash(
    hash: Option<&String>,
    state: &AppState,
) -> std::result::Result<FsTrashInspectResponse, ApiError> {
    let hash = hash.ok_or(ApiError::BadRequest("hash Query-Parameter fehlt"))?;
    let chunk_hash = decode_chunk_hash(hash)?;
    let layer = open_fs_layer(state)?;
    let trashed_at_ms = layer
        .meta()
        .get_trash_timestamp(&chunk_hash)
        .map_err(|_| ApiError::ServiceUnavailable("Trash-Queue nicht lesbar"))?;
    let in_chunk_index = layer.cas().contains(&chunk_hash);
    let refcount = layer
        .meta()
        .get_refcount(&chunk_hash)
        .map_err(|_| ApiError::ServiceUnavailable("Chunk-Refcount nicht lesbar"))?;
    Ok(FsTrashInspectResponse {
        found: trashed_at_ms.is_some(),
        chunk_hash: sentinel_fs::cas::hex_encode(&chunk_hash),
        trashed_at_ms,
        age_ms: trashed_at_ms.map(|value| now_ms().saturating_sub(value)),
        in_chunk_index,
        refcount,
    })
}

fn create_fs_trash_fixture(
    payload: FsTrashFixtureRequest,
    state: &AppState,
) -> std::result::Result<FsTrashFixtureResponse, ApiError> {
    let agent_name = payload.agent_name.trim();
    if agent_name.is_empty() {
        return Err(ApiError::BadRequest("agent_name fehlt"));
    }
    validate_relative_path(&payload.relative_path)?;
    let fs_agent_dir = fs_agent_dir_for_name(state, agent_name)?;
    let layer = open_fs_layer(state)?;
    let metadata = layer.meta();
    let (parent_inode, file_name) =
        fs_parent_and_name(&layer, &fs_agent_dir, &payload.relative_path)?;
    if let Some(existing_inode) = layer
        .lookup_dirent(&fs_agent_dir, parent_inode, &file_name)
        .map_err(|_| ApiError::ServiceUnavailable("Fixture-Dirent-Lookup fehlgeschlagen"))?
    {
        layer
            .unlink(&fs_agent_dir, parent_inode, &file_name, existing_inode)
            .map_err(|_| {
                ApiError::ServiceUnavailable("Vorhandene Fixture-Datei nicht entfernbar")
            })?;
    }
    let inode = layer
        .write_file(
            &fs_agent_dir,
            parent_inode,
            &file_name,
            payload.content.as_bytes(),
            0o644,
        )
        .map_err(|_| ApiError::ServiceUnavailable("Fixture-Write fehlgeschlagen"))?;
    let inode_data = layer
        .lookup_inode(&fs_agent_dir, inode)
        .map_err(|_| ApiError::ServiceUnavailable("Fixture-Inode nicht lesbar"))?
        .ok_or(ApiError::ServiceUnavailable(
            "Fixture-Inode fehlt nach Write",
        ))?;
    layer
        .unlink(&fs_agent_dir, parent_inode, &file_name, inode)
        .map_err(|_| ApiError::ServiceUnavailable("Fixture-Unlink fehlgeschlagen"))?;
    let trashed_chunks = u64::from(
        metadata
            .get_trash_timestamp(&inode_data.hash)
            .map_err(|_| ApiError::ServiceUnavailable("Trash-Queue nicht lesbar"))?
            .is_some(),
    );
    Ok(FsTrashFixtureResponse {
        accepted: true,
        agent_name: agent_name.to_string(),
        relative_path: payload.relative_path,
        object_id: inode,
        chunk_hashes: vec![sentinel_fs::cas::hex_encode(&inode_data.hash)],
        trashed_chunks,
    })
}

fn set_fs_trash_age(
    payload: FsTrashAgeRequest,
    state: &AppState,
) -> std::result::Result<FsTrashAgeResponse, ApiError> {
    let chunk_hash = decode_chunk_hash(&payload.chunk_hash)?;
    let layer = open_fs_layer(state)?;
    let trashed_at_ms = now_ms().saturating_sub(payload.hours_ago * 3600 * 1000);
    let updated = layer
        .meta()
        .set_trash_timestamp(&chunk_hash, Some(trashed_at_ms))
        .map_err(|_| ApiError::ServiceUnavailable("Trash-Queue nicht schreibbar"))?;
    if !updated {
        return Err(ApiError::NotFound("chunk_hash nicht in fs_trash_queue"));
    }
    Ok(FsTrashAgeResponse {
        accepted: true,
        chunk_hash: sentinel_fs::cas::hex_encode(&chunk_hash),
        trashed_at_ms,
    })
}

fn run_fs_trash_gc(
    payload: FsTrashGcRequest,
    state: &AppState,
) -> std::result::Result<FsTrashGcResponse, ApiError> {
    let layer = open_fs_layer(state)?;
    let stats = layer
        .meta()
        .gc_trash(layer.cas(), payload.grace_period_hours)
        .map_err(|_| ApiError::ServiceUnavailable("gc_trash fehlgeschlagen"))?;
    Ok(FsTrashGcResponse {
        accepted: true,
        grace_period_hours: payload.grace_period_hours,
        freed_from_trash: stats.freed_from_trash,
        freed_bytes: stats.freed_bytes,
    })
}

fn inspect_fs_storage_stats(
    state: &AppState,
) -> std::result::Result<FsStorageStatsResponse, ApiError> {
    let layer = open_fs_layer(state)?;
    let stats = layer
        .storage_stats()
        .map_err(|_| ApiError::ServiceUnavailable("sentinel-fs Storage-Stats nicht lesbar"))?;
    Ok(FsStorageStatsResponse {
        accepted: true,
        fs_mount: state.fs_mount.clone(),
        cas_blob_count: stats.cas_blob_count,
        cas_bytes_on_disk: stats.cas_bytes_on_disk,
        regular_file_count: stats.regular_file_count,
        logical_regular_file_bytes: stats.logical_regular_file_bytes,
        dedup_savings_bytes: stats.dedup_savings_bytes,
        dedup_ratio_percent: stats.dedup_ratio_percent,
        unreadable_inode_rows: stats.unreadable_inode_rows,
    })
}

fn run_fs_dedup_benchmark(
    payload: FsDedupBenchmarkRequest,
    state: &AppState,
) -> std::result::Result<FsDedupBenchmarkResponse, ApiError> {
    let agent_name = payload.agent_name.trim();
    if agent_name.is_empty() {
        return Err(ApiError::BadRequest("agent_name fehlt"));
    }
    if payload.writes == 0 || payload.writes > 512 {
        return Err(ApiError::BadRequest(
            "writes muss zwischen 1 und 512 liegen",
        ));
    }
    if payload.bytes_per_write == 0 || payload.bytes_per_write > 64 * 1024 {
        return Err(ApiError::BadRequest(
            "bytes_per_write muss zwischen 1 und 65536 liegen",
        ));
    }

    let requested_prefix = payload.file_prefix.as_deref().unwrap_or("issue379");
    let file_prefix = format!(
        "{}-{}",
        validate_benchmark_file_prefix(requested_prefix)?,
        uuid::Uuid::now_v7()
    );
    let fs_agent_dir = fs_agent_dir_for_name(state, agent_name)?;
    let layer = open_fs_layer(state)?;
    let parent_inode = ensure_issue379_benchmark_dir(&layer, &fs_agent_dir)?;
    let content = dedup_benchmark_content(payload.bytes_per_write, &file_prefix);
    let content_hash = CasStore::hash(&content);

    let stats_before_started = Instant::now();
    let before = layer
        .storage_stats()
        .map_err(|_| ApiError::ServiceUnavailable("sentinel-fs Storage-Stats nicht lesbar"))?;
    let storage_stats_before_us = elapsed_us(stats_before_started.elapsed());
    let seed_started = Instant::now();
    layer
        .write_file(
            &fs_agent_dir,
            parent_inode,
            &format!("{file_prefix}-seed.bin"),
            &content,
            0o644,
        )
        .map_err(|_| ApiError::ServiceUnavailable("Dedup-Benchmark Seed-Write fehlgeschlagen"))?;
    let seed_write_us = elapsed_us(seed_started.elapsed());

    let mut cas_check_latencies = Vec::with_capacity(payload.writes as usize);
    let mut write_file_latencies = Vec::with_capacity(payload.writes as usize);
    let mut loop_latencies = Vec::with_capacity(payload.writes as usize);
    let hit_file_names: Vec<String> = (0..payload.writes)
        .map(|idx| format!("{file_prefix}-hit-{idx:04}.bin"))
        .collect();
    let mut dedup_hits = 0u32;
    for name in &hit_file_names {
        let loop_started = Instant::now();
        let cas_check_started = Instant::now();
        if layer.cas().contains(&content_hash) {
            dedup_hits += 1;
        }
        cas_check_latencies.push(elapsed_us(cas_check_started.elapsed()));

        let write_started = Instant::now();
        layer
            .write_file(&fs_agent_dir, parent_inode, name, &content, 0o644)
            .map_err(|_| ApiError::ServiceUnavailable("Dedup-Benchmark Write fehlgeschlagen"))?;
        write_file_latencies.push(elapsed_us(write_started.elapsed()));
        loop_latencies.push(elapsed_us(loop_started.elapsed()));
    }

    let stats_after_started = Instant::now();
    let after = layer
        .storage_stats()
        .map_err(|_| ApiError::ServiceUnavailable("sentinel-fs Storage-Stats nicht lesbar"))?;
    let storage_stats_after_us = elapsed_us(stats_after_started.elapsed());
    let logical_bytes_written =
        u64::from(payload.writes + 1).saturating_mul(payload.bytes_per_write as u64);
    let cas_bytes_delta = after
        .cas_bytes_on_disk
        .saturating_sub(before.cas_bytes_on_disk);
    let dedup_ratio_percent = if logical_bytes_written == 0 {
        0.0
    } else {
        (logical_bytes_written.saturating_sub(cas_bytes_delta) as f64 * 100.0)
            / logical_bytes_written as f64
    };
    let cas_check_summary = latency_summary(cas_check_latencies);
    let write_file_summary = latency_summary(write_file_latencies);
    let loop_summary = latency_summary(loop_latencies);
    let latency_min = write_file_summary.min_us;
    let latency_max = write_file_summary.max_us;
    let latency_p50 = write_file_summary.p50_us;
    let latency_p95 = write_file_summary.p95_us;
    let latency_mean = write_file_summary.mean_us;

    Ok(FsDedupBenchmarkResponse {
        accepted: true,
        agent_name: agent_name.to_string(),
        fs_agent_dir,
        file_prefix,
        bytes_per_write: payload.bytes_per_write,
        seed_write_us,
        dedup_hit_writes: payload.writes,
        dedup_hits,
        logical_bytes_written,
        cas_blob_count_before: before.cas_blob_count,
        cas_blob_count_after: after.cas_blob_count,
        cas_bytes_before: before.cas_bytes_on_disk,
        cas_bytes_after: after.cas_bytes_on_disk,
        cas_bytes_delta,
        dedup_ratio_percent,
        target_87_percent_met: dedup_ratio_percent >= 87.0,
        dedup_hit_latency_us_min: latency_min,
        dedup_hit_latency_us_p50: latency_p50,
        dedup_hit_latency_us_p95: latency_p95,
        dedup_hit_latency_us_max: latency_max,
        dedup_hit_latency_us_mean: latency_mean,
        dedup_hit_cas_check_latency: cas_check_summary,
        dedup_hit_write_file_latency: write_file_summary,
        dedup_hit_loop_latency: loop_summary,
        storage_stats_before_us,
        storage_stats_after_us,
        target_100us_met: latency_p95 <= 100,
    })
}

fn latency_summary(mut latencies: Vec<u64>) -> FsLatencySummary {
    latencies.sort_unstable();
    let min_us = *latencies.first().unwrap_or(&0);
    let max_us = *latencies.last().unwrap_or(&0);
    let p50_us = percentile_us(&latencies, 0.50);
    let p95_us = percentile_us(&latencies, 0.95);
    let mean_us = if latencies.is_empty() {
        0.0
    } else {
        latencies.iter().sum::<u64>() as f64 / latencies.len() as f64
    };

    FsLatencySummary {
        min_us,
        p50_us,
        p95_us,
        max_us,
        mean_us,
    }
}

fn validate_benchmark_file_prefix(prefix: &str) -> std::result::Result<String, ApiError> {
    let trimmed = prefix.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err(ApiError::BadRequest(
            "file_prefix muss 1 bis 64 Zeichen lang sein",
        ));
    }
    if !trimmed
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(ApiError::BadRequest(
            "file_prefix darf nur ASCII alnum, Punkt, Bindestrich und Unterstrich enthalten",
        ));
    }
    Ok(trimmed.to_string())
}

fn ensure_issue379_benchmark_dir(
    layer: &LayerManager,
    agent_id: &str,
) -> std::result::Result<u64, ApiError> {
    const BENCHMARK_DIR: &str = ".issue379-dedup";
    if let Some(inode) = layer
        .lookup_dirent(agent_id, 1, BENCHMARK_DIR)
        .map_err(|_| ApiError::ServiceUnavailable("Dedup-Benchmark Dirent-Lookup fehlgeschlagen"))?
    {
        let data = layer
            .lookup_inode(agent_id, inode)
            .map_err(|_| {
                ApiError::ServiceUnavailable("Dedup-Benchmark Inode-Lookup fehlgeschlagen")
            })?
            .ok_or(ApiError::ServiceUnavailable(
                "Dedup-Benchmark Verzeichnis-Inode fehlt",
            ))?;
        if data.kind != FileKind::Directory {
            return Err(ApiError::ServiceUnavailable(
                "Dedup-Benchmark Pfad ist kein Verzeichnis",
            ));
        }
        return Ok(inode);
    }
    layer
        .mkdir(agent_id, 1, BENCHMARK_DIR, 0o755)
        .map_err(|_| ApiError::ServiceUnavailable("Dedup-Benchmark Verzeichnis nicht anlegbar"))
}

fn dedup_benchmark_content(len: usize, file_prefix: &str) -> Vec<u8> {
    let digest = Sha256::digest(file_prefix.as_bytes());
    let mut seed_bytes = [0u8; 8];
    seed_bytes.copy_from_slice(&digest[..8]);
    let mut state = u64::from_le_bytes(seed_bytes);
    let mut output = Vec::with_capacity(len);
    for index in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state = state
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(index as u64);
        output.push((state >> 32) as u8);
    }
    output
}

fn elapsed_us(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn percentile_us(sorted_values: &[u64], percentile: f64) -> u64 {
    if sorted_values.is_empty() {
        return 0;
    }
    let index = ((sorted_values.len() - 1) as f64 * percentile).ceil() as usize;
    sorted_values[index.min(sorted_values.len() - 1)]
}

fn inspect_agent_runtime_state(
    agent_id: Option<&String>,
    state: &AppState,
) -> std::result::Result<AgentRuntimeStateResponse, ApiError> {
    let agent_id = agent_id
        .ok_or(ApiError::BadRequest("agent_id Query-Parameter fehlt"))?
        .parse::<u16>()
        .map_err(|_| ApiError::BadRequest("agent_id muss Integer sein"))?;
    let runtime_state = state
        .security_runtime_state
        .read()
        .map_err(|_| ApiError::ServiceUnavailable("Runtime-State nicht verfuegbar"))?;
    let runtime = runtime_state.get(&agent_id).cloned();
    drop(runtime_state);
    let platform = platform_agent_snapshot_for_id(state, agent_id)?;
    match (runtime, platform) {
        (Some(runtime), Some(platform)) => Ok(AgentRuntimeStateResponse {
            found: true,
            agent_id,
            aggregate_id: platform.aggregate_id,
            agent_name: platform.name,
            bwrap_pid: runtime.bwrap_pid,
            cgroup_path: platform.cgroup_path,
            current_profile: platform.current_profile,
            home_host_path: runtime.home_host_path,
            fs_mount: runtime.fs_mount,
        }),
        (Some(runtime), None) => Ok(AgentRuntimeStateResponse {
            found: true,
            agent_id,
            aggregate_id: runtime.aggregate_id,
            cgroup_path: sentinel_sandbox::cgroup_path(&runtime.agent_name),
            agent_name: runtime.agent_name,
            bwrap_pid: runtime.bwrap_pid,
            current_profile: String::new(),
            home_host_path: runtime.home_host_path,
            fs_mount: runtime.fs_mount,
        }),
        (None, _) => Ok(AgentRuntimeStateResponse {
            found: false,
            agent_id,
            aggregate_id: format!("AGENT-{agent_id:02}"),
            agent_name: String::new(),
            bwrap_pid: None,
            cgroup_path: String::new(),
            current_profile: String::new(),
            home_host_path: String::new(),
            fs_mount: state.fs_mount.clone(),
        }),
    }
}

fn run_fs_ransomware_test(
    payload: FsRansomwareTestRequest,
    state: &AppState,
) -> std::result::Result<FsRansomwareTestResponse, ApiError> {
    let agent_name = payload.agent_name.trim();
    if agent_name.is_empty() {
        return Err(ApiError::BadRequest("agent_name fehlt"));
    }
    if payload.snapshot_label.trim().is_empty() {
        return Err(ApiError::BadRequest("snapshot_label fehlt"));
    }
    validate_relative_path(&payload.relative_path)?;
    info!(
        agent_name,
        relative_path = %payload.relative_path,
        "Issue #264 fs-ransomware-test v2 gestartet"
    );
    let fs_agent_dir = fs_agent_dir_for_name(state, agent_name)?;
    let layer = open_fs_layer(state)?;
    let (parent_inode, file_name) =
        fs_parent_and_name(&layer, &fs_agent_dir, &payload.relative_path)?;
    let runtime = current_runtime_snapshot_for_agent_name(state, agent_name)?;
    let home_host_path = runtime.home_host_path;
    let target = Path::new(&home_host_path).join(payload.relative_path.trim());
    let before_content = format!(
        "issue-264-ransomware-original:{}:{}",
        payload.snapshot_label, agent_name
    )
    .into_bytes();
    replace_layer_file(
        &layer,
        &fs_agent_dir,
        parent_inode,
        &file_name,
        &before_content,
    )?;
    let before_sha256 = sha256_hex(&before_content);
    wait_for_expected_runtime_bytes(
        &layer,
        &fs_agent_dir,
        parent_inode,
        &file_name,
        &target,
        &before_sha256,
    )?;

    let known_snapshots: HashSet<String> = state
        .event_store
        .list_world_snapshots()
        .map_err(|_| ApiError::ServiceUnavailable("World Snapshot Liste nicht lesbar"))?
        .into_iter()
        .map(|snapshot| snapshot.id)
        .collect();
    let snapshot_started = Instant::now();
    state
        .snapshot_tx
        .send(sentinel_common::OperatorSnapshotCommand { tier: None })
        .map_err(|_| ApiError::ServiceUnavailable("Snapshot-Channel nicht verfuegbar"))?;
    let snapshot_id = wait_for_new_snapshot_id(state, &known_snapshots)?;
    let snapshot_wait_ms = snapshot_started.elapsed().as_millis() as u64;

    let mutated_content = format!(
        "issue-264-ransomware-encrypted:{}:{}:{}",
        payload.snapshot_label,
        agent_name,
        uuid::Uuid::now_v7()
    )
    .into_bytes();
    replace_layer_file(
        &layer,
        &fs_agent_dir,
        parent_inode,
        &file_name,
        &mutated_content,
    )?;
    let mutated_sha256 = sha256_hex(&mutated_content);
    wait_for_expected_runtime_bytes(
        &layer,
        &fs_agent_dir,
        parent_inode,
        &file_name,
        &target,
        &mutated_sha256,
    )?;

    let restore_started = Instant::now();
    state
        .restore_tx
        .send(sentinel_common::OperatorRestoreCommand {
            snapshot_id: snapshot_id.clone(),
        })
        .map_err(|_| ApiError::ServiceUnavailable("Restore-Channel nicht verfuegbar"))?;
    let restored_bytes = wait_for_expected_runtime_bytes(
        &layer,
        &fs_agent_dir,
        parent_inode,
        &file_name,
        &target,
        &before_sha256,
    )?;
    let restore_wait_ms = restore_started.elapsed().as_millis() as u64;
    let restored_sha256 = sha256_hex(&restored_bytes);
    info!(
        agent_name,
        snapshot_id = %snapshot_id,
        before_sha256 = %before_sha256,
        mutated_sha256 = %mutated_sha256,
        restored_sha256 = %restored_sha256,
        "Issue #264 fs-ransomware-test v2 abgeschlossen"
    );
    Ok(FsRansomwareTestResponse {
        accepted: true,
        hook_version: 2,
        agent_name: agent_name.to_string(),
        relative_path: payload.relative_path,
        snapshot_label: payload.snapshot_label,
        host_path: target.display().to_string(),
        snapshot_id,
        bytes_written: before_content.len(),
        before_sha256: before_sha256.clone(),
        mutated_sha256,
        restored_sha256: restored_sha256.clone(),
        restored: restored_sha256 == before_sha256,
        snapshot_wait_ms,
        restore_wait_ms,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    sentinel_fs::cas::hex_encode(digest.as_slice())
}

fn replace_layer_file(
    layer: &LayerManager,
    agent_id: &str,
    parent_inode: u64,
    file_name: &str,
    content: &[u8],
) -> std::result::Result<(), ApiError> {
    if let Some(existing_inode) = layer
        .lookup_dirent(agent_id, parent_inode, file_name)
        .map_err(|_| ApiError::ServiceUnavailable("Ransomware-Test-Dirent-Lookup fehlgeschlagen"))?
    {
        layer
            .unlink(agent_id, parent_inode, file_name, existing_inode)
            .map_err(|_| {
                ApiError::ServiceUnavailable("Vorhandene Ransomware-Testdatei nicht entfernbar")
            })?;
    }
    layer
        .write_file(agent_id, parent_inode, file_name, content, 0o644)
        .map_err(|_| {
            ApiError::ServiceUnavailable("Ransomware-Testdatei konnte nicht geschrieben werden")
        })?;
    Ok(())
}

fn read_layer_file_bytes(
    layer: &LayerManager,
    agent_id: &str,
    parent_inode: u64,
    file_name: &str,
) -> std::result::Result<Vec<u8>, ApiError> {
    let inode = layer
        .lookup_dirent(agent_id, parent_inode, file_name)
        .map_err(|_| ApiError::ServiceUnavailable("Ransomware-Test-Dirent-Lookup fehlgeschlagen"))?
        .ok_or(ApiError::ServiceUnavailable(
            "Ransomware-Testdatei nicht gefunden",
        ))?;
    layer
        .read_file(agent_id, inode)
        .map_err(|_| ApiError::ServiceUnavailable("Ransomware-Testdatei nicht lesbar"))
}

fn wait_for_expected_runtime_bytes(
    layer: &LayerManager,
    agent_id: &str,
    parent_inode: u64,
    file_name: &str,
    runtime_path: &Path,
    expected_sha256: &str,
) -> std::result::Result<Vec<u8>, ApiError> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let bytes = read_layer_file_bytes(layer, agent_id, parent_inode, file_name)?;
        if sha256_hex(&bytes) == expected_sha256
            && std::fs::read(runtime_path)
                .map(|runtime_bytes| runtime_bytes == bytes)
                .unwrap_or(false)
        {
            return Ok(bytes);
        }
        if Instant::now() >= deadline {
            return Err(ApiError::ServiceUnavailable(
                "Ransomware-Testdatei nicht auf aktivem Runtime-Pfad sichtbar",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_new_snapshot_id(
    state: &AppState,
    known_snapshots: &HashSet<String>,
) -> std::result::Result<String, ApiError> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let snapshots = state
            .event_store
            .list_world_snapshots()
            .map_err(|_| ApiError::ServiceUnavailable("World Snapshot Liste nicht lesbar"))?;
        if let Some(snapshot) = snapshots
            .into_iter()
            .find(|snapshot| !known_snapshots.contains(&snapshot.id))
        {
            return Ok(snapshot.id);
        }
        if Instant::now() >= deadline {
            return Err(ApiError::ServiceUnavailable(
                "World Snapshot wurde nicht rechtzeitig erstellt",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn build_bwrap_command(
    agent_name: &str,
    fs_host_agent_dir: Option<&str>,
    fs_mount: Option<&str>,
    extra_readonly_binds: &[(String, String)],
    inner_command: &[String],
) -> std::process::Command {
    let mut config = sentinel_sandbox::BwrapConfig::for_agent(agent_name);
    if let Some(mount) = fs_mount {
        config = config.with_fs_mount(mount, fs_host_agent_dir.unwrap_or(agent_name), agent_name);
    }
    config
        .readonly_binds
        .extend(extra_readonly_binds.iter().cloned());
    let wrapped = maybe_wrap_with_landlock(agent_name, &mut config, inner_command);
    let mut args = config.to_args();
    args.extend(wrapped);
    let mut command = std::process::Command::new("bwrap");
    command.args(args).stdin(Stdio::null());
    command
}

fn maybe_wrap_with_landlock(
    agent_name: &str,
    config: &mut sentinel_sandbox::BwrapConfig,
    inner_command: &[String],
) -> Vec<String> {
    if sentinel_sandbox::landlock::detect_abi().is_some() {
        let wrapper = security_landlock_wrapper_path();
        if wrapper.exists() {
            config.readonly_binds.push((
                wrapper.to_string_lossy().into_owned(),
                "/landlock-wrapper".to_string(),
            ));
            let mut command = vec![
                "/landlock-wrapper".to_string(),
                agent_name.to_string(),
                "--".to_string(),
            ];
            command.extend_from_slice(inner_command);
            return command;
        }
    }
    inner_command.to_vec()
}

fn security_landlock_wrapper_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let candidate = exe
            .parent()
            .unwrap_or(Path::new("."))
            .join("landlock-wrapper");
        if candidate.exists() {
            return candidate;
        }
    }
    let deploy = PathBuf::from("/opt/sentinel/bin/landlock-wrapper");
    if deploy.exists() {
        return deploy;
    }
    PathBuf::from("/usr/local/bin/landlock-wrapper")
}

fn security_breakout_helper_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let candidate = exe
            .parent()
            .unwrap_or(Path::new("."))
            .join("breakout-helper");
        if candidate.exists() {
            return candidate;
        }
    }
    let deploy = PathBuf::from("/opt/sentinel/bin/breakout-helper");
    if deploy.exists() {
        return deploy;
    }
    PathBuf::from("/usr/local/bin/breakout-helper")
}

fn landlock_test_blocked(exit_code: i32, stderr: &str) -> bool {
    exit_code == 0
        || stderr.contains("[landlock-wrapper] exec failed: Permission denied")
        || stderr.contains("[landlock-wrapper] exec failed: Operation not permitted")
}

fn landlock_attempted_path(agent_name: &str, scenario: &str) -> Option<String> {
    match scenario {
        "exec-from-tmp" => Some("/tmp/evil.sh".to_string()),
        "exec-from-home" => Some(format!("/home/{agent_name}/.issue264-evil.sh")),
        "exec-bin-sh" => Some("/bin/sh".to_string()),
        "exec-python3" => Some("/usr/bin/python3".to_string()),
        _ => None,
    }
}

fn next_write_anomaly_start_tick(
    current_tick: u64,
    cycle_interval_ticks: u64,
    ebpf_collect_interval_ticks: u64,
) -> u64 {
    let cycle_interval_ticks = cycle_interval_ticks.max(1);
    let ebpf_collect_interval_ticks = ebpf_collect_interval_ticks.max(1).min(cycle_interval_ticks);
    let target_mod =
        (cycle_interval_ticks + 1 - ebpf_collect_interval_ticks) % cycle_interval_ticks;
    let current_mod = current_tick % cycle_interval_ticks;
    let delta = if current_mod <= target_mod {
        target_mod - current_mod
    } else {
        cycle_interval_ticks - current_mod + target_mod
    };
    current_tick + delta
}

fn run_write_anomaly_test(
    payload: WriteAnomalyTestRequest,
    state: &AppState,
) -> std::result::Result<WriteAnomalyTestResponse, ApiError> {
    let agent_name = payload.agent_name.trim();
    if agent_name.is_empty() {
        return Err(ApiError::BadRequest("agent_name fehlt"));
    }
    if payload.mode.trim().is_empty() {
        return Err(ApiError::BadRequest("mode fehlt"));
    }
    if payload.bytes_per_sec == 0 {
        return Err(ApiError::BadRequest("bytes_per_sec muss > 0 sein"));
    }
    let duration_secs = payload.duration_secs.unwrap_or(60);
    if duration_secs == 0 {
        return Err(ApiError::BadRequest("duration_secs muss > 0 sein"));
    }

    let runtime = current_runtime_snapshot_for_agent_name(state, agent_name)?;
    let _platform = platform_agent_snapshot_for_name(state, agent_name)?
        .ok_or(ApiError::NotFound("agent_name nicht im Platform-State"))?;
    let platform_state = state
        .platform_state
        .read()
        .ok()
        .map(|snapshot| snapshot.clone())
        .unwrap_or_default();
    let scheduled_start_tick = if payload.align_to_observation_window {
        next_write_anomaly_start_tick(
            platform_state.current_tick,
            platform_state.cycle_interval_ticks,
            platform_state.ebpf_collect_interval_ticks,
        )
    } else {
        platform_state.current_tick
    };
    let start_delay_secs = scheduled_start_tick.saturating_sub(platform_state.current_tick);
    let bwrap_pid = runtime
        .bwrap_pid
        .ok_or(ApiError::ServiceUnavailable("tracked bwrap_pid fehlt"))?;
    // The runtime FUSE mount is currently read-focused; use a daemon-owned test
    // path on the host FS so the operator hook can deterministically generate I/O
    // inside the target agent cgroup.
    let host_path = state
        .data_dir
        .join("security-write-anomaly")
        .join(&runtime.aggregate_id)
        .join(".issue264-write-anomaly.bin")
        .display()
        .to_string();
    let _ = std::fs::remove_file(&host_path);
    let script = r#"import os, sys, time
path = sys.argv[1]
bps = int(sys.argv[2])
delay = float(sys.argv[3])
duration = float(sys.argv[4])
chunk = b'x' * max(4096, min(1048576, max(4096, bps // 4)))
if delay > 0:
    time.sleep(delay)
deadline = time.time() + duration
os.makedirs(os.path.dirname(path), exist_ok=True)
with open(path, 'wb', buffering=0) as handle:
    while time.time() < deadline:
        loop_start = time.time()
        written = 0
        while written < bps and time.time() < deadline:
            piece = chunk[:min(len(chunk), bps - written)]
            handle.write(piece)
            handle.flush()
            os.fsync(handle.fileno())
            written += len(piece)
        sleep_for = 1.0 - (time.time() - loop_start)
        if sleep_for > 0:
            time.sleep(sleep_for)
"#;
    // Operator-only test hook: spawn a host-side writer and move it into the
    // target agent cgroup so write-rate detection is deterministic and does not
    // depend on the sandbox's execute policy.
    let mut child = Command::new("/usr/bin/python3");
    child
        .arg("-c")
        .arg(script)
        .arg(&host_path)
        .arg(payload.bytes_per_sec.to_string())
        .arg(start_delay_secs.to_string())
        .arg(duration_secs.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = child.spawn().map_err(|_| {
        ApiError::ServiceUnavailable("Write-Anomaly-Test konnte nicht gestartet werden")
    })?;
    let helper_pid = child.id();
    let _ = sentinel_sandbox::cgroups::add_pid_to_cgroup(agent_name, helper_pid);
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(WriteAnomalyTestResponse {
        accepted: true,
        agent_name: agent_name.to_string(),
        mode: payload.mode,
        bytes_per_sec: payload.bytes_per_sec,
        duration_secs,
        align_to_observation_window: payload.align_to_observation_window,
        start_delay_secs,
        scheduled_start_tick,
        bwrap_pid,
        helper_pid,
        host_path,
    })
}

fn run_landlock_test(
    payload: LandlockTestRequest,
    state: &AppState,
) -> std::result::Result<LandlockTestResponse, ApiError> {
    let agent_name = payload.agent_name.trim();
    let scenario = payload.scenario.trim();
    if agent_name.is_empty() {
        return Err(ApiError::BadRequest("agent_name fehlt"));
    }
    if scenario.is_empty() {
        return Err(ApiError::BadRequest("scenario fehlt"));
    }
    let runtime = current_runtime_snapshot_for_agent_name(state, agent_name)?;
    let breakout_helper = security_breakout_helper_path();
    if !breakout_helper.exists() {
        return Err(ApiError::ServiceUnavailable(
            "breakout-helper Binary nicht verfuegbar",
        ));
    }
    let inner_command = vec!["/breakout-helper".to_string(), scenario.to_string()];
    let binds = vec![(
        breakout_helper.to_string_lossy().into_owned(),
        "/breakout-helper".to_string(),
    )];
    let mut command = build_bwrap_command(
        agent_name,
        Some(&runtime.aggregate_id),
        state.fs_mount.as_deref(),
        &binds,
        &inner_command,
    );
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = command
        .spawn()
        .map_err(|_| ApiError::ServiceUnavailable("Landlock-Test konnte nicht gestartet werden"))?;
    let helper_pid = child.id();
    let _ = sentinel_sandbox::cgroups::add_pid_to_cgroup(agent_name, helper_pid);
    let output = child
        .wait_with_output()
        .map_err(|_| ApiError::ServiceUnavailable("Landlock-Test lieferte kein Ergebnis"))?;
    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let attempted_path = landlock_attempted_path(agent_name, scenario);
    let blocked = landlock_test_blocked(exit_code, &stderr);
    let audit_event_id = if blocked {
        persist_landlock_block_event(
            state,
            agent_name,
            scenario,
            attempted_path.as_deref(),
            exit_code,
            &stderr,
        )?
    } else {
        None
    };
    Ok(LandlockTestResponse {
        accepted: true,
        agent_name: agent_name.to_string(),
        scenario: scenario.to_string(),
        helper_pid,
        exit_code,
        blocked,
        attempted_path,
        audit_event_id,
        stdout,
        stderr,
    })
}

fn persist_landlock_block_event(
    state: &AppState,
    agent_name: &str,
    scenario: &str,
    attempted_path: Option<&str>,
    exit_code: i32,
    stderr: &str,
) -> std::result::Result<Option<String>, ApiError> {
    let Some(attempted_path) = attempted_path else {
        return Ok(None);
    };
    let tick = state
        .platform_state
        .read()
        .ok()
        .map(|snapshot| snapshot.current_tick)
        .unwrap_or_default();
    let payload = DomainEventPayload::SecurityExecBlocked {
        agent_name: agent_name.to_string(),
        scenario: scenario.to_string(),
        attempted_path: attempted_path.to_string(),
        exit_code,
        stderr: stderr.to_string(),
    };
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let op_id = format!("security-exec-blocked-{agent_name}-{scenario}-{tick}-{ts}");
    let event = DomainEvent::new(
        payload.event_type_str(),
        agent_name,
        &payload.to_json(),
        &op_id,
        tick,
    );
    let topic = format!("sentinel/events/security_exec_blocked/{agent_name}");
    state
        .event_store
        .append_with_outbox(&event, &topic)
        .map_err(|_| {
            ApiError::ServiceUnavailable("Security-Exec-Event konnte nicht persistiert werden")
        })?;
    Ok(Some(event.event_id))
}

fn is_authorized(headers: &HashMap<String, String>, shared_secret: Option<&str>) -> bool {
    let Some(shared_secret) = shared_secret else {
        return true;
    };

    headers
        .get(OPERATOR_KEY_HEADER)
        .is_some_and(|value| value == shared_secret)
        || headers
            .get("authorization")
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|value| value == shared_secret)
}

async fn read_http_request(stream: &mut TcpStream) -> std::result::Result<HttpRequest, ApiError> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 2048];
    let header_end = loop {
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|_| ApiError::BadRequest("Request konnte nicht gelesen werden"))?;
        if read == 0 {
            return Err(ApiError::BadRequest("Leerer Request"));
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_REQUEST_BYTES {
            return Err(ApiError::PayloadTooLarge);
        }
        if let Some(pos) = find_header_end(&buffer) {
            break pos;
        }
    };

    let header_bytes = &buffer[..header_end];
    let header_text = std::str::from_utf8(header_bytes)
        .map_err(|_| ApiError::BadRequest("Header-Encoding ungueltig"))?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or(ApiError::BadRequest("Request-Line fehlt"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or(ApiError::BadRequest("HTTP-Methode fehlt"))?;
    let path = parts
        .next()
        .ok_or(ApiError::BadRequest("Request-Pfad fehlt"))?;
    let max_body_bytes = max_body_bytes_for_path(path);

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(ApiError::BadRequest("Header ungueltig"));
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }

    let content_length = headers
        .get("content-length")
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| ApiError::BadRequest("Content-Length ungueltig"))
        })
        .transpose()?
        .unwrap_or(0);
    if content_length > max_body_bytes {
        return Err(ApiError::PayloadTooLarge);
    }

    let body_start = header_end + 4;
    let mut body = buffer[body_start..].to_vec();
    while body.len() < content_length {
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|_| ApiError::BadRequest("Request-Body konnte nicht gelesen werden"))?;
        if read == 0 {
            return Err(ApiError::BadRequest("Request-Body unvollstaendig"));
        }
        body.extend_from_slice(&chunk[..read]);
        if body.len() > max_body_bytes {
            return Err(ApiError::PayloadTooLarge);
        }
    }
    body.truncate(content_length);

    Ok(HttpRequest {
        method: method.to_string(),
        path: path.to_string(),
        headers,
        body,
    })
}

fn max_body_bytes_for_path(path: &str) -> usize {
    match request_path(path) {
        OPERATOR_APICP_SNAPSHOT_PATH => MAX_APICP_SNAPSHOT_BODY_BYTES,
        OPERATOR_CONFIG_APPLY_PATH => MAX_CONFIG_APPLY_BODY_BYTES,
        _ => MAX_BODY_BYTES,
    }
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn write_http_response(stream: &mut TcpStream, response: HttpResponse) -> AnyResult<()> {
    let status_text = match response.status {
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        status_text,
        response.body.len()
    );
    stream
        .write_all(header.as_bytes())
        .await
        .context("Operator-API Header schreiben")?;
    stream
        .write_all(&response.body)
        .await
        .context("Operator-API Body schreiben")?;
    Ok(())
}

fn json_response<T: Serialize>(status: u16, payload: T) -> HttpResponse {
    let body = serde_json::to_vec(&payload)
        .unwrap_or_else(|_| br#"{"error":"Serialisierung fehlgeschlagen"}"#.to_vec());
    HttpResponse { status, body }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_redb::{ApiCpPatternSnapshot, StateStore};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::oneshot;

    fn test_state(
        secret: Option<&str>,
    ) -> (
        AppState,
        mpsc::Receiver<OperatorCommand>,
        mpsc::Receiver<PlatformControlCommand>,
        mpsc::Receiver<RuntimeControlCommand>,
    ) {
        let (tx, rx) = mpsc::channel();
        let (platform_tx, platform_rx) = mpsc::channel();
        let (runtime_tx, runtime_rx) = mpsc::channel();
        let (nightrun_tx, _nightrun_rx) = mpsc::channel();
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let path = dir.path().join("test.redb");
        let state_store = Arc::new(StateStore::open(path.to_str().unwrap()).unwrap());
        let security_runtime_state = Arc::new(std::sync::RwLock::new(HashMap::from([(
            7,
            SecurityAgentRuntimeSnapshot {
                agent_id: 7,
                aggregate_id: "AGENT-07".to_string(),
                agent_name: "Test Agent".to_string(),
                bwrap_pid: Some(4242),
                home_host_path: "/ram/agents/Test Agent".to_string(),
                fs_mount: None,
            },
        )])));
        let state = AppState {
            allowed_rooms: Arc::new(
                ["empfang".to_string(), "flur_eg".to_string()]
                    .into_iter()
                    .collect(),
            ),
            shared_secret: secret.map(str::to_string),
            data_dir,
            fs_mount: None,
            fs_layer: None,
            command_tx: tx,
            platform_tx,
            runtime_tx,
            nightrun_tx,
            nightrun_agent_counts: NightrunAgentCounts::from_shift_sets([1, 1, 2, 3]),
            snapshot_tx: mpsc::channel().0,
            restore_tx: mpsc::channel().0,
            config_apply_tx: mpsc::channel().0,
            migrate_tx: mpsc::channel().0,
            config_apply_max_agents: 60,
            config_apply_validation:
                sentinel_common::agent_config::AgentConfigValidation::with_max_agent_id(60),
            event_store: Arc::new(
                sentinel_limbo::EventStore::open(":memory:")
                    .expect("in-memory EventStore fuer Tests"),
            ),
            prune_tx: mpsc::channel().0,
            state_store,
            platform_state: Arc::new(std::sync::RwLock::new(PlatformStateSnapshot {
                current_tick: 42,
                cycle_interval_ticks: 60,
                ebpf_collect_interval_ticks: 10,
                stall_detection_threshold_secs: 30,
                llm_enabled: true,
                llm_analysis_interval_secs: 300,
                llm_retry_delay_secs: 60,
                stall_recent_activity_grace_ticks: 120,
                agents: vec![crate::platform_controlplane::PlatformAgentSnapshot {
                    agent_id: 7,
                    aggregate_id: "AGENT-07".to_string(),
                    name: "Test Agent".to_string(),
                    last_activity_tick: 11,
                    cgroup_path: "/sys/fs/cgroup/sentinel/Test Agent".to_string(),
                    current_profile: "normal".to_string(),
                }],
                ..PlatformStateSnapshot::default()
            })),
            runtime_health: Arc::new(std::sync::RwLock::new(
                crate::runtime_health::RuntimeHealthSnapshot {
                    current_shift: 1,
                    expected_active_agents: 26,
                    runtime_agents: 3,
                    projection_agents: 2,
                    security_runtime_entries: 1,
                    sandbox_handles: 1,
                    tracked_processes: 1,
                    live_cgroup_dirs: 1,
                    stale_runtime_entries: 2,
                    orphan_cgroups: 1,
                    zombie_tracked_pids: 0,
                    projection_drift_detected: true,
                    projection_drift_agents: 1,
                    worker_states: std::collections::BTreeMap::from([(
                        "service_health".to_string(),
                        crate::runtime_health::RuntimeWorkerState {
                            running: true,
                            restart_count: 0,
                            last_error: None,
                            thread_name: "service-health-checker".to_string(),
                        },
                    )]),
                    analysis_queue_depth: 0,
                    analysis_queue_dropped_total: 0,
                    analysis_queue_coalesced_total: 0,
                    reconcile_runs_total: 0,
                    reconcile_repairs_total: 0,
                    respawn_failures: 0,
                    last_repair_error: None,
                    repair_last_status: Some("drift_detected".to_string()),
                    operator_auth_required: secret.is_some(),
                    snapshot_build_elapsed_us: 0,
                    agents: vec![crate::runtime_health::RuntimeHealthAgentSnapshot {
                        agent_id: 7,
                        aggregate_id: "AGENT-07".to_string(),
                        name: "Test Agent".to_string(),
                        runtime_present: true,
                        projection_present: false,
                        tracked_pid: Some(4242),
                        tracked_pid_alive: false,
                        tracked_pid_state: Some("Z".to_string()),
                        cgroup_live_pid_count: 0,
                        security_runtime_present: true,
                        last_repair_status: Some("stale".to_string()),
                    }],
                },
            )),
            security_runtime_state,
        };
        std::mem::forget(dir);
        (state, rx, platform_rx, runtime_rx)
    }

    fn test_request(path: &str, body: serde_json::Value) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            path: path.to_string(),
            headers: HashMap::new(),
            body: serde_json::to_vec(&body).unwrap(),
        }
    }

    fn test_get_request(path: &str) -> HttpRequest {
        HttpRequest {
            method: "GET".to_string(),
            path: path.to_string(),
            headers: HashMap::new(),
            body: Vec::new(),
        }
    }

    fn attach_test_fs_layer(state: &mut AppState) -> Arc<LayerManager> {
        let cas = CasStore::open(&state.data_dir).unwrap();
        let meta = MetadataStore::open(state.data_dir.join("metadata.redb")).unwrap();
        let layer = Arc::new(LayerManager::new(cas, meta));
        layer.init_base_root().unwrap();
        state.fs_layer = Some(Arc::clone(&layer));
        layer
    }

    #[test]
    fn valid_trigger_request_is_accepted_and_forwarded() {
        let (state, rx, _platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            test_request(
                OPERATOR_CHAOS_PATH,
                serde_json::json!({
                    "room_id": "empfang",
                    "chaos_type": "AirConBroken",
                    "duration_ticks": 45
                }),
            ),
            &state,
        );

        assert_eq!(response.status, 202);
        let parsed: TriggerChaosResponse = serde_json::from_slice(&response.body).unwrap();
        assert!(parsed.accepted);
        assert_eq!(parsed.room_id, "empfang");
        assert_eq!(parsed.chaos_type, EventType::AirConBroken);

        let command = rx.recv().unwrap();
        match command {
            OperatorCommand::Chaos(command) => {
                assert_eq!(command.room_id, "empfang");
                assert_eq!(command.chaos_type, EventType::AirConBroken);
                assert_eq!(command.duration_ticks, Some(45));
                assert_eq!(command.description, "Klimaanlage defekt");
                assert_eq!(command.event_id, parsed.event_id);
            }
            other => panic!("unerwartetes Kommando: {other:?}"),
        }
    }

    #[test]
    fn invalid_room_returns_not_found() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            test_request(
                OPERATOR_CHAOS_PATH,
                serde_json::json!({
                    "room_id": "unbekannt",
                    "chaos_type": "PrinterBroken"
                }),
            ),
            &state,
        );

        assert_eq!(response.status, 404);
    }

    #[test]
    fn missing_shared_secret_is_rejected() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(Some("topsecret"));
        let response = handle_http_request(
            test_request(
                OPERATOR_CHAOS_PATH,
                serde_json::json!({
                    "room_id": "empfang",
                    "chaos_type": "PrinterBroken"
                }),
            ),
            &state,
        );

        assert_eq!(response.status, 401);
    }

    #[test]
    fn valid_shared_secret_via_header_is_accepted() {
        let (state, rx, _platform_rx, _runtime_rx) = test_state(Some("topsecret"));
        let mut request = test_request(
            OPERATOR_CHAOS_PATH,
            serde_json::json!({
                "room_id": "flur_eg",
                "chaos_type": "PrinterBroken",
                "description": "Manuell getestet"
            }),
        );
        request
            .headers
            .insert(OPERATOR_KEY_HEADER.to_string(), "topsecret".to_string());

        let response = handle_http_request(request, &state);
        assert_eq!(response.status, 202);

        match rx.recv().unwrap() {
            OperatorCommand::Chaos(command) => {
                assert_eq!(command.room_id, "flur_eg");
                assert_eq!(command.description, "Manuell getestet");
            }
            other => panic!("unerwartetes Kommando: {other:?}"),
        }
    }

    #[test]
    fn valid_stimulus_request_is_accepted_and_forwarded() {
        let (state, rx, _platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            test_request(
                OPERATOR_STIMULUS_PATH,
                serde_json::json!({
                    "room_id": "empfang",
                    "stimulus_type": "co2",
                    "delta": 900,
                    "duration_ticks": 90
                }),
            ),
            &state,
        );

        assert_eq!(response.status, 202);
        let parsed: TriggerStimulusResponse = serde_json::from_slice(&response.body).unwrap();
        assert!(parsed.accepted);
        assert_eq!(parsed.room_id, "empfang");
        assert_eq!(parsed.stimulus_type, RoomStimulusType::Co2);
        assert_eq!(parsed.delta, 900.0);

        match rx.recv().unwrap() {
            OperatorCommand::RoomStimulus(command) => {
                assert_eq!(command.room_id, "empfang");
                assert_eq!(command.stimulus_type, RoomStimulusType::Co2);
                assert_eq!(command.delta, 900.0);
                assert_eq!(command.duration_ticks, Some(90));
                assert_eq!(command.event_id, parsed.event_id);
            }
            other => panic!("unerwartetes Kommando: {other:?}"),
        }
    }

    fn config_apply_agent(id: u16, openness: f32) -> sentinel_common::agent_config::AgentConfig {
        use sentinel_common::agent_config::*;
        AgentConfig {
            identity: IdentityConfig {
                id,
                name: format!("Agent{id}"),
                role: "Dev".to_string(),
                department: "Dev".to_string(),
                shift_set: 1,
                kpis: vec![],
                reports_to: None,
                direct_reports: vec![],
            },
            personality: PersonalityConfig {
                openness,
                conscientiousness: 0.5,
                extraversion: 0.5,
                agreeableness: 0.5,
                neuroticism: 0.5,
                caffeine_tolerance: 0.5,
                morning_person: true,
            },
            preferences: PreferencesConfig {
                favorite_room: "empfang".to_string(),
                coffee_preference: "espresso".to_string(),
                lunch_time: "12:30".to_string(),
            },
            background: BackgroundConfig {
                bio: "bio".to_string(),
                quirks: vec![],
            },
            runtime: RuntimeSelectionConfig::default(),
            capabilities: CapabilitiesConfig::default(),
        }
    }

    fn config_apply_building() -> sentinel_common::room::BuildingConfig {
        use sentinel_common::room::*;
        BuildingConfig {
            building: BuildingMeta {
                name: "Test".to_string(),
                address: "Teststr. 1".to_string(),
                floors: 1,
            },
            rooms: vec![RoomConfig {
                id: "empfang".to_string(),
                name: "Empfang".to_string(),
                floor: 0,
                capacity: 10,
                room_type: RoomType::Common,
                adjacent: vec![],
                department: None,
                has_coffee_machine: false,
                has_printer: false,
            }],
        }
    }

    fn config_apply_cmd(
        agents: Vec<sentinel_common::agent_config::AgentConfig>,
    ) -> serde_json::Value {
        let cmd = sentinel_common::OperatorConfigApplyCommand {
            mode: sentinel_common::ApplyMode::Live,
            agents,
            building: config_apply_building(),
        };
        serde_json::to_value(&cmd).unwrap()
    }

    #[test]
    fn config_apply_valid_request_is_accepted_and_forwarded() {
        let (mut state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let (tx, apply_rx) = mpsc::channel();
        state.config_apply_tx = tx;

        let response = handle_http_request(
            test_request(
                OPERATOR_CONFIG_APPLY_PATH,
                config_apply_cmd(vec![config_apply_agent(1, 0.5), config_apply_agent(2, 0.6)]),
            ),
            &state,
        );
        assert_eq!(response.status, 202);

        let received = apply_rx.recv().unwrap();
        assert_eq!(received.agents.len(), 2);
        assert_eq!(received.mode, sentinel_common::ApplyMode::Live);
    }

    #[test]
    fn config_apply_invalid_personality_rejected_without_send() {
        let (mut state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let (tx, apply_rx) = mpsc::channel();
        state.config_apply_tx = tx;

        // openness 1.5 > 1.0 -> Validierung schlaegt fehl
        let response = handle_http_request(
            test_request(
                OPERATOR_CONFIG_APPLY_PATH,
                config_apply_cmd(vec![config_apply_agent(1, 1.5)]),
            ),
            &state,
        );
        assert_eq!(response.status, 400);
        let parsed: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(parsed["accepted"], serde_json::json!(false));
        assert!(parsed["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("personality invalid")));
        // KEIN Command gesendet
        assert!(
            apply_rx.try_recv().is_err(),
            "invalid config must not be sent"
        );
    }

    #[test]
    fn config_apply_requires_auth() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(Some("topsecret"));
        let response = handle_http_request(
            test_request(
                OPERATOR_CONFIG_APPLY_PATH,
                config_apply_cmd(vec![config_apply_agent(1, 0.5)]),
            ),
            &state,
        );
        assert_eq!(response.status, 401);
    }

    #[test]
    fn migrate_request_is_accepted_and_forwarded() {
        let (mut state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let (tx, migrate_rx) = mpsc::channel();
        state.migrate_tx = tx;

        let response = handle_http_request(
            test_request(
                OPERATOR_MIGRATE_PATH,
                serde_json::json!({ "reason": "resource_rebalance" }),
            ),
            &state,
        );
        assert_eq!(response.status, 202);

        let received = migrate_rx.recv().unwrap();
        assert_eq!(received.reason, "resource_rebalance");
    }

    #[test]
    fn migrate_empty_body_defaults_reason_and_is_forwarded() {
        let (mut state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let (tx, migrate_rx) = mpsc::channel();
        state.migrate_tx = tx;

        // Leerer/teilweiser Body erlaubt — reason ist serde(default).
        let response = handle_http_request(
            test_request(OPERATOR_MIGRATE_PATH, serde_json::json!({})),
            &state,
        );
        assert_eq!(response.status, 202);
        let received = migrate_rx.recv().unwrap();
        assert!(received.reason.is_empty());
    }

    #[test]
    fn migrate_requires_auth() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(Some("topsecret"));
        let response = handle_http_request(
            test_request(
                OPERATOR_MIGRATE_PATH,
                serde_json::json!({ "reason": "manual" }),
            ),
            &state,
        );
        assert_eq!(response.status, 401);
    }

    #[test]
    fn nightrun_request_returns_agents_queued_and_forwards_command() {
        let (mut state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let (nightrun_tx, nightrun_rx) = mpsc::channel();
        state.nightrun_tx = nightrun_tx;
        state.nightrun_agent_counts = NightrunAgentCounts::from_shift_sets([1, 1, 2, 3]);

        let response = handle_http_request(
            test_request(
                OPERATOR_NIGHTRUN_PATH,
                serde_json::json!({
                    "shift_set": 1,
                    "dry_run": false
                }),
            ),
            &state,
        );

        assert_eq!(response.status, 202);
        let payload: TriggerNightrunResponse = serde_json::from_slice(&response.body).unwrap();
        assert!(payload.accepted);
        assert_eq!(payload.agents_queued, 2);
        let command = nightrun_rx.recv().unwrap();
        assert_eq!(command.shift_set, Some(1));
        assert!(!command.dry_run);
    }

    #[test]
    fn fs_storage_stats_get_reports_logical_and_cas_bytes() {
        let (mut state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let layer = attach_test_fs_layer(&mut state);
        let fs_agent_dir = fs_agent_dir_for_name(&state, "Test Agent").unwrap();
        let content = b"same content for fs stats";
        layer
            .write_file(&fs_agent_dir, 1, "stats-a.bin", content, 0o644)
            .unwrap();
        layer
            .write_file(&fs_agent_dir, 1, "stats-b.bin", content, 0o644)
            .unwrap();

        let response =
            handle_http_request(test_get_request(OPERATOR_SECURITY_FS_STATS_PATH), &state);

        assert_eq!(response.status, 200);
        let payload: FsStorageStatsResponse = serde_json::from_slice(&response.body).unwrap();
        assert!(payload.accepted);
        assert_eq!(payload.cas_blob_count, 1);
        assert_eq!(
            payload.logical_regular_file_bytes,
            (content.len() * 2) as u64
        );
        assert!(payload.dedup_savings_bytes > 0);
    }

    #[test]
    fn fs_dedup_benchmark_reports_hits_ratio_and_latency() {
        let (mut state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        attach_test_fs_layer(&mut state);

        let response = handle_http_request(
            test_request(
                OPERATOR_SECURITY_FS_DEDUP_BENCHMARK_PATH,
                serde_json::json!({
                    "agent_name": "Test Agent",
                    "writes": 8,
                    "bytes_per_write": 512,
                    "file_prefix": "test-379"
                }),
            ),
            &state,
        );

        assert_eq!(response.status, 200);
        let payload: FsDedupBenchmarkResponse = serde_json::from_slice(&response.body).unwrap();
        assert!(payload.accepted);
        assert_eq!(payload.agent_name, "Test Agent");
        assert_eq!(payload.fs_agent_dir, "AGENT-07");
        assert_eq!(payload.dedup_hit_writes, 8);
        assert_eq!(payload.dedup_hits, 8);
        assert_eq!(payload.logical_bytes_written, 9 * 512);
        assert!(payload.cas_blob_count_after > payload.cas_blob_count_before);
        assert!(payload.target_87_percent_met);
        assert!(payload.dedup_hit_latency_us_max >= payload.dedup_hit_latency_us_min);
        assert_eq!(
            payload.dedup_hit_latency_us_p95,
            payload.dedup_hit_write_file_latency.p95_us
        );
        assert!(
            payload.dedup_hit_cas_check_latency.max_us
                >= payload.dedup_hit_cas_check_latency.min_us
        );
        assert!(
            payload.dedup_hit_loop_latency.p95_us >= payload.dedup_hit_write_file_latency.min_us
        );
        assert!(payload.storage_stats_after_us > 0);
    }

    #[test]
    fn landlock_blocked_detects_wrapper_exec_denied() {
        assert!(landlock_test_blocked(
            1,
            "[landlock-wrapper] exec failed: Permission denied (os error 13)"
        ));
        assert!(landlock_test_blocked(
            1,
            "[landlock-wrapper] exec failed: Operation not permitted (os error 1)"
        ));
        assert!(!landlock_test_blocked(
            1,
            "SECURITY FINDING: /usr/bin/python3 executed"
        ));
    }

    #[test]
    fn landlock_attempted_path_maps_exec_scenarios() {
        assert_eq!(
            landlock_attempted_path("Test Agent", "exec-from-home").as_deref(),
            Some("/home/Test Agent/.issue264-evil.sh")
        );
        assert_eq!(
            landlock_attempted_path("Test Agent", "exec-python3").as_deref(),
            Some("/usr/bin/python3")
        );
        assert_eq!(landlock_attempted_path("Test Agent", "write-etc"), None);
    }

    #[test]
    fn zero_delta_stimulus_is_rejected() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            test_request(
                OPERATOR_STIMULUS_PATH,
                serde_json::json!({
                    "room_id": "empfang",
                    "stimulus_type": "temperature",
                    "delta": 0
                }),
            ),
            &state,
        );

        assert_eq!(response.status, 400);
    }

    #[test]
    fn platform_analyze_is_forwarded_to_platform_channel() {
        let (state, _rx, platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            HttpRequest {
                method: "POST".to_string(),
                path: OPERATOR_PLATFORM_ANALYZE_PATH.to_string(),
                headers: HashMap::new(),
                body: b"{}".to_vec(),
            },
            &state,
        );

        assert_eq!(response.status, 202);
        assert_eq!(
            platform_rx.recv().unwrap(),
            PlatformControlCommand::AnalyzeNow
        );
    }

    #[test]
    fn unresolved_trigger_test_requires_rule_and_target() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            test_request(
                OPERATOR_PLATFORM_TRIGGER_TEST_PATH,
                serde_json::json!({
                    "trigger": "unresolved_escalation",
                    "count": 3
                }),
            ),
            &state,
        );

        assert_eq!(response.status, 400);
    }

    #[test]
    fn platform_trigger_test_is_forwarded() {
        let (state, _rx, platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            test_request(
                OPERATOR_PLATFORM_TRIGGER_TEST_PATH,
                serde_json::json!({
                    "trigger": "unresolved_escalation",
                    "rule_name": "projection_lag",
                    "target": "system",
                    "count": 3
                }),
            ),
            &state,
        );

        assert_eq!(response.status, 202);
        assert_eq!(
            platform_rx.recv().unwrap(),
            PlatformControlCommand::TriggerTest(PlatformTriggerTestCommand {
                trigger: "unresolved_escalation".to_string(),
                rule_name: Some("projection_lag".to_string()),
                target: Some("system".to_string()),
                count: Some(3),
            })
        );
    }

    #[test]
    fn platform_analysis_test_is_forwarded() {
        let (state, _rx, platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            test_request(
                OPERATOR_PLATFORM_ANALYSIS_TEST_PATH,
                serde_json::json!({
                    "trigger": "operator_test",
                    "severity": "warning",
                    "summary": "force idle",
                    "recommendation": "apply idle profile",
                    "suggested_action": "force_profile",
                    "target": "AGENT-07",
                    "parameters": { "profile": "Idle" }
                }),
            ),
            &state,
        );

        assert_eq!(response.status, 202);
        assert_eq!(
            platform_rx.recv().unwrap(),
            PlatformControlCommand::ApplyAnalysis(PlatformAnalysisCommand {
                trigger: "operator_test".to_string(),
                severity: "warning".to_string(),
                summary: "force idle".to_string(),
                recommendation: "apply idle profile".to_string(),
                suggested_action: Some("force_profile".to_string()),
                target: "AGENT-07".to_string(),
                provider: None,
                model: None,
                unresolved_keys: Vec::new(),
                parameters: std::collections::BTreeMap::from([(
                    "profile".to_string(),
                    serde_json::json!("Idle"),
                )]),
            })
        );
    }

    #[test]
    fn platform_analysis_test_rejects_missing_force_profile_parameters() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            test_request(
                OPERATOR_PLATFORM_ANALYSIS_TEST_PATH,
                serde_json::json!({
                    "trigger": "operator_test",
                    "severity": "warning",
                    "summary": "force idle",
                    "recommendation": "apply idle profile",
                    "suggested_action": "force_profile",
                    "target": "AGENT-07",
                    "parameters": {}
                }),
            ),
            &state,
        );

        assert_eq!(response.status, 400);
    }

    #[test]
    fn platform_state_endpoint_returns_snapshot() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            HttpRequest {
                method: "GET".to_string(),
                path: OPERATOR_PLATFORM_STATE_PATH.to_string(),
                headers: HashMap::new(),
                body: Vec::new(),
            },
            &state,
        );

        assert_eq!(response.status, 200);
        let payload: PlatformStateSnapshot = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(payload.current_tick, 42);
        assert_eq!(payload.cycle_interval_ticks, 60);
        assert_eq!(payload.agents.len(), 1);
        assert_eq!(payload.agents[0].aggregate_id, "AGENT-07");
    }

    #[test]
    fn runtime_health_endpoint_returns_snapshot() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            HttpRequest {
                method: "GET".to_string(),
                path: OPERATOR_RUNTIME_HEALTH_PATH.to_string(),
                headers: HashMap::new(),
                body: Vec::new(),
            },
            &state,
        );

        assert_eq!(response.status, 200);
        let payload: crate::runtime_health::RuntimeHealthSnapshot =
            serde_json::from_slice(&response.body).unwrap();
        assert_eq!(payload.current_shift, 1);
        assert_eq!(payload.expected_active_agents, 26);
        assert_eq!(payload.stale_runtime_entries, 2);
        assert_eq!(payload.agents.len(), 1);
        assert_eq!(payload.agents[0].agent_id, 7);
    }

    #[test]
    fn runtime_health_endpoint_requires_auth_when_secret_is_set() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(Some("secret"));
        let response = handle_http_request(
            HttpRequest {
                method: "GET".to_string(),
                path: OPERATOR_RUNTIME_HEALTH_PATH.to_string(),
                headers: HashMap::new(),
                body: Vec::new(),
            },
            &state,
        );

        assert_eq!(response.status, 401);
    }

    #[test]
    fn runtime_reconcile_is_forwarded_and_returns_response() {
        let (state, _rx, _platform_rx, runtime_rx) = test_state(None);
        let worker = std::thread::spawn(move || match runtime_rx.recv().unwrap() {
            RuntimeControlCommand::Reconcile {
                request,
                response_tx,
            } => {
                assert!(request.respawn_missing);
                assert!(!request.dry_run);
                response_tx
                    .send(RuntimeReconcileResponse {
                        accepted: true,
                        dry_run: false,
                        current_shift: 1,
                        stale_agents_before: 2,
                        stale_agents_after: 0,
                        orphan_cgroups_before: 1,
                        orphan_cgroups_after: 0,
                        security_snapshots_removed: 1,
                        unexpected_runtime_removed: 1,
                        orphan_cgroups_removed: 1,
                        respawned_agents: 2,
                        respawn_skipped_backoff: 0,
                        respawn_blocked_agents: 0,
                        projection_drift_before: true,
                        projection_drift_after: false,
                        projection_restart_attempted: false,
                        projection_restart_succeeded: false,
                        projection_rebuild_requested: true,
                        respawn_failures_total: 0,
                        repair_last_status: "repaired".to_string(),
                        repaired_agents: vec!["Test Agent".to_string()],
                        blocked_agents: Vec::new(),
                        errors: Vec::new(),
                        elapsed_us: 0,
                    })
                    .unwrap();
            }
            RuntimeControlCommand::StallRestartTest { .. } => {
                panic!("unerwartetes StallRestartTest-Kommando");
            }
            RuntimeControlCommand::AnalysisFloodTest { .. } => {
                panic!("unerwartetes AnalysisFloodTest-Kommando");
            }
            RuntimeControlCommand::PanicTest { .. } => {
                panic!("unerwartetes PanicTest-Kommando");
            }
        });
        let response = handle_http_request(
            test_request(
                OPERATOR_RUNTIME_RECONCILE_PATH,
                serde_json::json!({
                    "dry_run": false,
                    "projection_rebuild": true,
                    "respawn_missing": true
                }),
            ),
            &state,
        );

        worker.join().unwrap();
        assert_eq!(response.status, 200);
        let payload: RuntimeReconcileResponse = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(payload.current_shift, 1);
        assert_eq!(payload.respawned_agents, 2);
        assert!(payload.projection_rebuild_requested);
        assert_eq!(payload.repair_last_status, "repaired");
    }

    #[test]
    fn runtime_analysis_flood_test_is_forwarded_and_returns_response() {
        let (state, _rx, _platform_rx, runtime_rx) = test_state(None);
        let worker = std::thread::spawn(move || match runtime_rx.recv().unwrap() {
            RuntimeControlCommand::AnalysisFloodTest {
                request,
                response_tx,
            } => {
                assert_eq!(request.count, 10_000);
                response_tx
                    .send(RuntimeAnalysisFloodTestResponse {
                        accepted: true,
                        requested: 10_000,
                        queue_depth: 17,
                        dropped_total: 9_983,
                        coalesced_total: 9_999,
                        enqueue_elapsed_us: 0,
                        enqueue_per_request_ns: 0,
                        note: "bounded queues exercised".to_string(),
                    })
                    .unwrap();
            }
            other => panic!("unerwartetes Kommando: {other:?}"),
        });

        let response = handle_http_request(
            test_request(
                OPERATOR_RUNTIME_ANALYSIS_FLOOD_TEST_PATH,
                serde_json::json!({
                    "count": 10000
                }),
            ),
            &state,
        );

        worker.join().unwrap();
        assert_eq!(response.status, 200);
        let payload: RuntimeAnalysisFloodTestResponse =
            serde_json::from_slice(&response.body).unwrap();
        assert!(payload.accepted);
        assert_eq!(payload.requested, 10_000);
        assert_eq!(payload.queue_depth, 17);
        assert_eq!(payload.dropped_total, 9_983);
        assert_eq!(payload.coalesced_total, 9_999);
    }

    #[test]
    fn runtime_analysis_flood_test_rejects_zero_count() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            test_request(
                OPERATOR_RUNTIME_ANALYSIS_FLOOD_TEST_PATH,
                serde_json::json!({
                    "count": 0
                }),
            ),
            &state,
        );

        assert_eq!(response.status, 400);
    }

    #[test]
    fn runtime_panic_test_is_forwarded_and_returns_response() {
        let (state, _rx, _platform_rx, runtime_rx) = test_state(None);
        let worker = std::thread::spawn(move || match runtime_rx.recv().unwrap() {
            RuntimeControlCommand::PanicTest {
                request,
                response_tx,
            } => {
                assert_eq!(request.worker, "service_health");
                response_tx
                    .send(RuntimePanicTestResponse {
                        accepted: true,
                        worker: "service_health".to_string(),
                        note: "panic-test dispatched".to_string(),
                    })
                    .unwrap();
            }
            other => panic!("unerwartetes Kommando: {other:?}"),
        });

        let response = handle_http_request(
            test_request(
                OPERATOR_RUNTIME_PANIC_TEST_PATH,
                serde_json::json!({
                    "worker": "service_health"
                }),
            ),
            &state,
        );

        worker.join().unwrap();
        assert_eq!(response.status, 200);
        let payload: RuntimePanicTestResponse = serde_json::from_slice(&response.body).unwrap();
        assert!(payload.accepted);
        assert_eq!(payload.worker, "service_health");
        assert_eq!(payload.note, "panic-test dispatched");
    }

    #[test]
    fn runtime_panic_test_rejects_invalid_worker() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            test_request(
                OPERATOR_RUNTIME_PANIC_TEST_PATH,
                serde_json::json!({
                    "worker": "ecs_tick_loop"
                }),
            ),
            &state,
        );

        assert_eq!(response.status, 400);
    }

    #[test]
    fn runtime_stall_restart_test_is_forwarded_and_returns_response() {
        let (state, _rx, _platform_rx, runtime_rx) = test_state(None);
        let worker = std::thread::spawn(move || match runtime_rx.recv().unwrap() {
            RuntimeControlCommand::StallRestartTest {
                request,
                response_tx,
            } => {
                assert_eq!(request.agent_id, 7);
                assert_eq!(request.mode, "sigstop");
                assert_eq!(request.stall_secs, 35);
                response_tx
                    .send(RuntimeStallRestartTestResponse {
                        accepted: true,
                        agent_id: 7,
                        aggregate_id: "AGENT-07".to_string(),
                        agent_name: "Test Agent".to_string(),
                        mode: "sigstop".to_string(),
                        stall_secs: 35,
                        pid_before: Some(4242),
                        pid_after: Some(5252),
                        runtime_present_after: true,
                        security_runtime_present_after: true,
                        bookkeeping_elapsed_ns: 0,
                        note: "fast_path_restart_executed_without_shift_wait".to_string(),
                    })
                    .unwrap();
            }
            other => panic!("unerwartetes Kommando: {other:?}"),
        });

        let response = handle_http_request(
            test_request(
                OPERATOR_RUNTIME_STALL_RESTART_TEST_PATH,
                serde_json::json!({
                    "agent_id": 7,
                    "mode": "sigstop",
                    "stall_secs": 35
                }),
            ),
            &state,
        );

        worker.join().unwrap();
        assert_eq!(response.status, 200);
        let payload: RuntimeStallRestartTestResponse =
            serde_json::from_slice(&response.body).unwrap();
        assert!(payload.accepted);
        assert_eq!(payload.agent_id, 7);
        assert_eq!(payload.pid_before, Some(4242));
        assert_eq!(payload.pid_after, Some(5252));
        assert_eq!(
            payload.note,
            "fast_path_restart_executed_without_shift_wait"
        );
    }

    #[test]
    fn runtime_stall_restart_test_rejects_invalid_mode() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            test_request(
                OPERATOR_RUNTIME_STALL_RESTART_TEST_PATH,
                serde_json::json!({
                    "agent_id": 7,
                    "mode": "bogus",
                    "stall_secs": 35
                }),
            ),
            &state,
        );

        assert_eq!(response.status, 400);
    }

    #[test]
    fn write_anomaly_alignment_targets_observation_window_start() {
        assert_eq!(next_write_anomaly_start_tick(111531, 60, 10), 111531);
        assert_eq!(next_write_anomaly_start_tick(111538, 60, 10), 111591);
    }

    #[test]
    fn write_anomaly_alignment_handles_equal_cycle_and_ebpf_interval() {
        assert_eq!(next_write_anomaly_start_tick(120, 10, 10), 121);
        assert_eq!(next_write_anomaly_start_tick(121, 10, 10), 121);
    }

    #[test]
    fn persist_landlock_block_event_writes_security_event() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let event_id = persist_landlock_block_event(
            &state,
            "Test Agent",
            "exec-python3",
            Some("/usr/bin/python3"),
            0,
            "Exec /usr/bin/python3 blocked: Permission denied",
        )
        .unwrap()
        .expect("event id");
        let events = state.event_store.get_all_events().unwrap();
        let event = events
            .into_iter()
            .find(|event| event.event_id == event_id)
            .expect("security event");
        assert_eq!(event.event_type, "security_exec_blocked");
        let payload: DomainEventPayload = serde_json::from_str(&event.payload).unwrap();
        match payload {
            DomainEventPayload::SecurityExecBlocked {
                agent_name,
                scenario,
                attempted_path,
                exit_code,
                ..
            } => {
                assert_eq!(agent_name, "Test Agent");
                assert_eq!(scenario, "exec-python3");
                assert_eq!(attempted_path, "/usr/bin/python3");
                assert_eq!(exit_code, 0);
            }
            other => panic!("unerwarteter Payload: {other:?}"),
        }
    }

    #[test]
    fn agent_runtime_state_endpoint_returns_snapshot() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            HttpRequest {
                method: "GET".to_string(),
                path: format!("{OPERATOR_SECURITY_AGENT_RUNTIME_STATE_PATH}?agent_id=7"),
                headers: HashMap::new(),
                body: Vec::new(),
            },
            &state,
        );

        assert_eq!(response.status, 200);
        let payload: AgentRuntimeStateResponse = serde_json::from_slice(&response.body).unwrap();
        assert!(payload.found);
        assert_eq!(payload.agent_id, 7);
        assert_eq!(payload.bwrap_pid, Some(4242));
        assert_eq!(payload.current_profile, "normal");
    }

    #[test]
    fn fs_trash_fixture_and_inspect_roundtrip() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let fixture = handle_http_request(
            test_request(
                OPERATOR_SECURITY_FS_TRASH_FIXTURE_PATH,
                serde_json::json!({
                    "agent_name": "Test Agent",
                    "relative_path": "ac1.txt",
                    "content": "issue-264"
                }),
            ),
            &state,
        );
        assert_eq!(fixture.status, 202);
        let payload: FsTrashFixtureResponse = serde_json::from_slice(&fixture.body).unwrap();
        assert!(!payload.chunk_hashes.is_empty());

        let inspect = handle_http_request(
            HttpRequest {
                method: "GET".to_string(),
                path: format!(
                    "{OPERATOR_SECURITY_FS_TRASH_PATH}?hash={}",
                    payload.chunk_hashes[0]
                ),
                headers: HashMap::new(),
                body: Vec::new(),
            },
            &state,
        );
        assert_eq!(inspect.status, 200);
        let inspect_payload: FsTrashInspectResponse =
            serde_json::from_slice(&inspect.body).unwrap();
        assert!(inspect_payload.found);
        assert!(inspect_payload.in_chunk_index);
    }

    #[test]
    fn security_get_requires_auth_when_configured() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(Some("topsecret"));
        let unauthorized = handle_http_request(
            HttpRequest {
                method: "GET".to_string(),
                path: format!("{OPERATOR_SECURITY_AGENT_RUNTIME_STATE_PATH}?agent_id=7"),
                headers: HashMap::new(),
                body: Vec::new(),
            },
            &state,
        );
        assert_eq!(unauthorized.status, 401);

        let mut authorized = HttpRequest {
            method: "GET".to_string(),
            path: format!("{OPERATOR_SECURITY_AGENT_RUNTIME_STATE_PATH}?agent_id=7"),
            headers: HashMap::new(),
            body: Vec::new(),
        };
        authorized
            .headers
            .insert(OPERATOR_KEY_HEADER.to_string(), "topsecret".to_string());
        let response = handle_http_request(authorized, &state);
        assert_eq!(response.status, 200);
    }

    #[test]
    fn write_anomaly_test_requires_tracked_runtime() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(None);
        let response = handle_http_request(
            test_request(
                OPERATOR_SECURITY_WRITE_ANOMALY_TEST_PATH,
                serde_json::json!({
                    "agent_name": "Unbekannt",
                    "mode": "absolute-threshold",
                    "bytes_per_sec": 1000
                }),
            ),
            &state,
        );
        assert_eq!(response.status, 404);
    }

    #[test]
    fn apicp_snapshot_roundtrip_requires_auth_when_configured() {
        let (state, _rx, _platform_rx, _runtime_rx) = test_state(Some("topsecret"));

        let unauthorized = HttpRequest {
            method: "GET".to_string(),
            path: OPERATOR_APICP_SNAPSHOT_PATH.to_string(),
            headers: HashMap::new(),
            body: Vec::new(),
        };
        let response = handle_http_request(unauthorized, &state);
        assert_eq!(response.status, 401);

        let mut post = HttpRequest {
            method: "POST".to_string(),
            path: OPERATOR_APICP_SNAPSHOT_PATH.to_string(),
            headers: HashMap::new(),
            body: serde_json::to_vec(&ApiCpSnapshot {
                patterns: vec![ApiCpPatternSnapshot {
                    agent_id: "AGENT-01".to_string(),
                    fingerprint: "fp1".to_string(),
                    count: 3,
                    response_hashes: HashMap::from([(42_u64, 3_usize)]),
                    top_hash: 42,
                    top_content: "ok".to_string(),
                    confidence: 1.0,
                    last_seen: "2026-03-28T12:00:00Z".to_string(),
                    promoted: true,
                }],
                synth_count: 7,
                last_evolution_versions: HashMap::from([(
                    "AGENT-01".to_string(),
                    "v2".to_string(),
                )]),
            })
            .unwrap(),
        };
        post.headers
            .insert(OPERATOR_KEY_HEADER.to_string(), "topsecret".to_string());
        let post_response = handle_http_request(post, &state);
        assert_eq!(post_response.status, 200);

        let mut get = HttpRequest {
            method: "GET".to_string(),
            path: OPERATOR_APICP_SNAPSHOT_PATH.to_string(),
            headers: HashMap::new(),
            body: Vec::new(),
        };
        get.headers
            .insert(OPERATOR_KEY_HEADER.to_string(), "topsecret".to_string());
        let get_response = handle_http_request(get, &state);
        assert_eq!(get_response.status, 200);

        let payload: ApiCpSnapshot = serde_json::from_slice(&get_response.body).unwrap();
        assert_eq!(payload.patterns.len(), 1);
        assert_eq!(payload.patterns[0].fingerprint, "fp1");
        assert_eq!(payload.synth_count, 7);
        assert_eq!(
            payload.last_evolution_versions.get("AGENT-01"),
            Some(&"v2".to_string())
        );
    }

    #[test]
    fn apicp_snapshot_path_has_larger_body_limit() {
        assert_eq!(
            max_body_bytes_for_path(OPERATOR_APICP_SNAPSHOT_PATH),
            MAX_APICP_SNAPSHOT_BODY_BYTES
        );
        assert_eq!(max_body_bytes_for_path(OPERATOR_CHAT_PATH), MAX_BODY_BYTES);
    }

    #[test]
    fn config_apply_path_has_larger_body_limit() {
        // #425: eine ganze Firma (60+ Agents inline) muss durch das Body-Limit passen.
        assert_eq!(
            max_body_bytes_for_path(OPERATOR_CONFIG_APPLY_PATH),
            MAX_CONFIG_APPLY_BODY_BYTES
        );
        // Eine 60-Agent-Firma ist ~64 KB -> das Limit muss komfortabel darueber liegen.
        assert_ne!(
            max_body_bytes_for_path(OPERATOR_CONFIG_APPLY_PATH),
            MAX_BODY_BYTES
        );
    }

    #[tokio::test]
    async fn read_http_request_accepts_large_apicp_snapshot_body() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = vec![b'a'; MAX_BODY_BYTES + 512];
        let request = format!(
            "POST {} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
            OPERATOR_APICP_SNAPSHOT_PATH,
            body.len()
        );
        let (tx, rx) = oneshot::channel();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let parsed = read_http_request(&mut stream).await;
            tx.send(parsed).unwrap();
        });

        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        client.write_all(&body).await.unwrap();

        let parsed = rx.await.unwrap().unwrap();
        assert_eq!(parsed.path, OPERATOR_APICP_SNAPSHOT_PATH);
        assert_eq!(parsed.body.len(), body.len());
    }

    #[tokio::test]
    async fn read_http_request_rejects_large_non_apicp_body() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = vec![b'a'; MAX_BODY_BYTES + 1];
        let request = format!(
            "POST {} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
            OPERATOR_CHAT_PATH,
            body.len()
        );
        let (tx, rx) = oneshot::channel();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let parsed = read_http_request(&mut stream).await;
            tx.send(parsed).unwrap();
        });

        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        client.write_all(&body).await.unwrap();

        let err = rx.await.unwrap().unwrap_err();
        assert_eq!(err, ApiError::PayloadTooLarge);
    }
}
