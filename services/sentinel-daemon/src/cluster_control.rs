//! Cluster 12 control-stream wiring (#569, Phase 3a0 part 3): spawn the QUIC control
//! server and hold a client for outbound RPCs.
//!
//! The node's control cert is **persisted** (stable SHA-256 fingerprint across
//! restarts — otherwise a peer's pin would break on every reboot, V10). Initial peers
//! come from `[daemon.cluster].control_peers`; ProvisionNode adds reciprocal peers to
//! a durable dynamic registry. Every fingerprint is bound one-to-one to a `NodeId`
//! before the owner, membership, block-map, or block-pull handlers run. Started only
//! when `[daemon.cluster].control_bind` is set.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use sentinel_cluster_control::{
    BlockMapGossipHandler, BlockProvider, BlockPullClient, BlockPullServer, CertFingerprint,
    ControlClient, ControlEnvelope, ControlReply, ControlRequest, ControlResponse, ControlServer,
    NodeCertificate, PeerRegistry, StubHandler,
};
use sentinel_common::cluster::ControlPeer;
use sentinel_common::{
    BlockMap, BlockNamespace, BlockRef, HashAlgorithm, Heartbeat, HolderAdvertisement, NodeId,
};
use sentinel_fs::artifact::ChunkHash;
use sentinel_fs::block_resolver::{BlockResolver, BlockStore, RemotePull};
use sentinel_fs::cas::CasStore;
use sentinel_fs::cas_holder::CasHolderState;
use sentinel_redb::ClusterMetaStore;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::cluster_membership::{MembershipRuntime, QuicMembershipHandler};
use crate::owner_handler::OwnerControlHandler;

const MEMBERSHIP_RPC_TIMEOUT: Duration = Duration::from_millis(750);
const DYNAMIC_PEERS_FILE: &str = "control-peers.json";

/// A running control-plane handle: the server (kept alive for its lifetime) + a
/// client + the resolved pinned peers, for outbound RPCs (the live RefQuery/PinQuery
/// AC and later #496/#499).
pub struct ClusterControl {
    _server: ControlServer,
    client: ControlClient,
    peers: RwLock<BTreeMap<NodeId, ResolvedPeer>>,
    peer_registry: PeerRegistry,
    peer_store_path: PathBuf,
    my_fingerprint: CertFingerprint,
    /// #498 block map populated by inbound holder gossip (the server merges into it);
    /// the daemon read paths (#498 PR2/PR3) resolve remote holders from this handle.
    block_map: Arc<Mutex<BlockMap>>,
    /// #498 4b block-pull server (serves local blobs by hash) — `None` if its bind failed
    /// (kept alive for its lifetime; the control stream still runs).
    _pull_server: Option<BlockPullServer>,
    /// #498 4b block-pull client for outbound pull-by-hash.
    pull_client: BlockPullClient,
}

#[derive(Clone, PartialEq, Eq)]
struct ResolvedPeer {
    node_id: NodeId,
    alias: String,
    addr: SocketAddr,
    /// Block-pull endpoint = control addr with port+1 (#498 4b).
    pull_addr: SocketAddr,
    fingerprint: CertFingerprint,
}

impl ResolvedPeer {
    fn from_config(peer: &ControlPeer) -> anyhow::Result<Self> {
        let addr: SocketAddr = peer
            .addr
            .parse()
            .map_err(|e| anyhow::anyhow!("control peer {} addr {}: {e}", peer.alias, peer.addr))?;
        let fingerprint = CertFingerprint::from_hex(&peer.cert_fingerprint).ok_or_else(|| {
            anyhow::anyhow!(
                "control peer {} has a malformed cert fingerprint",
                peer.alias
            )
        })?;
        Ok(Self {
            node_id: peer.node_id,
            alias: peer.alias.clone(),
            addr,
            pull_addr: pull_addr_of(addr),
            fingerprint,
        })
    }

    fn to_config(&self) -> ControlPeer {
        ControlPeer {
            node_id: self.node_id,
            alias: self.alias.clone(),
            addr: self.addr.to_string(),
            cert_fingerprint: self.fingerprint.to_hex(),
        }
    }
}

