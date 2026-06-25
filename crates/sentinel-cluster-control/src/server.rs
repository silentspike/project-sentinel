//! The QUIC control server: accept cert-pinned peer connections + bidi RPC streams,
//! dedup per `idempotency_key`, dispatch to a [`ControlHandler`].

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;

use quinn::Endpoint;
use tracing::{debug, info, warn};

use crate::cert::{CertFingerprint, NodeCertificate};
use crate::envelope::{decode_frame, encode_frame, ControlEnvelope, ControlReply, MAX_FRAME_BYTES};
use crate::handler::ControlHandler;
use crate::idempotency::IdempotencyCache;
use crate::tls::{peer_fingerprint, quic_server_config};

/// A running control server. Holds the quinn endpoint; the accept loop runs on a
/// spawned task for the server's lifetime.
pub struct ControlServer {
    endpoint: Endpoint,
    local_addr: SocketAddr,
}

impl ControlServer {
    /// Bind a QUIC control server on `bind_addr` with `node`'s identity. Only peers
    /// whose cert fingerprint is in `pinned_peers` are served (V10); requests are
    /// deduplicated per `idempotency_key` and dispatched to `handler`.
    pub fn bind<H: ControlHandler + 'static>(
        bind_addr: SocketAddr,
        node: &NodeCertificate,
        pinned_peers: HashSet<CertFingerprint>,
        handler: Arc<H>,
    ) -> anyhow::Result<Self> {
        let server_cfg = quic_server_config(node)?;
        let endpoint = Endpoint::server(server_cfg, bind_addr)?;
        let local_addr = endpoint.local_addr()?;
        let ep = endpoint.clone();
        let pins = Arc::new(pinned_peers);
        let cache: Arc<IdempotencyCache<ControlReply>> = Arc::new(IdempotencyCache::new());
        tokio::spawn(async move {
            while let Some(incoming) = ep.accept().await {
                let pins = pins.clone();
                let handler = handler.clone();
                let cache = cache.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_connection(incoming, pins, handler, cache).await {
                        warn!(error = %e, "control connection ended with error");
                    }
                });
            }
        });
        info!(%local_addr, "Cluster 12: control server listening");
        Ok(Self {
            endpoint,
            local_addr,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stop accepting + close existing connections.
    pub fn close(&self) {
        self.endpoint.close(0u32.into(), b"shutdown");
    }
}

async fn serve_connection<H: ControlHandler + 'static>(
    incoming: quinn::Incoming,
    pins: Arc<HashSet<CertFingerprint>>,
    handler: Arc<H>,
    cache: Arc<IdempotencyCache<ControlReply>>,
) -> anyhow::Result<()> {
    let conn = incoming.await?;
    // V10: enforce the cert pin post-handshake. The TLS layer accepted any cert
    // identity but verified key ownership; serving requires the pin to match.
    let fp = peer_fingerprint(&conn)?;
    if !pins.contains(&fp) {
        conn.close(1u32.into(), b"unpinned peer");
        anyhow::bail!("rejected unpinned peer cert {fp}");
    }
    debug!(peer = %fp, "control peer accepted (pinned)");
    loop {
        match conn.accept_bi().await {
            Ok((send, recv)) => {
                let handler = handler.clone();
                let cache = cache.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_request(send, recv, handler, cache).await {
                        debug!(error = %e, "control request stream error");
                    }
                });
            }
            Err(quinn::ConnectionError::ApplicationClosed(_))
            | Err(quinn::ConnectionError::LocallyClosed) => break,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

async fn serve_request<H: ControlHandler>(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    handler: Arc<H>,
    cache: Arc<IdempotencyCache<ControlReply>>,
) -> anyhow::Result<()> {
    let frame = recv.read_to_end(MAX_FRAME_BYTES + 4).await?;
    let env: ControlEnvelope = decode_frame(&frame)?;
    let method = env.request.method_name();
    let (reply, cached) = cache.get_or_compute(&env.idempotency_key, || {
        let response = handler.handle(&env.request);
        ControlReply {
            request_id: env.request_id,
            response,
        }
    });
    debug!(method, key = %env.idempotency_key, cached, "control request handled");
    let out = encode_frame(&reply)?;
    send.write_all(&out).await?;
    send.finish()?;
    // Keep the stream open until the peer has acknowledged the response.
    let _ = send.stopped().await;
    Ok(())
}
