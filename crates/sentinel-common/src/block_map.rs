//! Distributed block map (#498, V8) — the cluster-wide locator `BlockRef → {NodeId}`.
//!
//! When a container moves cross-node, the 1:n principle "move pointers/hashes, never
//! bytes" only holds if a hash pointer is *reachable* on the target node. The block
//! map answers exactly one question: **"which nodes might hold this block?"** A node
//! that lacks a blob consults the map to find a peer to pull it from (#498 PR2).
//!
//! ## V8 — locator, never liveness
//!
//! The block map is a **locator only**. It deliberately exposes no `is_live`,
//! `is_needed` or `ref_count` method, because **GC must never treat the block map as
//! a liveness proof** (#499 / V8). "A holder is advertised" means "the bytes might be
//! fetchable there", **not** "this block is still referenced". Whether a block may be
//! deleted is answered by the local pin/ref index and a remote *ref* query
//! (`ClusterDeleteGuard`, #499) — never by this map. Conversely, an empty holder set
//! means "no peer is known to hold it", **not** "it is garbage".
//!
//! ## N-node-native
//!
//! Holders are a `NodeId`-keyed map, never a hard source/target pair — two nodes are
//! the first test, not the ceiling. Iteration is deterministic (`BTreeMap` over the
//! `NodeId`/`BlockRef` total order) so anti-entropy summaries and tests never depend
//! on `HashMap` order.
//!
//! This module is the in-memory data model + conflict-free merge mechanics. The
//! gossip wire (`HolderAdvertisement` over Zenoh) and the CAS coupling live in later
//! #498 steps; the bytes themselves travel over QUIC, never over the map.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::block_ref::BlockRef;
use crate::cluster::NodeId;

/// What the map records about one node that advertises holding one block.
///
/// `cas_generation` is the advertising node's monotonically increasing CAS epoch: a
/// higher generation is newer information about that node and supersedes a lower one
/// (the conflict rule applied by the gossip layer, V16). `boot_id` is the node's
/// current process-boot id, kept so a stale advertisement from a *previous* boot can
/// be distinguished from current state (ABA guard, V13/V16; enforced in the
/// advertisement-apply step).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolderRecord {
    /// The advertising node's current boot id (ABA guard).
    pub boot_id: Uuid,
    /// The advertising node's CAS generation at advertisement time (monotone).
    pub cas_generation: u64,
}

impl HolderRecord {
    /// A holder record for a node at a given boot + CAS generation.
    pub fn new(boot_id: Uuid, cas_generation: u64) -> Self {
        Self {
            boot_id,
            cas_generation,
        }
    }
}

/// The cluster-wide locator `BlockRef → {NodeId → HolderRecord}` (V8).
///
/// Empty by construction; populated by local CAS advertisements and peer gossip. See
/// the module docs for the V8 locator-not-liveness contract.
#[derive(Debug, Clone, Default)]
pub struct BlockMap {
    /// `BlockRef → (NodeId → HolderRecord)`. The inner map is ordered for
    /// deterministic holder iteration; the outer map is keyed by `BlockRef` (hashed)
    /// and sorted on demand via [`BlockMap::blocks`].
    entries: HashMap<BlockRef, BTreeMap<NodeId, HolderRecord>>,
}

impl BlockMap {
    /// An empty block map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record (or refresh) that `node` holds `block`, generation-monotonically.
    ///
    /// Returns `true` if the map changed. A record is accepted iff it is new or
    /// strictly newer (`cas_generation` higher) than what is already known for that
    /// node — older/equal generations from the *same* node are ignored, so the merge
    /// is idempotent and order-independent. Boot-id/ABA and `Remove` tombstones are
    /// layered on top by the advertisement-apply step (V16); this method is the
    /// monotone primitive it builds on.
    pub fn add_holder(&mut self, block: BlockRef, node: NodeId, record: HolderRecord) -> bool {
        let holders = self.entries.entry(block).or_default();
        match holders.get(&node) {
            Some(existing) if existing.cas_generation >= record.cas_generation => false,
            _ => {
                holders.insert(node, record);
                true
            }
        }
    }

    /// Drop `node` as a holder of `block`. Returns `true` if a holder was removed.
    /// Empties the block's entry entirely when it loses its last holder (an empty
    /// holder set carries no meaning — V8).
    pub fn remove_holder(&mut self, block: &BlockRef, node: &NodeId) -> bool {
        let Some(holders) = self.entries.get_mut(block) else {
            return false;
        };
        let removed = holders.remove(node).is_some();
        if holders.is_empty() {
            self.entries.remove(block);
        }
        removed
    }

    /// The nodes known to hold `block`, in deterministic `NodeId` order. Empty if no
    /// holder is known — which is **not** a liveness statement (V8).
    pub fn holders(&self, block: &BlockRef) -> Vec<NodeId> {
        self.entries
            .get(block)
            .map(|h| h.keys().copied().collect())
            .unwrap_or_default()
    }

