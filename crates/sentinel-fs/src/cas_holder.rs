//! #498 (V8/V16/V28) — this node's CAS holder state.
//!
//! Couples the local [`CasStore`] to the cluster block map: it tracks which blobs this
//! node durably holds (a [`CasInventory`]) and produces the [`HolderAdvertisement`]s it
//! gossips over the #569 control stream. Metadata only — bytes never leave here.
//!
//! Freshness (V16): every change bumps a monotone CAS generation, so a later
//! advertisement (`Add` or `Remove`) strictly supersedes an earlier one for the same
//! `(block, node)`. A `Remove` is a tombstone at a higher generation than its `Add`, so
//! reordered gossip cannot resurrect a dropped holder.

use sentinel_common::{BlockRef, CasInventory, HolderAction, HolderAdvertisement, NodeId};
use uuid::Uuid;

use crate::cas::CasStore;

/// This node's CAS holder state: the durable-blob inventory + advertisement minting.
pub struct CasHolderState {
    node_id: NodeId,
    boot_id: Uuid,
    incarnation: u64,
    /// Monotone CAS generation, bumped on every change (V16 freshness).
    generation: u64,
    inventory: CasInventory,
}

impl CasHolderState {
    /// New, empty holder state for `node_id` at this boot/incarnation. Call
    /// [`CasHolderState::rebuild`] to populate it from the durable store at startup.
    pub fn new(node_id: NodeId, boot_id: Uuid, incarnation: u64) -> Self {
        Self {
            node_id,
            boot_id,
            incarnation,
            generation: 0,
            inventory: CasInventory::new(),
        }
    }

    fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    fn advertisement(
        &self,
        block_ref: BlockRef,
        action: HolderAction,
        generation: u64,
    ) -> HolderAdvertisement {
        HolderAdvertisement {
            block_ref,
            node_id: self.node_id,
            node_boot_id: self.boot_id,
            node_incarnation: self.incarnation,
            node_cas_generation: generation,
            action,
            expires_after: u64::MAX,
        }
    }

    /// Rebuild the inventory from the blobs that durably survived on disk (startup /
    /// V28 reconcile). Incomplete temp writes are skipped by [`CasStore::list_block_refs`].
    pub fn rebuild(&mut self, store: &CasStore) -> anyhow::Result<()> {
        let generation = self.next_generation();
        for block_ref in store.list_block_refs()? {
            self.inventory.insert(block_ref, generation);
        }
        Ok(())
    }

    /// Record that this node now durably holds `(hash, size)`; returns the `Add`
    /// advertisement to gossip.
    pub fn record_stored(&mut self, hash: [u8; 32], size: u64) -> HolderAdvertisement {
        let generation = self.next_generation();
        let block_ref = BlockRef::blob_sha256(hash, size);
        self.inventory.insert(block_ref.clone(), generation);
        self.advertisement(block_ref, HolderAction::Add, generation)
    }

    /// Record that this node dropped `block_ref`; returns the `Remove` (tombstone)
    /// advertisement at a higher generation than the matching `Add` (V16).
    pub fn record_removed(&mut self, block_ref: BlockRef) -> HolderAdvertisement {
        let generation = self.next_generation();
        self.inventory.remove(&block_ref);
        self.advertisement(block_ref, HolderAction::Remove, generation)
    }

    /// A bounded page of `Add` advertisements for the periodic republish (V25 — never
    /// the whole inventory at once). `after`/`limit` page through the held blocks in
    /// sorted order; the returned cursor (`Some`) is the inclusive start of the next page.
    pub fn advertisement_page(
        &self,
        after: Option<&BlockRef>,
        limit: usize,
    ) -> (Vec<HolderAdvertisement>, Option<BlockRef>) {
        let page = self.inventory.page(after, limit);
        let advs = page
            .entries
            .into_iter()
            .map(|(block_ref, generation)| {
                self.advertisement(block_ref, HolderAction::Add, generation)
            })
            .collect();
        (advs, page.next)
    }

    /// The local inventory (which blocks this node holds).
    pub fn inventory(&self) -> &CasInventory {
        &self.inventory
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::BlockNamespace;

    fn state() -> CasHolderState {
        CasHolderState::new(NodeId::new(), Uuid::new_v4(), 1)
    }

    #[test]
    fn record_stored_yields_an_add_advert_with_a_blob_block_ref() {
        let mut s = state();
        let hash = CasStore::hash(b"hello distributed cas");
        let adv = s.record_stored(hash, 21);
        assert_eq!(adv.action, HolderAction::Add);
        assert_eq!(adv.block_ref.namespace(), BlockNamespace::Blob);
        assert_eq!(adv.block_ref.digest(), &hash);
        assert_eq!(adv.block_ref.size_bytes(), 21);
        assert_eq!(adv.node_id, s.node_id);
        assert!(s.inventory().contains(&BlockRef::blob_sha256(hash, 21)));
    }

    #[test]
    fn record_removed_is_a_tombstone_at_a_higher_generation_than_the_add() {
        let mut s = state();
        let hash = CasStore::hash(b"to be removed");
        let add = s.record_stored(hash, 13);
        let block_ref = BlockRef::blob_sha256(hash, 13);
        let remove = s.record_removed(block_ref.clone());
        assert_eq!(remove.action, HolderAction::Remove);
        assert!(
            remove.node_cas_generation > add.node_cas_generation,
            "the Remove must out-rank its Add so reordered gossip cannot resurrect it"
        );
        assert!(!s.inventory().contains(&block_ref), "dropped locally");
    }

    #[test]
    fn rebuild_recovers_the_holder_set_from_durable_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let payloads: [&[u8]; 3] = [b"alpha", b"beta block", b"gamma payload here"];
        let mut expected = Vec::new();
        for p in payloads {
            let (hash, _) = store.store(p).unwrap();
            expected.push(BlockRef::blob_sha256(hash, p.len() as u64));
        }

        let mut s = state();
        s.rebuild(&store).unwrap();
        assert_eq!(s.inventory().len(), 3, "all durable blobs recovered");
        for block_ref in &expected {
            assert!(
                s.inventory().contains(block_ref),
                "rebuilt ref matches a stored ref (same size via decode)"
            );
        }
    }

    #[test]
    fn advertisement_page_is_bounded() {
        let mut s = state();
        for i in 0..5u8 {
            s.record_stored(CasStore::hash(&[i]), 1);
        }
        let (advs, next) = s.advertisement_page(None, 2);
        assert_eq!(advs.len(), 2, "page never exceeds the limit (V25)");
        assert!(next.is_some(), "more pages remain");
        assert!(advs.iter().all(|a| a.action == HolderAction::Add));
    }
}
