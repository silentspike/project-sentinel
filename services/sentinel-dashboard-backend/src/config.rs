//! Config Read+Write (#420, Epic #418): liest die Plattform-Config (Agent-TOMLs / rooms.toml /
//! daemon.toml) read-only fuer die SolidJS-Editoren und nimmt validierte Config-Applies entgegen.
//!
//! **Architektur (Option B):** Das Dashboard ist **Validator + Apply-Proxy** — es schreibt NIE selbst
//! in `config_dir` (der Daemon ist alleiniger Schreiber, #425: `persist_company_config` + Pre-Apply-Backup
//! sind daemon-intern). Ein valider `POST /api/config/apply` wird an die Daemon-Operator-API
//! `/operator/config/apply` (#425) weitergereicht; der Daemon macht den durablen Write + Backup + macht es
//! live. Die Vorvalidierung hier ist eine **exakte Teilmenge** der daemon-internen `validate_config_apply`
//! (NIE strenger) — fail-fast vor dem Proxy.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use axum::{
    body::Bytes,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use sentinel_common::agent_config::{load_all_agents, AgentConfig, AgentConfigValidation};
use sentinel_common::room::BuildingConfig;
use sentinel_common::{AgentId, ApplyMode, OperatorConfigApplyCommand};

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

/// GET /api/config/daemon — daemon.toml als Roh-Text PLUS die geparste read-only Whitelist
/// `{max_agents, time_scale, tick_rate_ms}` fuer den #423-Daemon-Viewer. daemon.toml-WRITE bleibt
/// out-of-scope: kein #425-Apply-Pfad (`OperatorConfigApplyCommand` traegt nur agents+building) und das
/// netz-exponierte Dashboard darf `config_dir` nicht schreiben (#420/#474). Editieren = Follow-up
/// (Daemon-Operator-Endpunkt fuer Runtime-Params).
pub async fn get_daemon(State(st): State<AppState>) -> Response {
    let path = Path::new(&st.config.config_dir).join("daemon.toml");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => return read_error("daemon.toml", e),
    };
    // Lenient: fehlende/unparsebare Felder → null (kein 400; reiner Viewer, nie strenger als der Daemon).
    #[derive(serde::Deserialize, Default)]
    struct DaemonPeek {
        #[serde(default)]
        max_agents: Option<usize>,
        #[serde(default)]
        time_scale: Option<f32>,
        #[serde(default)]
        tick_rate_ms: Option<u64>,
    }
    #[derive(serde::Deserialize, Default)]
    struct Peek {
        #[serde(default)]
        daemon: DaemonPeek,
    }
    let peek = toml::from_str::<Peek>(&content).unwrap_or_default();
    Json(json!({
        "content": content,
        "max_agents": peek.daemon.max_agents,
        "time_scale": peek.daemon.time_scale,
        "tick_rate_ms": peek.daemon.tick_rate_ms,
    }))
    .into_response()
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

// ── #421/#422/#423: Generate + per-Resource PUTs (validate → Apply-Proxy) ──

fn bad_request(msg: impl std::fmt::Display) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": msg.to_string() })),
    )
        .into_response()
}

/// Validiert das assemblierte `OperatorConfigApplyCommand` (Subset, NIE strenger) und reicht es bei
/// Erfolg an den Daemon weiter (alleiniger Schreiber). Invalide → 400, KEIN Proxy.
async fn validate_and_forward(st: &AppState, cmd: OperatorConfigApplyCommand) -> Response {
    let errors = validate_apply(&cmd, st.config.max_agents);
    if !errors.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "accepted": false, "errors": errors })),
        )
            .into_response();
    }
    let body = match serde_json::to_vec(&cmd) {
        Ok(b) => Bytes::from(b),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("serialize apply command: {e}") })),
            )
                .into_response()
        }
    };
    crate::control::forward(
        st,
        reqwest::Method::POST,
        format!("{}/operator/config/apply", st.config.operator_url),
        true,
        Some(body),
    )
    .await
}

/// Parst aus der `GeneratedCompany` die `agents/*.toml` → `Vec<AgentConfig>` und `rooms.toml`
/// → `BuildingConfig` (die deterministisch erzeugten Artefakte; Round-Trip beweist Schema-Treue).
fn parse_generated(
    generated: &sentinel_gaia::GeneratedCompany,
) -> Result<(Vec<AgentConfig>, BuildingConfig), String> {
    let mut agents = Vec::new();
    let mut building: Option<BuildingConfig> = None;
    for file in &generated.files {
        let rel = file.relative_path.to_string_lossy();
        if rel == "rooms.toml" {
            building =
                Some(toml::from_str(&file.contents).map_err(|e| format!("rooms.toml: {e}"))?);
        } else if rel.starts_with("agents/") && rel.ends_with(".toml") {
            agents.push(toml::from_str(&file.contents).map_err(|e| format!("{rel}: {e}"))?);
        }
    }
    let building = building.ok_or_else(|| "generated company has no rooms.toml".to_string())?;
    agents.sort_by_key(|a: &AgentConfig| a.identity.id);
    Ok((agents, building))
}

