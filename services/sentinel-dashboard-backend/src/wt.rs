//! WebTransport/QUIC-Endpoint (#431/#464) — server→client unidirektionale Streams
//! (topic+msgpack+zstd-Frames) plus Event-Log-CAS ueber bidirektionale Streams.
//!
//! Der Event-Log-CAS-Bi-Stream bleibt byte-kompatibel zu #439 (u32-LE length-prefixed JSON).
//! TLS = geteiltes self-signed Cert (siehe `tls`). Beim Session-Handshake wird ein kurzlebiges
//! Einmal-Ticket (`?t=`) validiert (AC-3):
//! ohne gueltiges Ticket wird die Session NICHT akzeptiert (Drop = Reject). Ablauf pro Session:
//! `hello`-Frame -> Projection-Connect-Snapshots -> kontinuierlicher Projection-Push aus dem
//! Broadcast-Kanal (#432/#433, gespeist von `event_sub`). Event-Log-Clients synchronisieren den
//! append-only Log per CAS-Bi-Stream (`HelloManifest -> EventLogCasResponse`).

use std::path::PathBuf;

use anyhow::Context;
use sentinel_console_plane::HelloManifest;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use wtransport::{endpoint::IncomingSession, Endpoint, Identity, ServerConfig};

use crate::AppState;

/// Startet den WebTransport-Server (blockiert in der Accept-Loop).
pub async fn run_server(
    state: AppState,
    cert_pem: PathBuf,
    key_pem: PathBuf,
) -> anyhow::Result<()> {
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
    tracing::info!(
        port,
        "sentinel-dashboard-backend WebTransport/QUIC listening"
    );

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

    // Connect-Snapshots: aktuelle Projection-Staende als topic-Frames, damit die Konsole beim
    // Verbinden sofort echte Daten rendert. Token-sicher (reine Projection-Reads).
    match crate::projection::agents_rows(&st.config.projection_db) {
        Ok(rows) => {
            let frame =
                crate::codec::encode_frame("agent_live", &serde_json::json!({ "agents": rows }))?;
            let mut snap = connection.open_uni().await?.await?;
            snap.write_all(&frame).await?;
            snap.finish().await?;
            tracing::debug!(
                agents = rows.len(),
                bytes = frame.len(),
                "wt agent_live snapshot pushed"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "wt agent_live snapshot skipped (projection read failed)")
        }
    }
    match crate::projection::rooms_rows(&st.config.projection_db) {
        Ok(rows) => {
            let frame =
                crate::codec::encode_frame("room_live", &serde_json::json!({ "rooms": rows }))?;
            let mut snap = connection.open_uni().await?.await?;
            snap.write_all(&frame).await?;
            snap.finish().await?;
            tracing::debug!(
                rooms = rows.len(),
                bytes = frame.len(),
                "wt room_live snapshot pushed"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "wt room_live snapshot skipped (projection read failed)")
        }
    }
    match crate::projection::metrics_row(&st.config.projection_db) {
        Ok(kpi) => {
            let frame = crate::codec::encode_frame("kpi", &serde_json::json!({ "kpi": kpi }))?;
            let mut snap = connection.open_uni().await?.await?;
            snap.write_all(&frame).await?;
            snap.finish().await?;
            tracing::debug!(bytes = frame.len(), "wt kpi snapshot pushed");
        }
        Err(e) => tracing::warn!(error = %e, "wt kpi snapshot skipped (projection read failed)"),
    }
    // Kontinuierlicher Delta-Push (#432): nach dem Connect-Snapshot abonniert die Session den
    // Broadcast-Kanal. Jeder vom Event-Subscriber gepushte topic-Frame wird als eigener uni-Stream
    // an den Client geschrieben. Parallel akzeptiert dieselbe Session Event-Log-CAS-Bi-Streams.
    // `Lagged` (langsamer Client) wird uebersprungen — der naechste Voll-Snapshot ist ohnehin
    // autoritativ (client-reconcile).
    use tokio::sync::broadcast::error::RecvError;
    let mut rx = st.broadcast_tx.subscribe();
    loop {
        tokio::select! {
            stream = connection.accept_bi() => match stream {
                Ok((send, recv)) => {
                    let st = st.clone();
                    tokio::spawn(async move {
                        if let Err(e) = serve_event_log_cas(recv, send, st).await {
                            tracing::warn!(error = %e, "event_log CAS bi-stream error");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "wt accept_bi failed");
                    break;
                }
            },
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

/// Reads a length-prefixed frame (u32-LE length + payload), byte-compatible with #439.
pub(crate) async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    reader.read_exact(&mut len).await?;
    let n = u32::from_le_bytes(len) as usize;
    let mut buf = vec![0u8; n];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Writes a length-prefixed frame (u32-LE length + payload), byte-compatible with #439.
pub(crate) async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    data: &[u8],
) -> std::io::Result<()> {
    writer.write_all(&(data.len() as u32).to_le_bytes()).await?;
    writer.write_all(data).await?;
    writer.flush().await?;
    Ok(())
}

async fn serve_event_log_cas(
    mut recv: wtransport::RecvStream,
    mut send: wtransport::SendStream,
    st: AppState,
) -> anyhow::Result<()> {
    let hello_bytes = read_frame(&mut recv)
        .await
        .context("read event_log CAS hello")?;
    let hello: HelloManifest =
        serde_json::from_slice(&hello_bytes).context("decode event_log CAS hello")?;

    if let Err(e) = crate::event_sub::refresh_event_log_cas(&st) {
        tracing::warn!(
            error = %e,
            path = %st.config.events_db,
            "event_log CAS refresh degraded; serving current plane"
        );
    }
    let response = {
        let guard = st
            .event_cas
            .lock()
            .map_err(|_| anyhow::anyhow!("event_log CAS lock poisoned"))?;
        guard.response_for(hello)
    };
    let response_bytes = serde_json::to_vec(&response).context("encode event_log CAS response")?;
    write_frame(&mut send, &response_bytes)
        .await
        .context("write event_log CAS response")?;
    send.finish().await?;
    tracing::info!(
        sent_blocks = response.delta.missing.len(),
        bytes = response.stats.delta_transfer_bytes,
        full_state_bytes = response.stats.full_state_bytes,
        event_count = response.stats.event_count,
        dedup_ratio = response.stats.dedup_ratio,
        "event_log CAS delta sent"
    );
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
