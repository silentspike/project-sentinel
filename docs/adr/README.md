# Architecture Decision Records (ADRs)

This directory holds **Architecture Decision Records** for Project Sentinel. An ADR
captures a single architectural decision, the context that forced it, the options
considered, and the consequences. ADRs are **immutable once `Accepted`**: a later
decision that overturns an earlier one is a **new** ADR that marks the old one
`Superseded`.

ADRs are **not** spikes. Exploratory/measurement work lives under `docs/spikes/`.
An ADR records a *decision*; a spike records an *experiment*.

## Scope of the current ADR set

The first ADRs in this directory govern the **TOGAF Cluster 12** target
architecture: the cross-node / N-node platform layer (operator-approved
self-provisioning node bootstrap, distributed CAS, owner fencing, per-container
state transfer, and cross-node migration). TOGAF Cluster 12 remains target
architecture rather than a Cluster GA claim. Several Track A primitives now exist;
the ADR statuses below distinguish accepted contracts from later implementation and
live-acceptance gates.

The set is split into:

- **Gate-#0 ADRs** (this PR): the top-level forks that everything else depends on —
  `G0`, `G-GENESIS`, `G-D0`, `ADR-2`, `ADR-3`.
- **Per-gate ADRs** (follow-up PRs): `G1`–`G7` (Track-A feature gates) and the
  Track-D/H gates (`G-N0`, `G-D2`, `G-D3`, `G-H2`, `G6`+`G8`, `G2` CAS-hash model).

Each feature issue blocks on its ADR gate(s).

## Naming convention

```
docs/adr/ADR-<primary-issue>-<short-slug>.md
```

`<primary-issue>` is the 4-digit GitHub issue number this decision most directly
serves (the issue body links the ADR as its gate). A decision that spans several
issues uses the issue it gates first. The internal gate label (`G0`, `G-D0`, …) is
recorded inside the document.

## Status lifecycle

`Proposed` → `Accepted` → (`Superseded by ADR-xxxx` | `Deprecated`)

A `Proposed` ADR may still change. An `Accepted` ADR is frozen.

## Mandatory N-node-native rule (Track A)

Every Track-A ADR carries this constraint verbatim:

> Even though the foundation is verified on a 2-node cluster first, **all schemas,
> messages and APIs MUST be N-node-native** (`NodeId`-keyed sets/maps, never a hard
> source/target pair as the cluster model). Two nodes are the first test, not the
> ceiling.

## Index

| ADR | Gate | Decision (one line) | Status |
|-----|------|---------------------|--------|
| [ADR-0397-G0-cross-node-simulation-time](ADR-0397-G0-cross-node-simulation-time.md) | G0 | Per-node sub-worlds + async causal messaging + per-node determinism (no global barrier tick) | Proposed |
| [ADR-0495-G-GENESIS-first-seed-bootstrap](ADR-0495-G-GENESIS-first-seed-bootstrap.md) | G-GENESIS | First seed node is the one allowed manual deploy; nodes 1..N only via `ProvisionNode` | Proposed |
| [ADR-0496-G-D0-ownermetadatalog](ADR-0496-G-D0-ownermetadatalog.md) | G-D0 | Quorum-backed `OwnerMetadataLog` for owner/voting metadata only — no Raft for agent state | Proposed |
| [ADR-0498-ADR2-control-plane-transport](ADR-0498-ADR2-control-plane-transport.md) | ADR-2 | Cert-pinned QUIC with separate control and bounded snapshot listeners; durable journals provide effect idempotency | Accepted |
| [ADR-0496-ADR3-cluster-meta-schema](ADR-0496-ADR3-cluster-meta-schema.md) | ADR-3 | Atomic complete owner snapshot installation plus durable migration/participant CAS journals | Accepted |
| [ADR-0496-G1-ownership-handoff-safety](ADR-0496-G1-ownership-handoff-safety.md) | G1 | Complete-term V19, fail-closed activation, and quiesce/copy/stage before retirement | Accepted |
| [ADR-0498-G2-cas-blockref-hash-model](ADR-0498-G2-cas-blockref-hash-model.md) | G2 | Namespaced `BlockRef`; block map is a locator not liveness; durable CAS publish | Proposed |
| [ADR-0495-G3-provisionnode-threat-model](ADR-0495-G3-provisionnode-threat-model.md) | G3 | Allowlist targets, out-of-band host-key pin, target-local keys, reciprocal cert/NodeId pins, repo-templated token-gates | Accepted |
| [ADR-0497-G4-snapshot-consistency](ADR-0497-G4-snapshot-consistency.md) | G4 | Real source watermark, canonical digest, durable staging, and sealed frozen restore | Accepted |
| [ADR-0501-G5-migration-saga](ADR-0501-G5-migration-saga.md) | G5 | Seventeen-step durable stop-and-copy saga with authority-commit forward recovery | Accepted |
| [ADR-0499-G7-cluster-delete-guard](ADR-0499-G7-cluster-delete-guard.md) | G7 / G-DELETE | Stage A dry-run query/pin before #501; Stage B destructive guard only in #547 after #501 | Accepted |
| [ADR-0397-G6-G8-state-durability-recovery-point](ADR-0397-G6-G8-state-durability-recovery-point.md) | G6 + G8 | `RecoveryPoint` + durability/RPO classes; forced failover only from a quorum RecoveryPoint at `min_rf` | Proposed |
| [ADR-0397-G9-binary-provenance](ADR-0397-G9-binary-provenance.md) | G9 | Track A = sha256 manifest; signed release manifest is a Track-H GA hardening | Proposed |
| [ADR-0397-G-H2-backup-restore](ADR-0397-G-H2-backup-restore.md) | G-H2 | Cold cluster backup/restore that rejects stale certs/retired owners + verifies CAS | Proposed |
| [ADR-0397-G-N0-n-node-object-model](ADR-0397-G-N0-n-node-object-model.md) | G-N0 | Unified reconcilable control-plane object model; distinct node-sets; fail-closed minority | Proposed |
| [ADR-0397-G-D2-node-lifecycle](ADR-0397-G-D2-node-lifecycle.md) | G-D2 | Cordon → drain → owner/CAS evacuation → voting-config transition → decommission; quarantine | Proposed |
| [ADR-0397-G-D3-cas-replication-repair](ADR-0397-G-D3-cas-replication-repair.md) | G-D3 | Continuous CAS replication/repair to `desired_rf`; refuse `RPO=0` failover under `min_rf` | Proposed |
