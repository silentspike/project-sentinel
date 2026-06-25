//! Owner-write fencing primitives (#496, V3/V19).
//!
//! Every persistent store mutation must present an [`OwnerWriteGuard`] — the
//! capability proving this node owns the scope being written at a given owner epoch.
//! Stores expose **one** fenced write entry (`begin_fenced_write`) and keep their raw
//! transaction private, so a write cannot bypass the fence (the type system is the
//! strongest barrier; grep/lint are only a backstop).
//!
//! **Phase ordering (#496):** PR1a (this) is the behavior-preserving *strangler* step
//! — the stores route every writer through the single fenced entry, and the only
//! guard that exists is [`OwnerWriteGuard::unfenced`] (a node without an owner
//! registry, i.e. the current single-node behavior, which `begin_fenced_write`
//! accepts). PR2 adds the owner registry that mints real guards and makes the stores
//! compare the full owner term at *begin and commit* (V19, TOCTOU), rejecting a stale
//! epoch with [`StaleEpochError`] (split-brain / stale-write protection).

use crate::types::StateTransferScope;

/// A capability proving this node may mutate a fenced `scope` at an owner `epoch`
/// (V3). Constructed by the owner registry (PR2); PR1a ships only the
/// [`unfenced`](Self::unfenced) guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerWriteGuard {
    scope: StateTransferScope,
    epoch: u64,
}

impl OwnerWriteGuard {
    /// The PR1a strangler guard: a write on a node with no owner registry (the current
    /// single-node behavior). `begin_fenced_write` accepts it unconditionally. PR2
    /// replaces this with the registry-issued guard carrying the committed owner epoch.
    pub fn unfenced(scope: StateTransferScope) -> Self {
        Self { scope, epoch: 0 }
    }

    /// The scope this guard authorizes a write to.
    pub fn scope(&self) -> &StateTransferScope {
        &self.scope
    }

    /// The owner epoch this guard was issued at.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

/// A fenced write was rejected because the guard's owner epoch is older than the
/// scope's current committed epoch (V19) — the writer lost ownership. Inactive in
/// PR1a (no owner registry sets an epoch); the type is the contract PR2 enforces.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("stale owner epoch for {scope:?}: guard epoch {guard_epoch} < current committed epoch {current_epoch}")]
pub struct StaleEpochError {
    pub scope: StateTransferScope,
    pub guard_epoch: u64,
    pub current_epoch: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfenced_guard_carries_scope_and_zero_epoch() {
        let g = OwnerWriteGuard::unfenced(StateTransferScope::World);
        assert_eq!(g.scope(), &StateTransferScope::World);
        assert_eq!(g.epoch(), 0);
        let n = OwnerWriteGuard::unfenced(StateTransferScope::NanoContainer("a".into()));
        assert_eq!(n.scope(), &StateTransferScope::NanoContainer("a".into()));
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
