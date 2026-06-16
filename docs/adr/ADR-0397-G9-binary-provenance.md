# ADR-0397: Binary provenance (G9)

- **Gate:** G9 (blocks Track-H GA; does **not** block Track A — sha256 manifest suffices there)
- **Status:** Proposed
- **Primary issue:** #397 (Cluster 12 epic) — Track H
- **Related issues / gates:** #494 (determinism profile), #495 (ProvisionNode binary push), V29
- **Supersedes / Superseded by:** —

> **N-node-native rule:** all schemas/messages/APIs MUST be `NodeId`-keyed, never a hard
> source/target pair. Two nodes are the first test, not the ceiling.

## Context

The deploy path already verifies binary integrity by **sha256**:
`deploy/deploy-preflight.sh` + `deploy/release-manifest.schema.json` (the manifest
carries `git_sha`, `sha256`). There is **no signature**. The determinism profile (#494,
DEV-010) requires the identical binary on all nodes; provisioning (#495) pushes a
sha256-verified binary.

## Problem

Is the existing sha256 manifest enough for cross-node binary trust, or is a signature
required, and when?

## Decision

**Track A uses the existing sha256 manifest. A signed release manifest (Ed25519) is a
Track-H/GA hardening, not a Track-A blocker.**

- Track A: `ProvisionNode` verifies the pushed binary against the existing
  sha256/`git_sha` manifest — sufficient inside one trusted cluster.
- Track H/GA: a signed release manifest (Ed25519 over the manifest) so a node can verify
  provenance, not just integrity; the signing key lifecycle is part of the GA security
  posture.

## Non-Goals

- Reproducible-build attestation beyond sha256/sig (out of scope unless a GA claim needs
  it).
- Per-node code signing / secure boot (host responsibility, not the platform).

## Data Types

Extend `release-manifest` with an optional signature field (`#[serde(default)]`), a
`RevocationSource` for signing-key revocation (couples Track D2/H).

## State Machine / Protocol

Build → sign manifest → publish; node verifies sha256 (Track A) and, at GA, the
signature before accepting a binary (ProvisionNode / rolling upgrade).

## Failure Modes

- **Tampered binary:** sha256 mismatch (Track A) / signature mismatch (GA) → reject.
- **Compromised signing key:** revocation generation (Track D2/H) → fail-closed.

## Tests

sha256 mismatch rejected (Track A, already testable); signature verify + revocation
(Track H).

## Benchmarks

`n/a` (integrity/provenance gate; verification cost negligible vs. deploy).

## Backward Compatibility

Signature is additive/optional; existing sha256 manifests keep working.

## Security

This ADR is a provenance boundary. Track A = sha256 integrity in a trusted domain; GA =
signed provenance + revocation.

## Public Claim Boundary

- May claim today: sha256-verified identical binary across nodes.
- **May NOT claim:** signed provenance / supply-chain attestation before Track H.

## Open Follow-ups

- Signing-key lifecycle + revocation (Track D2/H); rolling-upgrade version negotiation
  (V32, Track H).
