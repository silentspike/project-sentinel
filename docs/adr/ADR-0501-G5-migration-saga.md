# ADR-0501: Migration saga & operation log (G5)

- **Gate:** G5 (blocks #501)
- **Status:** Proposed
- **Primary issue:** #501 (cross-node ECS-native stop-and-copy PoC)
- **Related issues / gates:** G1 (handoff), G4 (snapshot), G2 (CAS), V5/V6/V20/V22
- **Supersedes / Superseded by:** —

> **N-node-native rule:** all schemas/messages/APIs MUST be `NodeId`-keyed, never a hard
> source/target pair. Two nodes are the first test, not the ceiling.

## Context

#501 composes G1 (fence) + G4 (snapshot) + G2/#498 (pull) + #500a (manifest) into one
cross-node move. Single-node migration primitives exist and are live-verified
(`FencedStateTransfer`, `crates/sentinel-common/src/types.rs:700`; the daemon migrate
path #413). A move that is a transient command rather than a persistent, recoverable
saga loses correctness on a chef/target/source restart mid-move.

## Problem

How is a cross-node move made idempotent, recoverable at every step, and rolled back
epoch-correctly?

## Decision

**A persistent `MigrationOp` saga with a recoverable state machine and epoch-correct
rollback.**

- **Persistent op log (V5):** `MigrationOp { op_id, scope, source, target, source_epoch,
  target_epoch, state, snapshot_id, manifest_refs, started_at, updated_at,
  failure_reason }`, persisted in ADR-3 `MIGRATION_OPS`. Each step is idempotent and
  recoverable (chef/target/source restart mid-move).
- **State machine (V22 naming — target never serves too early):** `Idle →
  PreparingSource → SourceRetiring → SourceRetired → SnapshotCreated → TargetPrepared →
  OwnerCommittedToTarget → TargetActivatedNotRoutable → RouteSwitched → TargetRoutable →
  SourceRetiredFinal → Completed` (+ `RollbackBeforeCommit | RollbackAfterCommit |
  ManualRecoveryRequired`).
- **Epoch-correct rollback (V6):** failure **before** `SourceRetiredAck` → source keeps
  running, no change. After ack / before `OwnerCommit` → source unfreezes with a
  recovery epoch, target discards Prepared. **After `OwnerCommit(Target, E+1)` the
  source must NOT simply keep running** — either finish activating the target OR make
  the source owner again via `OwnerCommit(Source, E+2)`. After the target serves,
  rollback = a new handoff back.
- **Pin lifecycle (V20):** `Pin { pin_id, op_id, scope, block_ref, owner_node,
  owner_epoch, reason, created_at, expires_at, renewable, durable }`. On restart durable
  pins are reconciled against `MigrationOp`/manifest refs; only pins whose owning op is
  Completed/Failed **and** whose refs are gone expire (no eternal hang, no premature
  drop).
- **In-transit pin source:** the grace pin protecting a blob during transfer comes from
  **#497**'s migrate path (single-node testable), **not** a speculative #499 fragment.

## Non-Goals

- bwrap/WASM/microVM **live** cross-node move (Track E, needs #472/#500b); Track A is
  **ECS-native only**.
- The "ms" claim without a measured per-runtime-type pause (TOGAF bounded class).
- Forced-failover recovery (Track D); `qm rollback` is never migration recovery (V6 only).

## Data Types

`MigrationOp`, `Pin` (new, ADR-3 tables). Reuses `FencedStateTransfer` (`types.rs:700`,
now carrying source/target cursors) and the G4 `SnapshotCut`.

## State Machine / Protocol

As above. Idempotency: each transition checks the persisted state and is a no-op if
already past it. Recovery: on startup, an in-flight `MigrationOp` resumes from its last
durable state.

## Failure Modes

- **Chef/target/source restart mid-move:** the saga resumes from the persisted state.
- **Route-switch failure:** rollback per the current state (V6).
- **Double move / partition:** rejected by the fence (G1) + the saga's single-active-op
  invariant.

## Tests

Failure-injection: chef/target/source restart mid-move, route-switch fail, double-move,
partition → no state loss / no double write. 1 owner throughout (`owner_epoch` log;
source hard-fenced after OwnerCommit). V1 handoff (target never serves before durable
ack). V6 rollback per saga state.

## Benchmarks

**Pause time per runtime type p50/p95/p99/max (the central number, prove/refute the
"ms" class)** + components separated (V21:
`prewarm/prep_pull_bytes/pause/snapshot/restore/route_switch/post_validation/total`).
Bug-finder: 0 state-loss / 0 double-write. Sweep state-size + warm/cold. Register:
`sentinel-daemon-cross-node-migrate (#501)`.

## Backward Compatibility

New tables/types additive; the existing single-node migrate path is unchanged. No
`events` migration.

## Security

Single trust domain (Track A); the saga is driven over the cert-pinned QUIC control
stream (ADR-2). No 0-RTT for any migration state transition (V18).

## Public Claim Boundary

- May claim after #501: 2-node ECS-native stop-and-copy PoC, bounded class **measured**
  (excludes active inbound + active external side-effects).
- **May NOT claim:** Cluster-GA, all-runtime live migration, or "ms" without the cited
  bench value.

## Open Follow-ups

- All-runtime migration (Track E); microVM deep migration (Track F).
