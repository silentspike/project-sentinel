//! The real `#496` owner-registry control handler — replaces the Phase-3a0
//! `StubHandler` on cluster nodes. It is the daemon-side glue between the cluster
//! control RPCs (#569) and the ownership authority: the in-memory `OwnerRegistry` (the
//! working view, in `sentinel-common`) plus the durable `ClusterMetaStore` (ADR-3, the
//! persistent authority, in `sentinel-redb`). The registry cannot depend on the redb
//! store it is the authority for, so this handler — which sees both — orchestrates them.
//!
//! `OwnerCommit` (PR2b-2a) durably persists the new term (durable-first authority) and
//! re-establishes the in-memory registry, entering cluster mode so the old owner's guards
//! turn stale (V19). `PrepareHandoff` (PR2b-2ii) is the **source side** of the cooperative
//! handoff: it durably retires the scope (V4 local fence) before acking, so the source
//! stops writing it even during a partition. `SourceRetiredAck` is an idempotent
//! acknowledgement; `RefQuery` / `PinQuery` belong to the cluster-GC handler (#499) and
//! report no refs. The chef-side saga that orders these RPCs lives in `handoff.rs`.

use std::sync::Arc;

use sentinel_cluster_control::{
    AuthenticatedPeer, ControlHandler, ControlRequest, ControlResponse,
};
use sentinel_common::{
    LocalOwnerRole, LocalOwnerState, NodeId, OwnerRegistry, OwnerTerm, StateTransferScope,
};
use sentinel_redb::ClusterMetaStore;
use tracing::{info, warn};

/// The owner-registry control handler (#496). Holds the durable cluster-meta store; the
/// in-memory working view is the process-global [`OwnerRegistry`].
pub struct OwnerControlHandler {
    meta: Arc<ClusterMetaStore>,
}

impl OwnerControlHandler {
    pub fn new(meta: Arc<ClusterMetaStore>) -> Self {
        Self { meta }
    }

    /// Parse + durably persist a committed owner term (ADR-3, durable-first authority).
    /// Returns the term on success, or a typed `Rejected` response on a malformed
    /// scope / node id or a persist failure. Deliberately does **not** touch the
    /// in-memory registry — the caller applies that only after persistence succeeds.
    /// This keeps the process-global registry out of the persist unit tests; the
    /// in-memory cluster-mode effect itself is covered in `sentinel-common::fencing`.
    fn persist_commit(
        &self,
        scope_wire: &str,
        owner_node: &str,
        epoch: u64,
    ) -> Result<OwnerTerm, ControlResponse> {
        let Some(scope) = StateTransferScope::from_wire(scope_wire) else {
            warn!(
                scope = scope_wire,
                "OwnerCommit rejected: unrecognized scope"
            );
            return Err(ControlResponse::Rejected {
                reason: format!("unrecognized scope {scope_wire:?}"),
            });
        };
        let owner_node = match owner_node.parse::<uuid::Uuid>() {
            Ok(u) => NodeId(u),
            Err(_) => {
                warn!(owner_node, "OwnerCommit rejected: malformed owner node id");
                return Err(ControlResponse::Rejected {
                    reason: format!("malformed owner node id {owner_node:?}"),
                });
            }
        };
        let term = OwnerTerm {
            scope,
            owner_node,
            epoch,
        };
        if let Err(e) = self.meta.put_owner_term(&term) {
            warn!(error = %e, scope = scope_wire, "OwnerCommit rejected: persist failed");
            return Err(ControlResponse::Rejected {
                reason: format!("persist owner term failed: {e}"),
            });
        }
        Ok(term)
    }

    /// Source side of a cooperative handoff (V1/V4): durably persist this node's local
    /// retirement of `scope` at `epoch`. Returns the persisted state, or a typed
    /// `Rejected` on a malformed scope / persist failure. The caller then applies it to
    /// the in-memory registry (kept out of the persist unit test, like `persist_commit`).
    fn persist_retirement(
        &self,
        scope_wire: &str,
        epoch: u64,
    ) -> Result<LocalOwnerState, ControlResponse> {
        let Some(scope) = StateTransferScope::from_wire(scope_wire) else {
            warn!(
                scope = scope_wire,
                "PrepareHandoff rejected: unrecognized scope"
            );
            return Err(ControlResponse::Rejected {
                reason: format!("unrecognized scope {scope_wire:?}"),
            });
        };
        let state = LocalOwnerState {
            scope,
            node_id: OwnerRegistry::global().this_node(),
            epoch,
            role: LocalOwnerRole::Retired,
        };
        if let Err(e) = self.meta.put_local_state(&state) {
            warn!(error = %e, scope = scope_wire, "PrepareHandoff rejected: persist retirement failed");
            return Err(ControlResponse::Rejected {
                reason: format!("persist retirement failed: {e}"),
            });
        }
        Ok(state)
    }
}

