//! Cooperative cross-node ownership handoff saga (#496 PR2b-2ii).
//!
//! The chef (seed) hands ownership of a scope from its current owner (the **source**) to
//! a **target** node. This is **ownership only** — moving the per-container ECS/redb/FS
//! state is #497/#501; here only the `OwnerTerm` moves and the source's writes turn
//! stale.
//!
//! **V1 (durable-retire-before-serve):** the target only becomes owner **after** the
//! source has *durably* retired the scope. The saga sends `PrepareHandoff` first (it
//! returns only once the source has persisted its retirement) and *then* commits the new
//! owner term — never the reverse.
//!
//! **V2 (no forced failover / no steal):** if the source is unreachable, the saga
//! **aborts**. A 2-node system cannot distinguish a partition from a death, so ownership
//! is never stolen — only cooperative migration. Witness/quorum failover is Track D.
//!
//! **Chef-SPOF:** the saga runs only on the seed. If the chef is down, no new handoff
//! starts and existing owners keep writing against their committed terms — no data loss,
//! just no ownership changes until the chef returns.
//!
//! The `HandoffTransport` seam abstracts the #569 control RPCs so the saga logic is
//! unit-tested in-process (a fake wiring two registries) and driven live over the QUIC
//! control stream by the daemon (PR2b-2c provides the 2-VM live ACs).

use anyhow::{anyhow, Context, Result};
use sentinel_cluster_control::{ControlRequest, ControlResponse};
use sentinel_common::{
    ActivationState, LocalOwnerBaseRole, LocalOwnerBaseState, LocalOwnerStateSnapshot, NodeId,
    OwnerRegistry, OwnerSnapshotInstallOutcome, OwnerTerm, OwnerTermSnapshot, StateTransferScope,
    TRACK_A_COORDINATOR_GENERATION,
};
use sentinel_redb::ClusterMetaStore;
use std::sync::Arc;
use tracing::{info, warn};

use crate::cluster_control::ClusterControl;

/// The #569 control-RPC seam the saga drives.
pub trait HandoffTransport: Send + Sync {
    /// Resolve the configured alias for one durable node identity. The saga uses this
    /// to bind an operator-supplied source alias to the authoritative owner term before
    /// any participant mutation is attempted.
    fn alias_for_node(&self, node: NodeId) -> Result<String>;

    /// Send `PrepareHandoff(scope, epoch)` to the **source** (current owner) and return
    /// only once it has **durably retired** the scope (V1 — the durable SourceRetiredAck).
    /// An `Err` means the source is unreachable or refused: the saga aborts (V2 — no
    /// ownership steal).
    fn prepare_handoff(&self, source_alias: &str, scope_wire: &str, epoch: u64) -> Result<()>;

    /// Install the complete global authority snapshot plus its recipient-local state.
    fn replicate_owner_snapshot(
        &self,
        node_alias: &str,
        global: &OwnerTermSnapshot,
        local: &LocalOwnerStateSnapshot,
    ) -> Result<()>;

    /// Send `OwnerCommit(scope, owner_node, epoch)` to the **target**, which persists it
    /// and activates ownership at the new epoch.
    fn owner_commit(
        &self,
        target_alias: &str,
        scope_wire: &str,
        owner_node: NodeId,
        epoch: u64,
    ) -> Result<()>;
}

pub(crate) fn local_snapshot_for(
    global: &OwnerTermSnapshot,
    recipient: NodeId,
) -> Result<LocalOwnerStateSnapshot> {
    LocalOwnerStateSnapshot::new(
        recipient,
        global.coordinator_generation,
        global.term_snapshot_revision,
        global
            .sorted_terms
            .iter()
            .cloned()
            .map(|owner_term| {
                let owns_scope = owner_term.owner_node == recipient;
                LocalOwnerBaseState {
                    scope: owner_term.scope.clone(),
                    recipient_node: recipient,
                    owner_term,
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
                }
            })
            .collect(),
    )
    .map_err(Into::into)
}

