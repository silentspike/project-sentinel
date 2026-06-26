//! Owner-write fencing primitives (#496, V3/V19).
//!
//! Every persistent store mutation must present an [`OwnerWriteGuard`] — the
//! capability proving this node owns the scope being written at a given owner epoch.
//! Stores expose **one** fenced write entry (`begin_fenced_write`) and keep their raw
//! transaction private, so a write cannot bypass the fence (the type system is the
//! strongest barrier; grep/lint are only a backstop).
//!
//! **Phase ordering (#496):** PR1a/PR1b/PR1c were the behavior-preserving *strangler*
//! steps — the three stores route every writer through the single fenced entry under a
//! no-op guard. **PR2a (this) makes the fence real:** the [`OwnerRegistry`] mints
//! guards carrying the committed owner epoch for a scope, and `begin_fenced_write`
//! re-checks the guard against the registry, rejecting a stale or non-owning write with
//! [`StaleEpochError`] (split-brain / stale-write protection, V19). In single-node mode
//! the seed owns every scope, so every write passes — the live behavior is unchanged.
//! PR2b adds cooperative cross-node handoff (the registry then hands a scope to another
//! node at a higher epoch, and the old owner's guards turn stale).

use crate::cluster::NodeId;
use crate::types::StateTransferScope;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// The committed ownership of a scope: which node owns it, at which monotonic epoch.
/// Persisted (ADR-3 `CLUSTER_OWNER`) and replicated (PR2b); the in-memory copy here is
/// the registry's working view. `Serialize`/`Deserialize` so the dedicated cluster-meta
/// store can persist it durably across restarts (PR2b-1c).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerTerm {
    pub scope: StateTransferScope,
    pub owner_node: NodeId,
    pub epoch: u64,
}

/// A registry-issued capability proving **this** node may mutate `scope` at `epoch`
/// (V3). The fields are private and there is **no public constructor** — a guard can
/// only come from [`OwnerRegistry::issue`], so a mutating store path cannot be reached
/// without the registry agreeing this node owns the scope (the type system is the
/// barrier; the `check-fenced-writers` CI gate is the backstop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerWriteGuard {
    scope: StateTransferScope,
    owner_node: NodeId,
    epoch: u64,
}

impl OwnerWriteGuard {
    /// The scope this guard authorizes a write to.
    pub fn scope(&self) -> &StateTransferScope {
        &self.scope
    }

    /// The owner epoch this guard was issued at.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The node this guard asserts ownership for.
    pub fn owner_node(&self) -> NodeId {
        self.owner_node
    }

    /// Construct a guard directly, for tests of the fenced stores (e.g. the redb/fs
    /// commit-recheck path). This is **not** a production path — a real guard always
    /// comes from [`OwnerRegistry::issue`]. It is safe to expose because constructing a
    /// guard grants nothing: every fenced write re-validates the guard against the
    /// committed owner term at begin (and redb/fs again at commit), so a guard that does
    /// not match the registry is rejected. Hidden from the public docs.
    #[doc(hidden)]
    pub fn for_test(scope: StateTransferScope, owner_node: NodeId, epoch: u64) -> Self {
        Self {
            scope,
            owner_node,
            epoch,
        }
    }
}

/// A fenced write was rejected because the guard does not match the scope's current
/// committed owner term (V19) — the writer either lost ownership (a newer epoch
/// committed) or never owned the scope. Never fires in single-node mode (the seed owns
/// every scope); the contract PR2b's handoff exercises.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("stale owner term for {scope:?}: guard epoch {guard_epoch} < current committed epoch {current_epoch}")]
pub struct StaleEpochError {
    pub scope: StateTransferScope,
    pub guard_epoch: u64,
    pub current_epoch: u64,
}

/// A store's single fenced write entry, unifying the three persistence engines under
/// one capability-checked choke point. The transaction type differs per engine
/// (`limbo` holds a connection mutex guard, `redb`/`fs` a write transaction), so the
/// associated `Txn` is a GAT rather than a shared wrapper; the common contract is that
/// the guard is re-checked against the [`OwnerRegistry`] before any write is handed out.
pub trait FencedStore {
    /// The engine-specific write handle (borrowing `self`).
    type Txn<'a>
    where
        Self: 'a;

    /// Begin a fenced write: re-check `guard` against the current committed owner term
    /// (V19) and, if valid, return the engine write handle. A stale/non-owning guard is
    /// rejected with [`StaleEpochError`].
    fn begin_fenced_write(&self, guard: &OwnerWriteGuard) -> anyhow::Result<Self::Txn<'_>>;
}

/// What this node knows locally about a scope it owns or is retiring (V4). Durable per
/// node so a source can enforce its own retirement during a partition even if the
/// coordinator's update is invisible. PR2a only ever holds `Owner` (single-node); PR2b
/// drives the `Retiring`/`Retired` transitions during handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalOwnerState {
    pub scope: StateTransferScope,
    pub node_id: NodeId,
    pub epoch: u64,
    pub role: LocalOwnerRole,
}

/// The local role for a scope (V4). Single-node is always `Owner`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocalOwnerRole {
    #[default]
    Owner,
    Retiring,
    Retired,
    Follower,
    PreparedTarget,
}

