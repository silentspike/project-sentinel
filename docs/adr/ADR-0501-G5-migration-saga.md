# ADR-0501: Cross-node stop-and-copy migration saga (G5)

- **Gate:** G5 (blocks #501)
- **Status:** Accepted
- **Primary issue:** #501
- **Related gates:** G1, ADR-2, ADR-3, G4, G7

> Even though the foundation is verified on a two-node cluster first, all schemas,
> messages, and APIs are N-node-native and keyed by `NodeId`. Two nodes are the first
> test, not the cluster model.

## Context

#501 composes owner fencing, per-container snapshots, cert-pinned QUIC transfer, and
route resolution into one persistent and recoverable move. A transient command or a
process-local retry cache cannot preserve correctness across coordinator, source, or
target crashes.

## Problem

What single durable order makes a source-to-target move recoverable at every boundary,
prevents early target writes/routing, and defines honest RPO/rollback behavior?

## Decision

Use a coordinator-owned `MigrationOp`, per-participant durable step journals, and the
following one-way stop-and-copy saga. Cross-node migration remains behind a
repository-default-OFF flag; the existing single-node migration command is unchanged.

## Migration operation

`MigrationOp` contains at least:

`op_id(UUIDv7), scope, source, target, source_term, target_term, state, snapshot_id, snapshot_digest, snapshot_schema_version, staging_id, authority_snapshot_revision, transition_seq, started_at_ms, updated_at_ms, failure_reason`

`MIGRATION_OPS` and `ACTIVE_MIGRATION_BY_SCOPE` are claimed atomically. Only the
coordinator advances the operation through
`transition_migration(expected_state, expected_seq, next)`. The audit row remains after
completion; only the active-scope index is released.

## Durable states

States name completed durable steps:

`Claimed -> TransferReserved -> TargetStreamReady -> SourceQuiescing -> Frozen -> SnapshotCreated -> TargetStagedDurable -> SourceRetired -> OwnerAuthorityCommitted -> OwnerCommittedToTarget -> OwnerSnapshotReplicated -> TargetRestored -> TargetActivatedNotRoutable -> RouteSwitchCommitted -> TargetUnfrozen -> TargetRoutable -> SourceRetiredFinal -> Completed`

Terminal recovery states are `RollbackBeforeAuthority`, `RollbackAfterAuthority`, and
`ManualRecoveryRequired`.

## Canonical saga

1. **EligibilityPreflight:** verify bounded eligibility, feature flag, source/target
   liveness, complete terms, and route state.
2. **ReserveTransfer:** source persists a bounded reservation binding operation,
   source/target node ids, both certificate fingerprints, scope, complete source term,
   size cap, and expiry.
3. **PrepareTargetStream:** target establishes the certificate-pinned connection to
   the dedicated source snapshot listener but sends no snapshot request. Connection
   setup is outside the pause.
4. **BeginSourceQuiesce:** source overlay becomes `Retiring`, route becomes
   `Migrating`, agent-scoped admission closes, in-flight writes drain or fail commit
   recheck, and eligibility is checked again.
5. **Frozen:** source sets the ECS `Frozen` marker while other agents keep ticking.
6. **SnapshotCreated:** under the same barrier, source cuts ECS, every per-agent redb
   row, and the real source provenance watermark; it computes the versioned SHA-256
   digest and publishes the reserved transfer as ready.
7. **StartSnapshotPull:** only now target requests by operation id on the already-open
   connection, receives bounded bytes, durably stages them, and acknowledges digest and
   staging id.
8. **CommitSourceRetirement:** after durable staging, source persists `Retired` plus
   participant outcome and acknowledges. Rollback to source ownership remains possible
   until authority commit.
9. **OwnerAuthorityCommit:** one coordinator redb transaction writes the step claim,
   target global term E+1, incremented owner snapshot revision/metadata, and operation
   state `OwnerAuthorityCommitted`. Recovery becomes forward-first at this transaction.
10. **CommitMigrationOwner:** target transaction persists the target global term,
    base `Owner/NotRoutable`, `OwnerActivating` overlay, and participant outcome. Normal
    guard issue remains closed.
11. **ReplicateOwnerSnapshot:** coordinator sends the full global snapshot and each
    recipient-local snapshot to every Track A member, including source and target. All
    acknowledgements are required before restore. Replication leaves overlays intact.
12. **BeginRestore:** target restores only under the sealed permit, spawns `Frozen`,
    validates the digest, and atomically restores every per-agent row. It remains
    `OwnerActivating`.
13. **TargetActivatedNotRoutable:** target route remains `Prepared`; normal writes and
    external routing remain closed.
14. **RouteSwitch:** target persists only the durable route-switch decision and
    participant outcome. The cache route remains `Prepared` until final activation.
15. **ApplyTargetUnfreeze:** under the world/tick barrier target removes `Frozen`,
    persists `UnfreezeApplied`, then acknowledges. Startup reconciliation can refreeze
    or idempotently forward-complete this effect.
16. **FinalizeTargetActivation:** four serialized layers execute in order:
    - transactionally set base activation `Routable`, remove `OwnerActivating`, and
      persist the participant outcome;
    - rebuild owner and route caches under the activation lock;
    - open owner and route readiness latches last;
    - send `TargetRoutableAck`, after which only the coordinator advances the operation.
17. **FinalizeSourceRetirement:** source despawns the frozen agent, caches
    `Remote { target }`, persists the final participant outcome, then acknowledges.
    Source role remains `Retired`. If source is unreachable, the safe operation remains
    at `TargetRoutable` until reconciliation completes the final source step.

## Participant effect idempotency

Every mutating participant step uses the ADR-3 claim/probe/complete journal and a
request digest. The process-local control cache is reply deduplication only. A crash
after effect but before journal completion is resolved by a deterministic outcome
probe; the mutation is never blindly replayed.

## Recovery and RPO contract

- **Before `TargetStagedDurable`:** abort, reopen/unfreeze source E, and make no new
  migration-specific replica guarantee.
- **From `TargetStagedDurable`:** preserve the verified target digest while that
  target storage survives. Loss of the only target copy follows the existing
  single-replica RPO; replication is Track D.
- **After source retirement but before authority commit:** source may reactivate only
  under a new recovery epoch; target discards prepared/staged state idempotently.
- **From `OwnerAuthorityCommitted`:** always reconcile forward to the target. Source
  E+2 is permitted only after proving the exact digest or transferring/restoring the
  staged copy back under a new permit.
- **After target routability:** moving back is a new cooperative migration.
- There is no two-node forced failover and `qm rollback` is never migration recovery.

## Route and activation invariant

`RouteState = Local | Migrating | Prepared | Remote`. A route entry carries node id,
complete term, state, operation id, and transition sequence. Stale updates compare the
complete term plus operation/sequence. Startup rebuild uses owner terms, migration
operations, and participant outcomes, never owner terms alone.

Normal writes require both owner/activation and route latches. The target is never
normal-writable or externally routable during `OwnerActivating`, `Prepared`, or any
earlier state.

## Bounded eligibility

Track A supports resting, non-interacting ECS-native containers only. Active inbound
cross-agent traffic, active external side effects/LLM calls, and active scheduled or
delayed effects return typed `NotMigratable`. Per-agent time travel after a move returns
`NotSupportedForMigratedContainer` until Track E/H.

## Failure injection

Tests cover coordinator/source/target restart at every durable boundary, partition,
double move, route failure, authority commit before RPC, crash after each of the four
activation layers, and acknowledgement loss after activation is fully open. Every case
must deterministically resume, perform an allowed rollback, or require manual recovery
without a duplicate write.

## Pause measurement

Pause is source quiesce start through `TargetRoutableAck`, measured on a
coordinator/source monotonic clock. Target-local staging fsync and restore durations are
reported as local components and are never subtracted across node clocks.

Report connect, quiesce drain, snapshot, staging fsync, authority commit, restore,
route switch, pause, total, retries, and failures with p50/p95/p99/max. Warm and cold
bounded-state claim cells each require at least 1000 successful runs. Smaller
exploratory state-size sweeps carry no p99 claim.

## Tests and evidence

- State ordering and coordinator-only transition tests.
- Crash/resume tests at every state and double-move rejection.
- Atomic authority commit and forward-first tests.
- Complete participant outcome-probe matrix.
- Real two-node state/digest/reference, exactly-one-owner, typed stale write, and
  no-early-routability evidence.
- Recovery/RPO evidence uses only the boundaries stated above.
- Contention with CAS pull, #499a dry-run query, and the 1 Hz tick after #499a merges.

## Consequences

- The authority transaction, not RPC delivery, is the forward-first boundary.
- Durable target staging exists before source retirement.
- Activation is intentionally multi-layered and readiness opens last.
- Source cleanup can lag safely after target routability.

## Public claim boundary

After live acceptance, Sentinel may claim a measured two-node ECS-native stop-and-copy
PoC for the bounded resting class. It may not claim Cluster GA, forced failover,
post-copy, all runtime types, active-interaction continuity, absolute RPO=0, or a
millisecond class without cited VM values.