/// A request to hand ownership of `scope` from its current owner (`source_alias`) to
/// `target` (`target_alias` for routing, `target_node` for the committed term).
#[derive(Debug, Clone)]
pub struct HandoffRequest {
    pub scope: StateTransferScope,
    pub source_alias: String,
    pub target_alias: String,
    pub target_node: NodeId,
}

/// The outcome of a handoff attempt (returned to the operator / op log).
#[derive(Debug, PartialEq, Eq)]
pub enum HandoffOutcome {
    /// Ownership committed to the target at the new epoch (E+1).
    Committed { new_epoch: u64 },
    /// Aborted before any ownership change — the source was unreachable (V2: no steal).
    AbortedSourceUnreachable,
}

/// Drive one cooperative handoff on the chef. `registry`/`meta` are the chef's authority
/// (the process-global registry + the durable cluster-meta store in production).
pub fn run_handoff(
    registry: &OwnerRegistry,
    meta: &ClusterMetaStore,
    transport: &dyn HandoffTransport,
    req: &HandoffRequest,
) -> Result<HandoffOutcome> {
    let scope_wire = req.scope.to_wire();
    let current = registry.current_owner(&req.scope)?;
    let source_alias = transport.alias_for_node(current.owner_node)?;
    if req.source_alias != source_alias {
        anyhow::bail!(
            "source alias {:?} does not match authoritative owner {} ({source_alias:?}) for {scope_wire}",
            req.source_alias,
            current.owner_node
        );
    }
    let source_epoch = current.epoch;
    let new_epoch = source_epoch
        .checked_add(1)
        .ok_or_else(|| anyhow!("owner epoch overflow for {scope_wire}"))?;

    // (V1, step 1) Ask the source to durably retire the scope at its current epoch. This
    // returns only after the durable SourceRetiredAck; an error => abort (V2).
    if let Err(e) = transport.prepare_handoff(&source_alias, &scope_wire, source_epoch) {
        warn!(
            scope = %scope_wire, source = %source_alias, error = %e,
            "Handoff aborted: source unreachable/refused — no ownership steal (V2)"
        );
        return Ok(HandoffOutcome::AbortedSourceUnreachable);
    }

    // (V1, step 2) Only now commit E+1 as a higher-revision full snapshot. The chef
    // installs its own recipient-local half atomically, then replicates the same global
    // snapshot with recipient-specific local state to source and target. No participant
    // partially writes CLUSTER_OWNER or an install marker.
    let term = OwnerTerm {
        scope: req.scope.clone(),
        owner_node: req.target_node,
        epoch: new_epoch,
        coordinator_generation: TRACK_A_COORDINATOR_GENERATION,
    };
    let (installed_global, _) = meta
        .installed_owner_snapshot()?
        .context("handoff requires an installed owner snapshot")?;
    let mut terms = installed_global.sorted_terms;
    let existing = terms
        .iter_mut()
        .find(|candidate| candidate.scope == req.scope)
        .context("handoff scope is absent from installed owner snapshot")?;
    *existing = term.clone();
    let global = OwnerTermSnapshot::new(
        TRACK_A_COORDINATOR_GENERATION,
        installed_global
            .term_snapshot_revision
            .checked_add(1)
            .context("owner snapshot revision overflow")?,
        terms,
    )?;
    let chef_local = local_snapshot_for(&global, registry.this_node())?;
    {
        let _tick_barrier = sentinel_common::owner_tick_barrier();
        registry.close_owner_readiness();
        match meta.install_owner_snapshot(&global, &chef_local)? {
            OwnerSnapshotInstallOutcome::Installed
            | OwnerSnapshotInstallOutcome::AlreadyInstalled => {}
            outcome => anyhow::bail!("chef owner snapshot install failed: {outcome:?}"),
        }
        registry.rebuild_from_owner_snapshot(
            &global,
            &chef_local,
            meta.list_local_saga_states()?,
        )?;
    }

    if current.owner_node != registry.this_node() && current.owner_node != req.target_node {
        let source_local = local_snapshot_for(&global, current.owner_node)?;
        transport.replicate_owner_snapshot(&source_alias, &global, &source_local)?;
    }
    let target_local = local_snapshot_for(&global, req.target_node)?;
    transport.replicate_owner_snapshot(&req.target_alias, &global, &target_local)?;
    transport.owner_commit(&req.target_alias, &scope_wire, req.target_node, new_epoch)?;

    info!(
        scope = %scope_wire, source = %source_alias, target = %req.target_alias,
        new_epoch, "Handoff committed: ownership moved to target (V1)"
    );
    Ok(HandoffOutcome::Committed { new_epoch })
}

