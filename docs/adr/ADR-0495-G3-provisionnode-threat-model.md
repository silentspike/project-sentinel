# ADR-0495: ProvisionNode threat model (G3)

- **Gate:** G3 (blocks the #495 `ProvisionNode` ACs)
- **Status:** Proposed
- **Primary issue:** #495 (operator-approved self-provisioning node bootstrap)
- **Related issues / gates:** G-GENESIS (first seed), ADR-2 (SSH for bootstrap), V14
- **Supersedes / Superseded by:** —

> **N-node-native rule:** all schemas/messages/APIs MUST be `NodeId`-keyed, never a hard
> source/target pair. Two nodes are the first test, not the ceiling.

## Context

`ProvisionNode` lets a seed node absorb a bare VM shell into the cluster (provision the
binary, render config, install certs/token-gates, start the daemon). This onboarding is
an attack/error vector: a free `target_host` would make it a generic remote-install
tool, a blind host-key accept would be a first-contact MITM, and copying secrets would
leak tokens. `OperatorCommand` (`crates/sentinel-common/src/types.rs:272`) has no
provisioning variant today, so this is all new.

## Problem

How is node provisioning constrained so it cannot be turned into a remote-exec
primitive, MITM'd on first contact, or used to copy secrets?

## Decision

**Allowlist-only targets, out-of-band host-key pinning, target-local private keys,
repo-templated token-gates, role separation, short-lived bootstrap credential.**

- **Pending-target allowlist (V14):** the command is `ProvisionNode { pending_target_id,
  requested_alias, idempotency_key }` — **no free `target_host`/`target_user`**. The
  host comes from a previously, read-only-captured `PendingBareNode { target_ip,
  expected_host_key, expected_image_id, expected_hostname, expected_machine_id,
  expires_at }` registry.
- **Host-key out-of-band (exact source):** the bare VM's SSH host key is read via the
  **Proxmox guest agent** (a defined trusted out-of-band channel), not delivered
  unauthenticated by cloud-init. **Precondition:** `qemu-guest-agent` is installed +
  active in the bare image and the Proxmox channel is declared trusted out-of-band —
  otherwise AC-S1 is not executable. The seed pins the key.
- **Pinned certs, never key copy:** the verified target daemon binary generates or
  loads its self-signed QUIC identity **on the target** and returns only the public
  certificate fingerprint. The seed binds that fingerprint one-to-one to the assigned
  `NodeId` in its durable dynamic-peer registry; the target config pins the seed's
  `NodeId`, address, and fingerprint. **Node private keys are never scp'd.** Track A
  deliberately uses explicit pinned trust; CA issuance/rotation remains Track D2/H.
- **No secret transfer:** binary + config + systemd + token gates go over; the private
  key and self-signed certificate are created on the target. **No LLM API keys / `.env`
  / secrets** are transferred. `/etc/sentinel/allow-llm` is deliberately absent so
  gateway/judge stay condition-blocked.
- **Token-gate drop-ins from a repo template (single variant):** the gateway/judge/
  health-monitor `ConditionPathExists=/etc/sentinel/allow-llm` drop-ins are **VM-side
  drift, not in the repo** (`deploy/systemd/sentinel-gateway.service` has no such
  condition). `ProvisionNode` renders them from a repo-versioned template
  `deploy/templates/token-gate-dropin.service.conf` (created by Phase 0/#495) + checksum
  verify + install — **one variant**, no "synthesize vs. repo" fork.
- **Role separation:** `provision-node` ≠ `migrate-container` ≠ `cas-pull`. A node cert
  may CAS-pull, not ProvisionNode.
- **Bootstrap credential lifecycle (AC-S6):** cloud-init injects the seed pubkey + a
  minimal sudo scope (the seed runs as `ubuntu`, not root); the seed installs
  privileged via that cred; lifecycle = short-lived bootstrap cred → node cert →
  bootstrap cred revoked (no permanent seed-root access).
- **NTP / single-site assumption** for `expires_at` and cert validity windows.

## Non-Goals

- The first-seed Genesis (G-GENESIS).
- CA rotation / revocation / quarantine infra (Track D2/H) — Track A is pinned-trust
  without lifecycle.
- Multi-site clock-skew handling (Track I).

## Data Types

`OperatorCommand::ProvisionNode { pending_target_id, requested_alias, idempotency_key }`,
`PendingBareNode { … }`, `ProvisionOp` saga (V5, persisted in ADR-3 `PROVISION_OPS`),
`NodeIdentity`/`NodeLifecycleState` (G-GENESIS).

## State Machine / Protocol

`ProvisionOp`: `Idle -> VerifyTarget -> PinHostKey -> PushBinary(sha256) ->
IssueCert(target-local key) -> RenderConfig+TokenGates+reciprocal peer pins ->
StartDaemon -> AwaitHealth -> ObserveAuthenticatedJoin -> NodeProvisioned`.
Before SSH mutation, the seed atomically writes and fsyncs the `ProvisionOp`, including
its assigned `NodeId`, to `<data_dir>/provision-ops.json`. A retry after seed restart
must present the same idempotency key, pending target, and alias; it reuses that
operation and NodeId. A completed operation is a durable no-op. Any attempted key,
target, or alias rebinding is rejected.

`ObserveAuthenticatedJoin` polls the seed's receiver-local membership view. A running
systemd unit is necessary but insufficient: completion requires an accepted heartbeat
whose payload `node_id` equals the NodeId bound to the presented certificate. Timeout
stops the target service and revokes the seed-side dynamic peer entry.

## Failure Modes

- **Half-deploy (AC-S4/B6):** every fallible bootstrap fence persists `Failed`, disables
  the target daemon, removes staging files, revokes the seed-side peer, and writes a
  mode-0600 target quarantine marker. Cleanup failure is retained in the returned
  error; the seed-side failed operation remains durable for an explicit convergent
  retry with the same identity.
  A retry keeps the target marker until the exact reserved NodeId rejoins authenticated
  membership; marker removal must succeed before the operation becomes `Completed`.
  Tested with real `qm snapshot`/`qm rollback` of the fresh test VM (S9). `qm rollback`
  is the **provision drill only**, never migration-failure recovery.
- **Host-key mismatch:** target rejected (pinned key ≠ presented key).
- **NodeId/certificate collision (AC-S5):** the dynamic peer registry rejects a
  certificate already bound to another NodeId and a NodeId already bound to another
  certificate. A heartbeat claiming a NodeId other than its authenticated binding is
  typed-rejected. Removing a failed peer also closes every already authenticated
  control and block-pull QUIC connection under that binding; deleting only the durable
  registry row is not sufficient revocation.
- **NodeId collision (AC-S5):** `ProvisionNode` with an already-assigned `NodeId` ->
  reject (no split identity).
- **Re-run (AC-S2/B5):** idempotent across a seed restart: a completed run is a no-op;
  an incomplete/failed run reuses the persisted operation and NodeId (no split target
  identity or second membership entry).

## Tests

AC-S1..S6 + AC-B1..B7 on a real second VM: target starts without Sentinel (B1);
allowlist-only (B2); seed deploys binary/config/systemd/certs/token-gates (B3); target
self-registers through an authenticated heartbeat observed by the seed (B4);
idempotent re-run, including journal reopen (B5/S2); failed bootstrap at early and
late fences -> durable failed/quarantine record and no alive node
(B6/S4); no secrets/token (B7/S3); host-key pinned (S1); cred lifecycle (S6); NodeId
collision rejected (S5).

## Benchmarks

Bootstrap time (bare VM → joined) + the #495 network baseline. Register: under
`infra-vm2-network-baseline + node-bootstrap (#495)`.

## Backward Compatibility

Additive new command + tables; existing single-node deploy unaffected. The repo
token-gate template is new (no existing unit changed).

## Security

This whole ADR **is** the security boundary for onboarding. Single trust domain (Track
A); general certificate lifecycle is Track D2/H. Provision failure quarantine is a
bounded bootstrap compensation, not general cluster quarantine infrastructure. No
secret transfer; allowlist; out-of-band host-key; target-local private key;
certificate fingerprint bound to the assigned NodeId.

## Public Claim Boundary

- May claim after #495 AC-B/AC-S green on a real VM: operator-approved self-provisioning
  from a bare shell, with the listed safety properties.
- **May NOT claim:** it is live before those ACs are green; the TOGAF SSOT entry follows
  the live proof (main session).

## Open Follow-ups

- Cert rotation/revocation/quarantine and CA lifecycle (Track D2/H). Track A uses
  explicit self-signed certificate pins.
