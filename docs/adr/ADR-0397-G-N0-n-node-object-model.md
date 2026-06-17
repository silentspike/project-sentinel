# ADR-0397: N-node control-plane object model (G-N0)

- **Gate:** G-N0 (blocks Track-C/D; does **not** block Track A)
- **Status:** Proposed
- **Primary issue:** #397 (Cluster 12 epic) — Track C/D
- **Related issues / gates:** G-D0, G-D2, G-D3, Track C (#503), V37/V38/V39
- **Supersedes / Superseded by:** —

> **N-node-native rule:** all schemas/messages/APIs MUST be `NodeId`-keyed, never a hard
> source/target pair. Two nodes are the first test, not the ceiling.

## Context

The codebase has **0** cluster control-plane types (verified: `OperatorCommand` has no
cluster variant, redb has no cluster tables). Track C/D need a coherent object model
rather than ad-hoc fields scattered across handlers. TOGAF is "Beyond Kubernetes"
(control center) and silent on the object model → this is our own model; the Kubernetes
node-status is a **reference, not a template to copy**.

## Problem

What is the unified control-plane object model so that membership, scheduling, ownership,
migration, provisioning, and CAS repair share one reconcilable shape?

## Decision

**One object model where every control object carries generation + observed-generation +
status.conditions + last_transition + actor/op_id, and the node-set distinctions are
explicit and reconciled, not fire-and-forget.**

- **Objects:** `NodeIdentity, NodeStatus, NodeLifecycleState, VotingConfig, OwnerTerm,
  RecoveryPoint, MigrationOp, ProvisionOp, SchedulerBinding, Pin, CASRepairTask`.
- **Every object carries:** `generation, observed_generation, status.conditions,
  last_transition, actor/op_id` (audit).
- **`NodeStatus` holds:** `lifecycle_state, membership_state, schedulable, voting_role,
  capacity, allocatable, pressure{Cpu|Mem|Io|Disk|Net}, runtime_capabilities,
  cas_capacity, cas_under_replicated_refs, conditions[]`.
- **HA commit semantics (V37):** Track A (cooperative) — local `OwnerWriteGuard` +
  `LocalOwnerState` suffices. Track D (HA) — a local store commit is **not**
  cluster-committed; a protected write is cluster-committed only when the declared
  RPO/durability rule is met. **Partition minority is fail-closed by default.** Any
  local uncommitted write goes to a **separate, non-projected pending log — never a
  normal store commit** — reconciled/discarded after partition heals.
- **Node-sets are distinct (V38):** `ObservedMembership ≠ RegisteredNodes ≠
  SchedulableNodes ≠ VotingMembers ≠ CASReplicationTargets ≠ QuarantinedNodes ≠
  DrainingNodes`. **"Alive" implies none of:** schedulable, voting, CAS-trusted,
  drain-safe, migration-target-eligible (SWIM gives only ObservedMembership — V13/V16).
- **Reconcile, not fire-and-forget (V39):** `ProvisionOp/MigrationOp/DrainOp/
  DecommissionOp/CASRepairTask/SchedulerBinding` are desired-state objects with observed
  status + conditions; controllers reconcile idempotently to `Completed | Failed |
  Quarantined | ManualRecoveryRequired`.

## Non-Goals

- Not Track A (Track A stays slim + ECS-native; these contracts apply from Track C/D).
- We do **not** copy Kubernetes; the node-status is a reference only.

## Data Types

The objects above (new, `sentinel-common`/daemon), persisted in the ADR-3 tables. All
`NodeId`-keyed.

## State Machine / Protocol

Each desired-state object has a reconcile loop: `observe → diff(desired, observed) →
act(idempotent) → record condition`, terminating in a terminal condition.

## Failure Modes

- **Partition minority writes:** fail-closed; any local write is in a separate
  non-projected pending log, never a store commit.
- **Alive-but-not-ready node treated as schedulable:** prevented by the distinct
  node-sets (V38).
- **Lost reconcile:** controllers are idempotent and resume from observed status.

## Tests

Node-set membership tests (alive ≠ schedulable/voting/CAS-trusted); a partition-minority
protected write is rejected; a reconcile loop is idempotent and reaches a terminal
condition; the pending-log writes are never projected.

## Benchmarks

Reconcile-loop latency, object-store overhead. Register: Track-C/D.

## Backward Compatibility

New types/tables additive; single-node degenerates to a one-node object set.

## Security

Audit fields (actor/op_id) on every object; single trust domain (Track A→D).

## Public Claim Boundary

- May claim after Track C/D: a reconcilable N-node control-plane object model.
- **May NOT claim:** any of these objects exist/work in Track A (0 built today).

## Open Follow-ups

- Track C scheduler bindings (V36); G-D2 lifecycle; G-D3 CAS repair tasks.
