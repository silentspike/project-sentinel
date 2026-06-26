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

use anyhow::{anyhow, Result};
use sentinel_common::{NodeId, OwnerRegistry, OwnerTerm, StateTransferScope};
use sentinel_redb::ClusterMetaStore;
use tracing::{info, warn};

/// The #569 control-RPC seam the saga drives.
pub trait HandoffTransport: Send + Sync {
    /// Send `PrepareHandoff(scope, epoch)` to the **source** (current owner) and return
    /// only once it has **durably retired** the scope (V1 — the durable SourceRetiredAck).
    /// An `Err` means the source is unreachable or refused: the saga aborts (V2 — no
    /// ownership steal).
    fn prepare_handoff(&self, source_alias: &str, scope_wire: &str, epoch: u64) -> Result<()>;

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
    let current = registry.current_owner(&req.scope);
    let source_epoch = current.epoch;
    let new_epoch = source_epoch
        .checked_add(1)
        .ok_or_else(|| anyhow!("owner epoch overflow for {scope_wire}"))?;

    // (V1, step 1) Ask the source to durably retire the scope at its current epoch. This
    // returns only after the durable SourceRetiredAck; an error => abort (V2).
    if let Err(e) = transport.prepare_handoff(&req.source_alias, &scope_wire, source_epoch) {
        warn!(
            scope = %scope_wire, source = %req.source_alias, error = %e,
            "Handoff aborted: source unreachable/refused — no ownership steal (V2)"
        );
        return Ok(HandoffOutcome::AbortedSourceUnreachable);
    }

    // (V1, step 2) Only now commit the new owner term (E+1): durable in the chef's
    // authority first, then the chef's in-memory registry, then propagate to the target.
    let term = OwnerTerm {
        scope: req.scope.clone(),
        owner_node: req.target_node,
        epoch: new_epoch,
    };
    meta.put_owner_term(&term)
        .map_err(|e| anyhow!("persist owner term for {scope_wire}: {e}"))?;
    registry.commit_owner(term);
    transport.owner_commit(&req.target_alias, &scope_wire, req.target_node, new_epoch)?;

    info!(
        scope = %scope_wire, source = %req.source_alias, target = %req.target_alias,
        new_epoch, "Handoff committed: ownership moved to target (V1)"
    );
    Ok(HandoffOutcome::Committed { new_epoch })
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
            };
            self.target_meta.put_owner_term(&term)?;
            self.target.commit_owner(term);
            self.log.lock().unwrap().push(format!("commit@{epoch}"));
            Ok(())
        }
    }

    #[test]
    fn handoff_moves_ownership_and_fences_the_source() {
        let dir = tempfile::tempdir().unwrap();
        // The seed (chef == source) owns the scope; hand it to a target node.
        let seed = OwnerRegistry::new_for_test(node(1));
        let seed_meta = store(dir.path(), "seed.redb");
        let target = OwnerRegistry::new_for_test(node(2));
        let target_meta = store(dir.path(), "target.redb");
        let scope = StateTransferScope::NanoContainer("AGENT-07".into());

        // The source owns the scope at epoch 1 and can write it.
        let source_guard = seed.issue(scope.clone());
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
        let target_guard = target.issue(scope.clone());
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
        let seed = OwnerRegistry::new_for_test(node(1));
        let seed_meta = store(dir.path(), "seed.redb");
        let target = OwnerRegistry::new_for_test(node(2));
        let target_meta = store(dir.path(), "target.redb");
        let scope = StateTransferScope::NanoContainer("AGENT-07".into());
        let source_guard = seed.issue(scope.clone());

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
        assert!(seed_meta.get_owner_term(&scope).unwrap().is_none()); // nothing committed
        assert!(target_meta.get_owner_term(&scope).unwrap().is_none());
    }
}
