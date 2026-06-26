# SPIKE #498 -- Distributed CAS / Pull-by-Hash: tech selection (build vs. reuse)

**Status:** grounding spike (build-free, docs-only). Not a measured spike like SPIKE-490 --
its `## Method` is **SOTA pattern analysis (web-cited) + codebase reuse mapping + a build-vs-reuse
matrix**, not a benchmark. No `cargo` build was run.
**Verdict: REUSE the transport infrastructure and the gossip pattern; BUILD only the block-stream
wire protocol and the resolver/map types. No external storage, no DHT, no reciprocity ledger.**
Conditional on the prerequisites at the end (gated on #496 PR2 + #495 cross-node session).

## Question

Issue #498 wants a **distributed content-addressed store**: a block map (`BlockRef -> {NodeId}`)
gossiped across the cluster, and **pull-by-hash** of the actual block bytes over QUIC, so a node that
is missing a chunk/blob can fetch it from a node that holds it. The XL implementation is split into
three PRs and is **gated** (no cross-node write before the fencing of #496 PR2, plus the #495
cross-node session). Before that implementation starts, two questions must be answered against the
**real** code, not against assumptions:

1. **Build vs. reuse:** which parts of "distributed CAS" already exist in the workspace (QUIC, cert
   pinning, gossip, content identity) and must be reused, and which parts are genuinely new and must
   be built? Where is the line, honestly drawn?
2. **SOTA patterns:** which patterns from production distributed-CAS / content-exchange systems do we
   adopt, and which do we deliberately **avoid** because they solve a problem we do not have (a
   trusted, single-security-domain cluster -- not an open p2p swarm)?

## Method

Build-free. Three inputs, no measurement:

1. **SOTA pattern survey (web-cited).** Four production systems that move content by hash:
   IPFS/libp2p **Bitswap**, **casync/desync**, the **OCI Distribution** spec, and **Git's pack
   protocol**. For each: the pattern we adopt, the anti-pattern we avoid, and an explicit check
   against the project's **1:n block/byte principle** (move pointers over the bus, move bytes only
   on demand, once per node, then cache).
2. **Codebase reuse mapping.** Every reusable primitive located by file:line in the current
   workspace (verified by `rg` while writing this spike), so a #498 implementer can start PR1/2/3
   without re-discovery.
3. **Build-vs-reuse matrix.** A per-axis decision (transport, map gossip, content identity, codec,
   storage) marked DECIDED, feeding the Go/No-Go.

## SOTA survey

Each row: the **pattern** we take, the **anti-pattern** we leave, and the **1:n fit**.

| System | Pattern we adopt | Anti-pattern we avoid | 1:n block/byte fit |
| --- | --- | --- | --- |
| **IPFS/libp2p Bitswap** | Two-phase want-list: a lightweight `want-have` (returns `have`/`dont-have`) to locate holders, then a `want-block` to the holder that actually transfers bytes. Sessions group a transfer and remember which peers answered `have`, so later requests go to known holders. ([IPFS Docs][1], [IPFS spec][2]) | Falling back to a **DHT** for content routing when no connected peer has the block. In a known-membership cluster the holder set is the gossiped block map -- a global DHT is unnecessary machinery and adds latency and attack surface. ([IPFS Docs][1]) | Strong: `want-have` moves only the CID (a pointer), the block byte transfer is a separate, on-demand step. This is exactly "advertise pointers, pull bytes once." |
| **casync / desync** | An **index** (`.caibx`/`.caidx`) is a linear list of `(chunk hash, size)` -- a small stand-in for a large object -- and the client pulls **only the chunks it is missing**, reusing local "seed" data chunked with the same logic; chunks are named by a strong content hash and fetched by that hash from a chunk store (works over plain HTTP). ([casync blog][3], [desync][4]) | casync's chunk store is a **passive** HTTP directory with no holder discovery -- the client must already know the store URL. We need active holder discovery (the gossiped map), so we take the index/pull-missing model but **not** the "one well-known static store" assumption. ([desync][4]) | Strong, and closest to our model: the #500a home manifest **is** the index (chunk refs), the ArtifactPlane **is** the chunk store, and #498 pulls only missing chunks by hash. No re-chunking, no full copy. |
| **OCI Distribution** | Pull a blob strictly by digest: `GET /v2/<name>/blobs/<digest>`; the digest is the content hash, so the same digest always returns the same bytes, and the client **verifies** the downloaded bytes against the digest (immutability + integrity + dedup). ([OCI distribution-spec][5]) | The full registry surface (auth flows, tag mutation, manifest lists, cross-repo mount). We need only "give me the bytes for this digest, I will verify them" -- not a registry. ([OCI distribution-spec][5]) | Strong: digest-addressed pull maps 1:1 to "the server accepts only a `BlockRef`, never a path" and to hash-verify-on-receive (V10/V28). |
| **Git pack protocol** | `have`/`want` negotiation: the client advertises `want` (object ids it needs) and `have` (ids it already holds), so the two sides converge on the **minimal** set to transfer -- the conceptual core of leveled anti-entropy. ([git protocol-v2][6], [git pack-protocol][7]) | **Thin packs and delta chains** -- packs whose objects are deltas against bases not in the pack, requiring the receiver to "thicken" them. ([git pack-protocol][7]) Our chunks are already CDC-deduplicated and individually content-addressed; layering delta encoding on top adds reconstruction complexity for little gain. | Good for the **map** layer (advertise what you have, reconcile the difference), not for the byte layer (we transfer whole verified chunks, not deltas). |

