# ADR-0498: CAS BlockRef & hash model (G2)

- **Gate:** G2 (blocks #498; the BlockRef type also blocks #500a)
- **Status:** Proposed
- **Primary issue:** #498 (distributed CAS)
- **Related issues / gates:** ADR-2 (transport), V7/V8/V21/V25/V28
- **Supersedes / Superseded by:** —

> **N-node-native rule:** all schemas/messages/APIs MUST be `NodeId`-keyed, never a hard
> source/target pair. Two nodes are the first test, not the ceiling.

## Context

**Two hash worlds exist today.** `CasStore` uses **SHA-256**: `hash(data) -> [u8; 32]`
(`crates/sentinel-fs/src/cas.rs:58`), `blob_path(&[u8; 32])` (`:65`, already private),
`store -> ([u8; 32], bool)` (`:80`). The chunker / artifact plane uses **BLAKE3-128**:
`ChunkHash = [u8; 16]` (`crates/sentinel-fs/src/chunker.rs:17`), `blake3_hash_128`
truncates `blake3::hash` to 128 bits (`:184`). A distributed block map that picks one
hash space blindly would force an expensive rebuild later, and `1:n everywhere` would
guess a hash space.

## Problem

What is the canonical cross-node block identifier, given two incompatible local hash
spaces, and what is each hash's trust boundary?

## Decision

**Introduce a namespaced, self-describing `BlockRef`; the block map is a locator, never
a liveness/ref source; CAS publish is crash-durable before advertisement.**

- **`BlockRef` (V7):** `BlockRef { namespace: Blob | Chunk | FsTrash | Manifest,
  algorithm, digest, size_bytes, chunk_profile, version }` — e.g.
  `cas-blob:v1:sha256:<hex>`, `artifact-chunk:v1:blake3-128:fastcdc-v1:<hex>`.
  `namespace` prevents FS_TRASH/Blob/Chunk/Manifest mixing; `algorithm` prevents
  SHA-256/BLAKE3 mixing; `size_bytes` guards against cache poisoning / stream abuse;
  `version` allows migration. **BLAKE3-128 is a trusted-cluster dedup identity only,
  NOT an adversarial security boundary** — a transport-verified remote content ID uses
  BLAKE3-256/SHA-256 (stated here, normative).
- **Block map = locator only (V8):** `BlockMap` answers *"where might the block be?"*;
  a pin/ref index answers *"is it still needed?"*. **GC must never treat the block map
  as a liveness proof.** Uncertainty/timeout/unknown-node → **keep** (see G7).
- **CAS auth is not a capability + plane separation (V21):** a `BlockRef` is neither
  secret nor an access token. Track A = **one trusted single-security-domain cluster**.
  Control-plane (ownership/membership/GC-decision) ≠ data-plane (CAS pull/resolve) ≠
  state-plane (events/redb/FS/refs). *"Blob pulled ≠ owner moved"*, *"holder exists ≠
  block is live"*.
- **Anti-entropy is leveled (V25):** L1 generation summary per namespace/profile; L2
  paginated inventory by prefix/range; L3 on-demand reconciliation for suspected
  missing. #498 must **not** define a "gossip all block IDs" API (unscalable at
  100k–1M blocks).
- **Durable publish (V28):** `cas.rs:80` does `fs::write(tmp)+rename` with **no fsync**.
  Remote-pull publish + local store become `temp → stream-hash-verify → fsync(file) →
  atomic rename → fsync(parent dir) → then holder-advertisement/pin-release`.

## Non-Goals

- The actual pull transport (ADR-2 / #498 PR2) and the `BlockResolver` routing (#498
  PR3, V9) — G2 fixes the identifier + trust + durability rules, not the wire.
- Multi-tenant CAS authz / capability tokens (Track H).
- A Merkle tree (not now; the ADR only forbids the unscalable gossip-all variant).

## Data Types

`BlockRef` (new, `sentinel-fs`/`sentinel-common`). `HolderAdvertisement { node_id,
node_boot_id, node_cas_generation, block_ref, action: Add|Remove,
advertised_at_generation, expires_after }` (V16). All `NodeId`-keyed.

## State Machine / Protocol

Publish: `temp → verify → fsync → rename → fsync(dir) → advertise`. Anti-entropy
conflict (V16): same `node_id+boot_id+block_ref` → higher generation wins, `Remove@G`
suppresses older `Add<G`; different `boot_id` → newer membership incarnation wins (ABA
after reboot).

## Failure Modes

- **Crash between write and advertisement:** the CAS startup reconcile scans temp dirs
  (delete incomplete temps), lazily verifies canonical dirs, rebuilds advertisements
  from durably-verified files, reconciles pins (closes the V28 window).
- **Cache poisoning:** `size_bytes` + digest verify in temp before rename.
- **Hash-space confusion:** `namespace`+`algorithm` in the `BlockRef` make a cross-space
  collision a type/parse error.

## Tests

- A `BlockRef` round-trips and rejects a wrong-namespace/wrong-algorithm reference.
- A manipulated pulled block is rejected (digest mismatch in temp; never published).
- Publish is durable before advertisement (crash-injection between rename and advertise
  → reconcile recovers).
- Anti-entropy generation conflict resolution.

## Benchmarks

Pull latency p50/p99 vs block size + dedup ratio + cache-hit (#498 bench); durable
publish: fsync strategy file+dir vs batched (V28). Register:
`sentinel-fs-distributed-cas-pull (#498)`.

## Backward Compatibility

`BlockRef` is additive; existing `CasStore` SHA-256 blobs map to `cas-blob:v1:sha256`,
existing chunks to `artifact-chunk:v1:blake3-128:fastcdc-v1`. No data rewrite.

## Security

Single trust domain (Track A); `BlockRef` is not a capability. The QUIC pull server
accepts only `BlockRef`s, never a file path; cert-pinned peers; rate-limited per node
(V10).

## Public Claim Boundary

- May claim after #498: distributed pull-by-hash with verified, namespaced block IDs.
- **May NOT claim:** adversarial security from BLAKE3-128, multi-tenant CAS, or "ms"
  pull without the measured bench.

## Open Follow-ups

- `BlockResolver` over all read paths (#498 PR3, V9); CAS replication/repair (G-D3).
