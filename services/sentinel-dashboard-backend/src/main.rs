//! sentinel-dashboard-backend entrypoint (#431): HTTPS (axum) + WebTransport/QUIC.
//!
//! - HTTPS :8001 (self-signed) — secure context fuer die WebTransport-API + ServeDir(Bundle) + API.
//! - WebTransport/QUIC :4434 — topic+msgpack+zstd Push.
//! - Auth: httpOnly-Session (#402/#405). Control-Routen hinter `require_auth`.
//!
//! Laeuft parallel zum Bun-Dashboard (:8000) — phased cutover.

use std::net::SocketAddr;

use axum::{
    extract::State,
    middleware,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use tower_http::{cors::CorsLayer, services::ServeDir};

use sentinel_dashboard_backend::{auth, control, projection, tls, wt, AppState, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();

    // rustls Crypto-Provider (0.23) einmalig installieren (wird von axum-server + wtransport genutzt).
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let mut config = Config::from_env();

    // Geteiltes self-signed Cert (HTTPS + WebTransport), Hash fuer /api/cert-hash.
    let cert_dir = std::env::var("SENTINEL_DASHBOARD_CERT_DIR")
        .unwrap_or_else(|_| "/opt/sentinel/console-cert".into());
    let cert = tls::generate(std::path::Path::new(&cert_dir), &["localhost", "127.0.0.1", "10.0.0.240"])?;
    config.cert_hash_b64 = Some(cert.cert_hash_b64.clone());

    let http_bind: SocketAddr = config.http_bind.parse()?;
    let state = AppState::new(config)?;

    // WebTransport-Server in eigenem Task.
    {
        let wt_state = state.clone();
        let (cp, kp) = (cert.cert_pem_path.clone(), cert.key_pem_path.clone());
        tokio::spawn(async move {
            if let Err(e) = wt::run_server(wt_state, cp, kp).await {
                tracing::error!(error = %e, "wt server terminated");
            }
        });
    }

    // Control-Proxy hinter require_auth.
    let control_routes = Router::new()
        .route("/chaos", post(control::chaos))
        .route("/stimulus", post(control::stimulus))
        .route("/nightrun", post(control::nightrun))
        .route("/config", get(control::get_config).patch(control::patch_config))
        .route("/provider", post(control::provider))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::require_auth));

    let api = Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/status", get(auth::status))
        .route("/cert-hash", get(cert_hash))
        .route("/agents", get(projection::agents))
        .route("/rooms", get(projection::rooms))
        .route("/metrics", get(projection::metrics))
        .route("/tasks", get(projection::tasks))
        .nest("/control", control_routes);

    let app = Router::new()
        .nest("/api", api)
        .fallback_service(ServeDir::new(&state.config.bundle_dir).append_index_html_on_directories(true))
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert.cert_pem_path, &cert.key_pem_path).await?;
    tracing::info!(%http_bind, bundle = %state.config.bundle_dir, "sentinel-dashboard-backend HTTPS listening");
    axum_server::bind_rustls(http_bind, tls_config)
        .serve(app.into_make_service())
        .await?;
    Ok(())
}

/// GET /api/cert-hash — base64(sha-256(cert DER)) fuer WebTransport `serverCertificateHashes` (leer bei CA-Cert).
async fn cert_hash(State(st): State<AppState>) -> Json<Value> {
    Json(json!({ "hash": st.config.cert_hash_b64, "algorithm": "sha-256" }))
}
