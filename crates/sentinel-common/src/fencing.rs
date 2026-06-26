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
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{OnceLock, RwLock};

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalOwnerState {
    pub scope: StateTransferScope,
    pub node_id: NodeId,
    pub epoch: u64,
    pub role: LocalOwnerRole,
}

/// The local role for a scope (V4). Single-node is always `Owner`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
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
/// `:2592`). In **single-node mode** the seed owns **every** scope, so every fenced
/// write passes and live behavior is unchanged. **Cluster mode** (entered the first
/// time [`commit_owner`](Self::commit_owner) runs — PR2b's cross-node handoff) tracks
/// the committed per-scope owner terms, and the old owner's guards then turn stale.
///
/// **Single-node fast path (V26):** `validate`/`current_owner` sit on the hot path of
/// every store write. An atomic `mode` flag is loaded `Acquire` (pairing with the
/// `Release` store in [`commit_owner`](Self::commit_owner)) and short-circuits the
/// `RwLock`/map lookup entirely while single-node — so the prod write path takes **no**
/// lock and is byte-for-byte the pre-cluster behavior (the acquire load is a plain load
/// on x86; one cheap load-acquire on weakly-ordered archs). Only after a real cross-node
/// commit does `validate` consult the term map under the read lock.
#[derive(Debug)]
pub struct OwnerRegistry {
    /// The node this registry acts for. Single-node: owns every scope at epoch 1.
    this_node: NodeId,
    /// Ownership mode (`MODE_SINGLE_NODE`/`MODE_CLUSTER`) — the V26 hot-path fast-path
    /// gate. Flipped to cluster the first time a term is committed; never flipped back
    /// (a node that has taken part in a handoff stays term-tracked).
    mode: AtomicU8,
    /// Committed per-scope owner terms — **only** consulted in cluster mode. Empty
    /// single-node (the seed synthesizes its ownership without touching this map).
    terms: RwLock<HashMap<StateTransferScope, OwnerTerm>>,
    /// Fast-path gate for the local-retirement fence (V4), same discipline as `mode`:
    /// `false` single-node, so `validate` skips the `local_state` lock entirely (V26).
    /// Set with `Release` once this node cooperatively retires a scope.
    has_local_retirement: AtomicBool,
    /// This node's local owner roles per scope (V4) — durable retirement so a source
    /// keeps rejecting its own writes to a handed-off scope even during a partition (when
    /// the chef's `OwnerCommit` to the new owner is invisible). Empty single-node.
    local_state: RwLock<HashMap<StateTransferScope, LocalOwnerState>>,
}

static GLOBAL: OnceLock<OwnerRegistry> = OnceLock::new();

/// The single-node owner epoch. Every scope the seed owns is committed at epoch 1; a
/// cross-node handoff (PR2b) is what advances an epoch and turns old guards stale.
const SINGLE_NODE_EPOCH: u64 = 1;

/// `mode`: the seed owns every scope; `validate` short-circuits without a lock (V26).
const MODE_SINGLE_NODE: u8 = 0;
/// `mode`: at least one cross-node term committed; `validate` consults the term map.
const MODE_CLUSTER: u8 = 1;

