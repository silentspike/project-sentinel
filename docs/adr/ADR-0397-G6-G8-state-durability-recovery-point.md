# ADR-0397: State durability & RecoveryPoint (G6 + G8)

- **Gate:** G6 + G8 (blocks Track-D forced failover + Track-H GA; does **not** block Track A)
- **Status:** Proposed
- **Primary issue:** #397 (Cluster 12 epic) — Track D/H
- **Related issues / gates:** G-D0 (OwnerMetadataLog), G-D3 (CAS replication), V31/G6
- **Supersedes / Superseded by:** —

> **N-node-native rule:** all schemas/messages/APIs MUST be `NodeId`-keyed, never a hard
> source/target pair. Two nodes are the first test, not the ceiling.

## Context

Quorum alone does **not** make forced failover safe: pull-by-hash only fetches a block
**if it is reachable somewhere** — it guarantees no redundant replication of the newest
state. TOGAF is silent on replication factor (Cluster 12 mentions only pull-by-hash,
no replication-factor). The codebase already has the building blocks: `WorldSnapshot`
binds snapshot_id + last_event_id + tick + redb-dump + ecs + projection_offsets +
fs_metadata (`crates/sentinel-common/src/types.rs:641` region); `FencedStateTransfer`
adds owner_epoch + cursors (`types.rs:700`); the event cursor exists
(`projection_offsets`, monotonic enforcement). `restore_generation` already lives in
Limbo `sim_metadata` (no separate FS generation needed).

## Problem

What does a durable `RecoveryPoint` bind, and under what durability/RPO may a forced
failover claim no data loss?

## Decision

**`RecoveryPoint` formalizes the verified `WorldSnapshot` + `FencedStateTransfer` and
adds explicit durability/RPO classes; forced failover starts only from a
quorum-accepted RecoveryPoint whose CAS refs meet `min_rf`.**

- **RecoveryPoint (V31, a wrapper, not a rebuild):** `RecoveryPoint { recovery_point_id,
  scope, owner_term, event_cursor, redb_generation, ecs_snapshot_ref, fs_dump_ref,
  cas_manifest_root, cas_ref_set_root, inbound_cursor, durability_class, replica_ack_set
  }` + `durability_level { LocalOnly | Replicated{rf} | Quorum }` + `rpo_class { RpoZero
  | LastReplicatedCheckpoint }`.
- **Durability ADR (G6) decides bindingly:** replicated event-log vs. periodic
  checkpoint; CAS replication factor; the write-acknowledgement rule (quorum/replica-safe
  before commit?); RPO/RTO; recovery source-of-truth; stale-checkpoint semantics.
- **RPO rule:** `RPO=0` → writes/event-log/snapshot-refs/manifests/CAS-refs must be
  replica-safe **before commit**; `RPO>0` → forced failover may only claim
  "best-effort / last replicated checkpoint" (a documented product boundary).
- **Forced failover authoritative source (G8):** failover starts **only** from a
  **quorum-accepted** RecoveryPoint; the target never becomes owner from ad-hoc local
  state; the RecoveryPoint metadata is part of the OwnerRegistry/Witness decision (G-D0);
  the CAS refs must meet `min_rf` before `RPO=0` is claimed (else the state lived only
  on the dead node — G-D3).

## Non-Goals

- Not Track A (Track A is cooperative, no forced failover).
- The replication mechanism itself = G-D3 (CAS) + G-D0 (metadata log).
- Backup/restore of the whole cluster config = G-H2.

## Data Types

`RecoveryPoint` (new, formalizes existing `WorldSnapshot`+`FencedStateTransfer`),
`durability_level`, `rpo_class`. Persisted in ADR-3 `RECOVERY_POINTS`; the metadata
subset is replicated by the OwnerMetadataLog (G-D0).

## State Machine / Protocol

A RecoveryPoint is created at a consistent cut (G4 `SnapshotCut`), its metadata
committed to the OwnerMetadataLog, its CAS refs driven to `desired_rf` by the repair
service (G-D3). Forced failover: quorum picks the newest RecoveryPoint whose refs meet
`min_rf`.

## Failure Modes

- **State only on the dead node:** forced failover with `RPO=0` is **refused** if
  required refs are under `min_rf` (G-D3) — a quorum owner change is worthless without
  the state.
- **Stale checkpoint:** `rpo_class = LastReplicatedCheckpoint` is surfaced honestly; no
  silent "no data loss" claim.

## Tests

Forced failover refused under `min_rf`; RecoveryPoint round-trips the cut; RPO classes
are enforced (a `RpoZero` commit blocks until replica-acked). Property/model tests
(Track H).

## Benchmarks

Replication lag, RecoveryPoint creation cost, failover RTO, the replica-ack write
latency (the RPO=0 cost). Register: Track-D/H.

## Backward Compatibility

`RecoveryPoint` wraps existing types additively; single-node = `LocalOnly` durability,
unchanged behavior.

## Security

Single trust domain (Track A→D). The RecoveryPoint authority is the quorum, not any
node.

## Public Claim Boundary

- May claim after Track D/H: failover from a quorum-committed RecoveryPoint with a
  declared RPO.
- **May NOT claim:** "no data loss" / HA before G6/G8/G-D0/G-D3 are built and measured.

## Open Follow-ups

- G-D3 CAS replication factor/repair; G-D0 quorum metadata; G-H2 cold backup/restore.
