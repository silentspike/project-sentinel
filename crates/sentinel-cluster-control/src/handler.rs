//! The control-RPC handler seam.
//!
//! The transport (server) decodes a `ControlRequest`, de-duplicates it, and calls a
//! `ControlHandler` for the response. The Phase-3a0 skeleton ships a deterministic
//! `StubHandler`; the real owner-registry (#496) and cluster-GC (#499) handlers
//! replace it without touching the transport.

use std::sync::{Arc, Mutex};

use sentinel_common::BlockMap;

use crate::envelope::{ControlRequest, ControlResponse};

/// Maps a `ControlRequest` to a `ControlResponse`. Implementations MUST be total
/// (return a typed `Rejected` rather than panic on anything they cannot serve).
pub trait ControlHandler: Send + Sync {
    fn handle(&self, request: &ControlRequest) -> ControlResponse;
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
    fn handle(&self, request: &ControlRequest) -> ControlResponse {
        match request {
            ControlRequest::AdvertiseHolders { advertisements } => {
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
                ControlResponse::HoldersApplied {
                    applied: applied as u32,
                }
            }
            other => self.inner.handle(other),
        }
    }
}

/// A deterministic stub for the skeleton + tests. Acknowledges the owner RPCs and
/// reports **no** refs/pins for the GC queries (a fresh skeleton holds no cluster
/// references); the real liveness answers come from #496/#499.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubHandler;

impl ControlHandler for StubHandler {
    fn handle(&self, request: &ControlRequest) -> ControlResponse {
        match request {
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
    use sentinel_common::{BlockRef, HolderAction, HolderAdvertisement};
    use uuid::Uuid;

    fn advert(n: u8, gen: u64, action: HolderAction) -> HolderAdvertisement {
        HolderAdvertisement {
            block_ref: BlockRef::blob_sha256([n; 32], 1024),
            node_id: sentinel_common::NodeId::new(),
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

        let a = advert(1, 5, HolderAction::Add);
        let b = advert(2, 5, HolderAction::Add);
        let resp = h.handle(&ControlRequest::AdvertiseHolders {
            advertisements: vec![a.clone(), b.clone()],
        });
        assert_eq!(resp, ControlResponse::HoldersApplied { applied: 2 });
        assert_eq!(map.lock().unwrap().block_count(), 2, "both holders merged");

        // Re-sending the same batch is a no-op (stale/duplicate) — 0 newly applied.
        let resp2 = h.handle(&ControlRequest::AdvertiseHolders {
            advertisements: vec![a, b],
        });
        assert_eq!(resp2, ControlResponse::HoldersApplied { applied: 0 });
    }

    #[test]
    fn gossip_handler_delegates_non_gossip_rpcs_to_inner() {
        let map = Arc::new(Mutex::new(BlockMap::new()));
        let h = BlockMapGossipHandler::new(map, StubHandler);
        // An owner RPC is served by the inner StubHandler unchanged.
        assert_eq!(
            h.handle(&ControlRequest::OwnerCommit {
                scope: "agent:7".into(),
                owner_node: "node-1".into(),
                epoch: 5,
            }),
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
            h.handle(&ControlRequest::OwnerCommit {
                scope: "agent:7".into(),
                owner_node: "node-1".into(),
                epoch: 5,
            }),
            ControlResponse::OwnerCommitted {
                scope: "agent:7".into(),
                epoch: 5
            }
        );
        assert_eq!(
            h.handle(&ControlRequest::RefQuery {
                block_ref: "cas-blob:v1:sha256:ab".into()
            }),
            ControlResponse::RefQueryResult {
                block_ref: "cas-blob:v1:sha256:ab".into(),
                referenced: false,
            }
        );
    }
}
