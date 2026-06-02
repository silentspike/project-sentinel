//! Console data-plane push server (#439): serves the CAS console data-plane over WebTransport/QUIC.
//!
//! Protocol per session (one bidirectional stream): the client sends a `HelloManifest` (the block
//! hashes it already has); the server replies with a `Delta` (only the missing blocks). This
//! replaces the dashboard's 1s full-state poll. The protocol is generic over the stream
//! (`serve_protocol`) so it is unit-testable in-memory; `run_server` binds it to QUIC bi-streams.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sentinel_console_plane::{BlockHash, ConsolePlane, Delta, HelloManifest};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Shared, mutex-guarded console data-plane (filled by the ingest thread, read by WT sessions).
pub type SharedPlane = Arc<Mutex<ConsolePlane>>;

/// Reads a length-prefixed frame (u32-LE length + payload).
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    reader.read_exact(&mut len).await?;
    let n = u32::from_le_bytes(len) as usize;
    let mut buf = vec![0u8; n];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Writes a length-prefixed frame.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    data: &[u8],
) -> std::io::Result<()> {
    writer.write_all(&(data.len() as u32).to_le_bytes()).await?;
    writer.write_all(data).await?;
    writer.flush().await?;
    Ok(())
}

/// Serves one console session: read the client's manifest, reply with the delta of missing blocks.
/// Generic over the stream halves so it can be unit-tested over an in-memory duplex.
pub async fn serve_protocol<R, W>(
    mut recv: R,
    mut send: W,
    plane: &SharedPlane,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let hello_bytes = read_frame(&mut recv).await?;
    let hello: HelloManifest = serde_json::from_slice(&hello_bytes)?;
    let client_has: HashSet<BlockHash> = hello.have.into_iter().collect();
    // Lock only briefly (sync) to compute the delta; never held across an await.
    let delta: Delta = {
        let guard = plane
            .lock()
            .map_err(|_| anyhow::anyhow!("plane lock poisoned"))?;
        guard.delta(&client_has)
    };
    let delta_bytes = serde_json::to_vec(&delta)?;
    write_frame(&mut send, &delta_bytes).await?;
    tracing::info!(
        sent_blocks = delta.missing.len(),
        bytes = delta.transfer_bytes(),
        "console delta sent"
    );
    Ok(())
}

/// Background ingest from the Limbo event store (sync API → runs on a dedicated thread).
/// Each new event's JSON is content-addressed into the data-plane (dedup of recurring blocks).
pub fn run_ingest(plane: SharedPlane, events_db: String, poll: Duration) {
    // Read-only: die Console ist ein reiner Consumer und laeuft unter `ReadOnlyPaths=`
    // (systemd-Hardening) — ein read-write Open wuerde mit "readonly database" scheitern.
    let store = match sentinel_limbo::EventStore::open_readonly(&events_db) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, db = %events_db, "console ingest: open event store failed");
            return;
        }
    };
    let mut offset = 0i64;
    let mut ingested_since_log = 0u64;
    loop {
        match store.get_events_since_with_id(offset, 500) {
            Ok(batch) if !batch.is_empty() => {
                if let Ok(mut guard) = plane.lock() {
                    for (id, event) in batch {
                        if let Ok(bytes) = serde_json::to_vec(&event) {
                            guard.ingest(&bytes);
                        }
                        offset = id;
                        ingested_since_log += 1;
                    }
                    // Periodischer Dedup-Benchmark auf echten Event-Daten (#439 AC-1, VM-Evidence).
                    if ingested_since_log >= 200 {
                        ingested_since_log = 0;
                        tracing::info!(
                            total_blocks = guard.total_blocks(),
                            unique_blocks = guard.unique_blocks(),
                            dedup_ratio = guard.dedup_ratio(),
                            savings_ratio = guard.savings_ratio(),
                            ingested_bytes = guard.total_ingested_bytes(),
                            stored_bytes = guard.stored_bytes(),
                            "console data-plane dedup stats"
                        );
                    }
                }
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "console ingest: event read failed"),
        }
        std::thread::sleep(poll);
    }
}

/// WebTransport/QUIC push server: binds `serve_protocol` to QUIC bidirectional streams.
pub async fn run_server(plane: SharedPlane, port: u16) -> anyhow::Result<()> {
    use wtransport::{Endpoint, Identity, ServerConfig};

    let identity = Identity::self_signed(["localhost", "127.0.0.1"])?;
    let config = ServerConfig::builder()
        .with_bind_default(port)
        .with_identity(identity)
        .build();
    let server = Endpoint::server(config)?;
    tracing::info!(port, "sentinel-console WebTransport/QUIC server listening");

    loop {
        let incoming = server.accept().await;
        let plane = plane.clone();
        tokio::spawn(async move {
            let result: anyhow::Result<()> = async {
                let session_request = incoming.await?;
                let connection = session_request.accept().await?;
                let (send, recv) = connection.accept_bi().await?;
                serve_protocol(recv, send, &plane).await
            }
            .await;
            if let Err(e) = result {
                tracing::warn!(error = %e, "console session error");
            }
        });
    }
}
