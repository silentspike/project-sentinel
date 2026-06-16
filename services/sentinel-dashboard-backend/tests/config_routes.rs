//! Integrationstests (#420): `/api/config` READ + WRITE (apply-proxy) hinter `require_auth`.
//! Option B (validate-then-proxy): invalide Config -> 400 (KEIN Upstream-Call); valide -> Forward an die
//! Operator-API (im Test ein toter Port -> 502, beweist: Validierung passierte + Forward wurde versucht).
//! Geprueft via `build_app` + `tower::ServiceExt::oneshot` (kein Live-Server noetig).

use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use sentinel_dashboard_backend::{auth, build_app, config, AppState, Config};
use tower::ServiceExt;

/// Repo-`config/` als Fixture (echte valide Agent-TOMLs + rooms.toml + daemon.toml).
const REPO_CONFIG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config");

fn test_state() -> AppState {
    let mut config = Config::from_env();
    config.dashboard_api_key = Some("test-key".into());
    config.config_dir = REPO_CONFIG.into();
    // max_agents aus derselben Single-Source (repo daemon.toml), nicht aus dem Default-Pfad.
    config.max_agents = config::read_max_agents(REPO_CONFIG);
    // Toter Operator-API-Port: ein erfolgreicher Forward => 502 (beweist Forward-Versuch).
    config.operator_url = "http://127.0.0.1:1".into();
    config.projection_db = "/nonexistent/p.db".into();
    config.events_db = "/nonexistent/e.db".into();
    AppState::new(config).unwrap()
}

const CONFIG_GET_ROUTES: [&str; 3] = [
    "/api/config/agents",
    "/api/config/rooms",
    "/api/config/daemon",
];

async fn authed_get(path: &str) -> (StatusCode, serde_json::Value) {
    let state = test_state();
    let token = state.sessions.create();
    let app = build_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::COOKIE, format!("{}={token}", auth::SESSION_COOKIE))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
    )
}

async fn authed_apply(body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let state = test_state();
    let token = state.sessions.create();
    let app = build_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/config/apply")
                .header(header::COOKIE, format!("{}={token}", auth::SESSION_COOKIE))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let b = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn config_routes_return_401_without_cookie() {
    for path in CONFIG_GET_ROUTES {
        let app = build_app(test_state());
        let resp = app
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{path} ohne Cookie muss 401 sein"
        );
    }
    // apply (POST) ist ebenfalls auth-gated.
    let app = build_app(test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/config/apply")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "apply ohne Cookie muss 401 sein"
    );
}

#[tokio::test]
async fn config_reads_return_parsed_config() {
    let (s, agents) = authed_get("/api/config/agents").await;
    assert_eq!(s, StatusCode::OK, "agents read 200");
    assert!(
        agents.as_array().map(|a| !a.is_empty()).unwrap_or(false),
        "agents-Liste nicht leer"
    );
    assert!(
        agents[0]["identity"]["id"].is_number(),
        "Agent hat identity.id"
    );

    let (s, rooms) = authed_get("/api/config/rooms").await;
    assert_eq!(s, StatusCode::OK, "rooms read 200");
    assert!(
        rooms["rooms"]
            .as_array()
            .map(|r| !r.is_empty())
            .unwrap_or(false),
        "rooms-Liste nicht leer"
    );

    let (s, daemon) = authed_get("/api/config/daemon").await;
    assert_eq!(s, StatusCode::OK, "daemon read 200");
    assert!(
        daemon["content"]
            .as_str()
            .unwrap_or("")
            .contains("[daemon]"),
        "daemon.toml content geliefert"
    );
}

#[tokio::test]
async fn apply_invalid_json_returns_400() {
    let state = test_state();
    let token = state.sessions.create();
    let app = build_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/config/apply")
                .header(header::COOKIE, format!("{}={token}", auth::SESSION_COOKIE))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("not valid json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "invalides JSON -> 400 (kein Forward)"
    );
}

#[tokio::test]
async fn apply_invalid_adjacency_returns_400_no_upstream() {
    // Valides Building via READ holen (Round-Trip), dann Adjacency brechen (dangling reference).
    let (_, rooms) = authed_get("/api/config/rooms").await;
    let mut bad = rooms.clone();
    bad["rooms"][0]["adjacent"]
        .as_array_mut()
        .expect("rooms[0].adjacent")
        .push(serde_json::json!("__nonexistent_room__"));
    let body = serde_json::json!({ "mode": "live", "agents": [], "building": bad });

    let (status, json) = authed_apply(body).await;
    // 400 = Validierung griff; NICHT 502 => Forward an die Operator-API wurde NICHT versucht.
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "invalide Adjacency -> 400, kein Upstream"
    );
    assert_eq!(json["accepted"], serde_json::json!(false));
    assert!(
        !json["errors"].as_array().unwrap().is_empty(),
        "errors gesetzt"
    );
}

#[tokio::test]
async fn apply_valid_forwards_to_operator_api() {
    // Valides Building (aus READ) + leere agents -> Validierung passt -> Forward an den toten
    // operator_url -> 502. Das beweist: Validierung passierte UND der Apply-Proxy wurde versucht.
    let (_, rooms) = authed_get("/api/config/rooms").await;
    let body = serde_json::json!({ "mode": "live", "agents": [], "building": rooms });

    let (status, _) = authed_apply(body).await;
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "valide Config -> Forward an Operator-API (toter Upstream -> 502), NICHT 400/401"
    );
}

