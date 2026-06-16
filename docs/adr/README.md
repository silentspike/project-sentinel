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
architecture — the cross-node / N-node platform layer (operator-approved
self-provisioning node bootstrap, distributed CAS, owner fencing, per-container
state transfer, cross-node migration). TOGAF Cluster 12 is **target architecture,
not current state**, and 0 % of the cross-node runtime is built today. These ADRs
decide the load-bearing forks **before** any feature code is written, so that the
implementation issues build against fixed decisions instead of re-litigating them.

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
| [ADR-0498-ADR2-control-plane-transport](ADR-0498-ADR2-control-plane-transport.md) | ADR-2 | One QUIC control stream (reuses the dashboard WebTransport frame codec); SSH only for the bare-shell bootstrap | Proposed |
| [ADR-0496-ADR3-cluster-meta-schema](ADR-0496-ADR3-cluster-meta-schema.md) | ADR-3 | Cluster metadata persists in dedicated redb tables behind the fenced write entry | Proposed |
