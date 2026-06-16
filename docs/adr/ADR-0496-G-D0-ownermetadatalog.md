# ADR-0496: OwnerMetadataLog — quorum-backed owner/voting metadata (G-D0)

- **Gate:** G-D0 (blocks Track-D forced failover — does **not** block Track A)
- **Status:** Proposed
- **Primary issue:** #496 (owner registry / fencing) — the quorum evolution of its owner metadata
- **Related issues / gates:** Track D (HA/Witness/Quorum), G6/G8 (state durability / RecoveryPoint), ADR-3 (cluster meta)
- **Supersedes / Superseded by:** —

> **N-node-native rule:** Even though the foundation is verified on a 2-node cluster
> first, all schemas, messages and APIs MUST be N-node-native (`NodeId`-keyed
> sets/maps, never a hard source/target pair as the cluster model). Two nodes are the
> first test, not the ceiling.

## Context

TOGAF is explicit (Cluster 12): *"no full Raft for agent state … a monotonic
`owner_epoch` / fencing token … an authoritative chef controller hands out ownership
for 1–2 nodes … real automatic failover only from a 3rd witness node onward."* So
Raft-for-agent-state is **forbidden by the SSOT**, but safe forced failover at N≥3
still needs *some* quorum-backed source of truth for **who owns what**.

In Track A the owner is fenced cooperatively: a monotonic `owner_epoch`
(`FencedStateTransfer.owner_epoch: u64`, `crates/sentinel-common/src/types.rs:702` —
today a bare `u64`) plus a single chef controller. That is correct for 1–2 nodes and
cannot do forced failover (a 2-node system cannot distinguish partition from death).
The codebase has **zero** cluster-metadata types today (`OperatorCommand` has no
cluster variant; redb has no cluster tables — verified).

## Problem

What is the source of truth that lets a majority safely move ownership to a surviving
node at N≥3, **without** running Raft over agent state?

## Decision

**A quorum-backed `OwnerMetadataLog` that replicates owner/voting metadata only — no
agent state.**

The log replicates exclusively:

- `OwnerTerm` (scope + epoch + owner node + coordinator generation),
- `VotingConfig` (voting members, non-voting members, witnesses, generation),
- `RecoveryPoint` metadata,
- `MigrationOp` summary,
- revocation generation,
- critical `NodeLifecycle` transitions.

It does **not** replicate agent state, CAS bytes, or EventStore bytes (this preserves
the TOGAF "no Raft for agent state" constraint — safety comes from quorum over a small
metadata log, not from replicating the world).

**Safety properties:**

- An `OwnerTerm` is valid **only when committed** by a durable quorum of the log.
- A protected write is cluster-committed only against a committed `OwnerTerm` (the
  Track-D HA commit semantic; Track A's local guard is *not* cluster-committed).
- **Witnesses are voters in the log**, not a mere alive signal. A witness persists a
  `WitnessVote { owner_term, scope, voting_config_generation, voted_for_node,
  recovery_point_id }`, never casts two conflicting votes per generation, survives
  restart, and rejects stale terms. A witness that is only an "alive" heartbeat is
  **insufficient** (that is liveness, not fencing) — it MUST participate in the
  token-granting quorum.
- **VotingConfig transitions** must never let an old and a new voting set commit
  `OwnerTerm`s independently. **Decision (no fork): overlapping two-phase config
  `Cold → Cold+new → Cnew`.** A constrained single-member transition (one voting
  member changed per step with a proof of preserved majority overlap) is allowed only
  as a *later* optimization, not as an alternative architecture and **never** as a
  direct single-step swap.
- A partition **minority** receives no quorum answer and therefore cannot commit a
  protected write (fail-closed). It need not learn that it lost ownership — it only
  needs to be unable to commit writes that would be accepted after the partition heals.

This is **not** a second architecture choice and **not** "option A or B": the source
of truth is the `OwnerMetadataLog`, decided.

## Non-Goals

- Not Track A. Track A stays cooperative single-chef fencing; G-D0 blocks only
  Track-D forced failover.
- Does not replicate agent state / CAS / events — those move by pull-by-hash and
  per-container transfer, gated separately (G2/#498, #497).
- Does not by itself guarantee the newest state is recoverable — that is G6/G8
  (RecoveryPoint / state durability), which also blocks forced failover.

## Data Types

`OwnerTerm { scope: StateTransferScope, epoch: NonZeroU64, owner_node: NodeId,
coordinator_generation: u64 }` (replaces the bare `owner_epoch: u64` at
`types.rs:702` for the cluster path). `VotingConfig { config_id, voting_members,
non_voting_members, witnesses, generation }`. `WitnessVote { owner_term, scope,
voting_config_generation, voted_for_node, recovery_point_id }`. All `NodeId`-keyed,
N-node-native. Persisted in redb cluster-meta tables (ADR-3: `VOTING_CONFIG`,
`CLUSTER_OWNER`, `RECOVERY_POINTS`).

## State Machine / Protocol

Log commit = durable quorum of voters (voting members + witnesses). VotingConfig
change = `Cold → Cold+new → Cnew`, each phase committed before the next. Owner change
= a new `OwnerTerm` committed by quorum; the previous owner is fenced for epochs ≤ its
last term.

## Failure Modes

- **Partition:** minority is fail-closed (no quorum → no committed `OwnerTerm` → no
  protected write). Majority can commit a new `OwnerTerm` and take over safely.
- **Witness restart mid-vote:** the persisted `WitnessVote` survives; the witness
  never contradicts a prior vote for the same generation.
- **Config transition interrupted:** the two-phase overlap guarantees no
  independent-quorum split; an interrupted transition is reconciled to the last
  committed phase.
- **Forced failover with required refs under `min_rf`:** **refused** (G-D3/G6 —
  otherwise the system runs at `rf=1` and a quorum owner change is worthless because
  the state lived only on the dead node).

## Tests

- VotingConfig transition committed before any node votes under the new config.
- Partition-minority cannot commit a protected write (negative test).
- Witness never double-votes per generation; survives restart.
- Forced failover refused when required RecoveryPoint refs are under `min_rf`.
- Property/model tests for the log (Track H: OwnerRegistry/voting safety, not
  happy-path only).

## Benchmarks

Quorum-commit latency, config-transition latency, witness-vote round-trip
(p50/p95/p99/max) at N=3/5/7. Recorded in the internal register. Tuning axis: quorum
size vs. commit latency vs. failover safety.

## Backward Compatibility

Track A's `owner_epoch: u64` path keeps working (cooperative, single-chef). The
cluster path introduces `OwnerTerm` additively; the bare epoch is the degenerate
single-owner case. No migration of existing `events`.

## Security

Trust domain is a single trusted cluster (Track A). Node certs / revocation freshness
are Track D2/H. The log's authority is the quorum, not any single node; a stale term
after restart is rejected.

## Public Claim Boundary

- May claim today: the HA source-of-truth is decided (`OwnerMetadataLog`, quorum,
  witnesses as voters, two-phase voting config).
- **May NOT claim:** automatic failover, HA, or "no split-brain" — none is built;
  this gate blocks Track-D forced failover and is not exercised in Track A.

## Open Follow-ups

- The full consensus log implementation (quorum commit + voting-config transition +
  witness-vote persistence) is a large Track-D build, comparable in size to the
  distributed CAS — a named top program risk.
- G6/G8 RecoveryPoint / state-durability (what a durable RecoveryPoint binds; RPO=0
  vs. last-checkpoint).
- G-D3 CAS replication factor / repair service (so failover has the state to recover).
