//! Control-plane cluster-metadata store (ADR-3).
//!
//! A **dedicated** redb database, separate from the agent-state [`crate::StateStore`].
//! It is the **authority** the owner fence consults — `OwnerRegistry` validates a write
//! against the owner terms committed here — so it is deliberately **not** a
//! [`sentinel_common::FencedStore`]: writing an owner term is a control-plane operation
//! governed by the `OwnerRegistry` (later quorum, Track D / `OwnerMetadataLog` G-D0), not
//! a fenced data write. Putting the owner table behind the fence it authorizes would be
//! circular. Keeping it in its own database also keeps control-plane state out of the
//! data-plane store's retention/GC. (This is why the `check-fenced-writers` gate, which
//! covers only the three data-store files, does not — and should not — cover this file.)
//!
//! PR2b-1c persists the single-node seed's owner term so it survives a restart; PR2b-2's
//! cooperative handoff writes `OwnerCommit(E+1)` here cross-node.

use anyhow::Context;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use sentinel_common::{LocalOwnerState, OwnerTerm, StateTransferScope};

/// `scope-key -> JSON-serialized OwnerTerm`. Keyed by a stable string form of the scope
/// (`world` or `nano:<container-id>`) so the same scope always maps to the same row.
const CLUSTER_OWNER: TableDefinition<&str, &[u8]> = TableDefinition::new("cluster_owner");

/// `scope-key -> JSON-serialized LocalOwnerState` (V4). This node's own per-scope role
/// (e.g. `Retired`), kept durable so a cooperative handoff's source keeps its
/// retirement fence across a restart — even during a partition that hides the new
/// owner's committed term.
const LOCAL_OWNER: TableDefinition<&str, &[u8]> = TableDefinition::new("local_owner");

/// The stable key for a scope in the `CLUSTER_OWNER` table — the canonical wire form
/// (`world` / `nano:<id>`), shared verbatim with the control RPCs so a scope keys the
/// same row everywhere (single source of truth in [`StateTransferScope::to_wire`]).
fn scope_key(scope: &StateTransferScope) -> String {
    scope.to_wire()
}

/// The control-plane cluster-metadata store (ADR-3): the durable home of the committed
/// [`OwnerTerm`]s the owner fence is validated against. **Not** a `FencedStore` — see the
/// module docs for why owner-term writes are control-plane, not fenced data writes.
pub struct ClusterMetaStore {
    db: Database,
}

