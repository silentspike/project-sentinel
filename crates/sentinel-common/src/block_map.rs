//! Distributed block map (#498, V8/V16) — the cluster-wide locator `BlockRef → {NodeId}`.
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
//! ## V16 — conflict-free merge (anti-entropy)
//!
//! Holder advertisements arrive out of order and duplicated. Each `(block, node)`
//! slot is a last-writer-wins register keyed by the monotone freshness pair
//! **`(incarnation, cas_generation)`**: a strictly newer pair wins, older/equal pairs
//! are ignored, so the merge is order-independent and idempotent. A `Remove` writes a
//! **tombstone** (not a deletion) at its freshness, so a late `Add` with a lower
//! freshness can no longer resurrect the holder ("`Remove@G` suppresses `Add<G`").
//! The `incarnation` is the membership incarnation (V13) and is the ABA guard: after a
//! reboot the `boot_id` changes and the CAS generation may reset, but the incarnation
//! only ever advances — so a stale advertisement from a previous boot can never
//! overwrite current state.
//!
//! ## N-node-native
//!
//! Holders are a `NodeId`-keyed map, never a hard source/target pair. Iteration is
//! deterministic (`BTreeMap` over the `NodeId`/`BlockRef` total order) so anti-entropy
//! summaries and tests never depend on `HashMap` order.
//!
//! This module is the in-memory data model + conflict-free merge mechanics. The
//! gossip wire (`AdvertiseHolders` over the cert-pinned QUIC control stream) and the
//! CAS coupling live in later #498 steps; block bytes use the separate QUIC pull
//! stream, never the map.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::block_ref::{BlockNamespace, BlockRef, HashAlgorithm};
use crate::cluster::NodeId;

/// What the map records about one node that advertises holding one block.
///
/// `cas_generation` is the advertising node's monotonically increasing CAS epoch
/// within a boot; `boot_id` is the node's current process-boot id (informational /
/// ABA evidence — the authoritative ABA guard is the incarnation, see the module
/// docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolderRecord {
    /// The advertising node's current boot id.
    pub boot_id: Uuid,
    /// The advertising node's CAS generation at advertisement time (monotone/boot).
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

/// Whether an advertisement asserts a node *gained* or *dropped* a block (V16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HolderAction {
    /// The node now holds the block (durably stored).
    Add,
    /// The node no longer holds the block (GC'd / evacuated) — writes a tombstone.
    Remove,
}

/// A gossiped statement that `node` holds (or dropped) `block`, as of a freshness
/// `(incarnation, cas_generation)` (V16/V25). `expires_after` is a logical deadline
/// (monotonic units chosen by the caller) after which the statement may be pruned, so
/// the map and its tombstones do not grow without bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolderAdvertisement {
    pub block_ref: BlockRef,
    pub node_id: NodeId,
    /// Current process-boot id of the advertising node (ABA evidence).
    pub node_boot_id: Uuid,
    /// Membership incarnation of the advertising node (V13) — the ABA guard.
    pub node_incarnation: u64,
    /// CAS generation within the current boot (monotone).
    pub node_cas_generation: u64,
    pub action: HolderAction,
    /// Logical deadline after which this statement may be pruned.
    pub expires_after: u64,
}

impl HolderAdvertisement {
    /// The monotone freshness key. A strictly larger key is strictly newer
    /// information about this `(block, node)` and wins the merge.
    fn freshness(&self) -> (u64, u64) {
        (self.node_incarnation, self.node_cas_generation)
    }
}

/// Internal per-`(block, node)` slot: the last applied advertisement's freshness plus
/// whether the node currently holds the block (`present`) or has a tombstone.
#[derive(Debug, Clone, Copy)]
struct HolderSlot {
    boot_id: Uuid,
    incarnation: u64,
    cas_generation: u64,
    present: bool,
    expires_after: u64,
}

impl HolderSlot {
    fn freshness(&self) -> (u64, u64) {
        (self.incarnation, self.cas_generation)
    }
}

