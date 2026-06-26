//! Leveled anti-entropy for the distributed block map (#498, V25/V16).
//!
//! Two nodes reconcile which blocks each holds **without ever shipping the full block
//! id list in one message** — at 100k–1M blocks a "gossip all ids" API does not
//! scale. Reconciliation is leveled, cheapest first:
//!
//! * **L1 — generation summary** ([`CasInventory::summaries`]): one compact
//!   [`GenerationSummary`] per `(namespace, chunk_profile)` group — block count, max
//!   CAS generation, and an order-independent digest fingerprint. Two nodes compare
//!   summaries in O(groups); equal summary ⇒ very likely equal set, stop.
//! * **L2 — paginated inventory** ([`CasInventory::page`]): on a summary mismatch,
//!   walk the group's blocks in sorted order in **bounded** pages (`limit`), each with
//!   a cursor — never the whole set at once.
//! * **L3 — on-demand reconcile** ([`CasInventory::held_among`]): for a small set of
//!   *suspected-missing* refs, answer which of them this node actually holds.
//!
//! This is the protocol's data model + pure logic over a node's local CAS inventory.
//! The wire (request/response over the cluster transport) and the live CAS source are
//! wired in later #498 steps.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::block_ref::{BlockNamespace, BlockRef};

/// Length of the order-independent set fingerprint carried by an L1 summary.
const FINGERPRINT_LEN: usize = 16;

/// A stable, dep-free 128-bit fingerprint of one block's canonical locator (two
/// independent FNV-1a lanes). Stable across nodes/builds (FNV constants are fixed), so
/// two nodes computing it agree; distinct blocks fold to distinct values (unlike a raw
/// digest XOR, which collapses for repeated-byte digests).
fn block_fingerprint(block: &BlockRef) -> [u8; FINGERPRINT_LEN] {
    let s = block.to_string();
    let lane = |basis: u64| -> u64 {
        let mut h = basis;
        for &b in s.as_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01B3); // FNV-1a 64-bit prime
        }
        h
    };
    let hi = lane(0xcbf2_9ce4_8422_2325); // FNV-1a offset basis
    let lo = lane(0x9e37_79b9_7f4a_7c15); // a second basis → independent lane
    let mut out = [0u8; FINGERPRINT_LEN];
    out[..8].copy_from_slice(&hi.to_le_bytes());
    out[8..].copy_from_slice(&lo.to_le_bytes());
    out
}

/// Group key while folding L1 summaries: `(namespace, chunk_profile)`.
type GroupKey = (BlockNamespace, Option<String>);

/// Per-group fold accumulator: `(block_count, max_generation, fingerprint)`.
type GroupAccumulator = (u64, u64, [u8; FINGERPRINT_LEN]);

/// L1 — a compact, comparable summary of one `(namespace, chunk_profile)` group of a
/// node's CAS inventory. Cheap to gossip (fixed size, independent of block count).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationSummary {
    pub namespace: BlockNamespace,
    pub chunk_profile: Option<String>,
    /// Number of blocks in this group.
    pub block_count: u64,
    /// Highest CAS generation observed in this group (monotone freshness hint).
    pub max_generation: u64,
    /// Order-independent XOR fold of the block digests — equal sets fold equal.
    pub fingerprint: [u8; FINGERPRINT_LEN],
}

/// L2 — one bounded page of a node's inventory, in sorted `BlockRef` order. `next` is
/// the cursor for the following page (`None` ⇒ last page).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryPage {
    pub entries: Vec<(BlockRef, u64)>,
    pub next: Option<BlockRef>,
}

/// A node's local CAS inventory: which blocks it holds and at which CAS generation.
/// Kept sorted (`BTreeMap` over the `BlockRef` total order) so anti-entropy pagination
/// and summaries are deterministic.
#[derive(Debug, Clone, Default)]
pub struct CasInventory {
    entries: BTreeMap<BlockRef, u64>,
}

