//! Config Read+Write (#420, Epic #418): liest die Plattform-Config (Agent-TOMLs / rooms.toml /
//! daemon.toml) read-only fuer die SolidJS-Editoren und nimmt validierte Config-Applies entgegen.
//!
//! **Architektur (Option B):** Das Dashboard ist **Validator + Apply-Proxy** — es schreibt NIE selbst
//! in `config_dir` (der Daemon ist alleiniger Schreiber, #425: `persist_company_config` + Pre-Apply-Backup
//! sind daemon-intern). Ein valider `POST /api/config/apply` wird an die Daemon-Operator-API
//! `/operator/config/apply` (#425) weitergereicht; der Daemon macht den durablen Write + Backup + macht es
//! live. Die Vorvalidierung hier ist eine **exakte Teilmenge** der daemon-internen `validate_config_apply`
//! (NIE strenger) — fail-fast vor dem Proxy.

use std::collections::HashSet;
use std::path::Path;

use axum::{
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use sentinel_common::agent_config::AgentConfigValidation;
use sentinel_common::room::BuildingConfig;
use sentinel_common::{AgentId, OperatorConfigApplyCommand};

use crate::AppState;

/// Liest `[daemon].max_agents` aus `config_dir/daemon.toml` (dieselbe Single-Source wie der Daemon → kein
/// Drift). `None` wenn daemon.toml fehlt/unparsebar/feldlos → der Aufrufer behandelt das lenient
/// (kein falsches 400; Bound-Check bleibt dem Daemon ueberlassen).
pub fn read_max_agents(config_dir: &str) -> Option<usize> {
    #[derive(serde::Deserialize)]
    struct Peek {
        daemon: DaemonSection,
    }
    #[derive(serde::Deserialize)]
    struct DaemonSection {
        max_agents: Option<usize>,
    }
    let text = std::fs::read_to_string(Path::new(config_dir).join("daemon.toml")).ok()?;
    toml::from_str::<Peek>(&text).ok()?.daemon.max_agents
}

/// Baut die `AgentConfigValidation`. Bekanntes `max` → echte Daemon-Grenze; `None` → `u16::MAX`
/// (lenient, Bound-Check dem Daemon ueberlassen — nie strenger als der Daemon).
fn validation_for(max_agents: Option<usize>) -> AgentConfigValidation {
    let max_id = max_agents
        .and_then(|m| u16::try_from(m).ok())
        .unwrap_or(u16::MAX);
    AgentConfigValidation::with_max_agent_id(max_id)
}

fn read_error(what: &str, e: impl std::fmt::Display) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": format!("{what} not readable: {e}") })),
    )
        .into_response()
}

// ── READ (read-only, geparst als JSON) ──

/// GET /api/config/agents — alle Agent-TOMLs aus `config_dir/agents/` geparst.
pub async fn get_agents(State(st): State<AppState>) -> Response {
    let dir = Path::new(&st.config.config_dir).join("agents");
    match sentinel_common::agent_config::load_all_agents_with_validation(
        &dir,
        validation_for(st.config.max_agents),
    ) {
        Ok(agents) => Json(agents).into_response(),
        Err(e) => read_error("agent configs", e),
    }
}

/// GET /api/config/rooms — rooms.toml geparst.
pub async fn get_rooms(State(st): State<AppState>) -> Response {
    let path = Path::new(&st.config.config_dir).join("rooms.toml");
    match BuildingConfig::load(&path) {
        Ok(building) => Json(building).into_response(),
        Err(e) => read_error("rooms.toml", e),
    }
}

/// GET /api/config/daemon — daemon.toml als Roh-Text (read-only; daemon.toml-WRITE ist out-of-scope,
/// nicht #425-apply-bar).
pub async fn get_daemon(State(st): State<AppState>) -> Response {
    let path = Path::new(&st.config.config_dir).join("daemon.toml");
    match std::fs::read_to_string(&path) {
        Ok(content) => Json(json!({ "content": content })).into_response(),
        Err(e) => read_error("daemon.toml", e),
    }
}

// ── WRITE (validate → Apply-Proxy an den Daemon) ──

/// Exakte Teilmenge der daemon-internen `validate_config_apply`
/// (services/sentinel-daemon/src/config_apply.rs) — **NIE strenger**. Sammelt alle Fehler (kein Early-Exit).
fn validate_apply(cmd: &OperatorConfigApplyCommand, max_agents: Option<usize>) -> Vec<String> {
    let mut errors = Vec::new();

    if let Some(max) = max_agents {
        if cmd.agents.len() > max {
            errors.push(format!(
                "agent count {} exceeds daemon.max_agents {}",
                cmd.agents.len(),
                max
            ));
        }
    }

    let min_capacity = u16::try_from(cmd.agents.len()).unwrap_or(u16::MAX);
    if let Err(room_errors) = cmd.building.validate(min_capacity) {
        errors.extend(room_errors);
    }

    let bounds = validation_for(max_agents).agent_id_bounds;
    let mut seen = HashSet::new();
    for agent in &cmd.agents {
        if !seen.insert(agent.identity.id) {
            errors.push(format!("duplicate agent id {}", agent.identity.id));
        }
        if let Err(e) = agent.personality.validate() {
            errors.push(format!(
                "agent {} personality invalid: {e}",
                agent.identity.id
            ));
        }
        if let Err(e) = AgentId::new_with_bounds(agent.identity.id, bounds) {
            errors.push(format!("agent {} id out of bounds: {e}", agent.identity.id));
        }
    }

    errors
}

/// POST /api/config/apply — validiert die Inline-Config (`OperatorConfigApplyCommand`) und reicht sie bei
/// Erfolg an die Daemon-Operator-API `/operator/config/apply` (#425) weiter. Invalide → `400` (KEIN Proxy).
/// Der Daemon ist alleiniger Schreiber (durabler Write + Pre-Apply-Backup + live).
pub async fn apply(State(st): State<AppState>, body: Bytes) -> Response {
    let cmd: OperatorConfigApplyCommand =
        match serde_json::from_slice(&body) {
            Ok(c) => c,
            Err(e) => return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "accepted": false, "errors": [format!("invalid config JSON: {e}")] })),
            )
                .into_response(),
        };

    let errors = validate_apply(&cmd, st.config.max_agents);
    if !errors.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "accepted": false, "errors": errors })),
        )
            .into_response();
    }

    // Valide → an den Daemon (alleiniger Schreiber); operator_auth=true (x-sentinel-operator-key).
    crate::control::forward(
        &st,
        reqwest::Method::POST,
        format!("{}/operator/config/apply", st.config.operator_url),
        true,
        Some(body),
    )
    .await
}
