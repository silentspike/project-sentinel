# ADR-0397: Node lifecycle — cordon / drain / decommission (G-D2)

- **Gate:** G-D2 (blocks Track-D2; does **not** block Track A)
- **Status:** Proposed
- **Primary issue:** #397 (Cluster 12 epic) — Track D2
- **Related issues / gates:** G-D0 (voting config), G-D3 (CAS evacuation), G-N0, V1
- **Supersedes / Superseded by:** —

> **N-node-native rule:** all schemas/messages/APIs MUST be `NodeId`-keyed, never a hard
> source/target pair. Two nodes are the first test, not the ceiling.

## Context

N-node operation needs safe **removal/maintenance** of a node, not only addition.
`NodeLifecycleState` is introduced at provisioning (G-GENESIS) as
`{PendingBare, Provisioning, Joining, NonVoting, Active, Cordoned, Draining,
Decommissioning, Removed, Quarantined}`. Removing a node naively risks quorum loss,
CAS data loss (blocks only that node held), and orphaned owners.

## Problem

How is a node cordoned/drained/decommissioned without losing quorum, CAS data, or
owners, and how are compromised nodes quarantined?

## Decision

**A staged lifecycle with cordon-before-drain, owner/CAS evacuation before removal, a
voting-config transition before physical removal, and cert revocation that is honored
everywhere.**

- **Cordon (D2 state before drain):** `schedulable=false`, no new owner placements; the
  node keeps running its current owners.
- **Drain:** no new containers/owners; running ones migrate away cooperatively via the
  V1 handoff.
- **OwnerEvacuation:** all owners the node holds are handed off (V1) — 0 orphaned.
- **CASEvacuation (couples G-D3):** blocks held **only** by this node are replicated to
  others **before** decommission — else pull-by-hash loses them. V8: on uncertainty,
  keep/abort, never delete.
- **RemoveVotingMember:** a `VotingConfig` transition (G-D0 `Cold→Cold+new→Cnew`)
  **before** physical removal — never drop below quorum.
- **Decommission:** after drain+evacuation, remove definitively.
- **CertRevocation / Quarantine:** revoke a node cert (membership/CAS/OwnerRegistry
  reject it afterward); a compromised node is `Quarantined` (no membership/CAS-ads/drain;
  recover only from trusted RecoveryPoints). Short-lived node certs, planned rotation
  without downtime, immediate revocation on compromise.

## Non-Goals

- Not Track A. Cert rotation infra is new here (Track A is pinned-trust without
  lifecycle, V35).
- Geo/failure-domain evacuation policy (Track I / optional V34).

## Data Types

`DrainOp`, `DecommissionOp`, `RevocationSource { generation, revoked_serials/node_ids }`,
`NodeLifecycleState` (extended with `Cordoned`). Reconciled per G-N0/V39.

## State Machine / Protocol

`Active → Cordoned → Draining (owner+CAS evacuation) → Decommissioning → Removed`;
`Quarantined` is reachable from any state on compromise. Voting removal is a separate
committed transition.

## Failure Modes

- **Quorum loss:** prevented — `RemoveVotingMember` transitions voting config first.
- **CAS data loss:** prevented — CASEvacuation proves replication before remove (G-D3);
  uncertainty → keep.
- **Revoked node rejoin:** rejected everywhere via `RevocationSource` (fail-closed if
  the revocation generation is unverifiable).

## Tests

Drain migrates all containers (0 orphaned); decommission preserves quorum; CASEvacuation
proves replication before remove; a revoked cert is rejected cluster-wide.

## Benchmarks

Drain duration vs. container count, evacuation bytes/time, quorum stability during
transition. Register: Track-D2.

## Backward Compatibility

New states/ops additive; single-node has no drain/decommission semantics (degenerate).

## Security

Cert revocation/quarantine is a security boundary; revocation is checked before any
membership/CAS/owner acceptance, fail-closed when unverifiable.

## Public Claim Boundary

- May claim after Track D2: safe node drain/decommission/quarantine with no quorum/CAS
  loss.
- **May NOT claim:** any lifecycle beyond add in Track A.

## Open Follow-ups

- G-D3 replication so evacuation has targets; failure-domain policy (optional, V34).
