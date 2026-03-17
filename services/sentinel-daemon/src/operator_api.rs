//! Lokale Operator-API fuer manuelle Chaos-Trigger.
//!
//! Dashboard schreibt nicht direkt in EventStore/Projection, sondern spricht
//! diese Loopback-API an. Die API validiert Raum und Payload und leitet das
//! Kommando via std::sync::mpsc in den laufenden ECS-Thread.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::sync::Arc;

use anyhow::{Context, Result as AnyResult};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};

use sentinel_common::{
    EventType, OperatorChaosCommand, OperatorCommand, OperatorNightrunCommand,
    OperatorRoomStimulusCommand, RoomStimulusType,
};

use crate::config::OperatorApiConfig;

const OPERATOR_CHAOS_PATH: &str = "/operator/chaos";
const OPERATOR_STIMULUS_PATH: &str = "/operator/stimulus";
const OPERATOR_NIGHTRUN_PATH: &str = "/operator/nightrun";
const OPERATOR_SNAPSHOTS_PATH: &str = "/operator/snapshots";
const OPERATOR_SNAPSHOT_PATH: &str = "/operator/snapshot";
const OPERATOR_RESTORE_PATH: &str = "/operator/restore";
const OPERATOR_PRUNE_PATH: &str = "/operator/prune";
const MAX_REQUEST_BYTES: usize = 32 * 1024;
const MAX_BODY_BYTES: usize = 8 * 1024;
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
    pub message: String,
}

#[derive(Clone)]
struct AppState {
    allowed_rooms: Arc<HashSet<String>>,
    shared_secret: Option<String>,
    command_tx: mpsc::Sender<OperatorCommand>,
    nightrun_tx: mpsc::Sender<OperatorNightrunCommand>,
    snapshot_tx: mpsc::Sender<sentinel_common::OperatorSnapshotCommand>,
    restore_tx: mpsc::Sender<sentinel_common::OperatorRestoreCommand>,
    event_store: Arc<sentinel_limbo::EventStore>,
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

#[derive(Debug, PartialEq, Eq)]
enum ApiError {
    BadRequest(&'static str),
    Unauthorized,
    NotFound(&'static str),
    MethodNotAllowed,
    PayloadTooLarge,
    ServiceUnavailable(&'static str),
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

pub async fn start_server(
    config: OperatorApiConfig,
    allowed_rooms: Vec<String>,
    command_tx: mpsc::Sender<OperatorCommand>,
    nightrun_tx: mpsc::Sender<OperatorNightrunCommand>,
    snapshot_tx: mpsc::Sender<sentinel_common::OperatorSnapshotCommand>,
    restore_tx: mpsc::Sender<sentinel_common::OperatorRestoreCommand>,
    event_store: Arc<sentinel_limbo::EventStore>,
) -> AnyResult<tokio::task::JoinHandle<()>> {
    let listener = TcpListener::bind(&config.bind_addr)
        .await
        .with_context(|| format!("Operator-API bind fehlgeschlagen: {}", config.bind_addr))?;
    let room_count = allowed_rooms.len();
    let state = AppState {
        allowed_rooms: Arc::new(allowed_rooms.into_iter().collect()),
        shared_secret: config.shared_secret,
        command_tx,
        nightrun_tx,
        snapshot_tx,
        restore_tx,
        event_store,
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
    // GET-Endpoints ohne Auth (read-only)
    if request.method == "GET" {
        return match request.path.as_str() {
            OPERATOR_SNAPSHOTS_PATH => match state.event_store.list_world_snapshots() {
                Ok(snapshots) => json_response(200, snapshots),
                Err(_e) => {
                    ApiError::ServiceUnavailable("Snapshot-Liste nicht verfuegbar").to_response()
                }
            },
            _ => ApiError::NotFound("Endpoint unbekannt").to_response(),
        };
    }
    if request.method != "POST" {
        return ApiError::MethodNotAllowed.to_response();
    }
    if !is_authorized(&request.headers, state.shared_secret.as_deref()) {
        return ApiError::Unauthorized.to_response();
    }

    match request.path.as_str() {
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
                        message: "Restore gestartet".to_string(),
                    },
                ),
                Err(_) => {
                    ApiError::ServiceUnavailable("Restore-Channel nicht verfuegbar").to_response()
                }
            }
        }
        OPERATOR_PRUNE_PATH => {
            info!("Manuelles Pruning via Operator-API angefordert — wird asynchron ausgefuehrt");
            // Prune + VACUUM laeuft im Tick-Loop Thread (nicht im HTTP-Thread!)
            // um den HTTP-Handler nicht zu blockieren (VACUUM auf GB-DBs dauert Minuten).
            let snapshots = state.event_store.list_world_snapshots().unwrap_or_default();
            if snapshots.len() < 2 {
                return json_response(
                    200,
                    serde_json::json!({"accepted": false, "message": "Zu wenige Snapshots fuer Pruning"}),
                );
            }
            let prune_point = snapshots[snapshots.len() - 2].last_event_id;
            let es = Arc::clone(&state.event_store);
            std::thread::spawn(move || match es.prune_events_before(prune_point) {
                Ok(deleted) => {
                    info!(deleted, "Pruning abgeschlossen, starte VACUUM");
                    match es.vacuum() {
                        Ok(()) => info!("VACUUM abgeschlossen"),
                        Err(e) => warn!(error = %e, "VACUUM fehlgeschlagen"),
                    }
                }
                Err(e) => warn!(error = %e, "Pruning fehlgeschlagen"),
            });
            json_response(
                202,
                serde_json::json!({"accepted": true, "message": "Pruning + VACUUM gestartet (asynchron)"}),
            )
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

    state
        .nightrun_tx
        .send(command)
        .map_err(|_| ApiError::ServiceUnavailable("Nightrun-Channel nicht verfuegbar"))?;

    Ok(TriggerNightrunResponse {
        accepted: true,
        message: "Nightrun-Konsolidierung gestartet".to_string(),
    })
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
    if content_length > MAX_BODY_BYTES {
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
        if body.len() > MAX_BODY_BYTES {
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

    fn test_state(secret: Option<&str>) -> (AppState, mpsc::Receiver<OperatorCommand>) {
        let (tx, rx) = mpsc::channel();
        let (nightrun_tx, _nightrun_rx) = mpsc::channel();
        let state = AppState {
            allowed_rooms: Arc::new(
                ["empfang".to_string(), "flur_eg".to_string()]
                    .into_iter()
                    .collect(),
            ),
            shared_secret: secret.map(str::to_string),
            command_tx: tx,
            nightrun_tx,
            snapshot_tx: mpsc::channel().0,
            restore_tx: mpsc::channel().0,
            event_store: Arc::new(
                sentinel_limbo::EventStore::open(":memory:")
                    .expect("in-memory EventStore fuer Tests"),
            ),
        };
        (state, rx)
    }

    fn test_request(path: &str, body: serde_json::Value) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            path: path.to_string(),
            headers: HashMap::new(),
            body: serde_json::to_vec(&body).unwrap(),
        }
    }

    #[test]
    fn valid_trigger_request_is_accepted_and_forwarded() {
        let (state, rx) = test_state(None);
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
        let (state, _rx) = test_state(None);
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
        let (state, _rx) = test_state(Some("topsecret"));
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
        let (state, rx) = test_state(Some("topsecret"));
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
        let (state, rx) = test_state(None);
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

    #[test]
    fn zero_delta_stimulus_is_rejected() {
        let (state, _rx) = test_state(None);
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
}
