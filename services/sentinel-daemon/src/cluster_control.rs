//! Cluster 12 control-stream wiring (#569, Phase 3a0 part 3): spawn the QUIC control
//! server and hold a client for outbound RPCs.
//!
//! The node's control cert is **persisted** (stable SHA-256 fingerprint across
//! restarts — otherwise a peer's pin would break on every reboot, V10). Pinned peers
//! come from `[daemon.cluster].control_peers` (exchanged out-of-band, single trust
//! domain V21). The server runs a `StubHandler` for now; the real owner/GC handlers
//! land with #496/#499. Started only when `[daemon.cluster].control_bind` is set.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use sentinel_cluster_control::{
    CertFingerprint, ControlClient, ControlEnvelope, ControlReply, ControlRequest, ControlServer,
    NodeCertificate, StubHandler,
};
use sentinel_common::cluster::ControlPeer;
use tracing::info;

/// A running control-plane handle: the server (kept alive for its lifetime) + a
/// client + the resolved pinned peers, for outbound RPCs (the live RefQuery/PinQuery
/// AC and later #496/#499).
pub struct ClusterControl {
    _server: ControlServer,
    client: ControlClient,
    peers: Vec<ResolvedPeer>,
    my_fingerprint: CertFingerprint,
}

struct ResolvedPeer {
    alias: String,
    addr: SocketAddr,
    fingerprint: CertFingerprint,
}

impl ClusterControl {
    /// Start the control stream: persist/load the node cert under `cert_dir`, spawn
    /// the server bound to `bind`, and resolve the pinned peers.
    pub fn start(
        bind: &str,
        cert_dir: &Path,
        node_alias: &str,
        peers: &[ControlPeer],
    ) -> anyhow::Result<Self> {
        let cert_path = cert_dir.join("control-node-cert.der");
        let key_path = cert_dir.join("control-node-key.der");
        let node = NodeCertificate::load_or_generate(&cert_path, &key_path, node_alias)?;
        let my_fingerprint = node.fingerprint();

        let mut resolved = Vec::with_capacity(peers.len());
        let mut pins: HashSet<CertFingerprint> = HashSet::new();
        for p in peers {
            let addr: SocketAddr = p
                .addr
                .parse()
                .map_err(|e| anyhow::anyhow!("control peer {} addr {}: {e}", p.alias, p.addr))?;
            let fingerprint = CertFingerprint::from_hex(&p.cert_fingerprint).ok_or_else(|| {
                anyhow::anyhow!("control peer {} has a malformed cert fingerprint", p.alias)
            })?;
            pins.insert(fingerprint);
            resolved.push(ResolvedPeer {
                alias: p.alias.clone(),
                addr,
                fingerprint,
            });
        }

        let bind_addr: SocketAddr = bind
            .parse()
            .map_err(|e| anyhow::anyhow!("control_bind {bind}: {e}"))?;
        let server = ControlServer::bind(bind_addr, &node, pins, Arc::new(StubHandler))?;
        let client = ControlClient::new(&node)?;
        info!(
            %bind_addr,
            fingerprint = %my_fingerprint,
            peers = resolved.len(),
            "Cluster 12: control stream started"
        );
        Ok(Self {
            _server: server,
            client,
            peers: resolved,
            my_fingerprint,
        })
    }

    /// This node's control cert fingerprint (configure peers' pins from it, V10).
    pub fn fingerprint(&self) -> CertFingerprint {
        self.my_fingerprint
    }

    /// Send a control RPC to the pinned peer `peer_alias` and return its reply. Used
    /// by the operator endpoint (the live RefQuery/PinQuery AC) and later by #496/#499.
    pub async fn rpc(
        &self,
        peer_alias: &str,
        idempotency_key: &str,
        request: ControlRequest,
    ) -> anyhow::Result<ControlReply> {
        let peer = self
            .peers
            .iter()
            .find(|p| p.alias == peer_alias)
            .ok_or_else(|| anyhow::anyhow!("unknown control peer {peer_alias}"))?;
        let env = ControlEnvelope::new(idempotency_key, request);
        self.client.rpc(peer.addr, peer.fingerprint, &env).await
    }
}
