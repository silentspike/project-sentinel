# ADR-0397: CAS replication & repair service (G-D3)

- **Gate:** G-D3 (blocks Track-D; does **not** block Track A)
- **Status:** Proposed
- **Primary issue:** #397 (Cluster 12 epic) — Track D
- **Related issues / gates:** G6/G8 (RecoveryPoint min_rf), G-D2 (evacuation), G2 (BlockRef), V8
- **Supersedes / Superseded by:** —

> **N-node-native rule:** all schemas/messages/APIs MUST be `NodeId`-keyed, never a hard
> source/target pair. Two nodes are the first test, not the ceiling.

## Context

TOGAF mentions only pull-by-hash, **no replication factor** (verified). Pull-by-hash
fetches a block only if it is reachable; with `rf=1` a node loss = permanent state loss,
making a quorum owner change worthless (the state lived only on the dead node). G6/G8
forced failover therefore needs a replication/repair service, not just a locator.

## Problem

How does the cluster keep RecoveryPoint-critical blocks replicated so forced failover
has the state to recover?

## Decision

**A continuous CAS replication/repair service driven by a per-namespace policy; forced
failover with `RPO=0` is refused when required refs are under `min_rf`.**

- `CasReplicationPolicy { namespace, desired_rf, min_rf, failure_domain_policy,
  repair_priority }`.
- `CasRepairTask { block_ref, current_holders, desired_holders, recovery_point_refs,
  state }` — a continuous repair loop keeps RecoveryPoint-critical `BlockRef`s at
  `desired_rf`.
- **Hard rule:** forced failover with `RPO=0` is **refused** if required refs are under
  `min_rf` (couples G6/G8) — otherwise the system runs permanently at `rf=1` and a
  quorum owner change is worthless.
- The repair loop respects the global QUIC pull budgets (so repair/scheduler do not
  saturate the pull server — RFC 9000 flow control).

## Non-Goals

- Not Track A (Track A is single-node-or-cooperative; no replication factor).
- Erasure coding / tiering (later optimization; this ADR fixes the policy + repair loop).
- Geo replication (Track I).

## Data Types

`CasReplicationPolicy`, `CasRepairTask` (new, reconciled per G-N0/V39). Reuses `BlockRef`
(G2) and `HolderAdvertisement` (V16). Persisted in the ADR-3 cluster tables.

## State Machine / Protocol

Repair loop: `observe holder set per BlockRef → diff(desired_rf, current) → schedule
pulls to new holders (budgeted) → update HolderAdvertisements → record condition`.
Priority ordered by `recovery_point_refs` (RecoveryPoint-critical first).

## Failure Modes

- **Under-replication after node loss:** the repair loop re-replicates to `desired_rf`;
  `cas_under_replicated_refs` (NodeStatus, G-N0) surfaces the gap.
- **Forced failover under `min_rf`:** refused (G6/G8) — never claim `RPO=0` without the
  refs.
- **Pull-server saturation:** budgets cap repair throughput.

## Tests

A block at `rf=1` after a node loss is re-replicated to `desired_rf`; forced failover is
refused when refs are under `min_rf`; repair respects pull budgets; CASEvacuation (G-D2)
uses the same primitives.

## Benchmarks

Repair throughput vs. block-count/node-count, time-to-`desired_rf` after a node loss,
pull-budget adherence. Register: Track-D.

## Backward Compatibility

New service/types additive; single-node = `rf=1` `LocalOnly`, unchanged.

## Security

Single trust domain; repair pulls are cert-pinned (V10) and budgeted.

## Public Claim Boundary

- May claim after Track D: RecoveryPoint-critical blocks held at `desired_rf`, `RPO=0`
  only when `min_rf` is met.
- **May NOT claim:** any replication factor in Track A (pull-by-hash only, `rf=1`).

## Open Follow-ups

- Erasure coding / storage tiering; geo replication (Track I).
