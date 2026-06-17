# ADR-0496: Ownership & handoff safety (G1)

- **Gate:** G1 (blocks #496 PR2; the route-quiescence part V17 also blocks #497)
- **Status:** Proposed
- **Primary issue:** #496 (owner registry / fencing)
- **Related issues / gates:** ADR-3 (cluster meta schema), G-D0 (HA evolution), V1–V6/V17/V19
- **Supersedes / Superseded by:** —

> **N-node-native rule:** all schemas/messages/APIs MUST be `NodeId`-keyed, never a
> hard source/target pair. Two nodes are the first test, not the ceiling.

## Context

`owner_epoch` is a **bare `u64`** today (`crates/sentinel-common/src/types.rs:702`),
in-memory only (`RestoreFence` in the daemon). There is **no single choke-point per
store**: the EventStore has ~9 write methods each with its own transaction
(`append_event:393`, `append_with_outbox:430`, `append_with_outbox_batch:491`,
`save_snapshot:847`, `delete_orphan_outbox:1264`, `update_offset:1288`,
`save_world_snapshot:1434`, `save_world_snapshot_at:1456`, `delete_world_snapshot:1526`
in `crates/sentinel-limbo/src/event_store.rs`), redb has **19** `begin_write()` sites
(`crates/sentinel-redb/src/lib.rs`), and the FS metadata layer **18**
(`crates/sentinel-fs/src/metadata.rs`). A single forgotten write path = split-brain
(the worst failure class).

## Problem

How is a stale or non-owning write made impossible across **every** persistence path,
without a distributed 2PC, and how is the handoff sequenced so a stale source can never
serve after the target takes over?

## Decision

**Refactor every store onto one fenced write entry per engine, gated by a typed
`OwnerWriteGuard`, and sequence the handoff so the source is durably retired before the
target serves.**

- **Typed guard, not a raw `u64` (V3/V19):** `OwnerWriteGuard { scope, node_id, epoch,
  coordinator_generation, issued_by }` is constructible **only** by the OwnerRegistry.
  Both `begin_fenced_write(&guard)` **and** `commit()` re-check the **whole**
  `OwnerTerm` (scope + owner_node + epoch + coordinator_generation + `local_role ==
  Owner`), not just `epoch < current` (TOCTOU protection). The strongest barrier is the
  **type system**: a mutating path does not compile without a guard; grep/lint is only
  an extra.
- **One entry per engine, three impls (no shared wrapper):** the three transaction
  types are incompatible (limbo `conn.lock()` mutex / redb `WriteTransaction` / the FS
  wrapper), so the contract is `trait FencedStore { type Txn; fn
  begin_fenced_write(&self, guard: &OwnerWriteGuard) -> Result<Self::Txn,
  StaleEpochError>; }` with **three** impls (matches the PR1a/b/c split). Raw
  `begin_write`/append become `pub(crate)`; all public write methods route through the
  fenced entry.
- **Per-write subject tagging (G-1 scope spec):** `OwnerTerm.scope =
  StateTransferScope { World | NanoContainer(agent_id) }` (the type exists,
  `types.rs:666`). The store is company-global, so each fenced write declares its
  subject from the `DomainEvent` (events carry `agent_id`/`target_agent_id`,
  `types.rs:289/357`); the entry check is *"does this node hold a current `OwnerTerm`
  for this subject scope?"*. World/system events without `agent_id` (e.g.
  `PsiBandChanged`/`ChaosTriggered`) belong to the `World` scope = seed/chef owner.
- **Handoff sequence (V1, the strict, TOGAF-compatible tightening):** source sets a
  durable `Retiring`/`no_new_writes`/`retired_after_epoch=E` marker, drains in-flight
  writes, emits a **durable `SourceRetiredAck(E)`**, *then* snapshot+cursor under fence
  E, target restores as `Prepared`, chef persists `Owner=Target epoch=E+1`, target
  activates, route switches, source stays hard-fenced for epoch ≤ E. **The target never
  serves before `SourceRetiredAck` is durable.**