impl ControlHandler for OwnerControlHandler {
    fn handle(&self, _peer: AuthenticatedPeer, request: &ControlRequest) -> ControlResponse {
        match request {
            ControlRequest::MembershipHeartbeat { .. } => ControlResponse::Rejected {
                reason: "membership heartbeat must be handled by the membership wrapper".into(),
            },
            ControlRequest::OwnerCommit {
                scope,
                owner_node,
                epoch,
            } => match self.persist_commit(scope, owner_node, *epoch) {
                Ok(term) => {
                    // Durably persisted -> re-establish the in-memory working view
                    // (enters cluster mode; the old owner's guards turn stale, V19).
                    OwnerRegistry::global().commit_owner(term);
                    info!(
                        scope = scope.as_str(),
                        owner_node = owner_node.as_str(),
                        epoch = *epoch,
                        "OwnerCommit: term persisted + registry updated (cluster mode)"
                    );
                    ControlResponse::OwnerCommitted {
                        scope: scope.clone(),
                        epoch: *epoch,
                    }
                }
                Err(rejected) => rejected,
            },

            // PR2b-2ii: the source side of the cooperative handoff — durably retire the
            // scope (V4) before acking, so this node stops writing it even during a
            // partition. Reject an unrecognized scope rather than retire it.
            ControlRequest::PrepareHandoff { scope, epoch } => {
                match self.persist_retirement(scope, *epoch) {
                    Ok(state) => {
                        // Durably retired -> apply to the in-memory registry so this node
                        // stops writing the scope (V4 local fence), then ack (V1 durable
                        // SourceRetiredAck).
                        OwnerRegistry::global().retire_local(state.scope, *epoch);
                        info!(
                            scope = scope.as_str(),
                            epoch = *epoch,
                            "PrepareHandoff: scope durably retired (source side, V4)"
                        );
                        ControlResponse::HandoffPrepared {
                            scope: scope.clone(),
                            epoch: *epoch,
                        }
                    }
                    Err(rejected) => rejected,
                }
            }
            ControlRequest::SourceRetiredAck { scope, epoch } => {
                if StateTransferScope::from_wire(scope).is_none() {
                    return ControlResponse::Rejected {
                        reason: format!("unrecognized scope {scope:?}"),
                    };
                }
                info!(
                    scope = scope.as_str(),
                    epoch = *epoch,
                    "SourceRetiredAck acknowledged (durable marker: PR2b-2b)"
                );
                ControlResponse::RetiredAckRecorded {
                    scope: scope.clone(),
                    epoch: *epoch,
                }
            }

            // RefQuery / PinQuery belong to the cluster-GC handler (#499); this owner
            // handler holds no CAS refs, so it reports none (same as the skeleton).
            ControlRequest::RefQuery { block_ref } => ControlResponse::RefQueryResult {
                block_ref: block_ref.clone(),
                referenced: false,
            },
            ControlRequest::PinQuery { block_ref } => ControlResponse::PinQueryResult {
                block_ref: block_ref.clone(),
                pinned: false,
            },

            // #498 holder gossip is intercepted by the BlockMapGossipHandler wrapper
            // before it reaches this owner handler. Reaching here means it was wired
            // without the wrapper — reject (typed, never panic) rather than silently drop.
            ControlRequest::AdvertiseHolders { .. } => ControlResponse::Rejected {
                reason: "holder gossip must be handled by the BlockMapGossipHandler wrapper".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Arc<ClusterMetaStore>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cluster_meta.redb");
        let store = Arc::new(ClusterMetaStore::open(path.to_str().unwrap()).unwrap());
        (dir, store)
    }

    #[test]
    fn owner_commit_persists_term_under_canonical_scope_key() {
        let (_dir, store) = store();
        let handler = OwnerControlHandler::new(store.clone());
        let node = uuid::Uuid::from_bytes([9u8; 16]);

        // persist_commit is the persist path (no global-registry mutation in the test).
        let term = handler
            .persist_commit("nano:AGENT-07", &node.to_string(), 2)
            .expect("commit should succeed");
        assert_eq!(term.epoch, 2);
        assert_eq!(term.owner_node, NodeId(node));

        // Durably persisted under the canonical scope key (round-trips via to_wire).
        let persisted = store
            .get_owner_term(&StateTransferScope::NanoContainer("AGENT-07".into()))
            .unwrap()
            .unwrap();
        assert_eq!(persisted, term);
    }

    #[test]
    fn owner_commit_rejects_malformed_scope_and_node() {
        let (_dir, store) = store();
        let handler = OwnerControlHandler::new(store);
        // Unrecognized scope wire form.
        assert!(matches!(
            handler.persist_commit("agent:7", &uuid::Uuid::nil().to_string(), 1),
            Err(ControlResponse::Rejected { .. })
        ));
        // Malformed node id.
        assert!(matches!(
            handler.persist_commit("world", "not-a-uuid", 1),
            Err(ControlResponse::Rejected { .. })
        ));
    }

    #[test]
    fn prepare_handoff_persists_retirement_and_rejects_unknown() {
        let (_dir, store) = store();
        let handler = OwnerControlHandler::new(store.clone());

        // persist_retirement is the durable path (no global-registry mutation in the test).
        let state = handler
            .persist_retirement("nano:AGENT-07", 3)
            .expect("retire should persist");
        assert_eq!(state.role, LocalOwnerRole::Retired);
        assert_eq!(state.epoch, 3);
        let persisted = store
            .get_local_state(&StateTransferScope::NanoContainer("AGENT-07".into()))
            .unwrap()
            .unwrap();
        assert_eq!(persisted, state);

        // Unknown scope -> rejected, nothing persisted.
        assert!(matches!(
            handler.persist_retirement("agent:7", 1),
            Err(ControlResponse::Rejected { .. })
        ));

        // GC queries still report no refs via the full handle() path (#499 authority).
        assert_eq!(
            handler.handle(
                AuthenticatedPeer {
                    fingerprint: sentinel_cluster_control::CertFingerprint([3; 32]),
                    node_id: sentinel_common::NodeId::new(),
                },
                &ControlRequest::RefQuery {
                    block_ref: "cas-blob:v1:sha256:ab".into()
                },
            ),
            ControlResponse::RefQueryResult {
                block_ref: "cas-blob:v1:sha256:ab".into(),
                referenced: false
            }
        );
    }
}
