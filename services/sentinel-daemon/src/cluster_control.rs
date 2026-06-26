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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sentinel_cluster_control::{
    BlockMapGossipHandler, CertFingerprint, ControlClient, ControlEnvelope, ControlReply,
    ControlRequest, ControlServer, NodeCertificate, StubHandler,
};
use sentinel_common::cluster::ControlPeer;
use sentinel_common::{BlockMap, HolderAdvertisement, NodeId};
use sentinel_fs::cas::CasStore;
use sentinel_fs::cas_holder::CasHolderState;
use sentinel_redb::ClusterMetaStore;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::owner_handler::OwnerControlHandler;

/// A running control-plane handle: the server (kept alive for its lifetime) + a
/// client + the resolved pinned peers, for outbound RPCs (the live RefQuery/PinQuery
/// AC and later #496/#499).
pub struct ClusterControl {
    _server: ControlServer,
    client: ControlClient,
    peers: Vec<ResolvedPeer>,
    my_fingerprint: CertFingerprint,
    /// #498 block map populated by inbound holder gossip (the server merges into it);
    /// the daemon read paths (#498 PR2/PR3) resolve remote holders from this handle.
    block_map: Arc<Mutex<BlockMap>>,
}

struct ResolvedPeer {
    alias: String,
    addr: SocketAddr,
    fingerprint: CertFingerprint,
}

impl ClusterControl {
    /// Start the control stream: persist/load the node cert under `cert_dir`, spawn the
    /// server bound to `bind`, and resolve the pinned peers. `owner_meta` is the durable
    /// cluster-meta store (ADR-3): when present the server runs the real `#496`
    /// [`OwnerControlHandler`] (persist `OwnerCommit` + update the registry); the
    /// Phase-3a0 `StubHandler` remains only as a fallback if the store failed to open.
    pub fn start(
        bind: &str,
        cert_dir: &Path,
        node_alias: &str,
        peers: &[ControlPeer],
        owner_meta: Option<Arc<ClusterMetaStore>>,
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
        // The real #496 owner handler when the durable meta store is available; the
        // Phase-3a0 stub only as a fallback (meta store failed to open). Both are wrapped
        // in the #498 BlockMapGossipHandler so inbound holder gossip merges into a shared
        // block map while every other RPC still reaches the owner/GC handler.
        let block_map = Arc::new(Mutex::new(BlockMap::new()));
        let server = match owner_meta {
            Some(meta) => ControlServer::bind(
                bind_addr,
                &node,
                pins,
                Arc::new(BlockMapGossipHandler::new(
                    Arc::clone(&block_map),
                    OwnerControlHandler::new(meta),
                )),
            )?,
            None => ControlServer::bind(
                bind_addr,
                &node,
                pins,
                Arc::new(BlockMapGossipHandler::new(
                    Arc::clone(&block_map),
                    StubHandler,
                )),
            )?,
        };
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
            block_map,
        })
    }

    /// This node's control cert fingerprint (configure peers' pins from it, V10).
    pub fn fingerprint(&self) -> CertFingerprint {
        self.my_fingerprint
    }

    /// The #498 block map populated by inbound holder gossip. The daemon read paths
    /// (#498 PR2/PR3) resolve a remote block's holders from this shared handle.
    pub fn block_map(&self) -> Arc<Mutex<BlockMap>> {
        Arc::clone(&self.block_map)
    }

    /// #498 block-map gossip: push `advertisements` to **every** pinned peer (fan-out).
    /// Best-effort — an unreachable peer is logged and skipped; the periodic republish +
    /// the conflict-free merge (V16) recover the drift. `round` makes each republish a
    /// fresh idempotency key (a retry within a round is deduped; the next round is not).
    /// Returns how many peers accepted the batch. Metadata only — no bytes here (AC-4).
    pub async fn broadcast_holders(
        &self,
        advertisements: Vec<HolderAdvertisement>,
        round: u64,
    ) -> usize {
        if advertisements.is_empty() || self.peers.is_empty() {
            return 0;
        }
        let mut delivered = 0;
        for peer in &self.peers {
            let env = ControlEnvelope::new(
                format!("advertise-holders-r{round}"),
                ControlRequest::AdvertiseHolders {
                    advertisements: advertisements.clone(),
                },
            );
            match self.client.rpc(peer.addr, peer.fingerprint, &env).await {
                Ok(_) => delivered += 1,
                Err(e) => warn!(
                    peer = %peer.alias,
                    error = %e,
                    "#498 block-map gossip to peer failed (will retry next republish)"
                ),
            }
        }
        delivered
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

/// #498 periodic block-map gossip republish (cluster mode only — single-node prod never
/// spawns it, Strangler S4). Each round: rebuild this node's CAS holder inventory from
/// the durable store (reflects new/deleted blobs) and broadcast it to the pinned peers
/// in **bounded pages** (V25 — never the whole inventory in one message). The receiver
/// merges conflict-free (V16). Metadata only — bytes never travel here (AC-4).
pub async fn run_cas_gossip_republish(
    cluster_control: Arc<ClusterControl>,
    data_dir: PathBuf,
    node_id: NodeId,
    boot_id: Uuid,
    interval: Duration,
    page_limit: usize,
) {
    let mut holder_state = CasHolderState::new(node_id, boot_id, 0);
    let mut ticker = tokio::time::interval(interval);
    let mut round = 0u64;
    info!(%node_id, "Cluster 12: #498 CAS block-map gossip republish started");
    loop {
        ticker.tick().await;
        round = round.saturating_add(1);

        // Cheap, zero-state CasStore::open; reflects new/deleted blobs each round.
        let store = match CasStore::open(&data_dir) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "#498 gossip: CAS open failed this round");
                continue;
            }
        };
        if let Err(e) = holder_state.rebuild(&store) {
            warn!(error = %e, "#498 gossip: CAS inventory rebuild failed this round");
            continue;
        }

        // Broadcast in bounded pages (V25). A bounded page also caps the control frame.
        let mut cursor: Option<sentinel_common::BlockRef> = None;
        let mut advertised = 0usize;
        loop {
            let (advs, next) = holder_state.advertisement_page(cursor.as_ref(), page_limit);
            if advs.is_empty() {
                break;
            }
            advertised += advs.len();
            cluster_control.broadcast_holders(advs, round).await;
            match next {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        debug!(
            round,
            advertised, "#498 CAS gossip republish round complete"
        );
    }
}
