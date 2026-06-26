# Distributed CAS (#498) -- Read-Path Inventory (Grounding G-4)

**Status:** grounding doc (build-free). Companion to `SPIKE-498-tech-selection.md` and
`distributed-cas-block-map-design.md`. **No code here** -- this is the complete, repo-wide inventory
of every blob/chunk/object reader, so the #498 implementation (PR3) routes **all** of them through a
future `BlockResolver` and leaves **no** silent local-only hole. The future CI gate
(`check-block-readers.py`) is only **sketched** here, not implemented.

Inventory method mirrors `docs/adr/ADR-0499-G7-cluster-delete-guard.md`: a fresh `rg` audit,
canonical/derived classification, and **keep-on-uncertainty** (if a reader cannot be proven
local-only, it is listed as an anchor candidate). Audit was run repo-wide over
`crates/sentinel-fs/`, `services/sentinel-daemon/`, and `crates/sentinel-console-plane/`.

## The fresh audit (reproduce this)

```text
rg -n 'blob_path|read_chunk|read_chunks|segments?\.read|FS_CHUNKS\.get|cas\.read|read_file|read_layer_file|fn read\b|fn read_batch' \
   crates/sentinel-fs/src/ services/sentinel-daemon/src/ crates/sentinel-console-plane/src/
```

Every non-test hit that actually reads block/chunk/blob bytes is a row below. RwLock `.read()` guards
in the daemon (`platform_state.read()`, `runtime_health.read()`, ...) and the TcpStream
`stream.read(&mut chunk)` in `operator_api.rs:2823/2882` (HTTP request parsing) are **not** content
reads and are excluded with that reason.

## Two data planes, no shared read chokepoint

There is **no** common `Read` trait and **no** single read funnel. Two independent planes exist, so
**two hook sets** are required:

- **CAS plane (whole-blob, SHA-256):** `crates/sentinel-fs/src/cas.rs` -- `CasStore` keyed by a
  32-byte SHA-256, blobs on disk under sharded prefix dirs.
- **Artifact plane (chunk, BLAKE3-128):** `crates/sentinel-fs/src/artifact.rs` -- `ArtifactPlane`
  over `segment.rs` segment files, chunk identity = BLAKE3-128 from the `gear-v1` CDC profile.

## Inventory table

Legend -- **Kind:** LEAF (touches bytes directly) / COMPOSITE (delegates to a leaf).
**Anchor:** is this a `BlockResolver` (V9) attach point? **Pin/SF:** does it need read-pin /
single-flight when remote pull is added?

| # | file:line | reads | Plane | Kind | Anchor (V9) | Pin/SF |
| --- | --- | --- | --- | --- | --- | --- |
| L1 | `cas.rs:104` `pub fn read(&self, hash:&[u8;32])` | whole blob by SHA-256 | CAS | LEAF | **YES (anchor 1)** -- `blob_path` (`cas.rs:65`) is **private** -> needs `pub(crate)` or a resolver method | pin + SF |
| B1 | `artifact.rs:299` `pub fn read_chunk_raw(&self, hash:&ChunkHash)` | one compressed chunk | Artifact | LEAF (calls `segments.read` at `:310`) | **YES (anchor 2)** -- chunk-single | pin + SF |
| B2 | `artifact.rs:316` `pub fn read_chunk_decompressed(&self, hash)` | one chunk via cache, else B1 (`:329`) | Artifact | COMPOSITE (cache + B1) | **optional (anchor 4)** -- cache hit = local; miss falls to B1 | SF (cache releases lock, see gaps) |
| B3 | `artifact.rs:348` `pub fn read_chunks_decompressed(&self, hashes:&[ChunkHash])` | a batch of chunks | Artifact | LEAF for the batch (calls `segments.read_batch` at `:394`, **bypasses B1**) | **YES (anchor 3)** -- separate hook; does **not** funnel through B1 | pin + SF (per chunk) |
| C1 | `read_planner.rs:23` `plane.read_chunks_decompressed(&manifest)` | manifest -> chunk batch | Artifact | COMPOSITE -> B3 | inherits B3 | inherits |
| C2 | `read_planner.rs:69` `self.plane.read_chunk_decompressed(hash)` | single chunk | Artifact | COMPOSITE -> B2 | inherits B2 | inherits |
| C3 | `layer.rs:212` `pub fn read_file(&self, agent_id, inode)` -> `cas.read` at `:221` | a layered file (whole-blob) | CAS | COMPOSITE -> L1 | inherits L1 | inherits |
| C4 | `home_manifest.rs:553` `plane.read_chunks_decompressed(&hashes)` | rehydrate a home manifest | Artifact | COMPOSITE -> B3 (second B3 caller) | inherits B3 | inherits |
| C5 | `fuse.rs:339` `fn read(...)` -> `layer.read_file` at `:356` | FUSE read syscall (latency-critical) | CAS | COMPOSITE -> C3 -> L1 | inherits L1 | inherits |
| C6 | `operator_api.rs:2392` `fn read_layer_file_bytes(...)` -> `layer.read_file` at `:2405` (called `:2419`) | daemon ransomware-test file read | CAS | COMPOSITE -> C3 -> L1 (cross-crate, in `sentinel-daemon`) | inherits L1 | inherits |
| X1 | `segment.rs:120` `pub fn read(&self, loc:&ChunkLocation)` | raw bytes at a segment location (**no hash**) | Artifact | LEAF (storage) | **NO** -- below the hash; **bypass surface** (see below) | n/a (no hash) |
| X2 | `segment.rs:141` `pub fn read_batch(&self, locations)` | raw bytes batch (**no hash**) | Artifact | LEAF (storage) | **NO** -- bypass surface | n/a (no hash) |

