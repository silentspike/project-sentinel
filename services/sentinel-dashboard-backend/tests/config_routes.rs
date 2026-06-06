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
