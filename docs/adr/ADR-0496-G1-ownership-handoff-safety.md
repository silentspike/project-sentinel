# ADR-0496: Ownership, activation, and handoff safety (G1)

- **Gate:** G1 (blocks the A5 follow-up to #496 and #501)
- **Status:** Accepted
- **Primary issues:** #496 and #615
- **Related issues / gates:** ADR-2, ADR-3, G4, G5, G-D0

> Even though the foundation is verified on a two-node cluster first, all schemas,
> messages, and APIs are N-node-native and keyed by `NodeId`. Two nodes are the first
> test, not the cluster model.

## Context

#496 introduced real fenced writes, durable owner terms, local retirement, and a
cooperative ownership handoff. The current implementation still has an unsafe cluster
bootstrap boundary: `OwnerTerm` lacks a coordinator generation, `LocalOwnerRole`
defaults to `Owner`, `OwnerRegistry::issue()` is infallible, and clustered nodes enter
through the single-node initializer. A complete handoff or migration must also stage a
recoverable copy before retiring the only live source.

## Problem

How does Sentinel make a stale, non-owning, non-routable, or not-yet-bootstrapped write
impossible while preserving the existing single-node fast path and allowing a
recoverable stop-and-copy move?

## Decision

Sentinel separates data-plane write authority, control-plane authority transitions,
local activation, and route readiness. A normal write is admitted only after every
layer agrees on the complete authority term.

### Complete authority term

The authoritative term is:

`OwnerTerm { scope, owner_node, epoch, coordinator_generation }`

- `epoch` is monotonic per scope.
- `coordinator_generation` identifies the authority line. Track A uses generation 1;
  generation 0 is legacy.
- A guard carries the complete term, not only the epoch.
- Unknown scopes are never synthesized as self-owned in cluster mode.

### Complete V19 validation

`OwnerRegistry::issue(scope)` is fallible in cluster mode. It returns a normal
`OwnerWriteGuard` only when:

1. owner/activation readiness is open;
2. scope, owner node, epoch, and coordinator generation exactly equal the current term;
3. `term.owner_node == this_node`;
4. the effective local role is `Owner`;
5. activation is `Routable`.

Write begin and commit recheck the same complete conditions. A mismatched generation,
owner, epoch, role, activation, scope, or readiness latch produces a typed rejection.
The final #501 path additionally requires route readiness before normal guards can be
issued.

### Local base state and saga overlay

Stable recipient state is stored as `LocalOwnerBaseState` with base role
`Owner|Follower` and `ActivationState`. Transitional state is a scope-keyed
`LocalOwnerSagaState` for `LegacyReconciliation|Handoff|Migration`.

The effective local state is the saga overlay when present, otherwise the base state.
General owner snapshot replication never removes an overlay. Only the active handoff
or migration can CAS-replace or complete it. Conflicting active operations for one
scope require manual recovery.

### Readiness and single-node compatibility

The owner/activation latch starts closed on every process start. Cluster mode opens it
only after a valid atomic snapshot marker is checked and owner/local caches are rebuilt
under the tick barrier. Network loss never creates self-ownership.

A daemon without `[daemon.cluster]` retains the single-node fast path and explicitly
promotes legacy activation to `Routable`.

### Quiesce, copy, then retire

The pre-authority portion of a migration is ordered as follows:

1. validate the bounded eligibility class;
2. reserve transfer and establish the target-to-source snapshot connection outside
   the pause;
3. set the source overlay to `Retiring`, route to `Migrating`, and close agent-scoped
   admission;
4. drain in-flight ECS, EventStore, redb, and operator writes and recheck eligibility;
5. freeze the source container;
6. cut ECS state, all per-agent rows, and the real source watermark under the same
   barrier;
7. transfer and durably stage the verified target copy;
8. only then persist source `Retired` and acknowledge retirement;
9. only after that acknowledgement may the coordinator commit target authority E+1.

This order preserves a verified staged copy before the only source is retired. The
target cannot become routable before source retirement, full owner snapshot
replication, restore, unfreeze, and final activation.

### No forced failover in Track A

Unreachability never steals ownership in the two-node foundation. Existing owners keep
their valid authority; new ownership and migration stop when the coordinator is
unavailable. Witness, quorum, coordinator replacement, and forced failover remain
Track D.

## Control-plane authority exception

The control-plane operations that replace an owner term cannot be authorized by the
old `OwnerWriteGuard` whose authority they are changing. They use the authenticated,
CAS-checked atomic store APIs defined by ADR-3. Normal simulation-state writes remain
behind `FencedStore`.

## Failure modes

- **Cluster bootstrap without a snapshot:** readiness remains closed and writes fail.
- **Partition after retirement:** the local saga overlay continues to fence the source.
- **TOCTOU:** commit rechecks the complete term and effective local state.
- **Crash before durable target staging:** source E can reopen and continue under the
  existing single-replica RPO.
- **Crash after source retirement but before authority commit:** source reactivation
  requires a new recovery epoch.
- **Crash after authority commit:** recovery is forward-first to the target.

## Tests and evidence

- Single-node compatibility and lock-free fast path.
- Legacy generation-0 and activation decoding.
- Fallible guard issue and complete V19 begin/commit rechecks.
- Restart latch and cache rebuild under the tick barrier.
- Scope-keyed overlay preservation and authorized counter-handoff CAS.
- Two-node live evidence: one owner, a real owner store write succeeds, non-owner write
  rejects, residency exists only at the effective owner, and restart opens only after
  rebuild.
- #501 ordering tests prove staging before retirement and no routability before the
  final activation acknowledgement.

## Consequences

- Cluster startup is intentionally fail closed.
- A handoff and a migration share one local overlay model without sharing their full
  orchestration state machine.
- Authority changes are explicit control-plane transactions rather than ordinary data
  writes.
- The single-node operator migration path remains unchanged.

## Public claim boundary

After #615 is live-accepted, Sentinel may claim complete owner/activation fencing for
the two-node Track A foundation. It may not claim forced failover, quorum, replicated
RPO, Cluster GA, or migration of active interaction/side-effect workloads.
