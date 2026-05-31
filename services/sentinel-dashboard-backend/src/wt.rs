//! WebTransport/QUIC-Endpoint (#431) — server→client unidirektionale Streams, topic+msgpack+zstd-Frames.
//!
//! Muster aus `sentinel-console/src/lib.rs:113-140` (wtransport 0.6). TLS = geteiltes self-signed Cert
//! (siehe `tls`). Beim Session-Handshake wird das `sentinel_session`-Cookie validiert (AC-3): ohne
//! gueltige Session wird die Session NICHT akzeptiert (Drop = Reject). #431-Scope: ein `hello`-Frame
//! als End-to-End-Codec-Beweis (voller Delta-Push = #432).

use std::path::PathBuf;

use anyhow::Context;
use wtransport::{endpoint::IncomingSession, Endpoint, Identity, ServerConfig};

use crate::auth::SESSION_COOKIE;
use crate::AppState;

/// Startet den WebTransport-Server (blockiert in der Accept-Loop).
pub async fn run_server(state: AppState, cert_pem: PathBuf, key_pem: PathBuf) -> anyhow::Result<()> {
    let identity = Identity::load_pemfiles(&cert_pem, &key_pem)
        .await
        .context("load self-signed pemfiles")?;
    let port: u16 = state
        .config
        .wt_bind
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .context("wt_bind port")?;
    let config = ServerConfig::builder()
        .with_bind_default(port)
        .with_identity(identity)
        .build();
    let server = Endpoint::server(config)?;
    tracing::info!(port, "sentinel-dashboard-backend WebTransport/QUIC listening");

    loop {
        let incoming = server.accept().await;
        let st = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_session(incoming, st).await {
                tracing::warn!(error = %e, "wt session error");
            }
        });
    }
}

/// Validiert die Session (Cookie) und pusht — bei Erfolg — einen `hello`-Frame.
async fn handle_session(incoming: IncomingSession, st: AppState) -> anyhow::Result<()> {
    let request = incoming.await?;

    // AC-3: Auth am Handshake. Nur erzwungen, wenn ein Dashboard-Key konfiguriert ist (sonst offen, wie Operator-API).
    if st.config.dashboard_api_key.is_some() {
        let token = request
            .headers()
            .get("cookie")
            .and_then(|c| cookie_value_from_header(c, SESSION_COOKIE));
        if !st.sessions.validate(token.as_deref()) {
            tracing::info!("wt session rejected: missing/invalid session cookie");
            // Kein accept() => Session wird verworfen (Reject).
            return Ok(());
        }
    }

    let connection = request.accept().await?;
    let hello = crate::codec::encode_frame(
        "hello",
        &serde_json::json!({
            "server": "sentinel-dashboard-backend",
            "proto": "topic-msgpack-zstd-v1",
        }),
    )?;
    let mut uni = connection.open_uni().await?.await?;
    uni.write_all(&hello).await?;
    uni.finish().await?;
    tracing::debug!(bytes = hello.len(), "wt hello frame pushed");
    Ok(())
}

/// Liest einen Cookie-Wert aus dem rohen `Cookie`-Headerwert.
fn cookie_value_from_header(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}