/// The live `HandoffTransport` (#496 PR2b-2c): a step whose alias is **this** node's is
/// applied locally; a step for a peer is driven over the #569 control stream. Held by the
/// seed's operator `/handoff` endpoint (the in-process saga tests use a fake instead).
pub struct RpcHandoffTransport {
    cluster_control: Arc<ClusterControl>,
    meta: Arc<ClusterMetaStore>,
    my_alias: String,
    idempotency_key: String,
}

impl RpcHandoffTransport {
    pub fn new(
        cluster_control: Arc<ClusterControl>,
        meta: Arc<ClusterMetaStore>,
        my_alias: String,
        idempotency_key: String,
    ) -> Self {
        Self {
            cluster_control,
            meta,
            my_alias,
            idempotency_key,
        }
    }

    /// Drive one control RPC to a peer, blocking the current (sync) handler thread. Must
    /// run from a sync context on the multi-thread runtime (the operator handler does).
    fn peer_rpc(
        &self,
        peer_alias: &str,
        key_suffix: &str,
        request: ControlRequest,
    ) -> Result<ControlResponse> {
        let key = format!("{}-{}", self.idempotency_key, key_suffix);
        let reply = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(self.cluster_control.rpc(peer_alias, &key, request))
        })?;
        Ok(reply.response)
    }
}

impl HandoffTransport for RpcHandoffTransport {
    fn alias_for_node(&self, node: NodeId) -> Result<String> {
        if node == OwnerRegistry::global().this_node() {
            return Ok(self.my_alias.clone());
        }
        self.cluster_control
            .configured_peers()
            .into_iter()
            .find_map(|(peer_node, alias)| (peer_node == node).then_some(alias))
            .with_context(|| format!("no cert-pinned control alias for owner node {node}"))
    }

    fn prepare_handoff(&self, source_alias: &str, scope_wire: &str, epoch: u64) -> Result<()> {
        if source_alias == self.my_alias {
            // This node is the source: retire locally (durable + in-memory), no RPC.
            let _tick_barrier = sentinel_common::owner_tick_barrier();
            let state = crate::owner_handler::persist_source_retirement(
                &self.meta,
                OwnerRegistry::global().this_node(),
                scope_wire,
                epoch,
            )
            .map_err(|reason| anyhow!(reason))?;
            OwnerRegistry::global().retire_local(state.scope, epoch);
            return Ok(());
        }
        match self.peer_rpc(
            source_alias,
            "prepare",
            ControlRequest::PrepareHandoff {
                scope: scope_wire.to_string(),
                epoch,
            },
        )? {
            ControlResponse::HandoffPrepared { .. } => Ok(()),
            other => Err(anyhow!(
                "PrepareHandoff to {source_alias} rejected: {other:?}"
            )),
        }
    }