impl OwnerRegistry {
    /// The process-global registry. Defaults to single-node `OwnsAll` with a nil node
    /// id, so any code path (tests, benches, a not-yet-initialized daemon) gets a
    /// registry that owns every scope — preserving the pre-PR2 single-node behavior.
    /// The daemon calls [`init_single_node`](Self::init_single_node) at startup to pin
    /// the real seed identity.
    pub fn global() -> &'static OwnerRegistry {
        GLOBAL.get_or_init(|| OwnerRegistry::single_node(NodeId(uuid::Uuid::nil())))
    }

    /// Initialize the global registry as the single-node owner with the seed's real
    /// node id. Idempotent-ish: a no-op if the registry was already initialized
    /// (returns whether this call performed the initialization).
    pub fn init_single_node(this_node: NodeId) -> bool {
        GLOBAL.set(OwnerRegistry::single_node(this_node)).is_ok()
    }

    /// A single-node registry: the seed owns every scope at epoch 1, the term map is
    /// empty and the fast-path mode flag is set, so `validate` takes no lock.
    fn single_node(this_node: NodeId) -> Self {
        OwnerRegistry {
            this_node,
            mode: AtomicU8::new(MODE_SINGLE_NODE),
            terms: RwLock::new(HashMap::new()),
            has_local_retirement: AtomicBool::new(false),
            local_state: RwLock::new(HashMap::new()),
        }
    }

    /// Construct a standalone (non-global) single-node registry, for tests that need
    /// several independent registries (e.g. the in-process handoff saga's source/target
    /// "worlds"). Hidden from docs and not a production path — the daemon uses the
    /// process-global [`global`](Self::global) registry.
    #[doc(hidden)]
    pub fn new_for_test(this_node: NodeId) -> Self {
        Self::single_node(this_node)
    }

    /// The node id this registry acts for.
    pub fn this_node(&self) -> NodeId {
        self.this_node
    }

    /// Whether the registry has entered cluster mode (a cross-node term was committed).
    /// Single-node prod stays `false`, so the V26 fast path stays active.
    pub fn is_cluster_mode(&self) -> bool {
        self.mode.load(Ordering::Acquire) == MODE_CLUSTER
    }

    /// Commit a cross-node owner term (PR2b's handoff `OwnerCommit(E+1)`): record it in
    /// the in-memory term map and switch the registry to cluster mode so `validate`
    /// consults the map from now on. The old owner's guards for this scope turn stale (a
    /// higher epoch / different owner is now committed) — the first real cross-node
    /// reject path (V19). Durable persistence to the dedicated cluster-meta store
    /// (ADR-3) is the daemon handler's job: the registry in `sentinel-common`
    /// deliberately holds only the working view (it cannot depend on the redb store it
    /// is the authority for).
    pub fn commit_owner(&self, term: OwnerTerm) {
        // **Publish ordering (load-bearing under weak memory):** insert the term under
        // the write lock FIRST, then flip `mode` to cluster with `Release`. A concurrent
        // `validate`/`current_owner` that observes `mode == MODE_CLUSTER` via an
        // `Acquire` load is then guaranteed (release-acquire happens-before) to also see
        // this insert — it can never read cluster mode, miss the term, fall back to the
        // seed term and wrongly accept or reject a write (a silent split-brain window).
        // The reverse order (mode before insert) would expose exactly that window. See
        // `tests/loom_owner_ordering.rs` for the model-checked proof.
        {
            let mut terms = self.terms.write().expect("owner term map poisoned");
            terms.insert(term.scope.clone(), term);
        }
        self.mode.store(MODE_CLUSTER, Ordering::Release);
    }

    /// The current committed owner term for a scope. **Single-node fast path (V26):** a
    /// `Relaxed` load of `mode` short-circuits to the synthesized seed term without ever
    /// touching the `RwLock` — the prod write path is unchanged. In cluster mode the
    /// committed per-scope term is looked up under the read lock, falling back to the
    /// seed (epoch 1) for a scope no handoff has touched yet.
    pub fn current_owner(&self, scope: &StateTransferScope) -> OwnerTerm {
        // `Acquire` pairs with the `Release` in `commit_owner`: observing `MODE_CLUSTER`
        // guarantees the committed term is visible to the `terms.read()` below. On the
        // single-node fast path this is still a single lock-free load (identical to a
        // plain load on x86; one cheap load-acquire on weakly-ordered archs).
        if self.mode.load(Ordering::Acquire) == MODE_SINGLE_NODE {
            return self.seed_term(scope);
        }
        let terms = self.terms.read().expect("owner term map poisoned");
        terms
            .get(scope)
            .cloned()
            .unwrap_or_else(|| self.seed_term(scope))
    }

    /// The synthesized seed ownership of `scope`: this node owns it at epoch 1 — the
    /// default for every scope no cross-node handoff has committed.
    fn seed_term(&self, scope: &StateTransferScope) -> OwnerTerm {
        OwnerTerm {
            scope: scope.clone(),
            owner_node: self.this_node,
            epoch: SINGLE_NODE_EPOCH,
        }
    }

    /// Record that this node has locally **retired** `scope` at `epoch` (V4) — the source
    /// side of a cooperative handoff (`PrepareHandoff`). After this, `validate` rejects
    /// this node's own writes to `scope` at epoch ≤ `epoch`, **even if** a partition hides
    /// the chef's `OwnerCommit` to the new owner — the durable local fence that prevents
    /// the source from continuing to write a scope it gave up (silent split-brain). Same
    /// publish ordering as `commit_owner`: write the state, then flip the flag with
    /// `Release`. Durable persistence (ADR-3 `LOCAL_OWNER`) is the daemon's job; this is
    /// the in-memory working view.
    pub fn retire_local(&self, scope: StateTransferScope, epoch: u64) {
        {
            let mut ls = self
                .local_state
                .write()
                .expect("local owner state poisoned");
            ls.insert(
                scope.clone(),
                LocalOwnerState {
                    scope,
                    node_id: self.this_node,
                    epoch,
                    role: LocalOwnerRole::Retired,
                },
            );
        }
        self.has_local_retirement.store(true, Ordering::Release);
    }

    /// This node's local owner state for a scope, if any (V4). `None` on the fast path
    /// (no retirement recorded), avoiding the lock.
    pub fn local_owner_state(&self, scope: &StateTransferScope) -> Option<LocalOwnerState> {
        if !self.has_local_retirement.load(Ordering::Acquire) {
            return None;
        }
        self.local_state
            .read()
            .expect("local owner state poisoned")
            .get(scope)
            .cloned()
    }

    /// Re-establish durable local retirements at startup (V4) so a handed-off scope stays
    /// fenced across a restart. Sets the fast-path flag iff any retirement is present.
    pub fn restore_local_retirements(&self, states: Vec<LocalOwnerState>) {
        if states.is_empty() {
            return;
        }
        {
            let mut ls = self
                .local_state
                .write()
                .expect("local owner state poisoned");
            for s in states {
                ls.insert(s.scope.clone(), s);
            }
        }
        self.has_local_retirement.store(true, Ordering::Release);
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

    /// Re-check a guard against the current committed owner term (V19) **and** this
    /// node's local retirement (V4). Rejects a guard whose epoch is older than the
    /// committed epoch, that asserts a different owner node, or that targets a scope this
    /// node has locally retired, with [`StaleEpochError`]. Always `Ok` in single-node
    /// mode (no committed term advances, no local retirement is recorded).
    pub fn validate(&self, guard: &OwnerWriteGuard) -> Result<(), StaleEpochError> {
        let term = self.current_owner(&guard.scope);
        if guard.owner_node != term.owner_node || guard.epoch < term.epoch {
            return Err(StaleEpochError {
                scope: guard.scope.clone(),
                guard_epoch: guard.epoch,
                current_epoch: term.epoch,
            });
        }
        // Local-retirement fence (V4): if this node cooperatively retired the scope at an
        // epoch >= the guard's, it gave up ownership — reject even when the registry's
        // committed term has not caught up (partition). Skipped on the lock-free fast path
        // when no retirement exists (V26): the `Acquire` pairs with `retire_local`'s
        // `Release`.
        if self.has_local_retirement.load(Ordering::Acquire) {
            let ls = self.local_state.read().expect("local owner state poisoned");
            if let Some(state) = ls.get(&guard.scope) {
                if matches!(
                    state.role,
                    LocalOwnerRole::Retiring | LocalOwnerRole::Retired
                ) && guard.epoch <= state.epoch
                {
                    return Err(StaleEpochError {
                        scope: guard.scope.clone(),
                        guard_epoch: guard.epoch,
                        current_epoch: state.epoch + 1, // ownership moved to the new owner at E+1
                    });
                }
            }
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
        let reg = OwnerRegistry::single_node(node(0));
        assert!(!reg.is_cluster_mode());
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
        let reg = OwnerRegistry::single_node(node(1));
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

    /// #496 PR2b-2a: the cross-node term-tracking foundation. Two simulated owners A and
    /// B; a `commit_owner` (PR2b's `OwnerCommit(E+1)`) hands a scope from A to B and
    /// turns A's guard stale — the first real cross-node reject path. Single-node stays
    /// on the lock-free fast path until that commit.
    #[test]
    fn commit_owner_enters_cluster_mode_and_turns_old_guard_stale() {
        let scope = StateTransferScope::NanoContainer("AGENT-07".into());

        // A (this node) single-node: owns every scope, fast path active, guard validates.
        let a = OwnerRegistry::single_node(node(1));
        assert!(!a.is_cluster_mode());
        let a_guard = a.issue(scope.clone()); // A @ epoch 1
        assert!(a.validate(&a_guard).is_ok());

        // A cooperative handoff commits the scope to B at epoch 2.
        a.commit_owner(OwnerTerm {
            scope: scope.clone(),
            owner_node: node(2), // B
            epoch: 2,
        });
        assert!(a.is_cluster_mode());

        // A's old guard is now stale (different owner AND lower epoch) — V19 reject.
        let err = a.validate(&a_guard).unwrap_err();
        assert_eq!(err.guard_epoch, 1);
        assert_eq!(err.current_epoch, 2);
        assert_eq!(err.scope, scope);

        // B's registry, after the same commit, issues a guard at epoch 2 that validates.
        let b = OwnerRegistry::single_node(node(2));
        b.commit_owner(OwnerTerm {
            scope: scope.clone(),
            owner_node: node(2),
            epoch: 2,
        });
        let b_guard = b.issue(scope.clone());
        assert_eq!(b_guard.epoch(), 2);
        assert_eq!(b_guard.owner_node(), node(2));
        assert!(b.validate(&b_guard).is_ok());

        // A scope no handoff has touched falls back to the seed (epoch 1) even in
        // cluster mode — unmapped scopes default to this node's ownership.
        let untouched = StateTransferScope::World;
        assert!(b.validate(&b.issue(untouched)).is_ok());
    }

    /// #496 PR2b-2ii: the V4 local-retirement fence. After a node cooperatively retires a
    /// scope (the source side of a handoff), its own writes to that scope at the retired
    /// epoch are rejected — even without a committed term update (the partition-safe local
    /// fence). A non-retired scope is unaffected; the fast path stays off until the first
    /// retirement.
    #[test]
    fn retire_local_fences_the_source_at_the_retired_epoch() {
        let reg = OwnerRegistry::single_node(node(1));
        let scope = StateTransferScope::NanoContainer("AGENT-07".into());
        let other = StateTransferScope::World;

        // Before any retirement: fast path, the guard validates, no local state.
        let g = reg.issue(scope.clone()); // node(1) @ epoch 1
        assert!(reg.validate(&g).is_ok());
        assert!(reg.local_owner_state(&scope).is_none());

        // A cooperative handoff retires the scope at epoch 1 (source side).
        reg.retire_local(scope.clone(), 1);
        assert_eq!(
            reg.local_owner_state(&scope).map(|s| s.role),
            Some(LocalOwnerRole::Retired)
        );

        // The source's own guard (epoch 1) is now fenced — it gave up the scope.
        let err = reg.validate(&g).unwrap_err();
        assert_eq!(err.guard_epoch, 1);
        assert_eq!(err.current_epoch, 2); // the new owner is at E+1

        // A different scope this node never retired still validates.
        assert!(reg.validate(&reg.issue(other)).is_ok());
    }

    /// `restore_local_retirements` re-establishes the fence after a restart (durable V4).
    #[test]
    fn restore_local_retirements_re_fences_after_restart() {
        let reg = OwnerRegistry::single_node(node(1));
        let scope = StateTransferScope::NanoContainer("AGENT-03".into());
        let g = reg.issue(scope.clone());
        assert!(reg.validate(&g).is_ok());

        // Simulate reload from the durable LOCAL_OWNER table at startup.
        reg.restore_local_retirements(vec![LocalOwnerState {
            scope: scope.clone(),
            node_id: node(1),
            epoch: 1,
            role: LocalOwnerRole::Retired,
        }]);
        assert!(reg.validate(&g).is_err());
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