impl ClusterMetaStore {
    /// Open or create the cluster-meta store at the given path.
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let db = Database::create(path)
            .map_err(|e| anyhow::anyhow!("ClusterMetaStore open at {path}: {e}"))?;
        let txn = db.begin_write()?;
        {
            txn.open_table(CLUSTER_OWNER)?;
            txn.open_table(LOCAL_OWNER)?;
        }
        txn.commit()?;
        Ok(Self { db })
    }

    /// Commit the owner term for its scope (control-plane write — not fenced, by design).
    pub fn put_owner_term(&self, term: &OwnerTerm) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(term).context("serialize OwnerTerm")?;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(CLUSTER_OWNER)?;
            table.insert(scope_key(&term.scope).as_str(), bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// The committed owner term for a scope, if one has been persisted.
    pub fn get_owner_term(&self, scope: &StateTransferScope) -> anyhow::Result<Option<OwnerTerm>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(CLUSTER_OWNER)?;
        match table.get(scope_key(scope).as_str())? {
            Some(guard) => Ok(Some(
                serde_json::from_slice(guard.value()).context("deserialize OwnerTerm")?,
            )),
            None => Ok(None),
        }
    }

    /// Every committed owner term. The daemon reads these at startup to re-establish the
    /// in-memory `OwnerRegistry` working view (PR2b-2): a persisted cross-node term
    /// re-enters cluster mode after a restart, so a handoff's `OwnerCommit(E+1)` is
    /// durable across a reboot (it does not silently fall back to the seed).
    pub fn list_owner_terms(&self) -> anyhow::Result<Vec<OwnerTerm>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(CLUSTER_OWNER)?;
        let mut terms = Vec::new();
        for entry in table.iter()? {
            let (_key, value) = entry?;
            terms.push(serde_json::from_slice(value.value()).context("deserialize OwnerTerm")?);
        }
        Ok(terms)
    }

    /// Persist this node's local owner state for its scope (V4 durable retirement) — a
    /// control-plane write like the owner terms (not a fenced data write, by design).
    pub fn put_local_state(&self, state: &LocalOwnerState) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(state).context("serialize LocalOwnerState")?;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(LOCAL_OWNER)?;
            table.insert(scope_key(&state.scope).as_str(), bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// This node's local owner state for a scope, if persisted.
    pub fn get_local_state(
        &self,
        scope: &StateTransferScope,
    ) -> anyhow::Result<Option<LocalOwnerState>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(LOCAL_OWNER)?;
        match table.get(scope_key(scope).as_str())? {
            Some(guard) => Ok(Some(
                serde_json::from_slice(guard.value()).context("deserialize LocalOwnerState")?,
            )),
            None => Ok(None),
        }
    }

    /// Every persisted local owner state — read at startup to re-establish the V4
    /// retirement fences in the in-memory registry (`restore_local_retirements`).
    pub fn list_local_states(&self) -> anyhow::Result<Vec<LocalOwnerState>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(LOCAL_OWNER)?;
        let mut states = Vec::new();
        for entry in table.iter()? {
            let (_key, value) = entry?;
            states.push(
                serde_json::from_slice(value.value()).context("deserialize LocalOwnerState")?,
            );
        }
        Ok(states)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::OwnerRegistry;

    /// #496 PR2b-1c: an owner term written to the dedicated cluster-meta store is durable
    /// across a restart (the store is reopened from the same file) — the precondition for
    /// re-establishing the registry from persisted ownership at startup (and for PR2b-2
    /// failover).
    #[test]
    fn owner_term_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cluster_meta.redb");
        let p = path.to_str().unwrap();
        let term = OwnerTerm {
            scope: StateTransferScope::World,
            owner_node: OwnerRegistry::global().this_node(),
            epoch: 1,
        };

        {
            let store = ClusterMetaStore::open(p).unwrap();
            assert!(store
                .get_owner_term(&StateTransferScope::World)
                .unwrap()
                .is_none());
            store.put_owner_term(&term).unwrap();
        }

        // Reopen from the same file — the term must still be there.
        let store = ClusterMetaStore::open(p).unwrap();
        assert_eq!(
            store.get_owner_term(&StateTransferScope::World).unwrap(),
            Some(term)
        );
        // A scope that was never written is absent.
        assert!(store
            .get_owner_term(&StateTransferScope::NanoContainer("AGENT-01".into()))
            .unwrap()
            .is_none());
    }

    /// #496 PR2b-2a: `list_owner_terms` returns every persisted term — the daemon reads
    /// these at startup to re-establish the registry's cluster-mode working view so a
    /// committed cross-node term survives a restart.
    #[test]
    fn list_owner_terms_returns_all_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cluster_meta.redb");
        let store = ClusterMetaStore::open(path.to_str().unwrap()).unwrap();
        assert!(store.list_owner_terms().unwrap().is_empty());

        let node = OwnerRegistry::global().this_node();
        let world = OwnerTerm {
            scope: StateTransferScope::World,
            owner_node: node,
            epoch: 1,
        };
        let nano = OwnerTerm {
            scope: StateTransferScope::NanoContainer("AGENT-07".into()),
            owner_node: node,
            epoch: 4,
        };
        store.put_owner_term(&world).unwrap();
        store.put_owner_term(&nano).unwrap();

        let mut terms = store.list_owner_terms().unwrap();
        terms.sort_by_key(|t| t.epoch);
        assert_eq!(terms, vec![world, nano]);
    }

    /// #496 PR2b-2ii: a local owner state (V4 retirement) is durable across a reopen and
    /// `list_local_states` returns every persisted one — read at startup to re-fence.
    #[test]
    fn local_owner_state_survives_reopen_and_lists() {
        use sentinel_common::{LocalOwnerRole, LocalOwnerState};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cluster_meta.redb");
        let p = path.to_str().unwrap();
        let scope = StateTransferScope::NanoContainer("AGENT-07".into());
        let state = LocalOwnerState {
            scope: scope.clone(),
            node_id: OwnerRegistry::global().this_node(),
            epoch: 4,
            role: LocalOwnerRole::Retired,
        };

        {
            let store = ClusterMetaStore::open(p).unwrap();
            assert!(store.get_local_state(&scope).unwrap().is_none());
            store.put_local_state(&state).unwrap();
        }
        // Reopen — the retirement must still fence.
        let store = ClusterMetaStore::open(p).unwrap();
        assert_eq!(store.get_local_state(&scope).unwrap(), Some(state.clone()));
        assert_eq!(store.list_local_states().unwrap(), vec![state]);
    }
}
