# ADR-0498: Cross-node control-plane transport (ADR-2)

- **Gate:** ADR-2 (top-level transport decision)
- **Status:** Proposed
- **Primary issue:** #498 (distributed CAS) — shares the transport; also serves #496/#499/Track-C-D control RPCs
- **Related issues / gates:** #495 (bootstrap, SSH-only), V10 (cert pinning), V18 (no 0-RTT for control)
- **Supersedes / Superseded by:** —

> **N-node-native rule:** Even though the foundation is verified on a 2-node cluster
> first, all schemas, messages and APIs MUST be N-node-native (`NodeId`-keyed
> sets/maps, never a hard source/target pair as the cluster model). Two nodes are the
> first test, not the ceiling.

## Context

There is **no cross-node RPC** today. Zenoh is pub/sub only — the bus exposes
`publish` / `subscribe` / `declare_subscriber`
(`crates/sentinel-zenoh/src/lib.rs`), with **no queryable and no `.get()`** —
so it cannot do request/response. A QUIC server with a frame codec already exists, but
only in the dashboard backend: `services/sentinel-dashboard-backend/src/wt.rs` uses
`wtransport::{Endpoint, ServerConfig}` with `Endpoint::server` and a topic+msgpack+zstd
frame codec (`crate::codec::encode_frame`); there is **no** `Endpoint::client` anywhere
(QUIC client does not exist yet). Self-signed TLS lives next to it
(`services/sentinel-dashboard-backend/src/tls.rs`).

The cluster needs request/response for: owner handoff RPCs (`PrepareHandoff`,
`SourceRetiredAck`, `OwnerCommit`), CAS ref/pin queries (`RefQuery`, `PinQuery`),
block pull (#498), and later Track-C/D control. The bootstrap of a **bare** target
(#495) is different: no Sentinel runs there yet.

## Problem

What transport carries cross-node control-plane request/response, and how does it
relate to the bare-shell bootstrap?

## Decision

**One QUIC control stream that reuses the dashboard WebTransport frame codec + bidi
stream + cert auth (the #498 block-pull transport). Zenoh stays metadata-only
(block-map + membership). The bare-shell bootstrap does NOT use QUIC.**

- A single QUIC transport tech is used cross-node (no second RPC stack). The
  daemon-side QUIC control stream reuses the `wt.rs` frame codec and bidirectional
  streams; a daemon-side QUIC **client** (which does not exist today) is built on
  `quinn`/`wtransport`'s client API.
- Messages are a request-scoped `ControlEnvelope` carrying an `idempotency_key`, over
  cert-pinned peers (V10). Request/response serves #496/#499/Track-C-D **after** a
  node has joined.
- **0-RTT is off for control.** It is allowed only for the idempotent `BlockPull`
  (V18) and forbidden for every control-plane mutation (`PinCreate`, `OwnerCommit`,
  `ProvisionNode`, GC delete, route switch, migration state transition) — each carries
  replay risk.
- **Bootstrap exception:** the seed → bare-target step (#495) runs over **SSH /
  bootstrap credential**, not QUIC, because no Sentinel (and no QUIC endpoint) runs on
  the target yet. The QUIC control stream exists only **after** the daemon is up on the
  node (node→node, after join).

**Phase ordering (resolves the transport-vs-#496 dependency):** the QUIC control
stream is built in **early Phase 3a0, before #496 PR2**. #496 PR2 has 2-VM live ACs
(second owner rejected, partition V2, cooperative handoff chef↔node) that need
cross-node request/response, and Zenoh has no queryable. #498 reuses the same stream
afterwards. Until the stream exists, the #496 saga logic is covered by **in-process
2-node tests** (two World instances in one test); the live 2-VM owner ACs hang on the
QUIC stream.

## Non-Goals

- Does not replace Zenoh for metadata (block-map gossip, membership liveness stay on
  Zenoh pub/sub).
- Does not define the bare-shell bootstrap credential flow (G3/#495).
- Does not specify the per-RPC wire schemas/timeouts (those are produced by the 3a0
  control-stream issue and #496/#498 ADRs — pre-inventing them here would be
  unverifiable against not-yet-built code).

## Data Types

`ControlEnvelope { request_id, idempotency_key, kind, payload, ... }` (produced by the
3a0 control-stream issue). Reuses the existing `wt.rs` frame codec. Peer identity via
pinned node certs (V10). N-node-native: addressing is by `NodeId`, not a fixed
source/target pair.

## State Machine / Protocol

Request/response over a bidi QUIC stream. `idempotency_key` deduplicates a re-sent RPC
(exactly-once *effect*). Stream may be reused across RPCs (a tuning axis). No 0-RTT for
any control RPC.

## Failure Modes

- **Re-sent RPC (retry):** `idempotency_key` dedup → no double effect.
- **Unpinned/foreign peer:** rejected (cert pinning enforced).
- **Unknown/foreign RPC kind:** typed reject.
- **Stream loss mid-RPC:** request is retried under the same `idempotency_key`;
  non-idempotent effects are guarded by the dedup cache, not by 0-RTT.

## Tests (for the 3a0 skeleton that realizes this ADR)

- `ControlEnvelope` round-trip stable.
- `idempotency_key` dedups a re-sent RPC (exactly-once effect).
- Unknown/foreign RPC → typed reject.
- Unpinned peer rejected (cert pinning enforced).
- `RefQuery`/`PinQuery` correct against 2-VM state.
- Inherently a 2-VM E2E.

## Benchmarks

RPC round-trip p50/p95/p99/max + idempotency-cache overhead (it sits on every
handoff/migrate/GC query → Tier-2 relevant) + a sweep of stream-reuse vs. new stream.
Register: `sentinel-cluster-control-rpc (3a0)`.

## Backward Compatibility

Additive: the dashboard WebTransport server is unchanged; the new daemon-side
client/server reuses its codec. No change to Zenoh's existing pub/sub usage.

## Security

Cert-pinned peers (V10); single trust domain (Track A). 0-RTT disallowed for control
(replay). The QUIC pull/control server accepts only typed envelopes / `BlockRef`s,
never a file path (see #498/V10), and rate-limits per node.

## Public Claim Boundary

- May claim today: transport decided (one QUIC control stream, codec reuse, SSH for
  bootstrap only).
- **May NOT claim:** working cross-node RPC — the QUIC client does not exist yet; the
  3a0 skeleton builds it and is verified on 2 VMs.

## Open Follow-ups

- The 3a0 control-stream skeleton (the `ControlEnvelope`, the daemon QUIC client).
- Zenoh persistent `connect`/`endpoints` config for the cross-node session (Phase 2).
- Per-RPC schemas as #496/#498/Track-C-D land.
