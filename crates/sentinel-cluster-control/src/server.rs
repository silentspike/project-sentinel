//! The QUIC control server: accept cert-pinned peer connections + bidi RPC streams,
//! dedup per `idempotency_key`, dispatch to a [`ControlHandler`].

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use quinn::Endpoint;
use tracing::{debug, info, warn};

use crate::cert::{CertFingerprint, NodeCertificate};
use crate::envelope::{decode_frame, encode_frame, ControlEnvelope, ControlReply, MAX_FRAME_BYTES};
use crate::handler::ControlHandler;
use crate::idempotency::IdempotencyCache;
use crate::tls::{peer_fingerprint, quic_server_config};

use sentinel_common::NodeId;

/// Certificate-authenticated identity supplied to every control handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthenticatedPeer {
    pub fingerprint: CertFingerprint,
    pub node_id: NodeId,
}

/// Dynamic pinned-peer registry shared by the control and block-pull servers.
/// Both directions are unique: one certificate maps to one NodeId and one NodeId
/// maps to one certificate until the explicit rotation lifecycle is implemented.
#[derive(Clone, Default)]
pub struct PeerRegistry {
    inner: Arc<RwLock<HashMap<CertFingerprint, NodeId>>>,
}

impl PeerRegistry {
    pub fn new(
        bindings: impl IntoIterator<Item = (CertFingerprint, NodeId)>,
    ) -> anyhow::Result<Self> {
        let registry = Self::default();
        for (fingerprint, node_id) in bindings {
            registry.authorize(fingerprint, node_id)?;
        }
        Ok(registry)
    }

    pub fn authorize(&self, fingerprint: CertFingerprint, node_id: NodeId) -> anyhow::Result<()> {
        let mut bindings = self.inner.write().expect("peer registry lock");
        if let Some(existing) = bindings.get(&fingerprint) {
            if *existing != node_id {
                anyhow::bail!("certificate {fingerprint} is already bound to node {existing}");
            }
            return Ok(());
        }
        if let Some((existing_fingerprint, _)) = bindings
            .iter()
            .find(|(_, existing_node_id)| **existing_node_id == node_id)
        {
            anyhow::bail!("node {node_id} is already bound to certificate {existing_fingerprint}");
        }
        bindings.insert(fingerprint, node_id);
        Ok(())
    }

    pub fn revoke(&self, node_id: NodeId) {
        self.inner
            .write()
            .expect("peer registry lock")
            .retain(|_, bound_node_id| *bound_node_id != node_id);
    }

    pub fn resolve(&self, fingerprint: CertFingerprint) -> Option<AuthenticatedPeer> {
        self.inner
            .read()
            .expect("peer registry lock")
            .get(&fingerprint)
            .copied()
            .map(|node_id| AuthenticatedPeer {
                fingerprint,
                node_id,
            })
    }

    pub fn fingerprints(&self) -> Vec<CertFingerprint> {
        self.inner
            .read()
            .expect("peer registry lock")
            .keys()
            .copied()
            .collect()
    }
}

/// A running control server. Holds the quinn endpoint; the accept loop runs on a
/// spawned task for the server's lifetime.
pub struct ControlServer {
    endpoint: Endpoint,
    local_addr: SocketAddr,
}

impl ControlServer {
    /// Bind a QUIC control server on `bind_addr` with `node`'s identity. Only peers
    /// whose cert fingerprint resolves through `peers` are served (V10). The resolved
    /// NodeId accompanies every dispatched request; cacheable requests are atomically
    /// deduplicated per `idempotency_key` within this daemon process.
    pub fn bind<H: ControlHandler + 'static>(
        bind_addr: SocketAddr,
        node: &NodeCertificate,
        peers: PeerRegistry,
        handler: Arc<H>,
    ) -> anyhow::Result<Self> {
        let server_cfg = quic_server_config(node)?;
        let endpoint = Endpoint::server(server_cfg, bind_addr)?;
        let local_addr = endpoint.local_addr()?;
        let ep = endpoint.clone();
        let cache: Arc<IdempotencyCache<ControlReply>> = Arc::new(IdempotencyCache::new());
        tokio::spawn(async move {
            while let Some(incoming) = ep.accept().await {
                let peers = peers.clone();
                let handler = handler.clone();
                let cache = cache.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_connection(incoming, peers, handler, cache).await {
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
    peers: PeerRegistry,
    handler: Arc<H>,
    cache: Arc<IdempotencyCache<ControlReply>>,
) -> anyhow::Result<()> {
    let conn = incoming.await?;
    // V10: enforce the cert pin post-handshake. The TLS layer accepted any cert
    // identity but verified key ownership; serving requires the pin to match.
    let fp = peer_fingerprint(&conn)?;
    let Some(peer) = peers.resolve(fp) else {
        conn.close(1u32.into(), b"unpinned peer");
        anyhow::bail!("rejected unpinned peer cert {fp}");
    };
    debug!(peer = %fp, "control peer accepted (pinned)");
    loop {
        match conn.accept_bi().await {
            Ok((send, recv)) => {
                let handler = handler.clone();
                let cache = cache.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_request(send, recv, peer, handler, cache).await {
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
    peer: AuthenticatedPeer,
    handler: Arc<H>,
    cache: Arc<IdempotencyCache<ControlReply>>,
) -> anyhow::Result<()> {
    let frame = recv.read_to_end(MAX_FRAME_BYTES + 4).await?;
    let env: ControlEnvelope = decode_frame(&frame)?;
    let method = env.request.method_name();
    let (reply, cached) = if env.request.cache_response() {
        cache.get_or_compute(&env.idempotency_key, || ControlReply {
            request_id: env.request_id,
            response: handler.handle(peer, &env.request),
        })
    } else {
        (
            ControlReply {
                request_id: env.request_id,
                response: handler.handle(peer, &env.request),
            },
            false,
        )
    };
    debug!(method, key = %env.idempotency_key, cached, "control request handled");
    let out = encode_frame(&reply)?;
    send.write_all(&out).await?;
    send.finish()?;
    // Keep the stream open until the peer has acknowledged the response.
    let _ = send.stopped().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_registry_enforces_one_to_one_certificate_node_binding() {
        let registry = PeerRegistry::default();
        let first_node = NodeId::new();
        let second_node = NodeId::new();
        let first_cert = CertFingerprint([1; 32]);
        let second_cert = CertFingerprint([2; 32]);

        registry.authorize(first_cert, first_node).unwrap();
        registry.authorize(first_cert, first_node).unwrap();
        assert_eq!(registry.resolve(first_cert).unwrap().node_id, first_node);
        assert!(registry.authorize(first_cert, second_node).is_err());
        assert!(registry.authorize(second_cert, first_node).is_err());

        registry.revoke(first_node);
        assert!(registry.resolve(first_cert).is_none());
        registry.authorize(second_cert, first_node).unwrap();
    }
}
