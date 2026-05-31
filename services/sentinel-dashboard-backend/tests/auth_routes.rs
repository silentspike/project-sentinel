//! Integrations-Test (#463): die Projection-Read-Routen liegen hinter `require_auth`, `/cert-hash`
//! bleibt public. Geprueft via `build_app` + `tower::ServiceExt::oneshot` (kein Live-Server noetig).

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use sentinel_dashboard_backend::{auth, build_app, AppState, Config};
use tower::ServiceExt;

/// Test-State mit gesetztem Dashboard-Key + nicht existierender projection.db (authed-Read erreicht
/// dann den Handler und liefert 503, nicht 401 — beweist, dass das Auth-Gate passiert wurde).
fn test_state() -> AppState {
    let mut config = Config::from_env();
    config.dashboard_api_key = Some("test-key".into());
    config.projection_db = "/nonexistent/dashboard-test-projection.db".into();
    AppState::new(config).unwrap()
}

const READ_ROUTES: [&str; 4] = ["/api/agents", "/api/rooms", "/api/metrics", "/api/tasks"];

#[tokio::test]
async fn projection_reads_return_401_without_cookie() {
    for path in READ_ROUTES {
        let app = build_app(test_state());
        let resp = app
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{path} ohne Cookie muss 401 sein");
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
async fn cert_hash_stays_public() {
    let app = build_app(test_state());
    let resp = app
        .oneshot(Request::builder().uri("/api/cert-hash").body(Body::empty()).unwrap())
        .await
        .unwrap();
    // cert-hash muss ohne Auth erreichbar bleiben (Browser braucht den Hash vor dem Login fuer das
    // WebTransport-`serverCertificateHashes`-Pinning).
    assert_eq!(resp.status(), StatusCode::OK, "cert-hash bleibt public");
}