    fn replicate_owner_snapshot(
        &self,
        node_alias: &str,
        global: &OwnerTermSnapshot,
        local: &LocalOwnerStateSnapshot,
    ) -> Result<()> {
        if node_alias == self.my_alias {
            let _tick_barrier = sentinel_common::owner_tick_barrier();
            OwnerRegistry::global().close_owner_readiness();
            match self.meta.install_owner_snapshot(global, local)? {
                OwnerSnapshotInstallOutcome::Installed
                | OwnerSnapshotInstallOutcome::AlreadyInstalled => {}
                outcome => anyhow::bail!("local owner snapshot install failed: {outcome:?}"),
            }
            OwnerRegistry::global().rebuild_from_owner_snapshot(
                global,
                local,
                self.meta.list_local_saga_states()?,
            )?;
            return Ok(());
        }
        let outcome = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(
                self.cluster_control.replicate_owner_snapshot(
                    node_alias,
                    global.clone(),
                    local.clone(),
                ),
            )
        })?;
        match outcome {
            OwnerSnapshotInstallOutcome::Installed
            | OwnerSnapshotInstallOutcome::AlreadyInstalled => Ok(()),
            outcome => {
                anyhow::bail!("owner snapshot replication to {node_alias} failed: {outcome:?}")
            }
        }
    }

    fn owner_commit(
        &self,
        target_alias: &str,
        scope_wire: &str,
        owner_node: NodeId,
        epoch: u64,
    ) -> Result<()> {
        if target_alias == self.my_alias {
            // The full snapshot is already installed. Complete only the target-local
            // handoff overlay and rebuild caches; never partially mutate authority.
            let scope = StateTransferScope::from_wire(scope_wire)
                .ok_or_else(|| anyhow!("unrecognized scope {scope_wire}"))?;
            let term = OwnerTerm {
                scope,
                owner_node,
                epoch,
                coordinator_generation: TRACK_A_COORDINATOR_GENERATION,
            };
            let _tick_barrier = sentinel_common::owner_tick_barrier();
            self.meta.complete_handoff_overlay(&term)?;
            let (global, local) = self
                .meta
                .installed_owner_snapshot()?
                .context("owner snapshot marker disappeared during local OwnerCommit")?;
            OwnerRegistry::global().rebuild_from_owner_snapshot(
                &global,
                &local,
                self.meta.list_local_saga_states()?,
            )?;
            return Ok(());
        }
        match self.peer_rpc(
            target_alias,
            "commit",
            ControlRequest::OwnerCommit {
                scope: scope_wire.to_string(),
                owner_node: owner_node.to_string(),
                epoch,
            },
        )? {
            ControlResponse::OwnerCommitted { .. } => Ok(()),
            other => Err(anyhow!("OwnerCommit to {target_alias} rejected: {other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::LocalOwnerRole;
    use std::sync::Mutex;

    fn node(b: u8) -> NodeId {
        NodeId(uuid::Uuid::from_bytes([b; 16]))
    }

    fn store(dir: &std::path::Path, name: &str) -> ClusterMetaStore {
        ClusterMetaStore::open(dir.join(name).to_str().unwrap()).unwrap()
    }

    fn install_initial(
        registry: &OwnerRegistry,
        meta: &ClusterMetaStore,
        owner: NodeId,
        scope: StateTransferScope,
    ) {
        let term = OwnerTerm {
            scope,
            owner_node: owner,
            epoch: 1,
            coordinator_generation: TRACK_A_COORDINATOR_GENERATION,
        };
        let global = OwnerTermSnapshot::new(TRACK_A_COORDINATOR_GENERATION, 1, vec![term]).unwrap();
        let local = local_snapshot_for(&global, registry.this_node()).unwrap();
        meta.install_owner_snapshot(&global, &local).unwrap();
        registry
            .rebuild_from_owner_snapshot(&global, &local, vec![])
            .unwrap();
    }

    /// A fake transport wiring the saga to two in-process "worlds": the source registry
    /// (which durably retires on `PrepareHandoff`) and the target registry (which commits
    /// on `OwnerCommit`). Records call order so the V1 invariant can be asserted.
    struct FakeCluster<'a> {
        source: &'a OwnerRegistry,
        source_meta: &'a ClusterMetaStore,
        target: &'a OwnerRegistry,
        target_meta: &'a ClusterMetaStore,
        fail_prepare: bool,
        log: Mutex<Vec<String>>,
    }

    impl HandoffTransport for FakeCluster<'_> {
        fn alias_for_node(&self, owner: NodeId) -> Result<String> {
            if owner == self.source.this_node() {
                Ok("node-1".into())
            } else if owner == self.target.this_node() {
                Ok("node-2".into())
            } else {
                anyhow::bail!("unknown fake node {owner}")
            }
        }

        fn prepare_handoff(&self, _src: &str, scope_wire: &str, epoch: u64) -> Result<()> {
            if self.fail_prepare {
                return Err(anyhow!("source unreachable"));
            }
            let scope = StateTransferScope::from_wire(scope_wire).unwrap();
            // Source durably retires (in-memory + persisted), then acks.
            self.source.retire_local(scope.clone(), epoch);
            self.source_meta
                .put_local_state(&self.source.local_owner_state(&scope).unwrap())?;
            self.log.lock().unwrap().push(format!("prepare@{epoch}"));
            Ok(())
        }

        fn replicate_owner_snapshot(
            &self,
            _alias: &str,
            global: &OwnerTermSnapshot,
            local: &LocalOwnerStateSnapshot,
        ) -> Result<()> {
            let (registry, meta) = if local.recipient_node == self.source.this_node() {
                (self.source, self.source_meta)
            } else if local.recipient_node == self.target.this_node() {
                (self.target, self.target_meta)
            } else {
                anyhow::bail!("unknown fake snapshot recipient")
            };
            assert!(matches!(
                meta.install_owner_snapshot(global, local)?,
                OwnerSnapshotInstallOutcome::Installed
                    | OwnerSnapshotInstallOutcome::AlreadyInstalled
            ));
            registry.rebuild_from_owner_snapshot(global, local, meta.list_local_saga_states()?)?;
            Ok(())
        }

        fn owner_commit(
            &self,
            _tgt: &str,
            scope_wire: &str,
            owner_node: NodeId,
            epoch: u64,
        ) -> Result<()> {
            // V1: a commit must never precede a durable prepare/ack.
            assert!(
                self.log
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| e.starts_with("prepare@")),
                "V1 violated: OwnerCommit before the durable SourceRetiredAck"
            );
            let scope = StateTransferScope::from_wire(scope_wire).unwrap();
            let term = OwnerTerm {
                scope,
                owner_node,
                epoch,
                coordinator_generation: TRACK_A_COORDINATOR_GENERATION,
            };
            self.target_meta.complete_handoff_overlay(&term)?;
            let (global, local) = self
                .target_meta
                .installed_owner_snapshot()?
                .context("target snapshot missing")?;
            self.target.rebuild_from_owner_snapshot(
                &global,
                &local,
                self.target_meta.list_local_saga_states()?,
            )?;
            self.log.lock().unwrap().push(format!("commit@{epoch}"));
            Ok(())
        }
    }

    #[test]
    fn handoff_moves_ownership_and_fences_the_source() {
        let dir = tempfile::tempdir().unwrap();
        // The seed (chef == source) owns the scope; hand it to a target node.
        let seed = OwnerRegistry::new_cluster_for_test(node(1));
        let seed_meta = store(dir.path(), "seed.redb");
        let target = OwnerRegistry::new_cluster_for_test(node(2));
        let target_meta = store(dir.path(), "target.redb");
        let scope = StateTransferScope::NanoContainer("AGENT-07".into());
        install_initial(&seed, &seed_meta, node(1), scope.clone());
        install_initial(&target, &target_meta, node(1), scope.clone());

        // The source owns the scope at epoch 1 and can write it.
        let source_guard = seed.issue(scope.clone()).unwrap();
        assert!(seed.validate(&source_guard).is_ok());

        let fake = FakeCluster {
            source: &seed,
            source_meta: &seed_meta,
            target: &target,
            target_meta: &target_meta,
            fail_prepare: false,
            log: Mutex::new(Vec::new()),
        };
        let req = HandoffRequest {
            scope: scope.clone(),
            source_alias: "node-1".into(),
            target_alias: "node-2".into(),
            target_node: node(2),
        };

        let outcome = run_handoff(&seed, &seed_meta, &fake, &req).unwrap();
        assert_eq!(outcome, HandoffOutcome::Committed { new_epoch: 2 });

        // V1 ordering: prepare happened before commit.
        assert_eq!(*fake.log.lock().unwrap(), vec!["prepare@1", "commit@2"]);

        // The source's old guard is now fenced (V19 term moved + V4 local retirement).
        assert!(seed.validate(&source_guard).is_err());
        assert_eq!(
            seed.local_owner_state(&scope).map(|s| s.role),
            Some(LocalOwnerRole::Retired)
        );
        // The target owns the scope at E+1 and can write it.
        let target_guard = target.issue(scope.clone()).unwrap();
        assert_eq!(target_guard.epoch(), 2);
        assert!(target.validate(&target_guard).is_ok());
        // Durable on both sides.
        assert_eq!(
            seed_meta
                .get_owner_term(&scope)
                .unwrap()
                .unwrap()
                .owner_node,
            node(2)
        );
        assert_eq!(
            target_meta
                .get_owner_term(&scope)
                .unwrap()
                .unwrap()
                .owner_node,
            node(2)
        );
    }

    #[test]
    fn handoff_aborts_without_steal_when_source_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let seed = OwnerRegistry::new_cluster_for_test(node(1));
        let seed_meta = store(dir.path(), "seed.redb");
        let target = OwnerRegistry::new_cluster_for_test(node(2));
        let target_meta = store(dir.path(), "target.redb");
        let scope = StateTransferScope::NanoContainer("AGENT-07".into());
        install_initial(&seed, &seed_meta, node(1), scope.clone());
        install_initial(&target, &target_meta, node(1), scope.clone());
        let source_guard = seed.issue(scope.clone()).unwrap();

        let fake = FakeCluster {
            source: &seed,
            source_meta: &seed_meta,
            target: &target,
            target_meta: &target_meta,
            fail_prepare: true, // source unreachable
            log: Mutex::new(Vec::new()),
        };
        let req = HandoffRequest {
            scope: scope.clone(),
            source_alias: "node-1".into(),
            target_alias: "node-2".into(),
            target_node: node(2),
        };

        let outcome = run_handoff(&seed, &seed_meta, &fake, &req).unwrap();
        // V2: aborted, no ownership change anywhere.
        assert_eq!(outcome, HandoffOutcome::AbortedSourceUnreachable);
        assert!(fake.log.lock().unwrap().is_empty());
        assert!(seed.validate(&source_guard).is_ok()); // source still owns + can write
        assert!(seed.local_owner_state(&scope).is_none()); // not retired
        assert_eq!(
            seed_meta
                .get_owner_term(&scope)
                .unwrap()
                .unwrap()
                .owner_node,
            node(1)
        );
        assert_eq!(
            target_meta
                .get_owner_term(&scope)
                .unwrap()
                .unwrap()
                .owner_node,
            node(1)
        );
    }

    #[test]
    fn handoff_rejects_source_alias_that_does_not_match_authoritative_owner() {
        let dir = tempfile::tempdir().unwrap();
        let seed = OwnerRegistry::new_cluster_for_test(node(1));
        let seed_meta = store(dir.path(), "seed.redb");
        let target = OwnerRegistry::new_cluster_for_test(node(2));
        let target_meta = store(dir.path(), "target.redb");
        let scope = StateTransferScope::NanoContainer("AGENT-07".into());
        install_initial(&seed, &seed_meta, node(1), scope.clone());
        install_initial(&target, &target_meta, node(1), scope.clone());
        let source_guard = seed.issue(scope.clone()).unwrap();
        let fake = FakeCluster {
            source: &seed,
            source_meta: &seed_meta,
            target: &target,
            target_meta: &target_meta,
            fail_prepare: false,
            log: Mutex::new(Vec::new()),
        };
        let req = HandoffRequest {
            scope: scope.clone(),
            source_alias: "node-2".into(),
            target_alias: "node-2".into(),
            target_node: node(2),
        };

        let error = run_handoff(&seed, &seed_meta, &fake, &req).unwrap_err();
        assert!(error
            .to_string()
            .contains("does not match authoritative owner"));
        assert!(fake.log.lock().unwrap().is_empty());
        assert!(seed.validate(&source_guard).is_ok());
        assert_eq!(
            seed_meta
                .get_owner_term(&scope)
                .unwrap()
                .unwrap()
                .owner_node,
            node(1)
        );
    }
}