    /// The holder records for `block` (node + boot/generation), in `NodeId` order.
    pub fn holder_records(&self, block: &BlockRef) -> Vec<(NodeId, HolderRecord)> {
        self.entries
            .get(block)
            .map(|h| h.iter().map(|(n, r)| (*n, *r)).collect())
            .unwrap_or_default()
    }

    /// Whether at least one node is known to hold `block` (a locator hit, **not** a
    /// liveness/needed answer — V8).
    pub fn has_holder(&self, block: &BlockRef) -> bool {
        self.entries.get(block).is_some_and(|h| !h.is_empty())
    }

    /// Number of distinct blocks with at least one known holder.
    pub fn block_count(&self) -> usize {
        self.entries.len()
    }

    /// Total number of `(block, node)` holder entries across the whole map.
    pub fn holder_entry_count(&self) -> usize {
        self.entries.values().map(BTreeMap::len).sum()
    }

    /// All known blocks, in a deterministic order (sorted by their canonical locator
    /// string) so anti-entropy summaries and tests are reproducible.
    pub fn blocks(&self) -> Vec<&BlockRef> {
        let mut refs: Vec<&BlockRef> = self.entries.keys().collect();
        refs.sort_by_key(|r| r.to_string());
        refs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_ref::BlockRef;

    fn blob(n: u8) -> BlockRef {
        BlockRef::blob_sha256([n; 32], 1024)
    }

    #[test]
    fn add_query_remove_is_node_keyed() {
        let mut map = BlockMap::new();
        let a = NodeId::new();
        let b = NodeId::new();
        let blk = blob(1);

        assert!(map.add_holder(blk.clone(), a, HolderRecord::new(Uuid::new_v4(), 1)));
        assert!(map.add_holder(blk.clone(), b, HolderRecord::new(Uuid::new_v4(), 1)));

        let holders = map.holders(&blk);
        assert_eq!(holders.len(), 2, "both nodes are holders");
        assert!(holders.contains(&a) && holders.contains(&b));
        assert!(map.has_holder(&blk));
        assert_eq!(map.block_count(), 1);
        assert_eq!(map.holder_entry_count(), 2);

        assert!(map.remove_holder(&blk, &a));
        assert_eq!(map.holders(&blk), vec![b], "only b remains");
        assert!(map.remove_holder(&blk, &b));
        assert!(
            !map.has_holder(&blk),
            "no holder left -> block entry gone (empty set carries no meaning, V8)"
        );
        assert_eq!(map.block_count(), 0);
    }

    #[test]
    fn add_holder_is_generation_monotone_and_idempotent() {
        let mut map = BlockMap::new();
        let node = NodeId::new();
        let boot = Uuid::new_v4();
        let blk = blob(2);

        assert!(map.add_holder(blk.clone(), node, HolderRecord::new(boot, 5)));
        // Older generation from the same node is ignored (order-independent merge).
        assert!(!map.add_holder(blk.clone(), node, HolderRecord::new(boot, 3)));
        // Equal generation is a no-op.
        assert!(!map.add_holder(blk.clone(), node, HolderRecord::new(boot, 5)));
        // Strictly newer generation wins.
        assert!(map.add_holder(blk.clone(), node, HolderRecord::new(boot, 6)));
        assert_eq!(map.holder_records(&blk)[0].1.cas_generation, 6);
    }

    #[test]
    fn unknown_block_has_no_holders_and_is_not_a_liveness_default() {
        // V8: an empty holder set means "no peer known to hold it", NOT "garbage" and
        // NOT "needed". The map exposes no is_live/is_needed/ref_count to mislead GC.
        let map = BlockMap::new();
        let blk = blob(9);
        assert!(map.holders(&blk).is_empty());
        assert!(!map.has_holder(&blk));
        assert_eq!(map.block_count(), 0);
    }

    #[test]
    fn block_and_holder_iteration_is_deterministic() {
        let mut map = BlockMap::new();
        let nodes: Vec<NodeId> = (0..4).map(|_| NodeId::new()).collect();
        for n in &nodes {
            map.add_holder(blob(7), *n, HolderRecord::new(Uuid::new_v4(), 1));
            map.add_holder(blob(3), *n, HolderRecord::new(Uuid::new_v4(), 1));
        }
        // Repeated reads yield a stable, sorted order (no HashMap-order dependence).
        assert_eq!(map.blocks(), map.blocks());
        assert_eq!(map.holders(&blob(7)), map.holders(&blob(7)));
        let mut sorted = map.holders(&blob(7));
        sorted.sort();
        assert_eq!(map.holders(&blob(7)), sorted, "holders come back NodeId-sorted");
    }
}
