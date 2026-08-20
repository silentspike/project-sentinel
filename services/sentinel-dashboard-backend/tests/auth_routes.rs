//! Integrations-Test (#463): die Projection-Read-Routen liegen hinter `require_auth`, `/cert-hash`
//! bleibt public. Geprueft via `build_app` + `tower::ServiceExt::oneshot` (kein Live-Server noetig).

use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use sentinel_dashboard_backend::{auth, build_app, AppState, Config};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

/// Test-State mit gesetztem Dashboard-Key + nicht existierender projection.db (authed-Read erreicht
/// dann den Handler und liefert 503, nicht 401 — beweist, dass das Auth-Gate passiert wurde).
fn test_state() -> AppState {
    let mut config = Config::from_env();
    config.dashboard_api_key = Some("test-key".into());
    config.projection_db = "/nonexistent/dashboard-test-projection.db".into();
    config.events_db = "/nonexistent/dashboard-test-events.db".into();
    config.gateway_proxy_url = "http://127.0.0.1:1".into();
    config.prometheus_url = "http://127.0.0.1:1".into();
    AppState::new(config).unwrap()
}

const READ_ROUTES: [&str; 14] = [
    "/api/agents",
    "/api/rooms",
    "/api/rooms/kueche/detail",
    "/api/metrics",
    "/api/metrics/ebpf",
    "/api/metrics/pipeline",
    "/api/metrics/tick",
    "/api/tasks",
    "/api/cost",
    "/api/cockpit",
    "/api/cockpit/incident/test-event",
    "/api/events",
    "/api/events/types",
    "/api/v1/delivery/lineage?tenant_id=m0-company&project_id=project-42",
];

const CONTROL_GET_ROUTES: [&str; 9] = [
    "/api/control/config",
    "/api/control/traffic-stats",
    // #429
    "/api/control/synthesis-rules",
    "/api/control/traffic-responses",
    "/api/control/judge-alerts",
    "/api/control/platform-state",
    "/api/control/platform-analyses",
    "/api/control/status",
    "/api/control/snapshots",
];

const CONTROL_POST_ROUTES: [&str; 9] = [
    "/api/control/chaos",
    "/api/control/stimulus",
    "/api/control/nightrun",
    "/api/control/provider",
    "/api/control/pause",
    "/api/control/resume",
    "/api/control/platform-analyze",
    "/api/control/snapshot",
    // #429: per-rule toggle (name path param)
    "/api/control/synthesis-rules/bio_hunger",
];