**Cross-cutting takeaway:** every one of these systems separates a cheap *locate/negotiate* step
(want-have, index, digest reference, have/want) from an expensive *transfer* step, and every one
addresses the payload by a content hash that the receiver verifies. That is precisely the #498 design
(`BlockRef` map gossip + QUIC pull + verify-on-receive) and precisely the project's 1:n principle.
None of the four justifies a DHT, a reciprocity ledger, delta chains, or an external storage tier for
**our** setting (a trusted, bounded, single-security-domain cluster).

## Reuse map (verified by file:line)

Located by `rg` in this worktree while writing the spike:

| Primitive | Location | Reuse for |
| --- | --- | --- |
| QUIC client + server (#569) | `crates/sentinel-cluster-control/`: `client.rs:13` `ControlClient`, `server.rs:19` `ControlServer` | Block-pull transport endpoint (PR2) -- **no new QUIC stack** |
| Cert pinning (V10) | `crates/sentinel-cluster-control/tls.rs:28` `PinnedTrustVerifier`, `cert.rs:73` `CertFingerprint` | Mutual-pinned QUIC for block pull |
| Idempotency | `crates/sentinel-cluster-control/idempotency.rs:12` `IdempotencyCache` | Dedup of pull/pin RPCs |
| Frame codec | `crates/sentinel-cluster-control/envelope.rs`: `MAX_FRAME_BYTES = 1 << 20` (u32 **big-endian** length prefix, `to_be_bytes`/`from_be_bytes`), and **already-sketched** `RefQuery { block_ref }` / `PinQuery { block_ref }` request variants | Control RPCs (ref/pin queries) reuse as-is; **block bytes do not** (see below) |
| Gossip pattern (#495) | `crates/sentinel-common/membership.rs`: `Heartbeat` (`:38`), `MembershipView` (`:95`), `ingest` (`:115`, boot_id/incarnation ABA, receiver-monotonic clock); subject in `services/sentinel-daemon/src/cluster_membership.rs:16` `sentinel/cluster/membership/{id}` + wildcard | Block-map `HolderAdvertisement` mirrors `Heartbeat` 1:1 (see block-map-design doc) |
| Content identity (#500a) | `crates/sentinel-common/block_ref.rs`: `BlockRef` (`:130`), `BlockNamespace` (`:30`), `HashAlgorithm` (`:66`) | Canonical cross-node identifier (V7) -- **reuse**, do not redefine |
| Chunk store + CDC | `crates/sentinel-fs/`: `artifact.rs` (ArtifactPlane, BLAKE3-128 chunks), `chunker.rs` gear-CDC `MIN/TARGET/MAX = 16_384/65_536/262_144` (`:10-12`) | The chunk store #498 pulls into; profile is a system invariant (below) |

## Build-vs-reuse matrix -> verdict

Honest line (Finding 1: the new part is **not** thin glue):

| Axis | Decision | Rationale |
| --- | --- | --- |
| **Transport infra** | **REUSE -- DECIDED.** cluster-control QUIC `Endpoint`/connection, `PinnedTrustVerifier` (V10), `CertFingerprint`, `IdempotencyCache`. | A pinned, idempotent QUIC client+server already exists (#569). Building a second one is waste and a second attack surface. |
| **Block-stream wire protocol** | **BUILD -- DECIDED (non-trivial).** A new bidi-stream protocol `BlockRef -> block bytes` with **large/streaming frames**, **backpressure**, and **hash-verify-on-receive**. | The reused codec is u32-BE with a **1 MiB frame cap (`MAX_FRAME_BYTES`)** -- that cap is control-plane sized and **too small for block payloads** (a 256 KB chunk fits, but whole-blob CAS objects and batching do not, and a single 1 MiB framed read is the wrong shape for streaming). The RPCs (`RefQuery`/`PinQuery`) reuse the existing codec; the **byte transfer is a new frame type**. PR2 must not under-scope this. |
| **Map gossip** | **REUSE pattern -- DECIDED.** `HolderAdvertisement` mirrors `Heartbeat`; `BlockMap = HashMap<BlockRef, HashSet<NodeId>>`; ABA via boot_id/generation; receiver-monotonic clock. | #495 already proved this shape for membership. The block map is the same gossip with a different payload. Transport of the gossip itself (Zenoh vs QUIC-control) is decided in the block-map-design doc (it is a real A/B, not left open). |
| **Codec endianness** | **u32 big-endian -- DECIDED.** | cluster-control `envelope.rs` is already big-endian. (Note: `console/.../wt.rs` is little-endian and doc comments that claim "reuse wt.rs u32-BE" are wrong; the block protocol follows the cluster-control BE line, not the console.) |
| **Content identity** | **REUSE `BlockRef` -- DECIDED.** | Defined and merged in #500a; redefining it would fork identity. |
| **Storage tier** | **No external storage, no DHT, no reciprocity ledger -- DECIDED.** | Trusted single-security-domain cluster (V21). The SOTA survey shows each of these solves an open-swarm problem we do not have. |

## Tech decisions to ground

1. **Transport (DECIDED):** extend the cluster-control QUIC line with a **dedicated block-pull stream
   type** (not the 1 MiB control frame). Pull is a bidi stream `BlockRef -> bytes` with
   **hash-verify-on-receive into a temp object, then atomic publish** (V28 temp->verify->rename).
   Codec endianness = big-endian (cluster-control line).
2. **Chunk profile is a SYSTEM INVARIANT, not a #498-local knob (Finding 2).** Chunk size is fixed at
   **ingest** by `chunker` (`MIN/TARGET/MAX = 16K/64K/256K`, recorded in `BlockRef.chunk_profile`),
   and that determines the BLAKE3-128 hashes in the **shared** ArtifactPlane space. **#498 pulls by
   hash and never re-chunks.** A different profile (e.g. 4K/64K/1M) produces *different hashes for the
   same content* -> it **forks the chunk space and breaks cross-dedup** between #500a manifests and
   #498 pulls. **DECIDED: keep `gear-v1` (16K/64K/256K).** The #498 benchmark measures pull latency
   and dedup **at gear-v1**. The issue body's "4K/64K/1M sweep" is therefore **not** a #498-local
   bench axis; any re-lock of the profile is a **cross-issue** proposal (#500a/#497), not a choice
   #498 may make alone.
3. **Zero-copy honesty (DECIDED: no zero-copy claim).** `sendfile`/`splice` are not used and io_uring
   is optional in `sentinel-fs`; the block path copies through user space. The design makes **no**
   zero-copy claim (that is a separate Track-G concern).

## What it is NOT

- **Not a DHT.** Holder discovery is the gossiped block map over a known membership, not a global
  routing table.
- **Not a reciprocity ledger / tit-for-tat.** Bitswap's exchange fairness machinery solves trust in
  an open swarm; our cluster is trusted, so there is nothing to account for.
- **Not delta/thin-pack transfer.** Whole CDC chunks are transferred and verified; no delta chains.
- **Not external storage and not a registry.** No object-storage tier, no OCI registry surface -- only
  "give me the bytes for this `BlockRef`, I verify them."
- **Not strong consensus over the map.** The map is light, eventually-consistent gossip; mutable
  metadata is #496's concern, not this map's.

## #498 benchmark plan (named here, measured in the implementation)

This docs PR has **no benchmarks** (it is build-free). The implementation (PR2/3) must add a
tool-specific bench, registered as `sentinel-fs-distributed-cas-pull (#498)`:

- **Pull latency** p50/p95/**p99/max** for a single chunk and for a batch.
- **Dedup ratio AT gear-v1** (no 4K/64K/1M sweep -- the profile is a system invariant, Finding 2).
- **Durable publish** cost: the fsync/atomic-rename strategy for temp->verify->publish (V28).
- **0 false content:** every pulled block verifies against its `BlockRef` (a correctness gate, not a
  speed number).
- **Host:** idle test VMs **.241/.242**, never **.240** (VM 1069 = the production sim) and **never
  `cargo remote`** (the build server -- benchmarking another machine is meaningless).

## Go / No-Go

**GO to ground #498 on reuse.** The transport, cert pinning, idempotency, gossip pattern, and content
identity all already exist and are the right primitives; the SOTA survey confirms the design (locate
cheap, transfer by verified hash, no DHT/ledger/delta/external store) and confirms it satisfies the
1:n principle. The genuinely new work is bounded and named: the **block-stream wire protocol** (large
frames + backpressure + verify-on-receive), the `BlockResolver`/`BlockMap`/`HolderAdvertisement`
types, and the read-path routing (the read-path-inventory doc enumerates every reader to route).

**Prerequisites (decided by this spike, to be satisfied by the implementation):**

1. **#496 PR2 (fencing) merged** -- no cross-node write before the writer fence; this gates all of
   #498's implementation.
2. **#495 cross-node session** -- the QUIC control plane / cross-subnet path the pull and (optionally)
   the map ride on. Zenoh cross-subnet `connect`/`endpoints` (#495 Phase 2) is **not yet implemented**
   today; same-L2 LAN gossip works now.
3. **Block-stream frame decision** -- a new large/streaming frame type distinct from the 1 MiB control
   frame; codec big-endian; verify-on-receive into temp then atomic publish (V28).
4. **The read-path inventory (Deliverable 2)** -- every blob/chunk reader routed through the future
   `BlockResolver`; no reader left as a silent local-only hole.
5. **Keep `gear-v1` chunk profile** -- #498 pulls by hash and never re-chunks; a re-lock is a
   cross-issue proposal, not a #498 choice.

## References

- [1] IPFS Docs -- Bitswap (want-have/want-block, sessions, DHT fallback): https://docs.ipfs.tech/concepts/bitswap/
- [2] IPFS Standards -- Bitswap Protocol spec (message format): https://specs.ipfs.tech/bitswap-protocol/
- [3] L. Poettering -- "casync -- A tool for distributing file system images" (CDC/buzhash, index, chunk store, seed reuse): https://0pointer.net/blog/casync-a-tool-for-distributing-file-system-images.html
- [4] desync -- alternative casync implementation (caibx/caidx index, chunk store, pull-missing over HTTP): https://github.com/folbricht/desync
- [5] OCI Distribution Specification -- pull blob by digest `GET /v2/<name>/blobs/<digest>`, content-addressable verification: https://github.com/opencontainers/distribution-spec/blob/main/spec.md
- [6] Git -- protocol v2 (have/want fetch negotiation): https://git-scm.com/docs/protocol-v2
- [7] Git -- pack-protocol (negotiation, thin packs, delta objects): https://git-scm.com/docs/pack-protocol
