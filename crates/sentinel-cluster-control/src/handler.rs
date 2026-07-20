//! The control-RPC handler seam.
//!
//! The transport (server) decodes a `ControlRequest`, de-duplicates it, and calls a
//! `ControlHandler` for the response. The Phase-3a0 skeleton ships a deterministic
//! `StubHandler`; the real owner-registry (#496) and cluster-GC (#499) handlers
//! replace it without touching the transport.

use std::sync::{Arc, Mutex};

use sentinel_common::BlockMap;

use crate::envelope::{ControlRequest, ControlResponse};
use crate::server::AuthenticatedPeer;

/// Maps a `ControlRequest` to a `ControlResponse`. Implementations MUST be total
/// (return a typed `Rejected` rather than panic on anything they cannot serve).
pub trait ControlHandler: Send + Sync {
    fn handle(&self, peer: AuthenticatedPeer, request: &ControlRequest) -> ControlResponse;
}

/// A composable handler that merges #498 `AdvertiseHolders` gossip into a **shared**
/// block map and delegates every other RPC to an inner handler (e.g. the #496
/// [`OwnerControlHandler`](../../sentinel-daemon)). The same `Arc<Mutex<BlockMap>>` is
/// held by the daemon so its read paths (#498 PR2/PR3) see the gossiped holders.
///
/// The merge is conflict-free (V16): each advertisement is applied only if strictly
/// newer than what the map already knew, so out-of-order / duplicated / re-broadcast
/// gossip converges. Block bytes never appear here — this is metadata only (AC-4).
pub struct BlockMapGossipHandler<H: ControlHandler> {
    block_map: Arc<Mutex<BlockMap>>,
    inner: H,
}

impl<H: ControlHandler> BlockMapGossipHandler<H> {
    /// Wrap `inner`, merging holder gossip into `block_map`.
    pub fn new(block_map: Arc<Mutex<BlockMap>>, inner: H) -> Self {
        Self { block_map, inner }
    }

    /// The shared block map (the daemon reads holders from the same handle).
    pub fn block_map(&self) -> Arc<Mutex<BlockMap>> {
        Arc::clone(&self.block_map)
    }
}

impl<H: ControlHandler> ControlHandler for BlockMapGossipHandler<H> {
    fn handle(&self, peer: AuthenticatedPeer, request: &ControlRequest) -> ControlResponse {
        match request {
            ControlRequest::AdvertiseHolders { advertisements } => {
                if advertisements
                    .iter()
                    .any(|advertisement| advertisement.node_id != peer.node_id)
                {
                    return ControlResponse::Rejected {
                        reason: format!(
                            "holder advertisement node_id must match authenticated peer {}",
                            peer.node_id
                        ),
                    };
                }
                // A poisoned lock means another thread panicked mid-merge; recover the
                // guard rather than cascade the panic (the map stays a valid locator).
                let mut map = self
                    .block_map
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let applied = advertisements
                    .iter()
                    .filter(|adv| map.apply_advertisement(adv))
                    .count();
                if applied > 0 {
                    tracing::info!(
                        applied,
                        block_count = map.block_count(),
                        "#498: merged holder gossip into the block map"
                    );
                }
                ControlResponse::HoldersApplied {
                    applied: applied as u32,
                }
            }
            other => self.inner.handle(peer, other),
        }
    }
}

/// Enforces the chef-controller authorization boundary before owner-state handlers.
/// `None` fails closed for mutations while still permitting read-only control RPCs.
pub struct ChefAuthorizingHandler<H: ControlHandler> {
    chef_node_id: Option<sentinel_common::NodeId>,
    inner: H,
}

impl<H: ControlHandler> ChefAuthorizingHandler<H> {
    pub fn new(chef_node_id: Option<sentinel_common::NodeId>, inner: H) -> Self {
        Self {
            chef_node_id,
            inner,
        }
    }
}

impl<H: ControlHandler> ControlHandler for ChefAuthorizingHandler<H> {
    fn handle(&self, peer: AuthenticatedPeer, request: &ControlRequest) -> ControlResponse {
        if request.requires_chef_authorization() && self.chef_node_id != Some(peer.node_id) {
            return ControlResponse::Rejected {
                reason: format!(
                    "{} requires the configured chef node",
                    request.method_name()
                ),
            };
        }
        self.inner.handle(peer, request)
    }
}