`segment.rs:155 read_batch_sync` / `:184 read_batch_uring` are **private** (internal to `read_batch`)
and not part of the public surface.

### Negative results (verified clean, listed so the audit is provably complete)

- **`crates/sentinel-fs/src/metadata.rs`:** **no** blob/chunk readers. It is refcount/pin bookkeeping
  only -- decoupled from the read path. (rg returned nothing for read primitives.)
- **`crates/sentinel-console-plane/`:** **no** direct CAS/Artifact reads. The console plane does not
  call `cas.read`, `read_chunk*`, `blob_path`, `read_layer_file`, or `segment.read` -- it consumes
  block deltas through its own codec, not the storage readers. Documented as **clean** so PR3 need not
  hook it.

## The four anchor points for `BlockResolver` (V9)

The resolver attaches at the **artifact/CAS level**, never at the segment level (no hash there):

1. **`cas.rs:104 read`** -- whole-blob CAS. Requires `blob_path` (`cas.rs:65`) to become `pub(crate)`
   or be replaced by a resolver-owned path method (it is currently private).
2. **`artifact.rs:299 read_chunk_raw`** -- single chunk.
3. **`artifact.rs:348 read_chunks_decompressed`** -- chunk **batch**; a **separate** hook because it
   bypasses B1 (goes straight to `segments.read_batch` at `:394`). Hooking B1 alone would miss every
   batch read (the FUSE planner C1 and the home-manifest rehydrate C4 both go through B3).
4. **`artifact.rs:316 read_chunk_decompressed`** (optional) -- a cache hit is local, but a miss must
   route to the pull path, so the resolver wraps it for single-flight even if the cache hit stays
   local.

All composite readers (C1-C6) inherit resolution from their leaf, so routing the four anchors covers
every current reader -- including the cross-crate daemon caller C6.

## Bypass surface (reader-side R2 logic)

This is the #496 "public low-level method = latent hole" argument, applied to **readers**:

`segment.rs:120 read` and `:141 read_batch` are `pub fn`. **Today** they are reached only via
`artifact.rs:310` (B1) and `:394` (B3) -- otherwise only by segment's own tests -- so the four
artifact/CAS anchors dominate the **current** readers. **But** a **future** direct caller of
`segment.read` would reach raw bytes **below** the hash and thus bypass the `BlockResolver`
**silently** (there is no hash at the segment level to trigger a pull). Mitigation, decided here:

- The resolver hooks at the **artifact** level (B1/B3), not the segment level.
- The **`check-block-readers` CI gate guards the segment level** -- any new caller of `segment.read`/
  `segment.read_batch` outside `artifact.rs` (and outside tests) fails the gate. Alternatively (or
  additionally), make `segment.read`/`read_batch` **`pub(crate)`** so the type system forbids an
  external direct reader.

## Gaps PR3 must close (not present today)

1. **No read-verify.** Neither plane verifies bytes against the hash on read (the CAS write path
   verifies on store, not on read). A remote-pulled block **must** be verified on receive (V28
   temp -> verify -> publish). This is a **new** stage.
2. **No single-flight.** `read_chunk_decompressed` (B2) consults the cache, releases the lock, then
   reads the segment -- two concurrent misses for the same chunk would both pull. The resolver needs
   an **in-flight map** (one pull per `BlockRef`, others await) to avoid a thundering herd.
3. **No read-pin.** A pull -> cache write races CAS GC (#499): GC could reclaim a just-pulled block
   before it is referenced. The resolver must **pin** a pulled block until it is referenced.
4. **Segment level is unsuitable as a hook** -- no hash is available there (it is location-addressed),
   so resolution and verification must live one level up, at the artifact/CAS readers.

## `BlockResolver` trait sketch (signature only -- NOT implemented here)

```rust
// PR3 artifact -- shown for grounding only.
pub trait BlockResolver {
    /// Resolve a block to a local handle, pulling it from a holder if it is missing.
    /// Stages: local hit -> block map lookup -> QUIC pull -> verify-in-temp ->
    ///         atomic publish/cache + pin -> deliver.
    fn resolve(&self, block: &BlockRef) -> anyhow::Result<BlockHandle>;
}
```

Attach points: anchors 1-4 above. `BlockRef` is reused from `sentinel-common` (`block_ref.rs`); no
new identifier.

## CI gate sketch (PR3 artifact -- NOT built here)

Modeled on `scripts/check-fenced-writers.py` (the fenced-writer gate), inverted for **readers**:

- **READERS list:** the segment storage primitives (`segment.read`, `segment.read_batch`) and the raw
  CAS path (`blob_path`, direct `fs::read` on a blob path).
- **read_re:** a regex per primitive that flags a call site.
- **Whitelist:** `artifact.rs` (the legitimate B1/B3 callers), the resolver module itself,
  constructors, and `#[cfg(test)]`.
- **Effect:** a new reader that does not route through `BlockResolver` (i.e. a direct segment/blob
  read outside the whitelist) fails CI -- the same guarantee `check-fenced-writers.py` gives for
  writers, for readers. This is documented as the PR3 enforcement, **not** implemented in this docs PR.

## Cross-references

- Anchors and identity: `BlockRef` in `crates/sentinel-common/src/block_ref.rs` (#500a).
- Transport + build-vs-reuse: `docs/spikes/SPIKE-498-tech-selection.md`.
- Map gossip: `docs/cas/distributed-cas-block-map-design.md`.
- Pattern source for this inventory: `docs/adr/ADR-0499-G7-cluster-delete-guard.md`;
  gate pattern: `scripts/check-fenced-writers.py`.
- GC interaction (read-pin vs reclaim): #499.