/// The cluster-wide locator `BlockRef → {NodeId → slot}` (V8/V16).
///
/// Empty by construction; populated by local CAS advertisements and peer gossip. See
/// the module docs for the V8 locator-not-liveness and V16 merge contracts.
#[derive(Debug, Clone, Default)]
pub struct BlockMap {
    /// `BlockRef → (NodeId → slot)`. The inner map is ordered for deterministic
    /// holder iteration; the outer map is keyed by `BlockRef` (hashed) and sorted on
    /// demand via [`BlockMap::blocks`]. Tombstone slots (`present == false`) are
    /// retained until pruned so a late `Add` cannot resurrect a removed holder.
    entries: HashMap<BlockRef, BTreeMap<NodeId, HolderSlot>>,
}

impl BlockMap {
    /// An empty block map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge a gossiped advertisement (V16). Returns `true` if the advertisement was
    /// **applied** (it was strictly newer than the known slot, so it should be
    /// re-gossiped); `false` if it was stale or a duplicate and dropped.
    ///
    /// The advertisement is applied iff its freshness `(incarnation, cas_generation)`
    /// is strictly greater than the slot's current freshness — older/equal statements
    /// (duplicates, reordered gossip, stale pre-reboot ads) are dropped. An applied
    /// `Add` marks the node present; an applied `Remove` writes a tombstone at that
    /// freshness, suppressing any later lower-freshness `Add`.
    pub fn apply_advertisement(&mut self, adv: &HolderAdvertisement) -> bool {
        let holders = self.entries.entry(adv.block_ref.clone()).or_default();

        if let Some(existing) = holders.get(&adv.node_id) {
            if existing.freshness() >= adv.freshness() {
                return false; // stale or duplicate — order-independent no-op
            }
        }

        holders.insert(
            adv.node_id,
            HolderSlot {
                boot_id: adv.node_boot_id,
                incarnation: adv.node_incarnation,
                cas_generation: adv.node_cas_generation,
                present: adv.action == HolderAction::Add,
                expires_after: adv.expires_after,
            },
        );
        // Applied: strictly newer than what was known. A tombstone entry is kept so a
        // later lower-freshness Add cannot resurrect the holder.
        true
    }

    /// Convenience: record that `node` holds `block` locally (generation-monotone),
    /// without going through the gossip wire. Equivalent to applying an `Add`
    /// advertisement at incarnation 0.
    pub fn add_holder(&mut self, block: BlockRef, node: NodeId, record: HolderRecord) -> bool {
        self.apply_advertisement(&HolderAdvertisement {
            block_ref: block,
            node_id: node,
            node_boot_id: record.boot_id,
            node_incarnation: 0,
            node_cas_generation: record.cas_generation,
            action: HolderAction::Add,
            expires_after: u64::MAX,
        })
    }

    /// Hard-drop `node` as a holder of `block` (local removal, no tombstone). Returns
    /// `true` if a present holder was removed. Use [`BlockMap::apply_advertisement`]
    /// with [`HolderAction::Remove`] when the removal must suppress reordered gossip.
    pub fn remove_holder(&mut self, block: &BlockRef, node: &NodeId) -> bool {
        let Some(holders) = self.entries.get_mut(block) else {
            return false;
        };
        let removed = holders.remove(node).is_some_and(|s| s.present);
        if holders.is_empty() {
            self.entries.remove(block);
        }
        removed
    }

