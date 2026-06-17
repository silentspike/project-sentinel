# ADR-0397: Cluster backup & restore (G-H2)

- **Gate:** G-H2 (blocks GA; does **not** block Track A)
- **Status:** Proposed
- **Primary issue:** #397 (Cluster 12 epic) — Track H
- **Related issues / gates:** G-D0 (owner/voting meta), ADR-3 (cluster tables), V33
- **Supersedes / Superseded by:** —

> **N-node-native rule:** all schemas/messages/APIs MUST be `NodeId`-keyed, never a hard
> source/target pair. Two nodes are the first test, not the ceiling.

## Context

TOGAF is silent on cluster backup/restore. A GA cluster must be cold-recoverable: the
control-plane metadata (OwnerRegistry/VotingConfig/MigrationOps/ProvisionOps/cluster
tables), the state stores (events/redb/FS), the CAS refs, and the cert policy all need a
consistent backup + a safe restore that does not resurrect stale identities.

## Problem

How is a whole cluster cold-backed-up and restored without re-admitting revoked node
certs or a retired owner?

## Decision

**A cold backup/restore of the ADR-3 cluster tables + state stores + CAS refs + cert
policy, where restore rejects stale node certs and retired owners and verifies CAS
integrity.**

- Backup: a consistent cut of the ADR-3 tables (`CLUSTER_OWNER`/`MIGRATION_OPS`/
  `PROVISION_OPS`/`CLUSTER_PINS`/`NODE_REGISTRY`/`RECOVERY_POINTS`/`VOTING_CONFIG`) + the
  state stores + the CAS ref set + the cert/revocation policy.
- Restore: rebuilds the cluster from the backup; **rejects** stale node certs and a
  retired/old owner term (a restore must not let a removed node rejoin or a stale owner
  write); verifies CAS integrity (digests) after restore.

## Non-Goals

- Continuous/online replication (that is G-D0/G-D3 + RecoveryPoint, G6/G8).
- Point-in-time per-agent time-travel across a restore (G-EVENTHIST / Track E).

## Data Types

A `ClusterBackupManifest` (versioned) listing the table/store/CAS-ref/cert-policy
artifacts + their digests. Restore validates against the current `RevocationSource`
(Track D2/H).

## State Machine / Protocol

`quiesce → consistent cut of all artifacts → manifest+digests → archive`; restore =
`load → validate certs/owner-terms/revocation → verify CAS integrity → bring up`.

## Failure Modes

- **Restore of a backup containing a revoked cert:** rejected (the node cannot rejoin).
- **Restore with a retired owner term:** rejected (no stale owner writes).
- **CAS integrity gap after restore:** detected by digest verify; missing refs surfaced
  (couples G-D3 repair).

## Tests

Cold backup → wipe → restore → cluster healthy; a revoked cert in the backup is rejected
on restore; a retired owner term is rejected; CAS integrity verified.

## Benchmarks

Backup size/time, restore time, CAS verify time. Register: Track-H.

## Backward Compatibility

New tooling; no change to running data formats. Single-node backup is the degenerate
one-node case.

## Security

Restore is a privileged operation gated by operator auth; it actively refuses stale
trust material (certs/owner terms) — a security property, not just availability.

## Public Claim Boundary

- May claim after Track H: cold cluster backup/restore with stale-trust rejection.
- **May NOT claim:** any backup/restore guarantee in Track A (not built).

## Open Follow-ups

- Online/incremental backup; revocation freshness (Track D2/H).
