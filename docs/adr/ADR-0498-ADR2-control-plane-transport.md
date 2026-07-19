# ADR-0498: Cross-node control-plane transport (ADR-2)

- **Gate:** ADR-2 (top-level transport decision)
- **Status:** Accepted
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
  identity. Membership rejects a claimed heartbeat identity that differs from it,
  block-map gossip rejects holder identities different from it, and owner mutations
  are accepted only from the configured chef node.
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
atomically registers every authenticated inbound or outbound control and block-pull
QUIC connection under that binding. Revoking a NodeId removes the binding and actively
closes every registered connection for its certificate. N-node-native: addressing is
by `NodeId`, not a fixed source/target pair.

## State Machine / Protocol

Request/response uses one bidi QUIC stream per RPC. For cacheable requests, the cache
scope is `(authenticated peer NodeId, RPC method, idempotency_key)` and the first
request's SHA-256 digest is bound to that scope. An identical concurrent resend waits
on the same response cell and does not execute the handler again; reuse with another
payload returns typed `IdempotencyConflict`. Completed entries expire after five
minutes and the cache holds at most 4096 entries. If all capacity is occupied by
in-flight effects, new cacheable work is rejected for retry rather than growing the
cache. This is deliberately process-local duplicate suppression, not crash-durable
exactly-once semantics. Membership heartbeats bypass the response cache so every
arrival refreshes receiver-local liveness. No 0-RTT is enabled for control RPCs.

## Failure Modes

- **Re-sent RPC (same process):** an identical peer/method/key/digest tuple shares one
  response cell, including concurrent duplicates. A changed digest under that scope is
  rejected rather than confused with the prior request. A daemon restart clears this
  cache; durable sagas retain their own operation record.
- **Unpinned/foreign peer:** rejected before request dispatch.
- **Peer revoked after handshake:** the shared registry closes every established
  control and block-pull connection for that NodeId. A request also re-checks the
  binding before control-handler dispatch; new streams and reconnects fail until an
  explicit re-authorization.
- **Pinned peer claiming another NodeId:** membership heartbeat is typed-rejected; the
  TLS fingerprint resolves to its configured NodeId and cannot be replaced by payload.
- **Pinned non-chef peer mutating owner state:** typed-rejected before owner handlers.
- **Pinned peer advertising another holder identity:** the whole batch is rejected
  before the shared block map is mutated.
- **Unknown/foreign RPC kind:** typed reject.
- **Cluster metastore unavailable:** the membership wrapper continues to accept
  authenticated liveness heartbeats, but every owner/GC request reaching the terminal
  handler returns typed `Rejected`; no synthetic ownership acknowledgement is emitted.
- **Stream loss mid-RPC:** request is retried under the same `idempotency_key`. The
  process-local cache can deduplicate a reply only while that process survives;
  durable journals, operation claims, and owner-term checks make mutations safe to
  reconcile after a crash. Neither the cache nor disabled 0-RTT provides durable
  exactly-once effects.

## Tests

- `ControlEnvelope` round-trip stable.
- Peer/method/key/digest scoping dedups identical sequential and concurrent re-sends,
  rejects digest conflicts, and enforces TTL/capacity bounds.
- Unknown/foreign RPC → typed reject.
- Unpinned peer rejected (cert pinning enforced).
- Certificate/NodeId collisions are rejected and a spoofed membership NodeId is rejected.
- Revoking an authenticated peer actively terminates established control and
  block-pull sessions, denies reconnect, and permits recovery only after explicit
  re-authorization.
- A missing cluster metastore rejects owner mutations while authenticated membership
  remains available.
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
`CertFingerprint <-> NodeId` registry in the single Track-A trust domain. Authentication
does not itself authorize mutation: the configured chef is required for owner-state
RPCs, and holder advertisements must identify the authenticated sender. 0-RTT is
disabled for control. The QUIC pull/control server accepts only typed envelopes /
`BlockRef`s, never a file path (see #498/V10). Provisioning generates the private key
on the target and persists only the public peer binding on the seed. Revoking that
binding actively terminates already authenticated sessions; a missing durable cluster
metastore fails closed for owner/GC control requests rather than substituting a
success-acknowledging stub.

## Public Claim Boundary

- May claim: working cert-pinned daemon QUIC request/response, membership liveness,
  and block-map/control consumers verified on the two-node test cluster.
- May claim bounded process-local duplicate suppression for an identical
  peer/method/key/digest tuple, not crash-durable exactly-once.
- May claim immediate process-local revocation of established sessions for a removed
  explicit peer binding and fail-closed owner control when the metastore is unavailable.
- May NOT claim dynamic CA/rotation/quarantine lifecycle; Track A uses explicit pins.

## Open Follow-ups

- Durable idempotency where a saga requires replay protection across daemon restarts.
- Cluster-wide durable certificate rotation, revocation distribution, and quarantine
  lifecycle (Track D2/H); Issue #618 covers immediate local removal of an explicit pin.
- Per-RPC schemas as later Track-C/D control flows land.