async fn get_json(path: &str) -> (StatusCode, serde_json::Value) {
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
    let json = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn projection_reads_return_401_without_cookie() {
    for path in READ_ROUTES {
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
}

#[tokio::test]
async fn control_proxy_routes_return_401_without_cookie() {
    for path in CONTROL_GET_ROUTES {
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
}

#[tokio::test]
async fn control_mutation_routes_return_401_without_cookie() {
    for path in CONTROL_POST_ROUTES {
        let app = build_app(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{path} ohne Cookie muss 401 sein"
        );
    }
}

#[tokio::test]
async fn authed_projection_read_passes_the_gate() {
    let state = test_state();
    let token = state.sessions.create(); // gueltige Session minten
    let app = build_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/agents")
                .header(header::COOKIE, format!("{}={token}", auth::SESSION_COOKIE))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Mit gueltiger Session erreicht der Request den Projection-Handler; da projection.db fehlt,
    // antwortet er 503 — entscheidend: NICHT 401, d.h. das require_auth-Gate wurde passiert.
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "authed Read erreicht den Handler (503), nicht 401"
    );
}

#[tokio::test]
async fn authed_degraded_routes_return_200_not_503() {
    let (status, cockpit) = get_json("/api/cockpit").await;
    assert_eq!(status, StatusCode::OK, "cockpit degradiert mit 200");
    assert_eq!(cockpit["events_db"], "offline");
    assert_eq!(cockpit["incidents"].as_array().unwrap().len(), 0);

    let (status, events) = get_json("/api/events").await;
    assert_eq!(status, StatusCode::OK, "events degradiert mit 200");
    assert_eq!(events["events_db"], "offline");
    assert_eq!(events["events"].as_array().unwrap().len(), 0);

    let (status, event_types) = get_json("/api/events/types").await;
    assert_eq!(status, StatusCode::OK, "event types degradiert mit 200");
    assert_eq!(event_types["events_db"], "offline");

    let (status, ebpf) = get_json("/api/metrics/ebpf").await;
    assert_eq!(status, StatusCode::OK, "ebpf degradiert mit 200");
    assert_eq!(ebpf["available"], false);
    assert_eq!(ebpf["prometheus"], "offline");

    let (status, pipeline) = get_json("/api/metrics/pipeline").await;
    assert_eq!(status, StatusCode::OK, "pipeline degradiert mit 200");
    assert_eq!(pipeline["gateway"], "offline");

    let (status, tick) = get_json("/api/metrics/tick").await;
    assert_eq!(status, StatusCode::OK, "tick degradiert mit 200");
    assert_eq!(tick["prometheus"], "offline");

    // #429: judge-alerts degrades to an empty array when events.db is offline.
    let (status, alerts) = get_json("/api/control/judge-alerts").await;
    assert_eq!(status, StatusCode::OK, "judge-alerts degradiert mit 200");
    assert_eq!(alerts.as_array().unwrap().len(), 0);

    // #427: cost degrades to empty arrays when projection.db is offline.
    let (status, cost) = get_json("/api/cost").await;
    assert_eq!(status, StatusCode::OK, "cost degradiert mit 200");
    assert_eq!(cost["projection"], "offline");
    assert_eq!(cost["by_agent"].as_array().unwrap().len(), 0);
    assert_eq!(cost["time_series"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn cert_hash_stays_public() {
    let mut state = test_state();
    state.config = std::sync::Arc::new({
        let mut config = (*state.config).clone();
        config.cert_hash_b64 = Some("test-hash".into());
        config
    });
    let app = build_app(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/cert-hash")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // cert-hash muss ohne Auth erreichbar bleiben (Browser braucht den Hash vor dem Login fuer das
    // WebTransport-`serverCertificateHashes`-Pinning).
    assert_eq!(resp.status(), StatusCode::OK, "cert-hash bleibt public");
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["hash"], "test-hash");
}

#[tokio::test]
async fn cert_hash_can_be_null_for_provided_cert_mode() {
    let app = build_app(test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/cert-hash")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "cert-hash bleibt public");
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["hash"].is_null());
    assert_eq!(json["algorithm"], "sha-256");
}

#[tokio::test]
async fn delivery_lineage_requires_the_dedicated_workflow_credential() {
    let state = test_state();
    let token = state.sessions.create();
    let response = build_app(state)
        .oneshot(
            Request::builder()
                .uri("/api/v1/delivery/lineage?tenant_id=m0-company&project_id=project-42")
                .header(header::COOKIE, format!("{}={token}", auth::SESSION_COOKIE))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn delivery_lineage_proxies_the_exact_scope_with_bearer_authority() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let operator_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0_u8; 8192];
        let read = stream.read(&mut request).await.unwrap();
        let request = String::from_utf8(request[..read].to_vec()).unwrap();
        let body = r#"{"server_redacted":true,"project_label":"project-42"}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        request
    });

    let mut state = test_state();
    state.config = std::sync::Arc::new({
        let mut config = (*state.config).clone();
        config.operator_url = operator_url;
        config.workflow_read_credential = Some("workflow-read-secret".into());
        config
    });
    let token = state.sessions.create();
    let response = build_app(state)
        .oneshot(
            Request::builder()
                .uri("/api/v1/delivery/lineage?tenant_id=m0-company&project_id=project-42")
                .header(header::COOKIE, format!("{}={token}", auth::SESSION_COOKIE))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["project_label"],
        "project-42"
    );

    let request = upstream.await.unwrap().to_ascii_lowercase();
    assert!(request.starts_with(
        "get /company/delivery/lineage?tenant_id=m0-company&project_id=project-42 http/1.1\r\n"
    ));
    assert!(request.contains("\r\nauthorization: bearer workflow-read-secret\r\n"));
}