impl CasInventory {
    /// An empty inventory.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record (or refresh) that this node holds `block` at `generation`. Generation is
    /// kept monotone (an older generation for a known block is ignored).
    pub fn insert(&mut self, block: BlockRef, generation: u64) -> bool {
        match self.entries.get(&block) {
            Some(g) if *g >= generation => false,
            _ => {
                self.entries.insert(block, generation);
                true
            }
        }
    }

    /// Drop `block` from the local inventory.
    pub fn remove(&mut self, block: &BlockRef) -> bool {
        self.entries.remove(block).is_some()
    }

    /// Whether this node holds `block`.
    pub fn contains(&self, block: &BlockRef) -> bool {
        self.entries.contains_key(block)
    }

    /// Total number of blocks held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the inventory is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// L1 — one summary per `(namespace, chunk_profile)` group, in deterministic
    /// order. Compact: its size depends on the number of groups, never the number of
    /// blocks.
    pub fn summaries(&self) -> Vec<GenerationSummary> {
        // Accumulate per group. The key is ordered so the output is deterministic.
        let mut groups: BTreeMap<GroupKey, GroupAccumulator> = BTreeMap::new();
        for (block, gen) in &self.entries {
            let key = (block.namespace(), block.chunk_profile().map(str::to_owned));
            let acc = groups.entry(key).or_insert((0, 0, [0u8; FINGERPRINT_LEN]));
            acc.0 += 1;
            acc.1 = acc.1.max(*gen);
            let fp = block_fingerprint(block);
            for (slot, b) in acc.2.iter_mut().zip(fp) {
                *slot ^= b;
            }
        }
        groups
            .into_iter()
            .map(
                |((namespace, chunk_profile), (block_count, max_generation, fingerprint))| {
                    GenerationSummary {
                        namespace,
                        chunk_profile,
                        block_count,
                        max_generation,
                        fingerprint,
                    }
                },
            )
            .collect()
    }

    /// L2 — a bounded page of the inventory in sorted order. Returns at most `limit`
    /// entries strictly after `after` (or from the start if `after` is `None`), plus a
    /// cursor for the next page. **Never** returns the whole inventory at once — that
    /// is the V25 scalability contract.
    pub fn page(&self, after: Option<&BlockRef>, limit: usize) -> InventoryPage {
        use std::ops::Bound;
        // `after` is the previous page's cursor: an *inclusive* start, so the cursor
        // block is the first entry of this page (never skipped).
        let lower = match after {
            Some(b) => Bound::Included(b.clone()),
            None => Bound::Unbounded,
        };
        let mut entries: Vec<(BlockRef, u64)> = self
            .entries
            .range((lower, Bound::Unbounded))
            .take(limit.saturating_add(1))
            .map(|(b, g)| (b.clone(), *g))
            .collect();
        // The (limit+1)-th entry, if any, is not part of this page — it is the
        // inclusive start cursor of the next page (so no block is skipped).
        let next = if entries.len() > limit {
            entries.pop().map(|(b, _)| b)
        } else {
            None
        };
        InventoryPage { entries, next }
    }

