# ADR-0497: Per-container snapshot, staging, and restore consistency (G4)

- **Gate:** G4 (blocks #501 snapshot transfer/restore)
- **Status:** Accepted
- **Primary issues:** #497 and #501
- **Related gates:** G1, ADR-2, ADR-3, G5, G-ROUTE

> Even though the foundation is verified on a two-node cluster first, all schemas,
> messages, and APIs are N-node-native and keyed by `NodeId`. Two nodes are the first
> test, not the cluster model.

## Context

#497 delivered the `Frozen` mutating-system matrix, per-agent ECS/redb/FS snapshot and
restore primitives, and route registry foundation. The current `SnapshotCut` still
stores a bare `event_cursor: i64`, and the per-agent ECS cut writes 0. #501 requires a
real source-local provenance watermark, a canonical digest, durable target staging,
and restore authorization bound to the committed target term.

## Problem

How does Sentinel produce a torn-free, source-identifiable snapshot, stage it durably,
and restore it without admitting writes or stale authority at the target?

## Decision

Keep the #497 per-container freeze model and extend the cut, digest, staging, and
restore contracts for cross-node use.

## Quiesce and frozen cut

Before the cut, agent-scoped admission closes across ECS, EventStore, redb, and
operator writes. Existing writes drain or fail their complete-term commit recheck.
Eligibility is rechecked under the closed barrier. The agent is then `Frozen`; other
agents and the global 1 Hz tick continue.

The cut captures:

- the complete owner term;
- ECS components for exactly the subject container;
- every per-agent redb row, including explicit absence;
- filtered FS metadata and referenced CAS rows;
- a typed source provenance watermark.

## Source provenance watermark

Replace the bare cursor with:

`SourceProvenanceWatermark { source_node, scope, local_event_row_id: i64 }`

The row id is the real source EventStore watermark obtained after the write drain under
the same barrier. Negative values are rejected. Legacy `event_cursor: i64` decodes
versionedly as `LegacySourceLocal(i64)` and never pretends to be a cluster-global
cursor.

## Canonical digest

The versioned SHA-256 snapshot digest covers canonical, fixed-order encoding of:

- header, schema, scope, and complete owner term;
- ECS state;
- every per-agent redb table row and explicit absence marker;
- FS metadata/CAS references;
- the source provenance watermark.

Golden vectors and round trips prevent serializer drift. `state_hash()` is only an
additional runtime smoke value and is not the transfer-integrity digest.

## Durable target staging

The staging header contains schema version, operation id, scope, complete term, byte
size, and digest. The target performs:

1. bounded temporary write;
2. size, schema, term, and digest validation;
3. `fsync(file)`;
4. atomic rename;
5. `fsync(directory)`;
6. durable participant outcome;
7. `StagingAck { digest, staging_id }`.

Startup reconciliation handles a crash between rename and journal completion. It may
delete only staging files that are unambiguously incomplete, invalid, or expired; a
valid staged copy for an active operation is preserved.

## Sealed restore permit

Restore requires:

`MigrationRestorePermit { scope, target, full_term, op_id, digest, schema, staging_id, transition_seq }`

The permit is sealed by the migration module. Restore begin and commit recheck target,
complete term, operation state/sequence, digest, schema, and staging id. ECS spawn
begins `Frozen`.

`restore_agent_tables(rows, &permit)` writes all per-agent rows in one redb
transaction. An input `None` deletes an existing target row, so stale target data
cannot survive a restore. The target remains `OwnerActivating` and `NotRoutable` after
restore.

## Reference integrity and route boundary

The restore despawns/respawns only the subject agent. Stable identity remains the
agent/container id; local `EntityId` is never a cross-node reference. Named
relationship/perception lookups resolve a typed `RemoteRoute { node, full_term }`
through the route registry after migration. This decision does not claim cross-node
message delivery.

## Bounded eligibility

Track A returns typed `NotMigratable` for active inbound cross-agent traffic, active
external side effects/LLM calls, and active scheduled/delayed effects. Per-agent time
travel after migration returns `NotSupportedForMigratedContainer` until Track E/H.

## Failure modes

- **Late writer:** commit recheck rejects after quiesce/term change.
- **Torn cut:** one barrier covers drain, watermark, ECS, and per-agent row capture.
- **Partial staging:** no acknowledgement before both fsyncs and durable outcome.
- **Crash after rename:** startup probe recognizes the valid staged artifact.
- **Wrong/stale permit:** restore rejects at begin or commit.
- **Stale target row:** explicit absence deletes it in the restore transaction.
- **Stale entity reference:** route lookup uses stable id plus complete term.

## Tests and evidence

- The #497 mutating-system matrix and frozen-agent bit-identity tests remain required.
- A write that begins before quiesce and commits late receives a typed reject.
- Real positive source watermark and legacy/negative decode tests.
- Canonical digest golden vectors include all per-agent rows and watermark.
- Staging fsync/rename/journal crash-point and startup-reconcile tests.
- Restore permit begin/commit TOCTOU, explicit-delete, and frozen-spawn tests.
- Two-node source/target digest equality and remote-reference resolution evidence.

## Consequences

- The snapshot has explicit source provenance rather than a misleading global cursor.
- Durable staging becomes the first migration-specific recoverable copy boundary.
- Restore cannot independently grant routability or normal write authority.
- The existing single-node snapshot/restore path remains compatible through versioned
  legacy decode.

## Public claim boundary

After #501, Sentinel may claim a versioned, digest-verified ECS-native snapshot transfer
and permitted restore for the resting bounded class. It may not claim active inbound or
side-effect continuity, per-agent event-history continuity, all-runtime migration, or
absolute RPO=0.
