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

**Allowlist-only targets, out-of-band host-key pinning, CSR-not-key-copy certs,
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
- **Certs via CSR, never key copy:** the target generates its **private key locally**
  and sends only a CSR/public key to the seed; the seed signs and returns only the
  cert/CA/config. **Node private keys are never scp'd.**
- **No secret transfer:** binary + config + systemd + cert go over; **no LLM API keys /
  `.env` / secrets**. `/etc/sentinel/allow-llm` is deliberately absent so gateway/judge
  stay condition-blocked.
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

`ProvisionOp`: `Idle → VerifyTarget → PinHostKey → PushBinary(sha256) → IssueCert(CSR)
→ RenderConfig+TokenGates → StartDaemon → AwaitHealth → ObserveJoin → NodeProvisioned`
(each step idempotent + recoverable; chef/target restart mid-bootstrap reconciles).

## Failure Modes

- **Half-deploy (AC-S4/B6):** error mid-bootstrap → defined cleanup (service stopped,
  partial artifacts removed, node NOT in membership) or `Quarantined/ProvisionFailed`.
  Tested with real `qm snapshot`/`qm rollback` of the fresh test VM (S9). `qm rollback`
  is the **provision drill only**, never migration-failure recovery.
- **Host-key mismatch:** target rejected (pinned key ≠ presented key).
- **NodeId collision (AC-S5):** `ProvisionNode` with an already-assigned `NodeId` →
  reject (no split identity).
- **Re-run (AC-S2/B5):** idempotent — second run is a no-op/convergent (no double
  deploy, no second membership entry).

## Tests

AC-S1..S6 + AC-B1..B7 on a real second VM: target starts without Sentinel (B1);
allowlist-only (B2); seed deploys binary/config/systemd/certs/token-gates (B3); target
self-registers (B4); idempotent re-run (B5/S2); failed bootstrap → no alive node
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
A); revocation/quarantine are Track D2/H. No secret transfer; CSR-not-key; allowlist;
out-of-band host-key.

## Public Claim Boundary

- May claim after #495 AC-B/AC-S green on a real VM: operator-approved self-provisioning
  from a bare shell, with the listed safety properties.
- **May NOT claim:** it is live before those ACs are green; the TOGAF SSOT entry follows
  the live proof (main session).

## Open Follow-ups

- Cert rotation/revocation/quarantine (Track D2/H); CA lifecycle.
