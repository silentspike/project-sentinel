//! Integrations-Test (#432 F4): faellt NATS aus, darf der Event-Subscriber weder panicken noch
//! exiten — er retried mit Backoff, und der Connect-Snapshot + die HTTP-Routen bleiben intakt.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use sentinel_dashboard_backend::{build_app, event_sub, AppState, Config};
use tower::ServiceExt;

#[tokio::test]
async fn nats_unreachable_keeps_subscriber_and_http_alive() {
    let mut config = Config::from_env();
    // Port 1 = nichts lauscht -> connect refused. subscribe_and_pump gibt Err zurueck -> Backoff-Retry
    // (kein endloses Haengen, kein Panic).
    config.nats_url = "nats://127.0.0.1:1".into();
    config.projection_db = "/nonexistent/resilience-test.db".into();
    let state = AppState::new(config).unwrap();

    let handle = tokio::spawn(event_sub::run_event_subscriber(state.clone()));
    // Genug Zeit fuer mehrere fehlgeschlagene Connect-Versuche (Backoff startet bei 500ms).
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Der Subscriber-Task laeuft weiter (Retry-Loop) — NICHT gepanickt/beendet.
    assert!(!handle.is_finished(), "Subscriber darf bei NATS-Ausfall nicht panicken/exiten");

    // HTTP bleibt funktionsfaehig (cert-hash ist public, kein DB/NATS noetig).
    let app = build_app(state);
    let resp = app
        .oneshot(Request::builder().uri("/api/cert-hash").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "HTTP muss trotz NATS-Ausfall antworten");

    handle.abort();
}
