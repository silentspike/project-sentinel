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
    ActivationState, LocalOwnerBaseRole, LocalOwnerRole, LocalOwnerState, NodeId, OwnerRegistry,
    OwnerSnapshotInstallOutcome, OwnerTerm, StateTransferScope, TRACK_A_COORDINATOR_GENERATION,
};
use sentinel_redb::ClusterMetaStore;
use tracing::{info, warn};

/// The owner-registry control handler (#496). Holds the durable cluster-meta store; the
/// in-memory working view is the process-global [`OwnerRegistry`].
pub struct OwnerControlHandler {
    meta: Arc<ClusterMetaStore>,
    this_node: NodeId,
}

impl OwnerControlHandler {
    pub fn new(meta: Arc<ClusterMetaStore>, this_node: NodeId) -> Self {
        Self { meta, this_node }
    }

    /// Complete the target-local activation of an ownership-only handoff. The complete
    /// global authority snapshot must already have been installed; this step never
    /// writes CLUSTER_OWNER or the install marker partially.
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
            coordinator_generation: TRACK_A_COORDINATOR_GENERATION,
        };
        let installed = match self.meta.installed_owner_snapshot() {
            Ok(Some(pair)) => pair,
            Ok(None) => {
                return Err(ControlResponse::Rejected {
                    reason: "owner snapshot must be installed before OwnerCommit".into(),
                });
            }
            Err(error) => {
                return Err(ControlResponse::Rejected {
                    reason: format!("owner snapshot readback failed: {error}"),
                });
            }
        };
        let Some(installed_term) = installed
            .0
            .sorted_terms
            .iter()
            .find(|candidate| candidate.scope == term.scope)
        else {
            return Err(ControlResponse::Rejected {
                reason: "committed owner term is absent from installed snapshot".into(),
            });
        };
        let Some(local) = installed
            .1
            .sorted_base_states
            .iter()
            .find(|candidate| candidate.scope == term.scope)
        else {
            return Err(ControlResponse::Rejected {
                reason: "recipient-local owner state is absent from installed snapshot".into(),
            });
        };
        if installed_term != &term
            || local.owner_term != term
            || local.recipient_node != term.owner_node
            || local.base_role != LocalOwnerBaseRole::Owner
            || local.activation_state != ActivationState::Routable
        {
            return Err(ControlResponse::Rejected {
                reason: "OwnerCommit does not match installed recipient-local authority".into(),
            });
        }
        if let Err(e) = self.meta.complete_handoff_overlay(&term) {
            warn!(error = %e, scope = scope_wire, "OwnerCommit rejected: overlay CAS failed");
            return Err(ControlResponse::Rejected {
                reason: format!("complete handoff overlay failed: {e}"),
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
        persist_source_retirement(&self.meta, self.this_node, scope_wire, epoch).map_err(|reason| {
            warn!(scope = scope_wire, %reason, "PrepareHandoff rejected: source authority mismatch");
            ControlResponse::Rejected { reason }
        })
    }
}

/// Validate and durably retire the source against the complete installed authority.
/// The caller holds the owner tick barrier so validation and overlay persistence cannot
/// race a local snapshot install. This is shared by inbound RPC and chef-local handoff.
pub(crate) fn persist_source_retirement(
    meta: &ClusterMetaStore,
    this_node: NodeId,
    scope_wire: &str,
    epoch: u64,
) -> Result<LocalOwnerState, String> {
    let scope = StateTransferScope::from_wire(scope_wire)
        .ok_or_else(|| format!("unrecognized scope {scope_wire:?}"))?;
    let (global, local) = meta
        .installed_owner_snapshot()
        .map_err(|error| format!("owner snapshot readback failed: {error}"))?
        .ok_or_else(|| "owner snapshot must be installed before PrepareHandoff".to_string())?;
    let term = global
        .sorted_terms
        .iter()
        .find(|candidate| candidate.scope == scope)
        .ok_or_else(|| "handoff scope is absent from installed owner snapshot".to_string())?;
    let local_state = local
        .sorted_base_states
        .iter()
        .find(|candidate| candidate.scope == scope)
        .ok_or_else(|| {
            "recipient-local owner state is absent from installed snapshot".to_string()
        })?;

    if global.coordinator_generation != TRACK_A_COORDINATOR_GENERATION
        || local.coordinator_generation != TRACK_A_COORDINATOR_GENERATION
        || term.coordinator_generation != TRACK_A_COORDINATOR_GENERATION
    {
        return Err("PrepareHandoff coordinator generation mismatch".into());
    }
    if term.epoch != epoch {
        return Err(format!(
            "PrepareHandoff epoch mismatch: installed {}, requested {epoch}",
            term.epoch
        ));
    }
    if term.owner_node != this_node {
        return Err(format!(
            "PrepareHandoff source mismatch: installed owner {}, this node {this_node}",
            term.owner_node
        ));
    }
    if local.recipient_node != this_node
        || local_state.recipient_node != this_node
        || local_state.owner_term != *term
        || local_state.base_role != LocalOwnerBaseRole::Owner
        || local_state.activation_state != ActivationState::Routable
    {
        return Err("PrepareHandoff recipient-local state is not active owner authority".into());
    }

    let state = LocalOwnerState {
        scope,
        node_id: this_node,
        epoch,
        role: LocalOwnerRole::Retired,
    };
    meta.put_local_state(&state)
        .map_err(|error| format!("persist retirement failed: {error}"))?;
    Ok(state)
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
            } => {
                let _tick_barrier = sentinel_common::owner_tick_barrier();
                match self.persist_commit(scope, owner_node, *epoch) {
                    Ok(_term) => {
                        let installed = self.meta.installed_owner_snapshot();
                        let overlays = self.meta.list_local_saga_states();
                        let rebuild = match (installed, overlays) {
                            (Ok(Some((global, local))), Ok(overlays)) => OwnerRegistry::global()
                                .rebuild_from_owner_snapshot(&global, &local, overlays),
                            (Ok(None), _) => {
                                return ControlResponse::Rejected {
                                    reason: "owner snapshot marker disappeared during OwnerCommit"
                                        .into(),
                                };
                            }
                            (Err(error), _) | (_, Err(error)) => {
                                return ControlResponse::Rejected {
                                    reason: format!("OwnerCommit durable readback failed: {error}"),
                                };
                            }
                        };
                        if let Err(error) = rebuild {
                            OwnerRegistry::global().close_owner_readiness();
                            return ControlResponse::Rejected {
                                reason: format!("OwnerCommit cache rebuild failed: {error}"),
                            };
                        }
                        info!(
                            scope = scope.as_str(),
                            owner_node = owner_node.as_str(),
                            epoch = *epoch,
                            "OwnerCommit: installed snapshot activated after overlay CAS"
                        );
                        ControlResponse::OwnerCommitted {
                            scope: scope.clone(),
                            epoch: *epoch,
                        }
                    }
                    Err(rejected) => rejected,
                }
            }
            ControlRequest::ReplicateOwnerSnapshot { global, local } => {
                if local.recipient_node != OwnerRegistry::global().this_node() {
                    return ControlResponse::Rejected {
                        reason: "owner snapshot recipient does not match this node".into(),
                    };
                }
                let _tick_barrier = sentinel_common::owner_tick_barrier();
                // Close before the durable install so no guard can be minted or
                // committed against the old cache after a newer authority snapshot
                // has reached local storage. Rebuild opens the latch last.
                OwnerRegistry::global().close_owner_readiness();
                let outcome = match self.meta.install_owner_snapshot(global, local) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        OwnerRegistry::global().close_owner_readiness();
                        warn!(error = %error, "owner snapshot install rejected");
                        return ControlResponse::Rejected {
                            reason: format!("owner snapshot install failed: {error}"),
                        };
                    }
                };
                match &outcome {
                    OwnerSnapshotInstallOutcome::Installed
                    | OwnerSnapshotInstallOutcome::AlreadyInstalled
                    | OwnerSnapshotInstallOutcome::StaleSnapshot { .. } => {
                        let installed = self.meta.installed_owner_snapshot();
                        let overlays = self.meta.list_local_saga_states();
                        match (installed, overlays) {
                            (Ok(Some((installed_global, installed_local))), Ok(overlays)) => {
                                if let Err(error) = OwnerRegistry::global()
                                    .rebuild_from_owner_snapshot(
                                        &installed_global,
                                        &installed_local,
                                        overlays,
                                    )
                                {
                                    OwnerRegistry::global().close_owner_readiness();
                                    return ControlResponse::Rejected {
                                        reason: format!(
                                            "owner snapshot cache rebuild failed: {error}"
                                        ),
                                    };
                                }
                            }
                            (Err(error), _) | (_, Err(error)) => {
                                OwnerRegistry::global().close_owner_readiness();
                                return ControlResponse::Rejected {
                                    reason: format!(
                                        "owner snapshot durable readback failed: {error}"
                                    ),
                                };
                            }
                            (Ok(None), _) => {
                                OwnerRegistry::global().close_owner_readiness();
                                return ControlResponse::Rejected {
                                    reason: "owner snapshot install marker disappeared".into(),
                                };
                            }
                        }
                    }
                    OwnerSnapshotInstallOutcome::SnapshotConflict => {
                        OwnerRegistry::global().close_owner_readiness();
                        warn!("owner snapshot conflict persisted; manual recovery required");
                    }
                    OwnerSnapshotInstallOutcome::GenerationMismatch {
                        installed_generation,
                        received_generation,
                    } => {
                        OwnerRegistry::global().close_owner_readiness();
                        warn!(
                            installed_generation,
                            received_generation,
                            "owner snapshot generation mismatch; readiness remains closed"
                        );
                    }
                }
                ControlResponse::OwnerSnapshotAck { outcome }
            }

            // PR2b-2ii: the source side of the cooperative handoff — durably retire the
            // scope (V4) before acking, so this node stops writing it even during a
            // partition. Reject an unrecognized scope rather than retire it.
            ControlRequest::PrepareHandoff { scope, epoch } => {
                let _tick_barrier = sentinel_common::owner_tick_barrier();
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

    fn install_authority(
        store: &ClusterMetaStore,
        recipient: NodeId,
        owner: NodeId,
        epoch: u64,
        revision: u64,
    ) {
        let scope = StateTransferScope::NanoContainer("AGENT-07".into());
        let term = OwnerTerm {
            scope: scope.clone(),
            owner_node: owner,
            epoch,
            coordinator_generation: TRACK_A_COORDINATOR_GENERATION,
        };
        let global = sentinel_common::OwnerTermSnapshot::new(
            TRACK_A_COORDINATOR_GENERATION,
            revision,
            vec![term.clone()],
        )
        .unwrap();
        let owns_scope = recipient == owner;
        let local = sentinel_common::LocalOwnerStateSnapshot::new(
            recipient,
            TRACK_A_COORDINATOR_GENERATION,
            revision,
            vec![sentinel_common::LocalOwnerBaseState {
                scope,
                recipient_node: recipient,
                owner_term: term,
                base_role: if owns_scope {
                    LocalOwnerBaseRole::Owner
                } else {
                    LocalOwnerBaseRole::Follower
                },
                activation_state: if owns_scope {
                    ActivationState::Routable
                } else {
                    ActivationState::NotRoutable
                },
            }],
        )
        .unwrap();
        assert!(matches!(
            store.install_owner_snapshot(&global, &local).unwrap(),
            OwnerSnapshotInstallOutcome::Installed | OwnerSnapshotInstallOutcome::AlreadyInstalled
        ));
    }

    #[test]
    fn owner_commit_requires_preinstalled_full_snapshot_and_preserves_marker() {
        let (_dir, store) = store();
        let node = NodeId(uuid::Uuid::from_bytes([9u8; 16]));
        let handler = OwnerControlHandler::new(store.clone(), node);
        let scope = StateTransferScope::NanoContainer("AGENT-07".into());
        let expected = OwnerTerm {
            scope: scope.clone(),
            owner_node: node,
            epoch: 2,
            coordinator_generation: TRACK_A_COORDINATOR_GENERATION,
        };
        let global = sentinel_common::OwnerTermSnapshot::new(
            TRACK_A_COORDINATOR_GENERATION,
            4,
            vec![expected.clone()],
        )
        .unwrap();
        let local = sentinel_common::LocalOwnerStateSnapshot::new(
            node,
            TRACK_A_COORDINATOR_GENERATION,
            4,
            vec![sentinel_common::LocalOwnerBaseState {
                scope: scope.clone(),
                recipient_node: node,
                owner_term: expected.clone(),
                base_role: LocalOwnerBaseRole::Owner,
                activation_state: ActivationState::Routable,
            }],
        )
        .unwrap();
        store.install_owner_snapshot(&global, &local).unwrap();
        let marker = store.install_marker().unwrap();

        let term = handler
            .persist_commit("nano:AGENT-07", &node.to_string(), 2)
            .expect("commit should succeed");
        assert_eq!(term, expected);
        assert_eq!(store.get_owner_term(&scope).unwrap().unwrap(), expected);
        assert_eq!(store.install_marker().unwrap(), marker);
    }

    #[test]
    fn owner_commit_rejects_malformed_scope_and_node() {
        let (_dir, store) = store();
        let handler = OwnerControlHandler::new(store, NodeId::new());
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
        let source = NodeId(uuid::Uuid::from_bytes([1u8; 16]));
        let handler = OwnerControlHandler::new(store.clone(), source);
        install_authority(&store, source, source, 3, 1);

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

    #[test]
    fn prepare_handoff_rejects_wrong_epoch_without_retiring_source() {
        let (_dir, store) = store();
        let source = NodeId(uuid::Uuid::from_bytes([1u8; 16]));
        let handler = OwnerControlHandler::new(store.clone(), source);
        install_authority(&store, source, source, 3, 1);

        let rejection = handler.persist_retirement("nano:AGENT-07", 2);
        assert!(matches!(rejection, Err(ControlResponse::Rejected { .. })));
        assert!(store
            .get_local_state(&StateTransferScope::NanoContainer("AGENT-07".into()))
            .unwrap()
            .is_none());
    }

    #[test]
    fn prepare_handoff_replay_after_owner_change_is_rejected() {
        let (_dir, store) = store();
        let source = NodeId(uuid::Uuid::from_bytes([1u8; 16]));
        let target = NodeId(uuid::Uuid::from_bytes([2u8; 16]));
        let handler = OwnerControlHandler::new(store.clone(), source);
        install_authority(&store, source, source, 3, 1);
        handler
            .persist_retirement("nano:AGENT-07", 3)
            .expect("initial retirement should succeed");
        install_authority(&store, source, target, 4, 2);

        let rejection = handler.persist_retirement("nano:AGENT-07", 3);
        assert!(matches!(rejection, Err(ControlResponse::Rejected { .. })));
        assert_eq!(
            store
                .get_owner_term(&StateTransferScope::NanoContainer("AGENT-07".into()))
                .unwrap()
                .unwrap()
                .owner_node,
            target
        );
    }
}
