# ADR-0496: Cluster-metadata persistence schema (ADR-3)

- **Gate:** ADR-3 (top-level persistence decision)
- **Status:** Proposed
- **Primary issue:** #496 (owner registry / fencing) — introduces the cluster tables
- **Related issues / gates:** G-D0 (OwnerMetadataLog), G1 (fencing scope), ADR-2 (transport)
- **Supersedes / Superseded by:** —

> **N-node-native rule:** Even though the foundation is verified on a 2-node cluster
> first, all schemas, messages and APIs MUST be N-node-native (`NodeId`-keyed
> sets/maps, never a hard source/target pair as the cluster model). Two nodes are the
> first test, not the ceiling.

## Context

Cluster metadata (who owns which scope, open migration/provision sagas, cluster pins,
node registry, recovery points, voting config) has **nowhere to live** today. redb
holds only per-agent / per-room simulation state — `AGENT_STATE`
(`TableDefinition<u16, &[u8]>`), `RELATIONSHIPS` (`<u32, &[u8]>`), `PERSONALITY`
(`<u16, &[u8]>`), `ROOM_STATE` (`<u16, &[u8]>`), plus voice/notes/nmda/facts and the
`SIM_META` (`<&str, &[u8]>`) / `API_PATTERNS` key-value tables
(`crates/sentinel-redb/src/lib.rs:15-33`). There are **no** cluster tables (verified:
`CLUSTER_OWNER`/`MIGRATION_OPS`/… = 0 hits). The Limbo SQLite event store is the
append-only world history and `sim_metadata` side-table; it is **not** the place for
mutable per-container ownership.

The `owner_epoch` is a bare in-memory value today (`RestoreFence` in the daemon) and a
bare `u64` field on `FencedStateTransfer` (`crates/sentinel-common/src/types.rs:702`)
— it is not persisted as cluster ownership.

## Problem

Where does mutable cluster metadata persist, durably and behind the fencing barrier,
without polluting the append-only event log or the per-agent simulation tables?

## Decision

**Dedicated redb tables, all written through the fenced write entry (#496). NodeId
also lives in `daemon.toml`.**

New redb tables:

- `CLUSTER_OWNER` — `OwnerTerm` per container/company scope (holds `owner_epoch` /
  `OwnerTerm`).
- `MIGRATION_OPS` — persistent `MigrationOp` sagas (recoverable mid-move).
- `PROVISION_OPS` — persistent `ProvisionOp` sagas (recoverable mid-bootstrap).
- `CLUSTER_PINS` — durable CAS pins (snapshot/migration/manifest/pull-in-progress).
- `NODE_REGISTRY` — `NodeIdentity` + `NodeLifecycleState` + membership metadata.
- `RECOVERY_POINTS` — `RecoveryPoint` metadata (G6/G8).
- `VOTING_CONFIG` — `VotingConfig` (G-D0, Track D).

**Rationale:**

- redb (not Limbo): cluster metadata is **mutable key→value** (owner of a scope
  changes; saga state advances), which is redb's model. Limbo is append-only event
  history — ownership is not an event stream.
- All cluster tables are written **only** through the `FencedStore` fenced write entry
  introduced by #496 (a raw write must not bypass the guard) — see G1. This is why the
  schema lives in #496's scope.
- `NodeId` is additionally recorded in `daemon.toml` so a node knows its own identity
  before opening redb.

The `OwnerMetadataLog` (G-D0, Track D) replicates a **subset** of this metadata
(`OwnerTerm`/`VotingConfig`/`RecoveryPoint`/`MigrationOp` summary) across nodes for
quorum safety; ADR-3 is the **local** durable schema, G-D0 is the **replicated**
metadata. They are consistent: the log replicates what these tables hold for the
metadata it covers, never agent state.

## Non-Goals

- Does not put agent state, CAS bytes, or events into these tables (those stay in
  their stores).
- Does not define the replication protocol (G-D0) — only the local schema.
- Does not specify exact serde field layouts of each row (produced by #496/Track-D
  ADRs as the types are built; pre-inventing them is unverifiable).

## Data Types

Keys are `NodeId`- / scope-keyed (N-node-native), values are versioned serde blobs
(`#[serde(default)]` + `schema_version`, the established #491 pattern). `OwnerTerm`,
`MigrationOp`, `ProvisionOp`, `Pin`, `NodeIdentity`, `RecoveryPoint`, `VotingConfig`
are defined by their respective gates (G1/G5/G-D0).

## State Machine / Protocol

Each table is mutated only inside a fenced write transaction (G1). Saga tables
(`MIGRATION_OPS`/`PROVISION_OPS`) advance through their state machines idempotently and
are reconciled on restart.

## Failure Modes

- **Crash mid-saga:** `MIGRATION_OPS`/`PROVISION_OPS` rows are durable and reconciled
  on startup (each step idempotent).
- **Raw write bypass attempt:** prevented by the type system (G1 `OwnerWriteGuard`) —
  cluster tables expose no un-fenced write path.
- **Pin reconcile on restart:** `CLUSTER_PINS` are reconciled against open
  MigrationOps/manifest refs (V20) so pins neither hang forever nor expire too early.

## Tests

- A cluster-table write without an `OwnerWriteGuard` does not compile (G1 type
  barrier).
- Saga tables survive a daemon restart and reconcile to a consistent state.
- Backward-compat decode of an older `schema_version` row.

## Benchmarks

Cluster-table write overhead is part of the #496 fencing-overhead bench (it sits on
the cluster write path). No separate latency target. Register: covered under
`sentinel-daemon-owner-fencing (#496)`.

## Backward Compatibility

Additive: new tables only; existing redb tables and data are untouched. Existing
single-node snapshots/restore are unaffected (cluster tables are empty/absent in
single-node mode). No migration of `events`.

## Security

Single trust domain (Track A). Writes gated by the typed `OwnerWriteGuard` (G1).
Cluster metadata is local-durable; cross-node trust is the OwnerMetadataLog quorum
(G-D0), not these tables alone.

## Public Claim Boundary

- May claim today: cluster metadata persists in dedicated redb tables behind the
  fenced write entry (decided).
- **May NOT claim:** that ownership is persisted/enforced — the tables and the fenced
  write entry are built by #496; today `owner_epoch` is in-memory only.

## Open Follow-ups

- G1 fencing scope spec (`OwnerWriteGuard`, `FencedStore` trait, the three engine
  impls) — #496.
- G-D0 replication of the owner/voting subset (Track D).
- RecoveryPoint field set (G6/G8).
