# ADR-0495: First-seed node bootstrap (G-GENESIS)

- **Gate:** G-GENESIS (top-level — precedes all `ProvisionNode` ACs)
- **Status:** Proposed
- **Primary issue:** #495 (operator-approved self-provisioning node bootstrap)
- **Related issues / gates:** G3 (ProvisionNode threat model), ADR-2 (transport), ADR-3 (cluster meta)
- **Supersedes / Superseded by:** —

> **N-node-native rule:** Even though the foundation is verified on a 2-node cluster
> first, all schemas, messages and APIs MUST be N-node-native (`NodeId`-keyed
> sets/maps, never a hard source/target pair as the cluster model). Two nodes are the
> first test, not the ceiling.

## Context

#495 introduces an **operator-approved self-provisioning node bootstrap from a bare
VM shell**: the operator creates only an empty Ubuntu VM shell, and an existing seed
node performs the full node bootstrap (verify against the allowlist, pin the host
identity, provision the sha256-verified binary, generate/install the node cert,
render config, write token-gated systemd units, start the daemon, observe the
membership join).

This bootstrap **runs on a seed node** — so the **first** node cannot come from
`ProvisionNode` (there is no seed yet). The codebase has no node-provisioning path
today: `OperatorCommand` (`crates/sentinel-common/src/types.rs:272`) has 10 variants
(`Chaos`, `RoomStimulus`, `Nightrun`, `Snapshot`, `Restore`, `Chat`, `Gaia`,
`Broadcast`, `Task`, `Dm`) and **none** is node/cluster related. Deployment today is a
manual flow: a sha256-verified binary (`deploy/deploy-preflight.sh`,
`deploy/release-manifest.schema.json`), `config/daemon.toml`, and systemd units.

## Problem

How does the **first** node of a cluster come into existence, given that the
self-provisioning bootstrap requires an already-running seed?

## Decision

**Genesis is the single permitted manual Sentinel deploy. Every subsequent node
(1..N) is created only via `ProvisionNode` from the seed.**

Genesis procedure for `test-node-0`:

1. Fresh bare VM (clean OS, **no production state**).
2. sha256-verified binary installed manually (the determinism profile of #494).
3. `daemon.toml` rendered: `node_id`, `cluster_id`, `seed = true`, **no LLM tokens**.
4. Token-gated systemd units written (gateway/judge/health-monitor blocked by
   `ConditionPathExists=/etc/sentinel/allow-llm`, which is absent by default).
5. Start the daemon.
6. Verify health, self-membership, binary hash, and `rustc` version.
7. Mark the node as `GenesisSeed`.

After Genesis, `node-1..N` join **only** via `ProvisionNode` (the seed absorbs a bare
shell). `ProvisionNode` runs **on** `test-node-0`.

**Hard rules:**

- VM 1069 (the production simulation) is **never** node-0 and is **never** part of any
  test cluster — it stays a read-only production reference, untouched.
- A wiped/fresh VM may become a node only after a clean wipe.
- Genesis happens **exactly once per `cluster_id`**.

## Non-Goals

- `ProvisionNode` itself (the bare-shell absorption) is specified by #495's
  `ProvisionNode` ACs and gated by G3 — G-GENESIS only fixes how the *first* node
  exists.
- HA promotion (voting membership) is Track D; a Genesis seed starts as the single
  chef/owner coordinator.

## Data Types

`NodeLifecycleState` gains `GenesisSeed` as a distinguished initial state.
`NodeIdentity { node_id, alias, cert_fingerprint, boot_id, incarnation }` (new, in
`sentinel-common`) is created at Genesis and persisted in `daemon.toml` + redb cluster
meta (ADR-3). `cluster_id` is fixed at Genesis.

## State Machine / Protocol

`(bare VM) → manual install → daemon start → health+self-membership verified →
GenesisSeed`. No network handshake is involved for node-0 (it is the origin of the
cluster).

## Failure Modes

- **Genesis attempted on a node that already has cluster state / a `cluster_id`:**
  rejected — Genesis is not repeatable over an existing node (AC-GEN-5).
- **Genesis on a VM with production state:** forbidden — node-0 must be a fresh OS
  (AC-GEN-1); VM 1069 is explicitly excluded.
- **Two Genesis runs for the same `cluster_id`:** the second is rejected (exactly-once
  per cluster).

## Tests

AC-GEN-1..5, verified on a fresh test VM:

1. node-0 comes up from a fresh OS with no production state.
2. node-0 has a `NodeIdentity` / cert / `cluster_id`.
3. node-0 can produce a `PendingBareNode` allowlist entry.
4. node-0 provisions `test-node-1` via `ProvisionNode`.
5. Genesis cannot be repeated over an existing node.

## Benchmarks

Genesis time (bare VM → `GenesisSeed`) recorded as part of #495's bootstrap-time
baseline in the internal register `/work/company/BENCHMARK-REGISTER.md`. Not a tuning
loop — a baseline.

## Backward Compatibility

No impact on existing single-node deployment; the current manual deploy *is* the
Genesis path, formalized. Existing VM 1069 is untouched and is never a cluster member.

## Security

- node-0 carries **no** LLM tokens; `/etc/sentinel/allow-llm` is absent by default so
  gateway/judge stay condition-blocked (token-bleed protection, same mechanism as the
  production VM hardening).
- The node cert/identity is generated at Genesis; the bootstrap credential lifecycle
  for *provisioned* nodes is specified in G3 (short-lived bootstrap cred → node cert →
  bootstrap cred revoked).

## Public Claim Boundary

- May claim today: the Genesis procedure is decided (single manual deploy, then
  ProvisionNode-only).
- **May NOT claim:** that the bootstrap is live — that requires #495 AC-B1..B7 and
  AC-GEN-1..5 green on a real second VM. Until then this is target architecture; the
  TOGAF SSOT entry follows the live proof (main session).

## Open Follow-ups

- `ProvisionNode` threat model and bootstrap credential lifecycle (G3 / #495).
- Promotion of a provisioned node to voting member (Track D, `non_voting` learner
  first).
