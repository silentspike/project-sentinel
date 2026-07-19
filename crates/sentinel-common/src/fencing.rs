//! Owner-write fencing primitives (#496, V3/V19).
//!
//! Every persistent store mutation must present an [`OwnerWriteGuard`] — the
//! capability proving this node owns the scope being written at a given owner epoch.
//! Stores expose **one** fenced write entry (`begin_fenced_write`) and keep their raw
//! transaction private, so a write cannot bypass the fence (the type system is the
//! strongest barrier; grep/lint are only a backstop).
//!
//! The #615 fail-closed bootstrap extends #496's fencing choke point: single-node mode
//! keeps the lock-free owns-all fast path, while explicit cluster mode starts closed and
//! can mint guards only after a canonical full authority/local-state snapshot rebuild.
//! Begin and commit recheck scope, node, epoch, coordinator generation, local role, and
//! activation, rejecting stale or non-owning writes with [`StaleEpochError`].

use crate::cluster::NodeId;
use crate::types::StateTransferScope;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, RwLock};

/// Serializes a daemon ECS tick with owner-authority installation and cache activation.
///
/// The tick loop holds this guard only while it can observe or mutate agent state. A
/// cluster control handler takes the same guard before it closes readiness and changes
/// the durable/effective owner view. Keeping the primitive in `sentinel-common` avoids
/// a dependency from the control-plane crate back into the daemon.
static OWNER_TICK_BARRIER: Mutex<()> = Mutex::new(());

pub fn owner_tick_barrier() -> MutexGuard<'static, ()> {
    OWNER_TICK_BARRIER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The committed ownership of a scope: which node owns it, at which monotonic epoch.
/// Persisted (ADR-3 `CLUSTER_OWNER`) and replicated (PR2b); the in-memory copy here is
/// the registry's working view. `Serialize`/`Deserialize` so the dedicated cluster-meta
/// store can persist it durably across restarts (PR2b-1c).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerTerm {
    pub scope: StateTransferScope,
    pub owner_node: NodeId,
    pub epoch: u64,
    /// Identifies the coordinator authority line. Generation zero is legacy data;
    /// Track A installs and validates generation one.
    #[serde(default)]
    pub coordinator_generation: u64,
}

pub const TRACK_A_COORDINATOR_GENERATION: u64 = 1;
pub const OWNER_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Whether an owner is allowed to publish routes and issue normal write guards.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivationState {
    #[default]
    LegacyUnknown,
    NotRoutable,
    Routable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalOwnerBaseRole {
    Owner,
    Follower,
}

/// Recipient-bound stable local state installed with the global owner snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalOwnerBaseState {
    pub scope: StateTransferScope,
    pub recipient_node: NodeId,
    pub owner_term: OwnerTerm,
    pub base_role: LocalOwnerBaseRole,
    #[serde(default)]
    pub activation_state: ActivationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalOwnerOperationKind {
    LegacyReconciliation,
    Handoff,
    Migration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalOwnerSagaRole {
    Retiring,
    Retired,
    PreparedTarget,
    OwnerActivating,
}

/// Process residency derived from the durable recipient-local base state plus the
/// scope-keyed saga overlay. Runtime code must consult this before spawning an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalResidency {
    Active,
    PreparedFrozen,
    Absent,
}

/// Scope-keyed overlay owned by one active handoff or migration operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalOwnerSagaState {
    pub scope: StateTransferScope,
    pub operation_kind: LocalOwnerOperationKind,
    pub op_id: Option<uuid::Uuid>,
    pub owner_term: OwnerTerm,
    pub role: LocalOwnerSagaRole,
    pub transition_seq: u64,
}

/// Canonical, full global authority snapshot replicated by the chef.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerTermSnapshot {
    pub schema_version: u32,
    pub coordinator_generation: u64,
    pub term_snapshot_revision: u64,
    pub sorted_terms: Vec<OwnerTerm>,
    pub checksum: [u8; 32],
}

/// Recipient-bound local base-state snapshot paired with an [`OwnerTermSnapshot`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalOwnerStateSnapshot {
    pub schema_version: u32,
    pub recipient_node: NodeId,
    pub coordinator_generation: u64,
    pub term_snapshot_revision: u64,
    pub sorted_base_states: Vec<LocalOwnerBaseState>,
    pub checksum: [u8; 32],
}