    /// L3 — of `suspected` refs (e.g. blocks a peer could not find), which does this
    /// node actually hold? Returned in deterministic sorted order.
    pub fn held_among(&self, suspected: &[BlockRef]) -> Vec<BlockRef> {
        let mut held: Vec<BlockRef> = suspected
            .iter()
            .filter(|b| self.entries.contains_key(b))
            .cloned()
            .collect();
        held.sort();
        held
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_ref::BlockRef;

    fn blob(n: u8) -> BlockRef {
        BlockRef::blob_sha256([n; 32], 1024)
    }

    fn chunk(n: u8, profile: &str) -> BlockRef {
        BlockRef::chunk_blake3_128([n; 16], 512, profile.to_string())
    }

    #[test]
    fn l1_summary_is_per_namespace_profile_and_compact() {
        let mut inv = CasInventory::new();
        inv.insert(blob(1), 3);
        inv.insert(blob(2), 7);
        inv.insert(chunk(1, "gear-v1"), 4);
        inv.insert(chunk(2, "gear-v1"), 2);

        let summaries = inv.summaries();
        // One summary per (namespace, chunk_profile): Blob/None and Chunk/gear-v1.
        assert_eq!(summaries.len(), 2, "compact: per group, not per block");

        let blob_s = summaries
            .iter()
            .find(|s| s.namespace == BlockNamespace::Blob)
            .unwrap();
        assert_eq!(blob_s.block_count, 2);
        assert_eq!(blob_s.max_generation, 7, "max generation in the group");
        assert!(blob_s.chunk_profile.is_none());

        let chunk_s = summaries
            .iter()
            .find(|s| s.namespace == BlockNamespace::Chunk)
            .unwrap();
        assert_eq!(chunk_s.block_count, 2);
        assert_eq!(chunk_s.chunk_profile.as_deref(), Some("gear-v1"));
    }

    #[test]
    fn l1_fingerprint_is_order_independent_and_detects_difference() {
        let mut a = CasInventory::new();
        a.insert(blob(1), 1);
        a.insert(blob(2), 1);
        let mut b = CasInventory::new();
        // Same set, inserted in the opposite order.
        b.insert(blob(2), 1);
        b.insert(blob(1), 1);
        assert_eq!(
            a.summaries()[0].fingerprint,
            b.summaries()[0].fingerprint,
            "equal sets fold to an equal fingerprint regardless of insert order"
        );

        let mut c = CasInventory::new();
        c.insert(blob(1), 1);
        c.insert(blob(3), 1); // different member
        assert_ne!(
            a.summaries()[0].fingerprint,
            c.summaries()[0].fingerprint,
            "a differing set yields a differing fingerprint"
        );
    }

    #[test]
    fn l2_pagination_is_bounded_and_walks_the_whole_set_in_sorted_order() {
        let mut inv = CasInventory::new();
        for n in 0..10u8 {
            inv.insert(blob(n), 1);
        }
        // Walk the whole inventory in bounded pages of 3 — never the whole set at once.
        let mut seen = Vec::new();
        let mut cursor: Option<BlockRef> = None;
        loop {
            let page = inv.page(cursor.as_ref(), 3);
            assert!(
                page.entries.len() <= 3,
                "page never exceeds the limit (V25)"
            );
            seen.extend(page.entries.iter().map(|(b, _)| b.clone()));
            match page.next {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        assert_eq!(seen.len(), 10, "pagination covers every block exactly once");
        let mut sorted = seen.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            seen, sorted,
            "pages are in sorted order, no overlap, no gap"
        );
    }

    #[test]
    fn l2_page_limit_zero_still_yields_a_next_cursor() {
        let mut inv = CasInventory::new();
        inv.insert(blob(1), 1);
        let page = inv.page(None, 0);
        assert!(page.entries.is_empty());
        assert!(page.next.is_some(), "a non-empty inventory still advances");
    }

    #[test]
    fn l3_reconcile_returns_only_held_suspected_refs() {
        let mut inv = CasInventory::new();
        inv.insert(blob(1), 1);
        inv.insert(blob(2), 1);
        let suspected = vec![blob(1), blob(3), blob(2), blob(9)];
        let held = inv.held_among(&suspected);
        assert_eq!(held, vec![blob(1), blob(2)], "only the held refs, sorted");
    }

    #[test]
    fn insert_is_generation_monotone() {
        let mut inv = CasInventory::new();
        assert!(inv.insert(blob(1), 5));
        assert!(!inv.insert(blob(1), 3), "older generation ignored");
        assert!(inv.insert(blob(1), 6), "newer generation accepted");
        assert_eq!(inv.len(), 1);
    }
}
