# ADR-0498: Cross-node control and snapshot transport (ADR-2)

- **Gate:** ADR-2 (blocks #615 and #501)
- **Status:** Accepted
- **Primary issues:** #498, #569, #615, and #501
- **Related issues / gates:** #495, G1, ADR-3, G4, G5, G7, V10, and V18

> Even though the foundation is verified on a two-node cluster first, all schemas,
> messages, and APIs are N-node-native and keyed by `NodeId`. Two nodes are the first
> test, not the cluster model.

## Context

Zenoh remains daemon-local pub/sub. Cross-node membership, block-map metadata, owner
control, and block pull now use the daemon-side `quinn` stack with mutual certificate
authentication, application-layer certificate pinning, bounded framing, and a shared
one-to-one certificate/NodeId peer registry. Revoking a peer actively closes its live
control and block-pull connections, and handlers authorize mutations from authenticated
peer identity rather than trusting payload identity.

The control idempotency cache scopes entries by authenticated peer, RPC method, key,
and request digest. It suppresses identical sequential and concurrent duplicates in
one process and rejects conflicting reuse, but it is neither durable nor atomic with a
mutating effect. It therefore cannot support a crash-durable exactly-once claim.

#501 also needs a bounded snapshot byte stream. Snapshot bytes have different limits,
backpressure, rate limiting, reservation, and lifecycle requirements from small control
messages and CAS block pull.

## Problem

How do control mutations and snapshot transfer reuse the proven QUIC/TLS/framing stack
without conflating authorization, byte-plane limits, reply deduplication, and durable
effect idempotency?

## Decision

Use one QUIC/TLS/framing technology with three distinct protocols:

- the existing control listener for small typed request/response messages;
- the existing bounded block-pull listener for CAS bytes;
- a dedicated, explicitly configured snapshot listener for bounded migration target
  pull.

Snapshot bytes do not multiplex with control messages or CAS block-pull traffic.

### Explicit snapshot configuration

Add additive, default-compatible fields:

- `ClusterConfig.snapshot_bind: Option<SocketAddr>`;
- `ControlPeer.snapshot_addr: Option<SocketAddr>`.

When the cross-node migration flag is enabled, every required snapshot endpoint must
be configured or startup fails. The implementation must not derive a snapshot endpoint
from `control_port + 1` or from the block-pull address.

### Authenticated handler context

The shared peer registry binds each inbound and outbound connection to exactly one
certificate fingerprint and `NodeId`. After TLS and pin verification, #615 and #501
handlers receive:

`AuthenticatedRequestContext { peer_node, fingerprint, actor }`

Each mutation declares its allowed direction, coordinator actor, peer, operation id,
step, and request digest. Authorization uses this authenticated context, never a node
or fingerprint supplied only by the request payload. Owner mutations remain restricted
to the configured chef. Holder advertisements must identify the authenticated sender.

### Control request and response model

Requests and responses remain separate enums. #615 adds
`ReplicateOwnerSnapshot`/`OwnerSnapshotAck`. #501 adds the migration step requests,
typed acknowledgements, `ProbeMigrationStep`, `StepOutcome`, and `Rejected`.

All control mutations disable 0-RTT. Read-only operations may use a separately proven
idempotent policy; no mutation inherits that policy implicitly. The bare-shell
provisioning exception remains SSH/bootstrap-credential based because no daemon or
QUIC endpoint exists on the target yet.

### Reply deduplication and durable effect idempotency

The bounded in-memory cache remains process-local duplicate suppression and reply
deduplication only. It may share a response cell for an identical
`(peer, method, key, digest)` tuple while the process survives. Reusing a key with a
different digest returns typed `IdempotencyConflict`. Membership heartbeats bypass the
cache so every authenticated arrival refreshes receiver-local liveness.

Mutating migration steps use the ADR-3 participant journal:

1. atomically claim `(peer_node, op_id, step, request_digest, boot_id, attempt)` as
   `Executing`;
2. execute or probe the deterministic effect;
3. CAS-complete the journal row as `Succeeded` with the durable outcome;
4. record digest reuse with different input as `DigestConflict`.

A crash after the effect but before completion is resolved by the outcome probe. The
effect is never blindly replayed. Generic control-cache survival is not part of this
contract.

### Snapshot target-pull protocol

The target establishes a certificate-pinned connection before source quiesce but does
not request bytes. After the source has cut the snapshot, the coordinator tells the
target to request by `op_id` on that already-open connection.

The source `PendingTransferRegistry` is a bounded state machine:

`Reserved -> Ready -> Served | Expired`

The reservation binds operation id, scope, source and target node ids, both certificate
fingerprints, the complete source term, byte cap, and expiry. The request carries only
the operation id; the server validates the full stored binding.

Receive limits include a global byte cap and a server-wide per-peer limiter keyed by
`NodeId + fingerprint`. A per-connection limiter is insufficient because a peer can
open multiple connections.

## Non-goals

- Replacing Zenoh for daemon-local pub/sub.
- Replacing or multiplexing the existing CAS block-pull protocol.
- Defining the bare-shell bootstrap credential flow from #495.
- Claiming dynamic CA rotation, cluster-wide revocation distribution, or quarantine
  lifecycle; those remain Track D/H.
- Claiming exactly-once effects from process memory.

## Data types

The implemented control plane uses `ControlEnvelope`, `ControlRequest`, `ControlReply`,
`ControlPeer`, and the shared `PeerRegistry`. #615/#501 add
`AuthenticatedRequestContext`, the owner-snapshot request/reply variants, and migration
step variants. #501 adds the snapshot listener wire type and `PendingTransferRegistry`.
Addressing is by `NodeId`, never a fixed source/target cluster model.

## Failure modes

- **Unpinned or mismatched peer:** reject before handler dispatch.
- **Peer revoked after handshake:** close all registered control and block-pull
  connections; reject new streams and reconnects until explicit re-authorization.
- **Unauthorized actor or direction:** typed reject with no effect claim.
- **Same process, identical tuple:** share the bounded response cell.
- **Same key, different digest:** typed conflict; migration journals persist
  `DigestConflict` where applicable.
- **Daemon restart:** process-local cache is lost; durable saga and participant journals
  drive recovery.
- **Crash after a migration effect:** outcome probe resolves and forward-completes the
  participant journal.
- **Cluster metastore unavailable:** membership liveness may continue, but owner and GC
  mutations return typed `Rejected`; no synthetic success acknowledgement is emitted.
- **Connection loss before snapshot request:** reconnect or re-prepare outside the
  migration pause.
- **Snapshot transfer expiry or cap violation:** typed failure with no staging
  acknowledgement.
- **Missing snapshot configuration with migration enabled:** startup failure.

## Tests and evidence

- Control-envelope round trip and unknown-kind typed rejection.
- Peer/method/key/digest scoping for sequential and concurrent duplicates, conflicts,
  TTL, and capacity.
- Certificate/NodeId collision and spoofed payload identity rejection.
- Revocation closes established inbound and outbound control/block-pull sessions,
  denies reconnect, and permits recovery only after explicit re-authorization.
- Missing cluster metastore rejects owner mutations while authenticated membership
  remains available.
- Authenticated context reaches every #615/#501 handler and cannot be forged by payload
  fields.
- Participant claim/probe/complete tests cover crash after effect and digest conflict.
- Dedicated snapshot-listener configuration, including missing-endpoint startup
  failure.
- Snapshot reservation binding, bounded receive, TTL cleanup, global cap, and shared
  per-peer rate-limit tests.
- Two-node live control and snapshot-stream evidence.

## Benchmarks

Control RPC round-trip p50/p95/p99/max plus duplicate-cache overhead remains registered
as `sentinel-cluster-control-rpc (3a0)`. #501 separately reports snapshot connect,
staging, restore, and migration-pause components; snapshot connection setup is outside
the measured pause.

## Backward compatibility

This is additive for single-node deployments. The dashboard WebTransport endpoint and
daemon-local Zenoh behavior are unchanged. Snapshot endpoints are required only when
the repository-default-off cross-node migration flag is enabled.

## Security

Mutual TLS proof-of-key, explicit certificate pins, and the shared one-to-one
certificate/NodeId registry authenticate peers. Authentication does not itself
authorize mutation. The configured chef is required for owner-state RPCs; request
direction and actor are checked from authenticated context. Control mutations disable
0-RTT. Snapshot requests carry only an operation id and are authorized against the
server-side reservation. Revocation closes established sessions. Missing durable
cluster metadata fails closed for owner/GC mutations.

## Public claim boundary

- Sentinel may claim working cert-pinned daemon QUIC request/response, membership
  liveness, and block-map/block-pull consumers verified on the two-node test cluster.
- Sentinel may claim bounded process-local duplicate suppression for an identical
  peer/method/key/digest tuple, not crash-durable exactly-once.
- Sentinel may claim immediate process-local revocation of sessions for a removed
  explicit peer binding and fail-closed owner control when the metastore is unavailable.
- After #501 evidence exists, Sentinel may claim a dedicated bounded snapshot
  target-pull stream.
- Sentinel may not claim dynamic CA rotation, cluster-wide revocation distribution,
  quarantine lifecycle, or exactly-once effects from the in-memory cache.

## Open follow-ups

- #615 supplies fail-closed owner snapshot replication over the control listener.
- #501 supplies durable migration participant journals and the snapshot listener.
- Track D/H supplies cluster-wide certificate rotation, revocation distribution, and
  quarantine lifecycle.
