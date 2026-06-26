# Distributed CAS (#498) -- Block-Map Design Sketch (Grounding)

**Status:** grounding design sketch (build-free). **Not a decision ADR** -- the normative identifier,
trust, and durability rules already live in `docs/adr/ADR-0498-G2-cas-blockref-hash-model.md` (V7/V8/
V16/V21/V25/V28) and the transport in `docs/adr/ADR-0498-ADR2-control-plane-transport.md` (V10/V18).
This doc expands G2's `HolderAdvertisement` sketch into a full block-map design and **decides the one
open axis: the gossip transport** (Finding 3). Companion to `SPIKE-498-tech-selection.md` and
`distributed-cas-read-path-inventory.md`.

> N-node-native: the block map is a distributed directory `BlockRef -> {NodeId}`. It carries **only
> metadata (locators)**; block **bytes never travel over the gossip bus** -- they move only on demand,
> over QUIC pull, once per block per node, then cached (the project's 1:n block/byte principle).

## Context

#498 needs every node to answer "which nodes might hold this block?" without a central index and
without shipping bytes. The membership layer (#495) already solves the structurally identical problem
for node liveness: a gossiped `Heartbeat` per node, an ABA-safe `ingest`, a receiver-monotonic clock,
and a `MembershipView = NodeId -> record`. The block map is the same machinery with a different
payload, so it should **reuse the pattern, not reinvent it**.

## Problem

1. Distribute `BlockRef -> {NodeId}` across the cluster as light, eventually-consistent gossip.
2. Keep it scalable at 100k-1M blocks -- **no** "gossip all block IDs" API (V25).
3. Never let GC treat the map as a liveness proof (V8): uncertainty -> keep (G7 coupling).
4. Pick a concrete gossip transport that works **today** and name its cross-subnet gate.

## Decision

### D1 -- Data type: reuse `BlockRef`, mirror `Heartbeat`

`BlockMap = HashMap<BlockRef, HashSet<NodeId>>` (NodeId-keyed, exactly like `MembershipView`).
`BlockRef` is reused verbatim from `crates/sentinel-common/src/block_ref.rs` (#500a) -- no new
identifier. The advertisement is G2's V16 record, which mirrors `Heartbeat` field-for-field:

```rust
// normative shape from ADR-0498-G2 (V16) -- reproduced, not redefined here.
HolderAdvertisement {
    node_id,                  // mirrors Heartbeat.node_id
    node_boot_id,             // mirrors Heartbeat.boot_id  (ABA across reboots)
    node_cas_generation,      // monotonic per node, like Heartbeat.incarnation
    block_ref,                // the payload that differs from Heartbeat
    action: Add | Remove,
    advertised_at_generation,
    expires_after,
}
```

Mapping to the membership primitives that already exist (`crates/sentinel-common/src/membership.rs`):

| Membership (#495) | Block map (#498) | Reused mechanism |
| --- | --- | --- |
| `Heartbeat { node_id, boot_id, incarnation, ... }` (`:38`) | `HolderAdvertisement { node_id, node_boot_id, node_cas_generation, block_ref, action, ... }` | same gossip envelope |
| `MembershipView` (`:95`), NodeId-keyed | `BlockMap = HashMap<BlockRef, HashSet<NodeId>>` | NodeId-keyed view |
| `ingest` (`:115`): boot_id change -> `Restarted`; older incarnation -> `RejectedStale` | higher `node_cas_generation` wins; `Remove@G` suppresses older `Add<G`; different `node_boot_id` -> newer incarnation (ABA) | same ABA / monotonic-merge logic |
| receiver-stamped `last_seen_ms` | receiver-stamped advertisement time + `expires_after` | receiver-monotonic clock (never trust sender wall-clock) |

### D2 -- Subject naming (mirrors membership)

Membership uses `sentinel/cluster/membership/{node_id}` with a wildcard subscribe `.../*`
(`services/sentinel-daemon/src/cluster_membership.rs:16`). The block map mirrors it:

- **Publish:** `sentinel/cluster/blockmap/{node_id}` (each node owns its subtree).
- **Subscribe:** wildcard `sentinel/cluster/blockmap/*`.
- **Encoding:** JSON serde, same as membership.

### D3 -- Gossip transport (Finding 3 -- DECIDED, not left open)

Two transports exist in the workspace, with different reach **today**:

- **Zenoh pub/sub:** works **same-L2 LAN now** -- membership rides it and #495 verified it live
  (heartbeats over Zenoh peer discovery). **Cross-subnet** Zenoh is gated on **#495 Phase 2**
  (`connect`/`endpoints`), which is **not yet implemented**.
- **QUIC control plane (#569):** the only cross-subnet session today; `cluster-control/envelope.rs`
  already sketches `RefQuery { block_ref }` / `PinQuery { block_ref }` request variants.

**Decision: option A.** Gossip the map over **Zenoh** (`sentinel/cluster/blockmap/*`), same-L2 now,
cross-subnet gated on #495 Phase 2; **plus** an on-demand **L3 `RefQuery` over the QUIC control plane**
as a point lookup for a specific `BlockRef` when a node's gossiped view is cold or cross-subnet.

Rationale:
- Zenoh for the map keeps the design **consistent with the architecture target** (the TOGAF Cluster
  view fixes Zenoh as the Rust-side bus; the gossip belongs there, not on the control plane).
- Multicast/peer pub-sub is the right shape for an eventually-consistent directory; doing the map over
  QUIC-control (option B) would force **O(N) unicast fanout** and has no multicast.
- The QUIC `RefQuery` is reused for the **point** case (cold/cross-subnet lookup), which is exactly
  what a request/response control RPC is good at -- no new endpoint.

**Named gate:** cross-subnet map propagation depends on **#495 Phase 2** (`connect`/`endpoints`).
Until then, the map is fully gossiped only within an L2 segment; cross-subnet nodes rely on the QUIC
`RefQuery` point lookup. This is the one explicit dependency the implementation must track.

(Option B -- map fully over QUIC-control -- is recorded as the rejected alternative: no Zenoh gate,
but no multicast and O(N) fanout, and it diverges from the Zenoh-fixed target architecture.)

### D4 -- Locator, not liveness (V8 / G7)

The map answers "where might the block be?", never "is the block still needed?". GC consults a
pin/ref index for liveness, never the block map. On uncertainty/timeout/unknown-node -> **keep** (the
exact G7 keep-on-uncertainty rule from `ADR-0499-G7-cluster-delete-guard.md`).

### D5 -- Leveled anti-entropy (V25 / V16) -- no "gossip all IDs"

- **L1:** a generation summary per `namespace`/`chunk_profile` (a single number per node per space) --
  cheap, gossiped continuously.
- **L2:** a paginated inventory by prefix/range, exchanged only when L1 generations diverge.
- **L3:** on-demand reconciliation for a suspected-missing `BlockRef` (the QUIC `RefQuery`).

There is **no** API that gossips every block ID (unscalable at 100k-1M blocks, V25).

## Non-Goals

- The QUIC pull **wire** for block bytes (that is PR2 -- see the spike's "BUILD" verdict).
- `BlockResolver` read-path routing (PR3 -- see the read-path-inventory doc, V9).
- CAS GC interaction beyond the V8/G7 keep rule (#499).
- Strong consensus over the map (it is light eventually-consistent gossip; mutable metadata is #496).

## Data Types

`BlockMap = HashMap<BlockRef, HashSet<NodeId>>`; `HolderAdvertisement` (V16, shape above);
`BlockRef`/`BlockNamespace`/`HashAlgorithm` reused from `sentinel-common` (`block_ref.rs:130/30/66`).

## State Machine / Protocol

`Add` on durable publish (after V28 `temp -> verify -> fsync -> rename -> fsync(dir)`); `Remove` on
GC reclaim. Merge per V16: same `node_id+boot_id+block_ref` -> higher generation wins; `Remove@G`
suppresses older `Add<G`; different `boot_id` -> newer membership incarnation (ABA after reboot).
Receiver stamps the accept time and honors `expires_after` (entries self-expire if a holder goes
silent -- a stale locator, **not** a liveness signal).

## Failure Modes

- **Stale holder** (advertised, block since gone): a pull to that holder fails -> the puller drops the
  holder from its local map (negative cache, PR3); the map entry self-expires via `expires_after`.
  This is a **locator miss**, never interpreted as liveness (V8).
- **Node reboot:** new `boot_id` -> the ABA path replaces the prior incarnation's advertisements
  (same logic as `ingest` `Restarted`).
- **Map says holder, block truly absent everywhere:** the resolver pull fails on all holders ->
  surfaced as a real "block unavailable" error; GC still must not reclaim references it cannot prove
  dead (keep-on-uncertainty).
- **Cross-subnet before #495 Phase 2:** no gossip across subnets -> fall back to QUIC `RefQuery` point
  lookup; documented gate, not a silent gap.

## Security

`BlockRef` is neither secret nor a capability (V21); the map is metadata only. Transport trust is the
pinned QUIC plane (V10) for the `RefQuery`/pull side; Zenoh gossip stays within the trusted
single-security-domain cluster (Track A). No bytes on the gossip bus.

## Public Claim Boundary

This is a **design sketch**, not an implemented map. It claims: a reuse-consistent shape (mirrors
`Heartbeat`), a decided gossip transport (Zenoh + QUIC `RefQuery`, gate = #495 Phase 2), and
conformance to G2's V8/V16/V25/V28. It does **not** claim a built or measured map.

## Open Follow-ups

- PR2: QUIC block-pull wire (large/streaming frames, verify-on-receive).
- PR3: `BlockResolver` routing over the four read anchors; negative-cache for stale holders.
- #495 Phase 2: cross-subnet Zenoh `connect`/`endpoints` (the named transport gate).

## Cross-references

- Identifier/trust/durability rules: `docs/adr/ADR-0498-G2-cas-blockref-hash-model.md` (V7/V8/V16/V21/V25/V28).
- Transport ADR: `docs/adr/ADR-0498-ADR2-control-plane-transport.md` (V10/V18).
- Reuse primitives: `crates/sentinel-common/src/membership.rs` (Heartbeat/MembershipView/ingest),
  `services/sentinel-daemon/src/cluster_membership.rs` (subject + wildcard),
  `crates/sentinel-common/src/block_ref.rs` (BlockRef).
- Keep-on-uncertainty: `docs/adr/ADR-0499-G7-cluster-delete-guard.md`.