// ── #421/#422/#423: generate + per-resource PUTs ──

async fn authed_request(
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let state = test_state();
    let token = state.sessions.create();
    let app = build_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::COOKIE, format!("{}={token}", auth::SESSION_COOKIE))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let b = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn generate_valid_spec_returns_preview() {
    // #421: minimaler valider GaiaSpec (Rest via serde-Defaults) -> 200 mit summary/agents/building.
    let spec = serde_json::json!({ "company_name": "TestCo", "agent_count": 30 });
    let (status, json) = authed_request("POST", "/api/config/generate", spec).await;
    assert_eq!(status, StatusCode::OK, "valide Spec -> 200 Preview");
    assert_eq!(json["summary"]["agent_count"], serde_json::json!(30));
    assert!(
        json["agents"].as_array().map(|a| a.len()).unwrap_or(0) == 30,
        "30 agents im Preview"
    );
    assert!(
        json["building"]["rooms"]
            .as_array()
            .map(|r| !r.is_empty())
            .unwrap_or(false),
        "building hat rooms"
    );
}

#[tokio::test]
async fn generate_invalid_spec_returns_400() {
    // agent_count=0 verletzt GaiaSpec::validate -> generate Err -> 400 (kein Persist).
    let spec = serde_json::json!({ "company_name": "TestCo", "agent_count": 0 });
    let (status, _) = authed_request("POST", "/api/config/generate", spec).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "agent_count=0 -> 400");
    // Auch kaputtes JSON -> 400.
    let (status, _) = authed_request("POST", "/api/config/generate", serde_json::json!("x")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "kaputter GaiaSpec -> 400");
}

#[tokio::test]
async fn put_agent_valid_forwards_to_operator_api() {
    // #422: existierenden Agenten unveraendert zurueck-PUTten -> volle Firma assembliert + valide
    // -> Forward an toten Upstream -> 502 (beweist load+merge+validate+forward).
    let (_, agents) = authed_get("/api/config/agents").await;
    let agent = agents[0].clone();
    let id = agent["identity"]["id"].as_u64().unwrap();
    let (status, _) = authed_request("PUT", &format!("/api/config/agents/{id}"), agent).await;
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "valider Agent-Edit -> Forward (toter Upstream -> 502)"
    );
}

#[tokio::test]
async fn put_agent_invalid_personality_returns_400_no_upstream() {
    // #422 AC-2: Big-Five > 1.0 -> validate_apply lehnt ab -> 400, KEIN Forward (NICHT 502).
    let (_, agents) = authed_get("/api/config/agents").await;
    let mut agent = agents[0].clone();
    let id = agent["identity"]["id"].as_u64().unwrap();
    agent["personality"]["openness"] = serde_json::json!(2.0);
    let (status, json) = authed_request("PUT", &format!("/api/config/agents/{id}"), agent).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "openness=2.0 -> 400, kein Upstream"
    );
    assert_eq!(json["accepted"], serde_json::json!(false));
}

#[tokio::test]
async fn put_agent_unknown_id_returns_404() {
    let (_, agents) = authed_get("/api/config/agents").await;
    let agent = agents[0].clone();
    let (status, _) = authed_request("PUT", "/api/config/agents/9999", agent).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unbekannte id -> 404");
}

#[tokio::test]
async fn put_rooms_one_sided_adjacency_returns_400_no_upstream() {
    // #423 AC-1: einseitige Adjazenz (r0->r1, aber r1->r0 fehlt) -> 400 not bidirectional, KEIN Forward.
    let (_, rooms) = authed_get("/api/config/rooms").await;
    let mut bad = rooms.clone();
    let r1_id = bad["rooms"][1]["id"].clone();
    // r1.adjacent leeren (garantiert r1->r0 nicht vorhanden), r1 zu r0.adjacent hinzufuegen.
    bad["rooms"][1]["adjacent"] = serde_json::json!([]);
    let r0_adj = bad["rooms"][0]["adjacent"].as_array_mut().unwrap();
    if !r0_adj.iter().any(|x| *x == r1_id) {
        r0_adj.push(r1_id);
    }
    let (status, json) = authed_request("PUT", "/api/config/rooms", bad).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "einseitige Adjazenz -> 400, kein Upstream"
    );
    assert_eq!(json["accepted"], serde_json::json!(false));
    assert!(!json["errors"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn put_rooms_valid_forwards_to_operator_api() {
    // #423: valides (unveraendertes) Building zurueck-PUTten -> Forward an toten Upstream -> 502.
    let (_, rooms) = authed_get("/api/config/rooms").await;
    let (status, _) = authed_request("PUT", "/api/config/rooms", rooms).await;
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "valides rooms-Edit -> Forward (toter Upstream -> 502)"
    );
}

#[tokio::test]
async fn daemon_read_exposes_parsed_whitelist() {
    // #423 Daemon-Viewer: get_daemon liefert die geparste read-only Whitelist.
    let (s, daemon) = authed_get("/api/config/daemon").await;
    assert_eq!(s, StatusCode::OK);
    assert!(daemon["max_agents"].is_number(), "max_agents geparst");
    assert!(
        daemon["tick_rate_ms"].is_number(),
        "tick_rate_ms geparst"
    );
}