fn load_persisted_peers(path: &Path) -> anyhow::Result<Vec<ControlPeer>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(anyhow::anyhow!("read {}: {e}", path.display())),
    }
}

fn persist_peers(path: &Path, peers: &BTreeMap<NodeId, ResolvedPeer>) -> anyhow::Result<()> {
    let configs: Vec<_> = peers.values().map(ResolvedPeer::to_config).collect();
    let bytes = serde_json::to_vec_pretty(&configs)?;
    let tmp = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp)
        .map_err(|e| anyhow::anyhow!("create {}: {e}", tmp.display()))?;
    use std::io::Write;
    file.write_all(&bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// The block-pull endpoint for a control endpoint: same host, control port + 1 (#498 4b).
fn pull_addr_of(control: SocketAddr) -> SocketAddr {
    SocketAddr::new(control.ip(), control.port().wrapping_add(1))
}

/// Extract the 32-byte SHA-256 hash from a Blob/SHA-256 `BlockRef`; `None` for any other
/// namespace/algorithm (the chunk plane is a later step).
fn blob_sha256_hash(block_ref: &BlockRef) -> Option<[u8; 32]> {
    if block_ref.namespace() == BlockNamespace::Blob
        && block_ref.algorithm() == HashAlgorithm::Sha256
    {
        block_ref.digest().try_into().ok()
    } else {
        None
    }
}

/// A `BlockProvider` over this node's CAS: serves a blob's raw on-disk encoded bytes by
/// content hash (V10 — never a path), for the block-pull server.
struct CasBlockProvider {
    cas: CasStore,
}

impl BlockProvider for CasBlockProvider {
    fn encoded_blob(&self, block_ref: &BlockRef) -> Option<Vec<u8>> {
        let hash = blob_sha256_hash(block_ref)?;
        self.cas.encoded_blob(&hash)
    }
}

/// A `BlockStore` (local existence check) over this node's CAS dir, for the #498 4c
/// resolver. A fresh `CasStore` is a stateless dir wrapper, so this avoids an Arc cycle
/// with the CAS the resolver is wired into.
struct CasBlockStore {
    data_dir: PathBuf,
}

impl BlockStore for CasBlockStore {
    fn has_blob(&self, hash: &[u8; 32]) -> bool {
        CasStore::open(&self.data_dir).is_ok_and(|c| c.contains(hash))
    }
    fn has_chunk(&self, _hash: &ChunkHash) -> bool {
        false // the chunk plane is not in the daemon's FUSE read path (Track B / #548)
    }
}

/// The #498 4c `RemotePull` for the daemon: bridges the (sync) read path to the (async)
/// block-pull. `pull_blob` resolves the `BlockRef` (size) from the block map by hash, then
/// `block_on`s [`ClusterControl::resolve_block`] (pull + verify + durable store). The CAS
/// read path is the FUSE/sync side, so `block_on` does not run inside a tokio worker.
struct DaemonRemotePull {
    cluster: std::sync::Weak<ClusterControl>,
    data_dir: PathBuf,
    handle: tokio::runtime::Handle,
}

impl RemotePull for DaemonRemotePull {
    fn pull_blob(&self, hash: &[u8; 32]) -> bool {
        let Some(cc) = self.cluster.upgrade() else {
            return false;
        };
        let block_ref = {
            let map = cc.block_map.lock().unwrap_or_else(|p| p.into_inner());
            map.find_blob_ref(hash)
        };
        let Some(block_ref) = block_ref else {
            return false; // no known holder for this blob digest
        };
        let Ok(cas) = CasStore::open(&self.data_dir) else {
            return false;
        };
        self.handle.block_on(cc.resolve_block(&cas, &block_ref))
    }
    fn pull_chunk(&self, _hash: &ChunkHash) -> bool {
        false // chunk cross-node pull is wired when the chunk plane goes cross-node (#548)
    }
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
        cluster_id: Uuid,
        local_node_id: NodeId,
        peers: &[ControlPeer],
        membership: Arc<MembershipRuntime>,
        owner_meta: Option<Arc<ClusterMetaStore>>,
    ) -> anyhow::Result<Self> {
        let cert_path = cert_dir.join("control-node-cert.der");
        let key_path = cert_dir.join("control-node-key.der");
        let node = NodeCertificate::load_or_generate(&cert_path, &key_path, node_alias)?;
        let my_fingerprint = node.fingerprint();

        let peer_store_path = cert_dir.join(DYNAMIC_PEERS_FILE);
        let mut resolved = BTreeMap::new();
        for peer in peers
            .iter()
            .cloned()
            .chain(load_persisted_peers(&peer_store_path)?)
        {
            let peer = ResolvedPeer::from_config(&peer)?;
            if let Some(existing) = resolved.insert(peer.node_id, peer.clone()) {
                if existing != peer {
                    anyhow::bail!(
                        "conflicting control peer definitions for node {}",
                        peer.node_id
                    );
                }
            }
        }
        let peer_registry = PeerRegistry::new(
            resolved
                .values()
                .map(|peer| (peer.fingerprint, peer.node_id)),
        )?;

        let bind_addr: SocketAddr = bind
            .parse()
            .map_err(|e| anyhow::anyhow!("control_bind {bind}: {e}"))?;
        // The block-pull server reuses the same pinned peers + cert (V10), on port+1.
        // The real #496 owner handler when the durable meta store is available; the
        // Phase-3a0 stub only as a fallback (meta store failed to open). Both are wrapped
        // in the #498 BlockMapGossipHandler so inbound holder gossip merges into a shared
        // block map while every other RPC still reaches the owner/GC handler.
        let block_map = Arc::new(Mutex::new(BlockMap::new()));
        let server = match owner_meta {
            Some(meta) => ControlServer::bind(
                bind_addr,
                &node,
                peer_registry.clone(),
                Arc::new(QuicMembershipHandler::new(
                    cluster_id,
                    local_node_id,
                    Arc::clone(&membership),
                    BlockMapGossipHandler::new(
                        Arc::clone(&block_map),
                        OwnerControlHandler::new(meta),
                    ),
                )),
            )?,
            None => ControlServer::bind(
                bind_addr,
                &node,
                peer_registry.clone(),
                Arc::new(QuicMembershipHandler::new(
                    cluster_id,
                    local_node_id,
                    Arc::clone(&membership),
                    BlockMapGossipHandler::new(Arc::clone(&block_map), StubHandler),
                )),
            )?,
        };
        let client = ControlClient::new(&node)?;

        // #498 4b: bind the block-pull server (serves local CAS blobs by hash, V10) on
        // port+1, backed by this node's CAS. A bind failure is logged, not fatal — the
        // control stream + gossip still run.
        let pull_bind = pull_addr_of(bind_addr);
        let pull_server = match CasStore::open(cert_dir) {
            Ok(cas) => {
                let provider = Arc::new(CasBlockProvider { cas });
                match BlockPullServer::bind(pull_bind, &node, peer_registry.clone(), provider) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        warn!(error = %e, "#498 block-pull server failed to bind");
                        None
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "#498 block-pull: CAS open failed; pull server not started");
                None
            }
        };
        let pull_client = BlockPullClient::new(&node)?;

        info!(
            %bind_addr,
            fingerprint = %my_fingerprint,
            peers = resolved.len(),
            pull_server = pull_server.is_some(),
            "Cluster 12: control stream started"
        );
        Ok(Self {
            _server: server,
            client,
            peers: RwLock::new(resolved),
            peer_registry,
            peer_store_path,
            my_fingerprint,
            block_map,
            _pull_server: pull_server,
            pull_client,
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

    fn peer_snapshot(&self) -> Vec<ResolvedPeer> {
        self.peers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect()
    }

    /// Authorize and persist a newly provisioned peer without restarting the seed.
    pub fn add_peer(&self, peer: ControlPeer) -> anyhow::Result<()> {
        let peer = ResolvedPeer::from_config(&peer)?;
        let mut peers = self
            .peers
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = peers.get(&peer.node_id) {
            if existing == &peer {
                return Ok(());
            }
            anyhow::bail!(
                "node {} already has a different control peer binding",
                peer.node_id
            );
        }
        self.peer_registry
            .authorize(peer.fingerprint, peer.node_id)?;
        peers.insert(peer.node_id, peer.clone());
        if let Err(error) = persist_peers(&self.peer_store_path, &peers) {
            peers.remove(&peer.node_id);
            self.peer_registry.revoke(peer.node_id);
            return Err(error);
        }
        Ok(())
    }

    /// Remove a failed provisioning peer from both live trust and durable state.
    pub fn remove_peer(&self, node_id: NodeId) -> anyhow::Result<()> {
        let mut peers = self
            .peers
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(removed) = peers.remove(&node_id) else {
            return Ok(());
        };
        if let Err(error) = persist_peers(&self.peer_store_path, &peers) {
            peers.insert(node_id, removed);
            return Err(error);
        }
        self.peer_registry.revoke(node_id);
        Ok(())
    }

    /// Send one liveness heartbeat concurrently to every explicitly configured,
    /// cert-pinned peer. Each request is bounded so an unreachable peer cannot stall
    /// the receiver-local membership TTL ticker.
    pub async fn broadcast_membership_heartbeat(
        &self,
        cluster_id: Uuid,
        heartbeat: Heartbeat,
    ) -> usize {
        let mut tasks = tokio::task::JoinSet::new();
        for peer in self.peer_snapshot() {
            let client = self.client.clone();
            let request = ControlEnvelope::new(
                format!(
                    "membership-{}-{}-{}",
                    heartbeat.node_id, heartbeat.boot_id, heartbeat.incarnation
                ),
                ControlRequest::MembershipHeartbeat {
                    cluster_id,
                    heartbeat: heartbeat.clone(),
                },
            );
            tasks.spawn(async move {
                let result = tokio::time::timeout(
                    MEMBERSHIP_RPC_TIMEOUT,
                    client.rpc(peer.addr, peer.fingerprint, &request),
                )
                .await;
                (peer.alias, result)
            });
        }

        let mut delivered = 0usize;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok((peer, Ok(Ok(reply)))) => match reply.response {
                    ControlResponse::MembershipAccepted {
                        node_id,
                        incarnation,
                    } if node_id == heartbeat.node_id && incarnation == heartbeat.incarnation => {
                        delivered += 1;
                    }
                    response => debug!(
                        %peer,
                        ?response,
                        "membership peer returned an unexpected QUIC response"
                    ),
                },
                Ok((peer, Ok(Err(error)))) => debug!(
                    %peer,
                    %error,
                    "membership QUIC heartbeat failed"
                ),
                Ok((peer, Err(_))) => debug!(
                    %peer,
                    timeout_ms = MEMBERSHIP_RPC_TIMEOUT.as_millis(),
                    "membership QUIC heartbeat timed out"
                ),
                Err(error) => debug!(%error, "membership heartbeat task failed"),
            }
        }
        delivered
    }

    /// #498 4b: ensure `block_ref` is available in `cas`, pulling it **by hash** from a
    /// peer if missing. Consults the 4a block map (which peers hold it, for observability)
    /// and pulls from a pinned peer's block-pull endpoint, then verifies + durably
    /// publishes (V28) before returning. Returns `true` if the block is local after the
    /// call. Blob/SHA-256 refs only (the chunk plane is a later step).
    ///
    /// Peer selection currently tries each pinned peer until one serves a verified copy;
    /// block-map-guided selection (skip non-holders) is a refinement once `NodeId`→peer
    /// mapping is configured. Single-flight / negative-cache are PR3 (4c).
    pub async fn resolve_block(&self, cas: &CasStore, block_ref: &BlockRef) -> bool {
        let Some(hash) = blob_sha256_hash(block_ref) else {
            return false;
        };
        if cas.contains(&hash) {
            return true; // local hit
        }
        let holder_count = {
            let map = self.block_map.lock().unwrap_or_else(|p| p.into_inner());
            map.holders(block_ref).len()
        };
        debug!(block_ref = %block_ref, holders = holder_count, "#498 resolve: pulling a missing block");
        for peer in self.peer_snapshot() {
            match self
                .pull_client
                .pull(peer.pull_addr, peer.fingerprint, block_ref)
                .await
            {
                Ok(Some(encoded)) => {
                    match cas.store_pulled_blob(&encoded, &hash, block_ref.size_bytes()) {
                        Ok(()) => {
                            info!(
                                peer = %peer.alias,
                                block_ref = %block_ref,
                                "#498 pulled + verified + durably published"
                            );
                            return true;
                        }
                        Err(e) => warn!(
                            peer = %peer.alias,
                            error = %e,
                            "#498 pulled blob rejected (integrity) — trying next peer"
                        ),
                    }
                }
                Ok(None) => {} // peer does not hold it
                Err(e) => debug!(
                    peer = %peer.alias,
                    error = %e,
                    "#498 block-pull to peer failed — trying next"
                ),
            }
        }
        false
    }

    /// Build the #498 4c blob resolver (V9) for the daemon's CAS read path: local-or-pull
    /// with single-flight + negative-cache + pull-pin, pulling missing blobs from a peer.
    /// Injected via `CasStore::set_resolver` in cluster mode only (single-node unchanged).
    pub fn blob_resolver(self: &Arc<Self>, data_dir: PathBuf) -> Arc<BlockResolver> {
        let store: Arc<dyn BlockStore> = Arc::new(CasBlockStore {
            data_dir: data_dir.clone(),
        });
        let remote: Arc<dyn RemotePull> = Arc::new(DaemonRemotePull {
            cluster: Arc::downgrade(self),
            data_dir,
            handle: tokio::runtime::Handle::current(),
        });
        Arc::new(BlockResolver::new(store, remote))
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
        let peers = self.peer_snapshot();
        if advertisements.is_empty() || peers.is_empty() {
            return 0;
        }
        let mut delivered = 0;
        for peer in peers {
            let env = ControlEnvelope::new(
                format!("advertise-holders-r{round}"),
                ControlRequest::AdvertiseHolders {
                    advertisements: advertisements.clone(),
                },
            );
            match self.client.rpc(peer.addr, peer.fingerprint, &env).await {
                Ok(reply) => {
                    delivered += 1;
                    if let ControlResponse::HoldersApplied { applied } = reply.response {
                        if applied > 0 {
                            info!(
                                peer = %peer.alias,
                                applied,
                                "#498 block-map gossip delivered (peer newly applied)"
                            );
                        }
                    }
                }
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
            .peer_snapshot()
            .into_iter()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_addr_is_control_port_plus_one() {
        let ctrl: SocketAddr = "10.0.0.242:8085".parse().unwrap();
        let pull = pull_addr_of(ctrl);
        assert_eq!(pull.port(), 8086, "block-pull rides control port + 1");
        assert_eq!(pull.ip(), ctrl.ip(), "same host");
    }

    #[test]
    fn blob_hash_extracts_only_blob_sha256_refs() {
        let blob = BlockRef::blob_sha256([9; 32], 100);
        assert_eq!(blob_sha256_hash(&blob), Some([9u8; 32]));
        // A chunk (BLAKE3-128) is not a blob/sha256 ref -> not pullable here (4b is the
        // blob plane; the chunk plane is a later step).
        let chunk = BlockRef::chunk_blake3_128([1; 16], 50, "gear-v1");
        assert_eq!(blob_sha256_hash(&chunk), None);
    }

    #[test]
    fn dynamic_peer_store_roundtrips_node_certificate_binding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DYNAMIC_PEERS_FILE);
        let node_id = NodeId::new();
        let peer = ResolvedPeer {
            node_id,
            alias: "node-2".into(),
            addr: "10.0.0.243:8085".parse().unwrap(),
            pull_addr: "10.0.0.243:8086".parse().unwrap(),
            fingerprint: CertFingerprint([7; 32]),
        };
        let peers = BTreeMap::from([(node_id, peer.clone())]);
        persist_peers(&path, &peers).unwrap();
        let loaded = load_persisted_peers(&path).unwrap();
        assert_eq!(loaded, vec![peer.to_config()]);
    }
}