/// Ownership authority: who owns which scope, at which epoch. The seed node ("chef")
/// is the sole authority that commits ownership (no Raft for agent state; TOGAF
/// `:2592`). PR2a runs it in single-node mode where the seed owns **every** scope, so
/// every fenced write passes and live behavior is unchanged; PR2b extends it to
/// cooperative cross-node handoff over the #569 control stream.
#[derive(Debug)]
pub struct OwnerRegistry {
    /// The node this registry acts for. In PR2a single-node mode it owns every scope;
    /// PR2b adds the committed per-scope term map + cluster mode for cross-node handoff.
    this_node: NodeId,
}

static GLOBAL: OnceLock<OwnerRegistry> = OnceLock::new();

/// The single-node owner epoch. Every scope the seed owns is committed at epoch 1; a
/// cross-node handoff (PR2b) is what advances an epoch and turns old guards stale.
const SINGLE_NODE_EPOCH: u64 = 1;

impl OwnerRegistry {
    /// The process-global registry. Defaults to single-node `OwnsAll` with a nil node
    /// id, so any code path (tests, benches, a not-yet-initialized daemon) gets a
    /// registry that owns every scope — preserving the pre-PR2 single-node behavior.
    /// The daemon calls [`init_single_node`](Self::init_single_node) at startup to pin
    /// the real seed identity.
    pub fn global() -> &'static OwnerRegistry {
        GLOBAL.get_or_init(OwnerRegistry::single_node_default)
    }

    /// Initialize the global registry as the single-node owner with the seed's real
    /// node id. Idempotent-ish: a no-op if the registry was already initialized
    /// (returns whether this call performed the initialization).
    pub fn init_single_node(this_node: NodeId) -> bool {
        GLOBAL.set(OwnerRegistry { this_node }).is_ok()
    }

    fn single_node_default() -> Self {
        OwnerRegistry {
            this_node: NodeId(uuid::Uuid::nil()),
        }
    }

    /// The node id this registry acts for.
    pub fn this_node(&self) -> NodeId {
        self.this_node
    }

    /// The current committed owner term for a scope. In PR2a single-node mode the seed
    /// owns every scope at the single-node epoch; PR2b looks up the committed per-scope
    /// term (falling back to the seed for the `World` scope) once cross-node handoff
    /// can hand a scope to another node at a higher epoch.
    pub fn current_owner(&self, scope: &StateTransferScope) -> OwnerTerm {
        OwnerTerm {
            scope: scope.clone(),
            owner_node: self.this_node,
            epoch: SINGLE_NODE_EPOCH,
        }
    }

    /// Mint a write guard for a scope this node owns. In single-node mode this always
    /// succeeds (the seed owns every scope); the guard carries the committed epoch so
    /// `begin_fenced_write`/commit can re-check it (V19).
    pub fn issue(&self, scope: StateTransferScope) -> OwnerWriteGuard {
        let term = self.current_owner(&scope);
        OwnerWriteGuard {
            scope,
            owner_node: term.owner_node,
            epoch: term.epoch,
        }
    }

    /// Re-check a guard against the current committed owner term (V19). Rejects a guard
    /// whose epoch is older than the committed epoch, or that asserts a different owner
    /// node, with [`StaleEpochError`]. Always `Ok` in single-node mode.
    pub fn validate(&self, guard: &OwnerWriteGuard) -> Result<(), StaleEpochError> {
        let term = self.current_owner(&guard.scope);
        if guard.owner_node != term.owner_node || guard.epoch < term.epoch {
            return Err(StaleEpochError {
                scope: guard.scope.clone(),
                guard_epoch: guard.epoch,
                current_epoch: term.epoch,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(b: u8) -> NodeId {
        NodeId(uuid::Uuid::from_bytes([b; 16]))
    }

    #[test]
    fn single_node_registry_owns_every_scope() {
        let reg = OwnerRegistry::single_node_default();
        // Issuing for any scope succeeds and the guard validates (0 stale in single-node).
        for scope in [
            StateTransferScope::World,
            StateTransferScope::NanoContainer("AGENT-07".into()),
            StateTransferScope::NanoContainer("anything".into()),
        ] {
            let g = reg.issue(scope.clone());
            assert_eq!(g.scope(), &scope);
            assert_eq!(g.epoch(), SINGLE_NODE_EPOCH);
            assert!(reg.validate(&g).is_ok());
        }
    }

    #[test]
    fn validate_rejects_stale_epoch_and_wrong_owner() {
        let reg = OwnerRegistry { this_node: node(1) };
        // A guard from an older epoch (epoch 0 < committed 1) is stale.
        let stale = OwnerWriteGuard::for_test(StateTransferScope::World, node(1), 0);
        let err = reg.validate(&stale).unwrap_err();
        assert_eq!(err.guard_epoch, 0);
        assert_eq!(err.current_epoch, SINGLE_NODE_EPOCH);
        // A guard asserting a different owner node is rejected too.
        let wrong =
            OwnerWriteGuard::for_test(StateTransferScope::World, node(2), SINGLE_NODE_EPOCH);
        assert!(reg.validate(&wrong).is_err());
        // The registry's own freshly-issued guard validates.
        assert!(reg.validate(&reg.issue(StateTransferScope::World)).is_ok());
    }

    #[test]
    fn stale_epoch_error_displays_scope_and_epochs() {
        let e = StaleEpochError {
            scope: StateTransferScope::World,
            guard_epoch: 2,
            current_epoch: 5,
        };
        let msg = e.to_string();
        assert!(msg.contains("guard epoch 2"));
        assert!(msg.contains("current committed epoch 5"));
    }
}
