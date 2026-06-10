//! sentinel-dashboard-backend entrypoint (#431/#432): HTTPS (axum) + WebTransport/QUIC + Event-Push.
//!
//! - HTTPS :8001 (self-signed) — secure context fuer die WebTransport-API + ServeDir(Bundle) + API.
//! - WebTransport/QUIC same-origin auf dem HTTPS-Port (`wt_bind`-Default :8001, UDP) — topic+msgpack+zstd Push.
//! - Event-Stream-Push (#432/#433): NATS SENTINEL_EVENTS -> Projection-Frames, events.db -> EventLog-Frames.
//! - Auth: httpOnly-Session (#402/#405) fuer HTTP/Control- + Projection-Read-Routen (#463); der
//!   WebTransport-Pfad nutzt ein kurzlebiges Einmal-Ticket (`?t=`, WT traegt keine Cookies).
//!
//! Der HTTP-Router wird in `sentinel_dashboard_backend::build_app` gebaut (testbar fuer die
//! Auth-Gates, #463).

use std::net::SocketAddr;

use sentinel_dashboard_backend::{build_app, event_sub, tls, wt, AppState, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    // rustls Crypto-Provider (0.23) einmalig installieren (wird von axum-server + wtransport genutzt).
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let mut config = Config::from_env();

    // Geteiltes self-signed Cert (HTTPS + WebTransport), Hash fuer /api/cert-hash.
    let cert_dir = std::env::var("SENTINEL_DASHBOARD_CERT_DIR")
        .unwrap_or_else(|_| "/opt/sentinel/console-cert".into());
    let cert = tls::generate(
        std::path::Path::new(&cert_dir),
        &["localhost", "127.0.0.1", "10.0.0.240"],
    )?;
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

    // Event-Stream-Subscriber (#432): NATS SENTINEL_EVENTS -> Delta-Frames in den Broadcast-Kanal
    // (ersetzt das alte 1s-Projection-Polling). Eigener Daemon-Task mit Reconnect-Backoff.
    tokio::spawn(event_sub::run_event_subscriber(state.clone()));
    tokio::spawn(event_sub::run_event_log_pusher(state.clone()));

    let app = build_app(state.clone());

    let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        &cert.cert_pem_path,
        &cert.key_pem_path,
    )
    .await?;
    tracing::info!(%http_bind, bundle = %state.config.bundle_dir, "sentinel-dashboard-backend HTTPS listening");
    // #474: ConnectInfo<SocketAddr> so the login handler can rate-limit per client IP.
    axum_server::bind_rustls(http_bind, tls_config)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await?;
    Ok(())
}