/// A production fail-closed terminal handler for a control-plane dependency outage.
/// Wrappers may still handle independent requests (for example membership), but every
/// request reaching this terminal is rejected instead of receiving a synthetic ack.
#[derive(Debug, Clone, Copy)]
pub struct FailClosedHandler {
    reason: &'static str,
}

impl FailClosedHandler {
    pub const fn new(reason: &'static str) -> Self {
        Self { reason }
    }
}

impl ControlHandler for FailClosedHandler {
    fn handle(&self, _peer: AuthenticatedPeer, _request: &ControlRequest) -> ControlResponse {
        ControlResponse::Rejected {
            reason: self.reason.into(),
        }
    }
}

/// A deterministic stub for the skeleton + tests. Acknowledges the owner RPCs and
/// reports **no** refs/pins for the GC queries (a fresh skeleton holds no cluster
/// references); the real liveness answers come from #496/#499.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubHandler;

impl ControlHandler for StubHandler {
    fn handle(&self, _peer: AuthenticatedPeer, request: &ControlRequest) -> ControlResponse {
        match request {
            ControlRequest::MembershipHeartbeat { .. } => ControlResponse::Rejected {
                reason: "membership heartbeat requires the daemon membership handler".into(),
            },
            ControlRequest::PrepareHandoff { scope, epoch } => ControlResponse::HandoffPrepared {
                scope: scope.clone(),
                epoch: *epoch,
            },
            ControlRequest::SourceRetiredAck { scope, epoch } => {
                ControlResponse::RetiredAckRecorded {
                    scope: scope.clone(),
                    epoch: *epoch,
                }
            }
            ControlRequest::OwnerCommit { scope, epoch, .. } => ControlResponse::OwnerCommitted {
                scope: scope.clone(),
                epoch: *epoch,
            },
            ControlRequest::ReplicateOwnerSnapshot { .. } => ControlResponse::Rejected {
                reason: "owner snapshot replication requires a durable owner handler".into(),
            },
            ControlRequest::RefQuery { block_ref } => ControlResponse::RefQueryResult {
                block_ref: block_ref.clone(),
                referenced: false,
            },
            ControlRequest::PinQuery { block_ref } => ControlResponse::PinQueryResult {
                block_ref: block_ref.clone(),
                pinned: false,
            },
            // Stateless stub: it holds no block map, so it applies nothing. The real
            // merge runs in `BlockMapGossipHandler`.
            ControlRequest::AdvertiseHolders { .. } => {
                ControlResponse::HoldersApplied { applied: 0 }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::{BlockRef, HolderAction, HolderAdvertisement, NodeId};
    use uuid::Uuid;

    fn peer() -> AuthenticatedPeer {
        AuthenticatedPeer {
            fingerprint: crate::CertFingerprint([1; 32]),
            node_id: sentinel_common::NodeId::new(),
        }
    }

    #[test]
    fn fail_closed_handler_never_acknowledges_owner_mutations() {
        let handler = FailClosedHandler::new("cluster metastore unavailable");
        let requests = [
            ControlRequest::PrepareHandoff {
                scope: "world".into(),
                epoch: 2,
            },
            ControlRequest::SourceRetiredAck {
                scope: "world".into(),
                epoch: 2,
            },
            ControlRequest::OwnerCommit {
                scope: "world".into(),
                owner_node: sentinel_common::NodeId::new().to_string(),
                epoch: 2,
            },
        ];
        for request in requests {
            assert_eq!(
                handler.handle(peer(), &request),
                ControlResponse::Rejected {
                    reason: "cluster metastore unavailable".into(),
                }
            );
        }
    }

    fn advert_for(
        node_id: sentinel_common::NodeId,
        n: u8,
        gen: u64,
        action: HolderAction,
    ) -> HolderAdvertisement {
        HolderAdvertisement {
            block_ref: BlockRef::blob_sha256([n; 32], 1024),
            node_id,
            node_boot_id: Uuid::new_v4(),
            node_incarnation: 1,
            node_cas_generation: gen,
            action,
            expires_after: u64::MAX,
        }
    }

    #[test]
    fn gossip_handler_merges_advertisements_into_shared_map() {
        let map = Arc::new(Mutex::new(BlockMap::new()));
        let h = BlockMapGossipHandler::new(Arc::clone(&map), StubHandler);
        let peer = peer();

        let a = advert_for(peer.node_id, 1, 5, HolderAction::Add);
        let b = advert_for(peer.node_id, 2, 5, HolderAction::Add);
        let resp = h.handle(
            peer,
            &ControlRequest::AdvertiseHolders {
                advertisements: vec![a.clone(), b.clone()],
            },
        );
        assert_eq!(resp, ControlResponse::HoldersApplied { applied: 2 });
        assert_eq!(map.lock().unwrap().block_count(), 2, "both holders merged");

        // Re-sending the same batch is a no-op (stale/duplicate) — 0 newly applied.
        let resp2 = h.handle(
            peer,
            &ControlRequest::AdvertiseHolders {
                advertisements: vec![a, b],
            },
        );
        assert_eq!(resp2, ControlResponse::HoldersApplied { applied: 0 });
    }

    #[test]
    fn gossip_rejects_holder_identity_spoof_before_mutation() {
        let map = Arc::new(Mutex::new(BlockMap::new()));
        let h = BlockMapGossipHandler::new(Arc::clone(&map), StubHandler);
        let peer = peer();
        let response = h.handle(
            peer,
            &ControlRequest::AdvertiseHolders {
                advertisements: vec![advert_for(NodeId::new(), 1, 1, HolderAction::Add)],
            },
        );
        assert!(matches!(response, ControlResponse::Rejected { .. }));
        assert_eq!(map.lock().unwrap().block_count(), 0);
    }

    #[test]
    fn owner_mutations_require_configured_chef_but_reads_do_not() {
        let chef = peer();
        let other = peer();
        let handler = ChefAuthorizingHandler::new(Some(chef.node_id), StubHandler);
        let mutation = ControlRequest::OwnerCommit {
            scope: "agent:7".into(),
            owner_node: chef.node_id.to_string(),
            epoch: 5,
        };
        assert!(matches!(
            handler.handle(other, &mutation),
            ControlResponse::Rejected { .. }
        ));
        assert!(matches!(
            handler.handle(chef, &mutation),
            ControlResponse::OwnerCommitted { .. }
        ));
        assert!(matches!(
            handler.handle(
                other,
                &ControlRequest::RefQuery {
                    block_ref: "cas-blob:v1:sha256:ab".into()
                }
            ),
            ControlResponse::RefQueryResult { .. }
        ));
    }

    #[test]
    fn gossip_handler_delegates_non_gossip_rpcs_to_inner() {
        let map = Arc::new(Mutex::new(BlockMap::new()));
        let h = BlockMapGossipHandler::new(map, StubHandler);
        // An owner RPC is served by the inner StubHandler unchanged.
        assert_eq!(
            h.handle(
                peer(),
                &ControlRequest::OwnerCommit {
                    scope: "agent:7".into(),
                    owner_node: "node-1".into(),
                    epoch: 5,
                }
            ),
            ControlResponse::OwnerCommitted {
                scope: "agent:7".into(),
                epoch: 5
            }
        );
    }

    #[test]
    fn stub_acknowledges_owner_rpcs_and_reports_no_refs() {
        let h = StubHandler;
        assert_eq!(
            h.handle(
                peer(),
                &ControlRequest::OwnerCommit {
                    scope: "agent:7".into(),
                    owner_node: "node-1".into(),
                    epoch: 5,
                }
            ),
            ControlResponse::OwnerCommitted {
                scope: "agent:7".into(),
                epoch: 5
            }
        );
        assert_eq!(
            h.handle(
                peer(),
                &ControlRequest::RefQuery {
                    block_ref: "cas-blob:v1:sha256:ab".into()
                }
            ),
            ControlResponse::RefQueryResult {
                block_ref: "cas-blob:v1:sha256:ab".into(),
                referenced: false,
            }
        );
    }
}