    /// The nodes known to currently hold `block`, in deterministic `NodeId` order.
    /// Tombstoned nodes are excluded. Empty is **not** a liveness statement (V8).
    pub fn holders(&self, block: &BlockRef) -> Vec<NodeId> {
        self.entries
            .get(block)
            .map(|h| {
                h.iter()
                    .filter(|(_, s)| s.present)
                    .map(|(n, _)| *n)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The present holder records for `block` (node + boot/generation), `NodeId`-ordered.
    pub fn holder_records(&self, block: &BlockRef) -> Vec<(NodeId, HolderRecord)> {
        self.entries
            .get(block)
            .map(|h| {
                h.iter()
                    .filter(|(_, s)| s.present)
                    .map(|(n, s)| (*n, HolderRecord::new(s.boot_id, s.cas_generation)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether at least one node currently holds `block` (a locator hit, **not** a
    /// liveness/needed answer — V8).
    pub fn has_holder(&self, block: &BlockRef) -> bool {
        self.entries
            .get(block)
            .is_some_and(|h| h.values().any(|s| s.present))
    }

    /// Number of distinct blocks with at least one present holder (tombstone-only
    /// blocks are not counted).
    pub fn block_count(&self) -> usize {
        self.entries
            .values()
            .filter(|h| h.values().any(|s| s.present))
            .count()
    }

    /// Total number of present `(block, node)` holder entries across the whole map.
    pub fn holder_entry_count(&self) -> usize {
        self.entries
            .values()
            .map(|h| h.values().filter(|s| s.present).count())
            .sum()
    }

    /// All blocks with at least one present holder, in a deterministic order (sorted
    /// by their canonical locator string) so anti-entropy summaries and tests are
    /// reproducible.
    pub fn blocks(&self) -> Vec<&BlockRef> {
        let mut refs: Vec<&BlockRef> = self
            .entries
            .iter()
            .filter(|(_, h)| h.values().any(|s| s.present))
            .map(|(r, _)| r)
            .collect();
        refs.sort_by_key(|r| r.to_string());
        refs
    }

    /// Find a known blob `BlockRef` (namespace `Blob`, SHA-256) whose digest matches
    /// `hash`, among blocks with a present holder. The read paths know only the hash;
    /// this recovers the full ref (size + namespace) the block-pull client needs (#498
    /// 4c). `None` if no holder is known for that blob digest.
    pub fn find_blob_ref(&self, hash: &[u8; 32]) -> Option<BlockRef> {
        self.entries
            .iter()
            .filter(|(_, h)| h.values().any(|s| s.present))
            .map(|(r, _)| r)
            .find(|r| {
                r.namespace() == BlockNamespace::Blob
                    && r.algorithm() == HashAlgorithm::Sha256
                    && r.digest() == hash
            })
            .cloned()
    }

    /// Drop slots whose `expires_after` is at or before `now` (logical units),
    /// reclaiming stale advertisements and tombstones so the map does not grow without
    /// bound (V25). Returns the number of slots pruned.
    pub fn prune_expired(&mut self, now: u64) -> usize {
        let mut pruned = 0;
        self.entries.retain(|_, holders| {
            let before = holders.len();
            holders.retain(|_, s| s.expires_after > now);
            pruned += before - holders.len();
            !holders.is_empty()
        });
        pruned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_ref::BlockRef;

    fn blob(n: u8) -> BlockRef {
        BlockRef::blob_sha256([n; 32], 1024)
    }

    fn adv(
        block: BlockRef,
        node: NodeId,
        boot: Uuid,
        incarnation: u64,
        gen: u64,
        action: HolderAction,
    ) -> HolderAdvertisement {
        HolderAdvertisement {
            block_ref: block,
            node_id: node,
            node_boot_id: boot,
            node_incarnation: incarnation,
            node_cas_generation: gen,
            action,
            expires_after: u64::MAX,
        }
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
            "no present holder left (empty set carries no meaning, V8)"
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
        assert!(!map.add_holder(blk.clone(), node, HolderRecord::new(boot, 3)));
        assert!(!map.add_holder(blk.clone(), node, HolderRecord::new(boot, 5)));
        assert!(map.add_holder(blk.clone(), node, HolderRecord::new(boot, 6)));
        assert_eq!(map.holder_records(&blk)[0].1.cas_generation, 6);
    }

    #[test]
    fn unknown_block_has_no_holders_and_is_not_a_liveness_default() {
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
        assert_eq!(map.blocks(), map.blocks());
        assert_eq!(map.holders(&blob(7)), map.holders(&blob(7)));
        let mut sorted = map.holders(&blob(7));
        sorted.sort();
        assert_eq!(
            map.holders(&blob(7)),
            sorted,
            "holders come back NodeId-sorted"
        );
    }

    // ── V16 conflict resolution ─────────────────

    #[test]
    fn higher_generation_wins_reordered_gossip_is_idempotent() {
        let mut map = BlockMap::new();
        let (node, boot, blk) = (NodeId::new(), Uuid::new_v4(), blob(4));

        assert!(map.apply_advertisement(&adv(blk.clone(), node, boot, 1, 10, HolderAction::Add)));
        // A reordered, older Add for the same boot/incarnation is dropped.
        assert!(!map.apply_advertisement(&adv(blk.clone(), node, boot, 1, 7, HolderAction::Add)));
        // A duplicate is a no-op.
        assert!(!map.apply_advertisement(&adv(blk.clone(), node, boot, 1, 10, HolderAction::Add)));
        assert!(map.has_holder(&blk));
    }

    #[test]
    fn remove_at_g_suppresses_later_lower_add() {
        let mut map = BlockMap::new();
        let (node, boot, blk) = (NodeId::new(), Uuid::new_v4(), blob(5));

        assert!(map.apply_advertisement(&adv(blk.clone(), node, boot, 1, 5, HolderAction::Add)));
        // Remove at generation 8 -> tombstone.
        assert!(map.apply_advertisement(&adv(blk.clone(), node, boot, 1, 8, HolderAction::Remove)));
        assert!(!map.has_holder(&blk), "removed -> no present holder");
        // A late Add at generation 6 (< 8) must NOT resurrect the holder.
        assert!(!map.apply_advertisement(&adv(blk.clone(), node, boot, 1, 6, HolderAction::Add)));
        assert!(!map.has_holder(&blk), "Remove@8 suppresses Add@6");
        // A fresh Add at generation 9 (> 8) legitimately re-adds it.
        assert!(map.apply_advertisement(&adv(blk.clone(), node, boot, 1, 9, HolderAction::Add)));
        assert!(map.has_holder(&blk));
    }

    #[test]
    fn aba_stale_pre_reboot_advertisement_cannot_overwrite_newer_incarnation() {
        let mut map = BlockMap::new();
        let node = NodeId::new();
        let blk = blob(6);
        let (boot_old, boot_new) = (Uuid::new_v4(), Uuid::new_v4());

        // After reboot: new incarnation 2, fresh boot, CAS generation reset low (1),
        // node has GC'd the block -> Remove.
        assert!(map.apply_advertisement(&adv(
            blk.clone(),
            node,
            boot_new,
            2,
            1,
            HolderAction::Remove
        )));
        assert!(!map.has_holder(&blk));
        // A delayed pre-reboot Add (old boot, incarnation 1) with a HIGH generation
        // must NOT overwrite the newer incarnation (ABA guard) — generation alone
        // would wrongly win here.
        assert!(!map.apply_advertisement(&adv(
            blk.clone(),
            node,
            boot_old,
            1,
            99,
            HolderAction::Add
        )));
        assert!(
            !map.has_holder(&blk),
            "stale pre-reboot ad (lower incarnation) loses despite higher generation"
        );
    }

    #[test]
    fn expired_slots_and_tombstones_are_pruned() {
        let mut map = BlockMap::new();
        let (node, boot, blk) = (NodeId::new(), Uuid::new_v4(), blob(8));
        let mut a = adv(blk.clone(), node, boot, 1, 1, HolderAction::Add);
        a.expires_after = 100;
        assert!(map.apply_advertisement(&a));
        assert_eq!(map.prune_expired(50), 0, "not yet expired");
        assert!(map.has_holder(&blk));
        assert_eq!(map.prune_expired(100), 1, "expired at deadline");
        assert!(!map.has_holder(&blk));
        assert_eq!(map.block_count(), 0);
    }
}