/// Agent-Verteilung pro `shift_set` (fuer die Wizard-Preview).
fn shift_distribution(agents: &[AgentConfig]) -> BTreeMap<String, usize> {
    let mut dist = BTreeMap::new();
    for agent in agents {
        *dist
            .entry(agent.identity.shift_set.to_string())
            .or_insert(0) += 1;
    }
    dist
}

/// POST /api/config/generate (#421) — deterministischer Company-Generator (preview-only, KEIN Persist).
/// Body = `GaiaSpec` → `sentinel_gaia::generate` → geparste `{summary, agents, building}`. Der Wizard
/// ruft danach den bestehenden `POST /api/config/apply {mode:"fresh"}` fuer den Deploy.
pub async fn generate_company(State(_st): State<AppState>, body: Bytes) -> Response {
    let spec: sentinel_gaia::GaiaSpec = match serde_json::from_slice(&body) {
        Ok(s) => s,
        Err(e) => return bad_request(format!("invalid GaiaSpec JSON: {e}")),
    };
    let generated = match sentinel_gaia::generate(spec) {
        Ok(g) => g,
        Err(e) => return bad_request(format!("generation failed: {e}")),
    };
    let (agents, building) = match parse_generated(&generated) {
        Ok(x) => x,
        Err(e) => return bad_request(format!("generated config unparseable: {e}")),
    };
    Json(json!({
        "summary": {
            "agent_count": agents.len(),
            "room_count": building.rooms.len(),
            "shift_distribution": shift_distribution(&agents),
        },
        "agents": agents,
        "building": building,
    }))
    .into_response()
}

/// PUT /api/config/agents/{id} (#422) — editiert EINEN Agenten: laedt die aktuelle Firma, ersetzt den
/// Agenten mit `identity.id == id` (404 falls unbekannt), assembliert das volle `mode:Live`-Command
/// (Apply traegt immer die ganze Firma) und proxyt validiert an den Daemon.
pub async fn put_agent(
    State(st): State<AppState>,
    AxumPath(id): AxumPath<u16>,
    body: Bytes,
) -> Response {
    let edited: AgentConfig = match serde_json::from_slice(&body) {
        Ok(a) => a,
        Err(e) => return bad_request(format!("invalid AgentConfig JSON: {e}")),
    };
    let agents_dir = Path::new(&st.config.config_dir).join("agents");
    let mut agents = match load_all_agents(&agents_dir) {
        Ok(a) => a,
        Err(e) => return read_error("agent configs", e),
    };
    match agents.iter().position(|a| a.identity.id == id) {
        Some(idx) => agents[idx] = edited,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("agent {id} not found") })),
            )
                .into_response()
        }
    }
    let rooms_path = Path::new(&st.config.config_dir).join("rooms.toml");
    let building = match BuildingConfig::load(&rooms_path) {
        Ok(b) => b,
        Err(e) => return read_error("rooms.toml", e),
    };
    validate_and_forward(
        &st,
        OperatorConfigApplyCommand {
            mode: ApplyMode::Live,
            agents,
            building,
        },
    )
    .await
}

/// PUT /api/config/rooms (#423) — editiert das Building: nimmt das `BuildingConfig`, laedt die aktuellen
/// Agents dazu, assembliert `mode:Live` und proxyt validiert (bidirektionale Adjazenz/Refs werden vom
/// `validate_apply`→`BuildingConfig::validate` server-autoritativ erzwungen) an den Daemon.
pub async fn put_rooms(State(st): State<AppState>, body: Bytes) -> Response {
    let building: BuildingConfig = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return bad_request(format!("invalid BuildingConfig JSON: {e}")),
    };
    let agents_dir = Path::new(&st.config.config_dir).join("agents");
    let agents = match load_all_agents(&agents_dir) {
        Ok(a) => a,
        Err(e) => return read_error("agent configs", e),
    };
    validate_and_forward(
        &st,
        OperatorConfigApplyCommand {
            mode: ApplyMode::Live,
            agents,
            building,
        },
    )
    .await
}