/// Deterministic result of installing a full owner snapshot pair. This lives in
/// `sentinel-common` so the durable store and authenticated control wire share one type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerSnapshotInstallOutcome {
    Installed,
    AlreadyInstalled,
    StaleSnapshot {
        installed_revision: u64,
        received_revision: u64,
    },
    GenerationMismatch {
        installed_generation: u64,
        received_generation: u64,
    },
    SnapshotConflict,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OwnerSnapshotError {
    #[error("unsupported owner snapshot schema {0}")]
    UnsupportedSchema(u32),
    #[error("owner snapshot rows are not sorted by canonical scope")]
    UnsortedRows,
    #[error("duplicate owner snapshot scope {0}")]
    DuplicateScope(String),
    #[error("term generation does not match snapshot generation for {0}")]
    TermGenerationMismatch(String),
    #[error("local snapshot recipient does not match row for {0}")]
    RecipientMismatch(String),
    #[error("local owner term does not match global term for {0}")]
    LocalTermMismatch(String),
    #[error("recipient-local owner role does not match global owner for {0}")]
    LocalRoleMismatch(String),
    #[error("legacy activation is not valid in a Track-A owner snapshot for {0}")]
    LegacyActivation(String),
    #[error("owner snapshot checksum mismatch")]
    ChecksumMismatch,
    #[error("global and local snapshot envelopes do not match")]
    EnvelopeMismatch,
    #[error("owner snapshot field exceeds canonical codec limit")]
    FieldTooLarge,
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_scope(out: &mut Vec<u8>, scope: &StateTransferScope) -> Result<(), OwnerSnapshotError> {
    let wire = scope.to_wire();
    let len = u32::try_from(wire.len()).map_err(|_| OwnerSnapshotError::FieldTooLarge)?;
    put_u32(out, len);
    out.extend_from_slice(wire.as_bytes());
    Ok(())
}

fn put_term(out: &mut Vec<u8>, term: &OwnerTerm) -> Result<(), OwnerSnapshotError> {
    put_scope(out, &term.scope)?;
    out.extend_from_slice(term.owner_node.0.as_bytes());
    put_u64(out, term.epoch);
    put_u64(out, term.coordinator_generation);
    Ok(())
}

fn rows_are_strictly_sorted<T>(rows: &[T], scope: impl Fn(&T) -> &StateTransferScope) -> bool {
    rows.windows(2)
        .all(|pair| scope(&pair[0]).to_wire() < scope(&pair[1]).to_wire())
}

impl OwnerTermSnapshot {
    pub fn new(
        coordinator_generation: u64,
        term_snapshot_revision: u64,
        mut terms: Vec<OwnerTerm>,
    ) -> Result<Self, OwnerSnapshotError> {
        terms.sort_by_key(|term| term.scope.to_wire());
        let mut snapshot = Self {
            schema_version: OWNER_SNAPSHOT_SCHEMA_VERSION,
            coordinator_generation,
            term_snapshot_revision,
            sorted_terms: terms,
            checksum: [0; 32],
        };
        snapshot.validate_rows()?;
        snapshot.checksum = Sha256::digest(snapshot.canonical_payload()?).into();
        Ok(snapshot)
    }

    pub fn canonical_payload(&self) -> Result<Vec<u8>, OwnerSnapshotError> {
        let mut out = Vec::new();
        put_u32(&mut out, self.schema_version);
        put_u64(&mut out, self.coordinator_generation);
        put_u64(&mut out, self.term_snapshot_revision);
        put_u32(
            &mut out,
            u32::try_from(self.sorted_terms.len())
                .map_err(|_| OwnerSnapshotError::FieldTooLarge)?,
        );
        for term in &self.sorted_terms {
            put_term(&mut out, term)?;
        }
        Ok(out)
    }

    fn validate_rows(&self) -> Result<(), OwnerSnapshotError> {
        if self.schema_version != OWNER_SNAPSHOT_SCHEMA_VERSION {
            return Err(OwnerSnapshotError::UnsupportedSchema(self.schema_version));
        }
        if !rows_are_strictly_sorted(&self.sorted_terms, |term| &term.scope) {
            let mut keys = self.sorted_terms.iter().map(|t| t.scope.to_wire());
            if let Some(first) = keys.next() {
                let mut previous = first;
                for key in keys {
                    if key == previous {
                        return Err(OwnerSnapshotError::DuplicateScope(key));
                    }
                    previous = key;
                }
            }
            return Err(OwnerSnapshotError::UnsortedRows);
        }
        for term in &self.sorted_terms {
            if term.coordinator_generation != self.coordinator_generation {
                return Err(OwnerSnapshotError::TermGenerationMismatch(
                    term.scope.to_wire(),
                ));
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), OwnerSnapshotError> {
        self.validate_rows()?;
        let checksum: [u8; 32] = Sha256::digest(self.canonical_payload()?).into();
        if checksum != self.checksum {
            return Err(OwnerSnapshotError::ChecksumMismatch);
        }
        Ok(())
    }
}

impl LocalOwnerStateSnapshot {
    pub fn new(
        recipient_node: NodeId,
        coordinator_generation: u64,
        term_snapshot_revision: u64,
        mut states: Vec<LocalOwnerBaseState>,
    ) -> Result<Self, OwnerSnapshotError> {
        states.sort_by_key(|state| state.scope.to_wire());
        let mut snapshot = Self {
            schema_version: OWNER_SNAPSHOT_SCHEMA_VERSION,
            recipient_node,
            coordinator_generation,
            term_snapshot_revision,
            sorted_base_states: states,
            checksum: [0; 32],
        };
        snapshot.validate_rows()?;
        snapshot.checksum = Sha256::digest(snapshot.canonical_payload()?).into();
        Ok(snapshot)
    }

    pub fn canonical_payload(&self) -> Result<Vec<u8>, OwnerSnapshotError> {
        let mut out = Vec::new();
        put_u32(&mut out, self.schema_version);
        out.extend_from_slice(self.recipient_node.0.as_bytes());
        put_u64(&mut out, self.coordinator_generation);
        put_u64(&mut out, self.term_snapshot_revision);
        put_u32(
            &mut out,
            u32::try_from(self.sorted_base_states.len())
                .map_err(|_| OwnerSnapshotError::FieldTooLarge)?,
        );
        for state in &self.sorted_base_states {
            put_scope(&mut out, &state.scope)?;
            out.extend_from_slice(state.recipient_node.0.as_bytes());
            put_term(&mut out, &state.owner_term)?;
            out.push(match state.base_role {
                LocalOwnerBaseRole::Owner => 1,
                LocalOwnerBaseRole::Follower => 2,
            });
            out.push(match state.activation_state {
                ActivationState::LegacyUnknown => 0,
                ActivationState::NotRoutable => 1,
                ActivationState::Routable => 2,
            });
        }
        Ok(out)
    }

    fn validate_rows(&self) -> Result<(), OwnerSnapshotError> {
        if self.schema_version != OWNER_SNAPSHOT_SCHEMA_VERSION {
            return Err(OwnerSnapshotError::UnsupportedSchema(self.schema_version));
        }
        if !rows_are_strictly_sorted(&self.sorted_base_states, |state| &state.scope) {
            let mut keys = self
                .sorted_base_states
                .iter()
                .map(|state| state.scope.to_wire());
            if let Some(first) = keys.next() {
                let mut previous = first;
                for key in keys {
                    if key == previous {
                        return Err(OwnerSnapshotError::DuplicateScope(key));
                    }
                    previous = key;
                }
            }
            return Err(OwnerSnapshotError::UnsortedRows);
        }
        for state in &self.sorted_base_states {
            let scope = state.scope.to_wire();
            if state.recipient_node != self.recipient_node {
                return Err(OwnerSnapshotError::RecipientMismatch(scope));
            }
            if state.owner_term.scope != state.scope
                || state.owner_term.coordinator_generation != self.coordinator_generation
            {
                return Err(OwnerSnapshotError::LocalTermMismatch(scope));
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), OwnerSnapshotError> {
        self.validate_rows()?;
        let checksum: [u8; 32] = Sha256::digest(self.canonical_payload()?).into();
        if checksum != self.checksum {
            return Err(OwnerSnapshotError::ChecksumMismatch);
        }
        Ok(())
    }
}

pub fn validate_owner_snapshot_pair(
    global: &OwnerTermSnapshot,
    local: &LocalOwnerStateSnapshot,
) -> Result<(), OwnerSnapshotError> {
    global.validate()?;
    local.validate()?;
    if global.schema_version != local.schema_version
        || global.coordinator_generation != local.coordinator_generation
        || global.term_snapshot_revision != local.term_snapshot_revision
    {
        return Err(OwnerSnapshotError::EnvelopeMismatch);
    }
    if global.sorted_terms.len() != local.sorted_base_states.len() {
        return Err(OwnerSnapshotError::EnvelopeMismatch);
    }
    let global_terms: HashMap<_, _> = global
        .sorted_terms
        .iter()
        .map(|term| (&term.scope, term))
        .collect();
    for state in &local.sorted_base_states {
        if global_terms.get(&state.scope).copied() != Some(&state.owner_term) {
            return Err(OwnerSnapshotError::LocalTermMismatch(state.scope.to_wire()));
        }
        let recipient_is_owner = state.owner_term.owner_node == local.recipient_node;
        if recipient_is_owner != (state.base_role == LocalOwnerBaseRole::Owner) {
            return Err(OwnerSnapshotError::LocalRoleMismatch(state.scope.to_wire()));
        }
        if state.activation_state == ActivationState::LegacyUnknown {
            return Err(OwnerSnapshotError::LegacyActivation(state.scope.to_wire()));
        }
        if state.base_role == LocalOwnerBaseRole::Follower
            && state.activation_state != ActivationState::NotRoutable
        {
            return Err(OwnerSnapshotError::LocalRoleMismatch(state.scope.to_wire()));
        }
    }
    Ok(())
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
    coordinator_generation: u64,
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

    pub fn coordinator_generation(&self) -> u64 {
        self.coordinator_generation
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
            coordinator_generation: 0,
        }
    }

    #[doc(hidden)]
    pub fn for_test_full(
        scope: StateTransferScope,
        owner_node: NodeId,
        epoch: u64,
        coordinator_generation: u64,
    ) -> Self {
        Self {
            scope,
            owner_node,
            epoch,
            coordinator_generation,
        }
    }
}

/// Typed fail-closed reasons for guard issuance and begin/commit revalidation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OwnerIssueError {
    #[error("owner readiness is closed for {scope:?}")]
    ReadinessClosed { scope: StateTransferScope },
    #[error("owner scope is not materialized: {scope:?}")]
    UnknownScope { scope: StateTransferScope },
    #[error("owner generation mismatch for {scope:?}: expected {expected}, actual {actual}")]
    GenerationMismatch {
        scope: StateTransferScope,
        expected: u64,
        actual: u64,
    },
    #[error("node {this_node} is not owner of {scope:?}; owner is {owner_node}")]
    NotOwner {
        scope: StateTransferScope,
        this_node: NodeId,
        owner_node: NodeId,
    },
    #[error("local base role is not Owner for {scope:?}")]
    RoleNotOwner { scope: StateTransferScope },
    #[error("owner activation is not routable for {scope:?}: {activation:?}")]
    NotRoutable {
        scope: StateTransferScope,
        activation: ActivationState,
    },
    #[error("active local saga overlay blocks normal writes for {scope:?}: {role:?}")]
    SagaOverlay {
        scope: StateTransferScope,
        role: LocalOwnerSagaRole,
    },
    #[error("owner term changed for {scope:?}")]
    TermChanged {
        scope: StateTransferScope,
        guard_owner: NodeId,
        guard_epoch: u64,
        guard_generation: u64,
        current_owner: NodeId,
        current_epoch: u64,
        current_generation: u64,
    },
}

/// Backward-compatible name used by existing store documentation and callers.
pub type StaleEpochError = OwnerIssueError;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalOwnerRole {
    Owner,
    Retiring,
    Retired,
    Follower,
    PreparedTarget,
    OwnerActivating,
}

/// Ownership authority: who owns which scope, at which epoch. The seed node ("chef")
/// is the sole authority that commits ownership (no Raft for agent state; TOGAF
/// `:2592`). In **single-node mode** the seed owns **every** scope, so every fenced
/// write passes and live behavior is unchanged. **Cluster mode** is entered explicitly
/// at bootstrap and tracks only scopes installed by the chef's full snapshot; unknown
/// scopes never synthesize self-ownership.
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
    /// gate. Explicit cluster bootstrap starts in cluster mode and never flips back.
    mode: AtomicU8,
    /// Committed per-scope owner terms — **only** consulted in cluster mode. Empty
    /// single-node (the seed synthesizes its ownership without touching this map).
    terms: RwLock<HashMap<StateTransferScope, OwnerTerm>>,
    /// Recipient-local base states installed from the same atomic snapshot as `terms`.
    base_states: RwLock<HashMap<StateTransferScope, LocalOwnerBaseState>>,
    /// Scope-keyed handoff/migration overlays. Snapshot installation never clears these.
    saga_states: RwLock<HashMap<StateTransferScope, LocalOwnerSagaState>>,
    /// Boot-local owner/activation latch. Every cluster process starts closed and only
    /// opens after a valid durable marker and cache rebuild (or a fresh full snapshot).
    readiness: AtomicBool,
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
            base_states: RwLock::new(HashMap::new()),
            saga_states: RwLock::new(HashMap::new()),
            readiness: AtomicBool::new(true),
        }
    }

    /// A cluster registry starts as a follower with no synthesized ownership and a
    /// closed boot-local latch.
    fn cluster_node(this_node: NodeId) -> Self {
        OwnerRegistry {
            this_node,
            mode: AtomicU8::new(MODE_CLUSTER),
            terms: RwLock::new(HashMap::new()),
            base_states: RwLock::new(HashMap::new()),
            saga_states: RwLock::new(HashMap::new()),
            readiness: AtomicBool::new(false),
        }
    }

    /// Initialize the process-global registry for explicit cluster mode. Unlike
    /// `init_single_node`, this never grants implicit ownership.
    pub fn init_cluster(this_node: NodeId) -> bool {
        GLOBAL.set(OwnerRegistry::cluster_node(this_node)).is_ok()
    }

    /// Construct a standalone (non-global) single-node registry, for tests that need
    /// several independent registries (e.g. the in-process handoff saga's source/target
    /// "worlds"). Hidden from docs and not a production path — the daemon uses the
    /// process-global [`global`](Self::global) registry.
    #[doc(hidden)]
    pub fn new_for_test(this_node: NodeId) -> Self {
        Self::single_node(this_node)
    }

    #[doc(hidden)]
    pub fn new_cluster_for_test(this_node: NodeId) -> Self {
        Self::cluster_node(this_node)
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

    pub fn owner_readiness(&self) -> bool {
        self.readiness.load(Ordering::Acquire)
    }

    pub fn close_owner_readiness(&self) {
        if self.is_cluster_mode() {
            self.readiness.store(false, Ordering::Release);
        }
    }

    /// Replace the in-memory global and recipient-local base view and open readiness
    /// only after all maps are visible. The durable store validates and atomically
    /// installs the pair before calling this under the daemon's tick barrier.
    pub fn rebuild_from_owner_snapshot(
        &self,
        global: &OwnerTermSnapshot,
        local: &LocalOwnerStateSnapshot,
        saga_states: Vec<LocalOwnerSagaState>,
    ) -> Result<(), OwnerSnapshotError> {
        validate_owner_snapshot_pair(global, local)?;
        if local.recipient_node != self.this_node {
            return Err(OwnerSnapshotError::RecipientMismatch("envelope".into()));
        }
        if global.coordinator_generation != TRACK_A_COORDINATOR_GENERATION {
            return Err(OwnerSnapshotError::TermGenerationMismatch(
                "envelope".into(),
            ));
        }

        self.readiness.store(false, Ordering::Release);
        {
            let mut terms = self.terms.write().expect("owner term map poisoned");
            terms.clear();
            terms.extend(
                global
                    .sorted_terms
                    .iter()
                    .cloned()
                    .map(|term| (term.scope.clone(), term)),
            );
        }
        {
            let mut bases = self
                .base_states
                .write()
                .expect("local owner base map poisoned");
            bases.clear();
            bases.extend(
                local
                    .sorted_base_states
                    .iter()
                    .cloned()
                    .map(|state| (state.scope.clone(), state)),
            );
        }
        {
            let mut overlays = self
                .saga_states
                .write()
                .expect("local owner saga map poisoned");
            overlays.clear();
            overlays.extend(
                saga_states
                    .into_iter()
                    .map(|state| (state.scope.clone(), state)),
            );
        }
        self.mode.store(MODE_CLUSTER, Ordering::Release);
        self.readiness.store(true, Ordering::Release);
        Ok(())
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
    /// installed full-snapshot term. Unknown scopes are rejected.
    pub fn current_owner(&self, scope: &StateTransferScope) -> Result<OwnerTerm, OwnerIssueError> {
        // `Acquire` pairs with the `Release` in `commit_owner`: observing `MODE_CLUSTER`
        // guarantees the committed term is visible to the `terms.read()` below. On the
        // single-node fast path this is still a single lock-free load (identical to a
        // plain load on x86; one cheap load-acquire on weakly-ordered archs).
        if self.mode.load(Ordering::Acquire) == MODE_SINGLE_NODE {
            return Ok(self.seed_term(scope));
        }
        let terms = self.terms.read().expect("owner term map poisoned");
        terms
            .get(scope)
            .cloned()
            .ok_or_else(|| OwnerIssueError::UnknownScope {
                scope: scope.clone(),
            })
    }

    /// The synthesized seed ownership of `scope`: this node owns it at epoch 1 — the
    /// default for every scope no cross-node handoff has committed.
    fn seed_term(&self, scope: &StateTransferScope) -> OwnerTerm {
        OwnerTerm {
            scope: scope.clone(),
            owner_node: self.this_node,
            epoch: SINGLE_NODE_EPOCH,
            coordinator_generation: 0,
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
        let term = self.current_owner(&scope).unwrap_or_else(|_| OwnerTerm {
            scope: scope.clone(),
            owner_node: self.this_node,
            epoch,
            coordinator_generation: if self.is_cluster_mode() {
                TRACK_A_COORDINATOR_GENERATION
            } else {
                0
            },
        });
        self.saga_states
            .write()
            .expect("local owner saga map poisoned")
            .insert(
                scope.clone(),
                LocalOwnerSagaState {
                    scope,
                    operation_kind: LocalOwnerOperationKind::Handoff,
                    op_id: None,
                    owner_term: term,
                    role: LocalOwnerSagaRole::Retired,
                    transition_seq: 0,
                },
            );
    }

    /// This node's local owner state for a scope, if any (V4). `None` on the fast path
    /// (no retirement recorded), avoiding the lock.
    pub fn local_owner_state(&self, scope: &StateTransferScope) -> Option<LocalOwnerState> {
        self.saga_states
            .read()
            .expect("local owner saga map poisoned")
            .get(scope)
            .map(|state| LocalOwnerState {
                scope: state.scope.clone(),
                node_id: self.this_node,
                epoch: state.owner_term.epoch,
                role: match state.role {
                    LocalOwnerSagaRole::Retiring => LocalOwnerRole::Retiring,
                    LocalOwnerSagaRole::Retired => LocalOwnerRole::Retired,
                    LocalOwnerSagaRole::PreparedTarget => LocalOwnerRole::PreparedTarget,
                    LocalOwnerSagaRole::OwnerActivating => LocalOwnerRole::OwnerActivating,
                },
            })
    }

    /// Resolve the fail-closed runtime residency for one scope. Unknown scopes,
    /// unreadiness, legacy activation and an Owner/NotRoutable base without the
    /// corresponding PreparedTarget overlay are typed errors rather than spawn hints.
    pub fn local_residency(
        &self,
        scope: &StateTransferScope,
    ) -> Result<LocalResidency, OwnerIssueError> {
        if !self.is_cluster_mode() {
            return Ok(LocalResidency::Active);
        }
        if !self.readiness.load(Ordering::Acquire) {
            return Err(OwnerIssueError::ReadinessClosed {
                scope: scope.clone(),
            });
        }
        // Materialization in the global authority snapshot is mandatory even when a
        // legacy or active saga overlay exists. An overlay may refine residency, but
        // it may never synthesize an unknown scope.
        let term = self.current_owner(scope)?;
        if let Some(overlay) = self
            .saga_states
            .read()
            .expect("local owner saga map poisoned")
            .get(scope)
        {
            return Ok(match overlay.role {
                LocalOwnerSagaRole::PreparedTarget => LocalResidency::PreparedFrozen,
                LocalOwnerSagaRole::Retiring
                | LocalOwnerSagaRole::Retired
                | LocalOwnerSagaRole::OwnerActivating => LocalResidency::Absent,
            });
        }
        let bases = self
            .base_states
            .read()
            .expect("local owner base map poisoned");
        let base = bases
            .get(scope)
            .ok_or_else(|| OwnerIssueError::RoleNotOwner {
                scope: scope.clone(),
            })?;
        if base.recipient_node != self.this_node || base.owner_term != term {
            return Err(OwnerIssueError::RoleNotOwner {
                scope: scope.clone(),
            });
        }
        match (base.base_role, base.activation_state) {
            (LocalOwnerBaseRole::Owner, ActivationState::Routable) => Ok(LocalResidency::Active),
            (LocalOwnerBaseRole::Follower, _) => Ok(LocalResidency::Absent),
            (LocalOwnerBaseRole::Owner, activation) => Err(OwnerIssueError::NotRoutable {
                scope: scope.clone(),
                activation,
            }),
        }
    }

    /// Re-establish durable local retirements at startup (V4) so a handed-off scope stays
    /// fenced across a restart. Sets the fast-path flag iff any retirement is present.
    pub fn restore_local_retirements(&self, states: Vec<LocalOwnerState>) {
        let mut overlays = self
            .saga_states
            .write()
            .expect("local owner saga map poisoned");
        for state in states {
            let role = match state.role {
                LocalOwnerRole::Retiring => LocalOwnerSagaRole::Retiring,
                LocalOwnerRole::Retired => LocalOwnerSagaRole::Retired,
                LocalOwnerRole::PreparedTarget => LocalOwnerSagaRole::PreparedTarget,
                LocalOwnerRole::OwnerActivating => LocalOwnerSagaRole::OwnerActivating,
                LocalOwnerRole::Owner | LocalOwnerRole::Follower => continue,
            };
            let owner_term = self
                .terms
                .read()
                .expect("owner term map poisoned")
                .get(&state.scope)
                .cloned()
                .unwrap_or(OwnerTerm {
                    scope: state.scope.clone(),
                    owner_node: state.node_id,
                    epoch: state.epoch,
                    coordinator_generation: 0,
                });
            overlays.insert(
                state.scope.clone(),
                LocalOwnerSagaState {
                    scope: state.scope,
                    operation_kind: LocalOwnerOperationKind::LegacyReconciliation,
                    op_id: None,
                    owner_term,
                    role,
                    transition_seq: 0,
                },
            );
        }
    }

    /// Mint a write guard for a scope this node owns. In single-node mode this always
    /// succeeds (the seed owns every scope); the guard carries the committed epoch so
    /// `begin_fenced_write`/commit can re-check it (V19).
    pub fn issue(&self, scope: StateTransferScope) -> Result<OwnerWriteGuard, OwnerIssueError> {
        if self.is_cluster_mode() && !self.readiness.load(Ordering::Acquire) {
            return Err(OwnerIssueError::ReadinessClosed { scope });
        }
        let term = self.current_owner(&scope)?;
        let guard = OwnerWriteGuard {
            scope,
            owner_node: term.owner_node,
            epoch: term.epoch,
            coordinator_generation: term.coordinator_generation,
        };
        self.validate(&guard)?;
        Ok(guard)
    }

    /// Re-check a guard against the current committed owner term (V19) **and** this
    /// node's local retirement (V4). Rejects a guard whose epoch is older than the
    /// committed epoch, that asserts a different owner node, or that targets a scope this
    /// node has locally retired, with [`StaleEpochError`]. Always `Ok` in single-node
    /// mode (no committed term advances, no local retirement is recorded).
    pub fn validate(&self, guard: &OwnerWriteGuard) -> Result<(), OwnerIssueError> {
        if self.is_cluster_mode() && !self.readiness.load(Ordering::Acquire) {
            return Err(OwnerIssueError::ReadinessClosed {
                scope: guard.scope.clone(),
            });
        }
        let term = self.current_owner(&guard.scope)?;
        if guard.owner_node != term.owner_node
            || guard.epoch != term.epoch
            || guard.coordinator_generation != term.coordinator_generation
        {
            return Err(OwnerIssueError::TermChanged {
                scope: guard.scope.clone(),
                guard_owner: guard.owner_node,
                guard_epoch: guard.epoch,
                guard_generation: guard.coordinator_generation,
                current_owner: term.owner_node,
                current_epoch: term.epoch,
                current_generation: term.coordinator_generation,
            });
        }
        if term.owner_node != self.this_node {
            return Err(OwnerIssueError::NotOwner {
                scope: guard.scope.clone(),
                this_node: self.this_node,
                owner_node: term.owner_node,
            });
        }
        if !self.is_cluster_mode() {
            return Ok(());
        }
        if term.coordinator_generation != TRACK_A_COORDINATOR_GENERATION {
            return Err(OwnerIssueError::GenerationMismatch {
                scope: guard.scope.clone(),
                expected: TRACK_A_COORDINATOR_GENERATION,
                actual: term.coordinator_generation,
            });
        }
        if let Some(overlay) = self
            .saga_states
            .read()
            .expect("local owner saga map poisoned")
            .get(&guard.scope)
        {
            return Err(OwnerIssueError::SagaOverlay {
                scope: guard.scope.clone(),
                role: overlay.role,
            });
        }
        let bases = self
            .base_states
            .read()
            .expect("local owner base map poisoned");
        let base = bases
            .get(&guard.scope)
            .ok_or_else(|| OwnerIssueError::RoleNotOwner {
                scope: guard.scope.clone(),
            })?;
        if base.recipient_node != self.this_node
            || base.owner_term != term
            || base.base_role != LocalOwnerBaseRole::Owner
        {
            return Err(OwnerIssueError::RoleNotOwner {
                scope: guard.scope.clone(),
            });
        }
        if base.activation_state != ActivationState::Routable {
            return Err(OwnerIssueError::NotRoutable {
                scope: guard.scope.clone(),
                activation: base.activation_state,
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

    fn snapshot_pair(
        recipient: NodeId,
        owner: NodeId,
        activation_state: ActivationState,
    ) -> (OwnerTermSnapshot, LocalOwnerStateSnapshot) {
        let term = OwnerTerm {
            scope: StateTransferScope::World,
            owner_node: owner,
            epoch: 1,
            coordinator_generation: TRACK_A_COORDINATOR_GENERATION,
        };
        let global =
            OwnerTermSnapshot::new(TRACK_A_COORDINATOR_GENERATION, 1, vec![term.clone()]).unwrap();
        let local = LocalOwnerStateSnapshot::new(
            recipient,
            TRACK_A_COORDINATOR_GENERATION,
            1,
            vec![LocalOwnerBaseState {
                scope: StateTransferScope::World,
                recipient_node: recipient,
                owner_term: term,
                base_role: if recipient == owner {
                    LocalOwnerBaseRole::Owner
                } else {
                    LocalOwnerBaseRole::Follower
                },
                activation_state,
            }],
        )
        .unwrap();
        (global, local)
    }

    #[test]
    fn single_node_registry_owns_every_scope() {
        let reg = OwnerRegistry::single_node(node(0));
        for scope in [
            StateTransferScope::World,
            StateTransferScope::for_agent("AGENT-07"),
        ] {
            let guard = reg.issue(scope.clone()).unwrap();
            assert_eq!(guard.scope(), &scope);
            assert_eq!(guard.epoch(), SINGLE_NODE_EPOCH);
            assert_eq!(guard.coordinator_generation(), 0);
            assert!(reg.validate(&guard).is_ok());
        }
    }

    #[test]
    fn cluster_starts_closed_and_never_synthesizes_unknown_scope() {
        let reg = OwnerRegistry::new_cluster_for_test(node(1));
        assert!(!reg.owner_readiness());
        assert!(matches!(
            reg.issue(StateTransferScope::World),
            Err(OwnerIssueError::ReadinessClosed { .. })
        ));
        let pair = snapshot_pair(node(1), node(1), ActivationState::Routable);
        reg.rebuild_from_owner_snapshot(&pair.0, &pair.1, vec![])
            .unwrap();
        assert!(reg.owner_readiness());
        assert!(reg.issue(StateTransferScope::World).is_ok());
        assert!(matches!(
            reg.issue(StateTransferScope::for_agent("AGENT-99")),
            Err(OwnerIssueError::UnknownScope { .. })
        ));
    }

    #[test]
    fn cluster_restart_latch_reopens_only_after_complete_snapshot_rebuild() {
        let reg = OwnerRegistry::new_cluster_for_test(node(1));
        let pair = snapshot_pair(node(1), node(1), ActivationState::Routable);
        reg.rebuild_from_owner_snapshot(&pair.0, &pair.1, vec![])
            .unwrap();
        assert!(reg.issue(StateTransferScope::World).is_ok());

        // A process restart creates the same closed state; explicitly closing the
        // standalone registry models that boot-local latch without a global OnceLock.
        reg.close_owner_readiness();
        assert!(matches!(
            reg.issue(StateTransferScope::World),
            Err(OwnerIssueError::ReadinessClosed { .. })
        ));
        reg.rebuild_from_owner_snapshot(&pair.0, &pair.1, vec![])
            .unwrap();
        assert!(reg.issue(StateTransferScope::World).is_ok());
    }

    #[test]
    fn snapshot_pair_rejects_invented_role_and_legacy_activation() {
        let owner = node(1);
        let recipient = node(2);
        let term = OwnerTerm {
            scope: StateTransferScope::World,
            owner_node: owner,
            epoch: 1,
            coordinator_generation: TRACK_A_COORDINATOR_GENERATION,
        };
        let global =
            OwnerTermSnapshot::new(TRACK_A_COORDINATOR_GENERATION, 1, vec![term.clone()]).unwrap();
        let invented_owner = LocalOwnerStateSnapshot::new(
            recipient,
            TRACK_A_COORDINATOR_GENERATION,
            1,
            vec![LocalOwnerBaseState {
                scope: term.scope.clone(),
                recipient_node: recipient,
                owner_term: term.clone(),
                base_role: LocalOwnerBaseRole::Owner,
                activation_state: ActivationState::Routable,
            }],
        )
        .unwrap();
        assert!(matches!(
            validate_owner_snapshot_pair(&global, &invented_owner),
            Err(OwnerSnapshotError::LocalRoleMismatch(_))
        ));

        let legacy = LocalOwnerStateSnapshot::new(
            owner,
            TRACK_A_COORDINATOR_GENERATION,
            1,
            vec![LocalOwnerBaseState {
                scope: term.scope.clone(),
                recipient_node: owner,
                owner_term: term,
                base_role: LocalOwnerBaseRole::Owner,
                activation_state: ActivationState::LegacyUnknown,
            }],
        )
        .unwrap();
        assert!(matches!(
            validate_owner_snapshot_pair(&global, &legacy),
            Err(OwnerSnapshotError::LegacyActivation(_))
        ));
    }

    #[test]
    fn complete_v19_requires_this_node_owner_role_and_routable_activation() {
        let follower = OwnerRegistry::new_cluster_for_test(node(2));
        let pair = snapshot_pair(node(2), node(1), ActivationState::NotRoutable);
        follower
            .rebuild_from_owner_snapshot(&pair.0, &pair.1, vec![])
            .unwrap();
        assert!(matches!(
            follower.issue(StateTransferScope::World),
            Err(OwnerIssueError::NotOwner { .. })
        ));

        let owner = OwnerRegistry::new_cluster_for_test(node(1));
        let pair = snapshot_pair(node(1), node(1), ActivationState::NotRoutable);
        owner
            .rebuild_from_owner_snapshot(&pair.0, &pair.1, vec![])
            .unwrap();
        assert!(matches!(
            owner.issue(StateTransferScope::World),
            Err(OwnerIssueError::NotRoutable { .. })
        ));
    }

    #[test]
    fn residency_is_derived_from_base_and_overlay_without_self_synthesis() {
        let recipient = node(2);
        let follower = OwnerRegistry::new_cluster_for_test(recipient);
        let pair = snapshot_pair(recipient, node(1), ActivationState::NotRoutable);
        follower
            .rebuild_from_owner_snapshot(&pair.0, &pair.1, vec![])
            .unwrap();
        assert_eq!(
            follower.local_residency(&StateTransferScope::World),
            Ok(LocalResidency::Absent)
        );

        let target = OwnerRegistry::new_cluster_for_test(recipient);
        let pair = snapshot_pair(recipient, node(1), ActivationState::NotRoutable);
        target
            .rebuild_from_owner_snapshot(
                &pair.0,
                &pair.1,
                vec![LocalOwnerSagaState {
                    scope: StateTransferScope::World,
                    operation_kind: LocalOwnerOperationKind::Handoff,
                    op_id: None,
                    owner_term: pair.0.sorted_terms[0].clone(),
                    role: LocalOwnerSagaRole::PreparedTarget,
                    transition_seq: 1,
                }],
            )
            .unwrap();
        assert_eq!(
            target.local_residency(&StateTransferScope::World),
            Ok(LocalResidency::PreparedFrozen)
        );
        assert!(matches!(
            target.local_residency(&StateTransferScope::for_agent("AGENT-99")),
            Err(OwnerIssueError::UnknownScope { .. })
        ));
    }

    #[test]
    fn full_term_is_rechecked_and_saga_overlay_fences_owner() {
        let reg = OwnerRegistry::new_cluster_for_test(node(1));
        let pair = snapshot_pair(node(1), node(1), ActivationState::Routable);
        reg.rebuild_from_owner_snapshot(&pair.0, &pair.1, vec![])
            .unwrap();
        let guard = reg.issue(StateTransferScope::World).unwrap();
        reg.commit_owner(OwnerTerm {
            scope: StateTransferScope::World,
            owner_node: node(2),
            epoch: 2,
            coordinator_generation: TRACK_A_COORDINATOR_GENERATION,
        });
        assert!(matches!(
            reg.validate(&guard),
            Err(OwnerIssueError::TermChanged { .. })
        ));

        let reg = OwnerRegistry::new_cluster_for_test(node(1));
        reg.rebuild_from_owner_snapshot(&pair.0, &pair.1, vec![])
            .unwrap();
        reg.retire_local(StateTransferScope::World, 1);
        assert!(matches!(
            reg.issue(StateTransferScope::World),
            Err(OwnerIssueError::SagaOverlay { .. })
        ));
    }

    #[test]
    fn canonical_snapshot_codec_has_stable_golden_vector() {
        let pair = snapshot_pair(node(1), node(1), ActivationState::Routable);
        let global_hex: String = pair
            .0
            .canonical_payload()
            .unwrap()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(global_hex, "00000001000000000000000100000000000000010000000100000005776f726c640101010101010101010101010101010100000000000000010000000000000001");
        let local_hex: String = pair
            .1
            .canonical_payload()
            .unwrap()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(local_hex, "0000000101010101010101010101010101010101000000000000000100000000000000010000000100000005776f726c640101010101010101010101010101010100000005776f726c6401010101010101010101010101010101000000000000000100000000000000010102");
        assert!(validate_owner_snapshot_pair(&pair.0, &pair.1).is_ok());
    }
}
