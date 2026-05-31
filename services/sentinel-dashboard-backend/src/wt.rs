//! WebTransport/QUIC-Endpoint (#431) — server→client unidirektionale Streams, topic+msgpack+zstd-Frames.
//!
//! Muster aus `sentinel-console/src/lib.rs:113-140` (wtransport 0.6). TLS = geteiltes self-signed Cert
//! (siehe `tls`). Beim Session-Handshake wird ein kurzlebiges Einmal-Ticket (`?t=`) validiert (AC-3):
//! ohne gueltiges Ticket wird die Session NICHT akzeptiert (Drop = Reject). Ablauf pro Session:
//! `hello`-Frame -> `agent_live`-Connect-Snapshot -> kontinuierlicher Delta-Push aus dem
//! Broadcast-Kanal (#432, gespeist vom NATS-Event-Subscriber `event_sub`).

use std::path::PathBuf;

use anyhow::Context;
use wtransport::{endpoint::IncomingSession, Endpoint, Identity, ServerConfig};

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

    // AC-3: Auth am Handshake via kurzlebigem Einmal-Ticket aus der WT-URL-Query (`?t=<ticket>`).
    // Browser senden bei WebTransport KEINE Cookies → das Ticket (vom authentifizierten /api/wt-ticket
    // geholt) ist der Auth-Traeger. Nur erzwungen, wenn ein Dashboard-Key konfiguriert ist.
    if st.config.dashboard_api_key.is_some() {
        let ticket = query_param(request.path(), "t");
        if !st.sessions.consume_ticket(ticket.as_deref()) {
            tracing::info!("wt session rejected: missing/invalid wt-ticket");
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

    // Connect-Snapshot: aktueller agent_live-Stand (read-only aus projection.db) als ein topic-Frame
    // — damit die Konsole beim Verbinden sofort die echten Agents reaktiv rendert. Der kontinuierliche
    // Delta-Stream (Push bei jedem neuen Event) bleibt #432. Token-sicher (reiner Projection-Read).
    match crate::projection::agents_rows(&st.config.projection_db) {
        Ok(rows) => {
            let frame = crate::codec::encode_frame("agent_live", &serde_json::json!({ "agents": rows }))?;
            let mut snap = connection.open_uni().await?.await?;
            snap.write_all(&frame).await?;
            snap.finish().await?;
            tracing::debug!(agents = rows.len(), bytes = frame.len(), "wt agent_live snapshot pushed");
        }
        Err(e) => tracing::warn!(error = %e, "wt agent_live snapshot skipped (projection read failed)"),
    }

    // Kontinuierlicher Delta-Push (#432): nach dem Connect-Snapshot abonniert die Session den
    // Broadcast-Kanal. Jeder vom Event-Subscriber gepushte topic-Frame wird als eigener uni-Stream
    // an den Client geschrieben. `Lagged` (langsamer Client) wird uebersprungen — der naechste
    // Voll-Snapshot ist ohnehin autoritativ (client-reconcile).
    use tokio::sync::broadcast::error::RecvError;
    let mut rx = st.broadcast_tx.subscribe();
    loop {
        tokio::select! {
            frame = rx.recv() => match frame {
                Ok(bytes) => {
                    let mut stream = connection.open_uni().await?.await?;
                    stream.write_all(&bytes).await?;
                    stream.finish().await?;
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::debug!(skipped = n, "wt delta receiver lagged, next snapshot is authoritative");
                }
                Err(RecvError::Closed) => break,
            },
            reason = connection.closed() => {
                tracing::debug!(?reason, "wt session closed by peer");
                break;
            }
        }
    }
    Ok(())
}

/// Liest einen Query-Parameter aus dem WT-Request-Pfad (z.B. `/?t=abc` -> "abc").
fn query_param(path: &str, key: &str) -> Option<String> {
    let q = path.split_once('?')?.1;
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}