- **No forced failover in 2-node mode (V2):** membership says only
  `Alive|Suspect|Dead|Left`; ownership is never stolen on unreachability. Cooperative
  migration only; forced failover is Track D (G-D0/G6).
- **LocalOwnerState (V4):** each node holds a durable
  `LocalOwnerState { scope, node_id, epoch, role, durable_retired_marker }` so a source
  rejects writes from its own store layer during a partition (the chef update may be
  invisible).
- **Route-quiescence before snapshot (V17):** before `SourceRetiredAck` the route layer
  enters `Migrating(scope, op_id)`; new inbound is handled per the #497 inbound policy
  (Track A excludes active inbound).

## Non-Goals

- Forced failover / witness / quorum (Track D, G-D0/G6).
- Per-container **write parallelism**: serialization stays node-global (one
  `conn.lock()` / one redb writer) — fencing granularity is logical per-container, not
  parallel writes (a deliberate Track-A boundary; relevant to "ms"/Tier-2 contention).
- The durable inbound queue (Track E/H, G0).

## Data Types

`OwnerWriteGuard`, `OwnerTerm`, `LocalOwnerState`, `StaleEpochError` (new in
`sentinel-common`/daemon). `OwnerTerm.scope` reuses `StateTransferScope` (`types.rs:666`).
Persisted in the ADR-3 redb tables (`CLUSTER_OWNER`).

## State Machine / Protocol

Handoff: `Idle → PreparingSource → SourceRetiring → SourceRetired(ack durable) →
SnapshotCreated → TargetPrepared → OwnerCommitted(E+1) → … (see G5 for the full saga)`.
The chef is a deliberate SPOF: chef death = no new ownerships/migrations, **existing
owners keep writing (no data loss)**.

## Failure Modes

- **Partition:** `LocalOwnerState` lets the source enforce its own retirement locally;
  no second owner is created (V2).
- **TOCTOU at commit:** `commit()` re-checks the whole `OwnerTerm`.
- **Forgotten write path:** prevented by the type barrier (no guard → no compile) +
  `pub(crate)` raw writes + a grep/lint CI extra.
- **Crash between ack and owner-commit (V6):** before ack → source keeps running; after
  ack/before commit → source unfreezes with a recovery epoch, target discards Prepared.

## Tests

- 2-VM: a second owner is rejected.
- Stale write → `StaleEpochError`.
- A raw write without a guard **does not compile** (type barrier).
- Commit re-check (TOCTOU) test.
- V1: target never serves without a durable `SourceRetiredAck`.
- V2 partition: source unreachable → no new owner.
- Chef-SPOF behavior. (2-VM live ACs hang on the QUIC control stream, ADR-2; saga logic
  also covered by in-process 2-World tests.)

## Benchmarks

Handoff latency p50/p95/**p99/max** + fencing overhead per write (the entry wrapper, on
**every** write) + steady-state no-op-guard vs real guard begin+commit vs stale-reject.
Bug-finder: **0 double writes** under partition/pause. Sweep heartbeat/lease-TTL.
Register: `sentinel-daemon-owner-fencing (#496)`.

## Backward Compatibility

The store refactor is behavior-preserving first (PR1: `begin_fenced_write` wraps the
existing write with a no-op guard → full suite proves zero change), then the real epoch
check lands in PR2. New fields via `#[serde(default)]`. No `events` migration.

## Security

Single trust domain (Track A). The guard authority is the OwnerRegistry; raw writes are
unconstructible without it.

## Public Claim Boundary

- May claim after #496: stale/non-owning writes rejected; cooperative handoff is
  V1-safe.
- **May NOT claim:** forced failover, HA, per-container write parallelism, or "ms"
  handoff without the measured bench.

## Open Follow-ups

- G-D0 quorum OwnerMetadataLog (HA); G6/G8 RecoveryPoint; G5 the full migration saga.
