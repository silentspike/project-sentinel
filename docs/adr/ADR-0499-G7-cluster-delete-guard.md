# ADR-0499: ClusterDeleteGuard — destructive-GC enablement (G7 / G-DELETE)

- **Gate:** G7 / G-DELETE (blocks #499b)
- **Status:** Proposed
- **Primary issue:** #499 (cluster CAS GC) — the destructive half #499b
- **Related issues / gates:** G2 (block map = locator), #498 (block map), #501 (move), V8
- **Supersedes / Superseded by:** —

> **N-node-native rule:** all schemas/messages/APIs MUST be `NodeId`-keyed, never a hard
> source/target pair. Two nodes are the first test, not the ceiling.

## Context

Cluster GC must cover **every** destructive path, not just one. The `rg` inventory shows
**≥11** delete/prune/gc paths: `prune_batch` (event-prune #250,
`crates/sentinel-limbo/src/event_store.rs:1182`), `delete_world_snapshot:1526`
(snapshot-prune), `delete_orphan_outbox:1264`, personality_evolution retention
(`services/sentinel-daemon/src/orchestrator.rs:62` agent-field + `:89` global, both
`DELETE FROM personality_evolution`), `gc_trash`
(`crates/sentinel-fs/src/metadata.rs:619`), `CasStore::remove`
(`crates/sentinel-fs/src/cas.rs:112`), `CasStore::gc:124`, `gc_chunks`
(`crates/sentinel-fs/src/gc.rs:20`), `gc_trash` (ArtifactPlane, `gc.rs:57`), plus the
projection DELETE. The dead-branch GC (#493) lives inside `prune_batch`. After a move
A(node-0→node-1), node-0's tiered retention (#250) would prune A's events/snapshots →
node-1's time-travel/pull breaks → **#250/#493 are NOT cluster-safe multi-node**.

## Problem

How is every destructive path made cluster-safe so a delete never removes data another
node still references, without distributed consensus?

## Decision

**One `ClusterDeleteGuard::decide` that every destructive path routes through, over the
complete `rg` delete inventory, with a fail-safe "keep on uncertainty" rule.**

- **Central guard:** no path calls delete/prune/remove directly — all go through
  `ClusterDeleteGuard::decide(DeleteKind, …) -> { AllowedLocalOnly |
  AllowedClusterSafe | BlockedByRemoteRef | BlockedByUncertainty | ForbiddenUntilTrackH
  }`.
- **Pre-classification (canonical vs. derived):** canonical/non-rebuildable
  (events/snapshots/CAS/personality_evolution) → cluster-ref-check **mandatory**;
  derived/rebuildable (projections / incidents / terminal-actions → node-local-safe).
  The complete inventory is enumerated by `rg` (≥11 known), and **every** path is
  classified — not "all 8".
- **Light cluster-ref query (V8), not consensus:** a destructive path asks the block map
  (#498) + remote snapshot pins (gossip): *"does any node reference/pin this hash?"*.
  Delete only when: no local refs/pins/manifests, no in-transit pins, remote query says
  "no known refs", and **uncertainty/timeout/unknown-node → KEEP**. The block map is a
  **locator, never liveness** (G2/V8).
- **CI gate:** a new delete path that is not registered with a `DeleteKind` fails CI.
- **Mandatory metrics (V8):** `blocked_by_uncertainty_count`,
  `blocked_by_unknown_node_count`, `blocked_by_remote_timeout_count`,
  `oldest_blocked_gc_age`.
- **Ordering:** #499b (destructive) is **mandatory after #501** (the real move path) —
  the most dangerous race (migration ∥ GC) must be tested against a real move.

## Non-Goals

- The non-destructive query/pin infra + dry-run GC = **#499a** (Phase 6.5, may be built
  before #501).
- CAS replication/repair (G-D3) and forced-failover evacuation (Track D2).

## Data Types

`DeleteKind` enum (one per inventory path), `ClusterDeleteGuard`, `DeleteDecision`
(above). Reuses `BlockRef` (G2) for the ref query and the `CLUSTER_PINS` table (ADR-3).

## State Machine / Protocol

`decide`: local-ref/pin check → in-transit-pin check → remote ref/pin query → decision.
Any failure/timeout/unknown → `BlockedByUncertainty`/`BlockedByUnknownNode`/
`BlockedByRemoteTimeout` (keep).

## Failure Modes

- **Migration ∥ GC race:** in-transit pin (from #497) + the remote-ref query make a
  delete of an in-flight blob impossible; tested live with the #501 move path.
- **Remote node unreachable:** `BlockedByUnknownNode` → keep (never delete on missing
  information).
- **Unregistered delete path:** CI fails (no silent bypass).

## Tests

No delete of a foreign-referenced blob (2-VM); no delete of a remote-pinned blob;
**migration ∥ GC race tested with the real #501 move**; after all refs gone → deletable;
the V8 metrics present; a per-`DeleteKind` registration test.

## Benchmarks

GC latency with the cluster query p50/p95; bug-finder **0 false delete under
migration**; sweep blob-count 1k/10k/100k + node-count. Register:
`sentinel-fs-cluster-cas-gc (#499)`.

## Backward Compatibility

The guard wraps existing delete paths behavior-preservingly first (local-only decision
= today's behavior), then the cluster-ref check activates. No data format change.

## Security

Single trust domain (Track A). The ref query is a light read, not a mutation; no 0-RTT.

## Public Claim Boundary

- May claim after #499b: cluster-safe destructive GC across the full delete inventory.
- **May NOT claim:** cluster-safe deletion before #501 exists (the race would be
  untested); #250/#493 are explicitly not multi-node-safe until this guard covers them.

## Open Follow-ups

- #499a non-destructive query/pin infra; G-D3 CAS replication so evacuation has targets.
