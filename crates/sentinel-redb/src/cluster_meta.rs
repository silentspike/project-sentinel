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
use redb::{Database, ReadableDatabase, TableDefinition};
use sentinel_common::{OwnerTerm, StateTransferScope};

/// `scope-key -> JSON-serialized OwnerTerm`. Keyed by a stable string form of the scope
/// (`world` or `nano:<container-id>`) so the same scope always maps to the same row.
const CLUSTER_OWNER: TableDefinition<&str, &[u8]> = TableDefinition::new("cluster_owner");

/// The stable key for a scope in the `CLUSTER_OWNER` table.
fn scope_key(scope: &StateTransferScope) -> String {
    match scope {
        StateTransferScope::World => "world".to_string(),
        StateTransferScope::NanoContainer(id) => format!("nano:{id}"),
    }
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
}
