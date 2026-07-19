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

Zenoh is pub/sub only — the bus exposes
`publish` / `subscribe` / `declare_subscriber`
(`crates/sentinel-zenoh/src/lib.rs`), with **no queryable and no `.get()`** —
so it cannot do request/response. `sentinel-cluster-control` now provides the daemon
QUIC server and client using `quinn`, a bounded length-prefixed frame codec, mutual
certificate authentication, and application-layer certificate pinning. The dashboard
WebTransport endpoint is a separate user-facing transport and remains unchanged.

The cluster needs request/response for: owner handoff RPCs (`PrepareHandoff`,
`SourceRetiredAck`, `OwnerCommit`), CAS ref/pin queries (`RefQuery`, `PinQuery`),
block pull (#498), and later Track-C/D control. The bootstrap of a **bare** target
(#495) is different: no Sentinel runs there yet.

## Problem

What transport carries cross-node control-plane request/response, and how does it
relate to the bare-shell bootstrap?

## Decision

**One daemon QUIC control transport with a bounded bidi frame codec and mutual cert
auth. Cross-node membership and block-map metadata use this control plane; Zenoh is
daemon-local. The bare-shell bootstrap does NOT use QUIC.**

- A single QUIC transport stack is used for daemon-to-daemon control. The implemented
  `ControlServer` and `ControlClient` use `quinn` bidirectional streams and a bounded
  u32-length-prefixed JSON frame. Block bytes use a separate bounded protocol on the
  adjacent pull port so they are never forced through the 1 MiB control-frame cap.
- Messages are a request-scoped `ControlEnvelope` carrying an `idempotency_key`, over
  cert-pinned peers (V10). Each configured or dynamically provisioned peer binds one
  certificate fingerprint to exactly one `NodeId`; handlers receive that authenticated
  identity and membership rejects a claimed heartbeat identity that differs from it.
- **0-RTT is off for control.** It is allowed only for the idempotent `BlockPull`
  (V18) and forbidden for every control-plane mutation (`PinCreate`, `OwnerCommit`,
  `ProvisionNode`, GC delete, route switch, migration state transition) — each carries
  replay risk.
- **Bootstrap exception:** the seed -> bare-target step (#495) runs over **SSH /
  bootstrap credential**, not QUIC, because no Sentinel (and no QUIC endpoint) runs on
  the target yet. The target generates its private QUIC key locally through the verified
  daemon binary; the seed durably authorizes the returned fingerprint/assigned-NodeId
  binding before startup. The QUIC stream exists once the target daemon is up, and an
  authenticated heartbeat observed as `Alive` is the join completion condition.

The control stream was introduced before the cross-node owner and distributed-CAS
flows that consume it. Those flows and membership now share the same authenticated
peer registry; Zenoh is not a fallback transport.

## Non-Goals

- Does not replace Zenoh for daemon-local pub/sub. Cross-node block-map gossip and
  membership liveness use this cert-pinned QUIC control plane because the Sentinel
  Zenoh session is intentionally loopback-only.
- Does not define the bare-shell bootstrap credential flow (G3/#495).
- Does not specify the per-RPC wire schemas/timeouts (those are produced by the 3a0
  control-stream issue and #496/#498 ADRs — pre-inventing them here would be
  unverifiable against not-yet-built code).

## Data Types

`ControlEnvelope { request_id, idempotency_key, request }`, `ControlRequest`, and
`ControlReply`. `ControlPeer { node_id, alias, addr, cert_fingerprint }` is the durable
trust declaration. `PeerRegistry` enforces a one-to-one certificate/NodeId mapping and
resolves the TLS peer before dispatch. N-node-native: addressing is by `NodeId`, not a
fixed source/target pair.

## State Machine / Protocol

Request/response uses one bidi QUIC stream per RPC. For cacheable requests,
`idempotency_key` is checked and the handler effect plus cache insert execute under one
critical section, so concurrent duplicates in the same daemon process produce one
effect. This cache is deliberately process-local: it is not a claim of crash-durable
exactly-once semantics. Membership heartbeats bypass the response cache so every
arrival refreshes receiver-local liveness. No 0-RTT is enabled for control RPCs.

## Failure Modes

- **Re-sent RPC (same process):** atomic `idempotency_key` dedup -> no double effect,
  including concurrent duplicates. A daemon restart clears the cache; durable sagas
  must retain their own operation record.
- **Unpinned/foreign peer:** rejected before request dispatch.
- **Pinned peer claiming another NodeId:** membership heartbeat is typed-rejected; the
  TLS fingerprint resolves to its configured NodeId and cannot be replaced by payload.
- **Unknown/foreign RPC kind:** typed reject.
- **Stream loss mid-RPC:** request is retried under the same `idempotency_key`;
  non-idempotent effects are guarded by the dedup cache, not by 0-RTT.

## Tests (for the 3a0 skeleton that realizes this ADR)

- `ControlEnvelope` round-trip stable.
- `idempotency_key` dedups sequential and concurrent re-sends in one process.
- Unknown/foreign RPC → typed reject.
- Unpinned peer rejected (cert pinning enforced).
- Certificate/NodeId collisions are rejected and a spoofed membership NodeId is rejected.
- `RefQuery`/`PinQuery` correct against 2-VM state.
- Inherently a 2-VM E2E.

## Benchmarks

RPC round-trip p50/p95/p99/max + idempotency-cache overhead (it sits on every
handoff/migrate/GC query → Tier-2 relevant) + a sweep of stream-reuse vs. new stream.
Register: `sentinel-cluster-control-rpc (3a0)`.

## Backward Compatibility

Additive for single-node deployments: the dashboard WebTransport server is unchanged.
In cluster mode, cross-node membership and block-map gossip use QUIC; the Sentinel
Zenoh listener remains loopback-only and daemon-local.

## Security

Mutual TLS proof-of-key plus cert-pinned peers (V10), with a one-to-one
`CertFingerprint <-> NodeId` registry in the single Track-A trust domain. 0-RTT is
disabled for control. The QUIC pull/control server accepts only typed envelopes /
`BlockRef`s, never a file path (see #498/V10). Provisioning generates the private key
on the target and persists only the public peer binding on the seed.

## Public Claim Boundary

- May claim: working cert-pinned daemon QUIC request/response, membership liveness,
  and block-map/control consumers verified on the two-node test cluster.
- May claim process-local atomic duplicate suppression, not crash-durable exactly-once.
- May NOT claim dynamic CA/rotation/quarantine lifecycle; Track A uses explicit pins.

## Open Follow-ups

- Durable idempotency where a saga requires replay protection across daemon restarts.
- Certificate rotation/revocation/quarantine lifecycle (Track D2/H).
- Per-RPC schemas as later Track-C/D control flows land.
