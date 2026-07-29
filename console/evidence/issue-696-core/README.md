# Issue #696 dependency-independent core evidence

This directory contains public-safe evidence for the dependency-independent
delivery core. It is not live acceptance evidence and does not authorize
deployment.

## Scope

- Runtime target class: `SINGLE_NODE`.
- Deploy, read-only, and benchmark targets for this phase: none.
- No VM, Proxmox, provider, customer-delivery, or runtime mutation occurred.
- Productive #694 workbench and #695 workflow/memory adapters remain gated.
- The Console view is a disjoint injected-snapshot surface; it is not connected
  to an authenticated API route in this phase.

## Implemented proof surface

| Area | Dependency-independent evidence |
| --- | --- |
| Schemas | Versioned QA, candidate, review, finding, approval, manifest, release, delivery, feedback, acceptance, rollback, closeout, recovery-publication records |
| State | Legal candidate, QA-run, release, and delivery transitions reject shortcuts and terminal reopen |
| Authority | Tenant/principal/role checks and developer/QA/release/customer separation |
| Currentness | Exact candidate, plan, evidence, gate, manifest, release, delivery, acceptance, and closeout bindings |
| Durability | redb aggregate, journal, namespaced idempotency and digest-bound outbox in one local commit |
| Recovery | Restart readback for candidate, QA lineage, outbox, rollback, and closeout |
| Effects | Stable QA request and deterministic fake; productive #694 exactly-once adapter remains deferred |
| Console | Fail-closed lineage validation, shortened digests, cost rendering, and public redaction |

## Acceptance boundary

AC-1, the dependency-independent portions of AC-2, AC-4, AC-5, AC-6, AC-8,
AC-9, AC-10, AC-12, and the schema/contract portions of AC-14 through AC-20 are
implemented. AC-3 productive execution, AC-7 authoritative work-item creation,
AC-9 authenticated API/live browser proof, AC-10 NMDA publication, AC-11 Gaia
observation, AC-13 `.240` acceptance, and all productive adapter/effect/recovery
claims remain open.

No screenshot alone is claimed as AC-N5 evidence. No build-server timing is
reported as benchmark evidence.

## Pre-integration checks

The following checks passed before the required final `origin/main` merge:

```text
bun run test -- tests/delivery-view.test.ts
  6 passed
bun run test
  76 passed before the sixth delivery-specific adapter-readiness case was added
bun run typecheck
  PASS
bun run build
  PASS
cargo remote -c .rustc_info.json -- fmt --all -- --check
  PASS before the final negative-test additions
cargo remote -c .rustc_info.json -- check -p sentinel-daemon --lib --tests
  PASS before the final negative-test additions
cargo remote -c .rustc_info.json -- test -p sentinel-daemon --test delivery_core
  10 passed before the final negative-test additions
```

One later focused Rust attempt ended with the remote builder error
`Disk quota exceeded (os error 122)` and is not a passed gate. After scoped
cache cleanup, the retry reached two test-fixture ownership compile errors and
is also not a passed gate. Both must be corrected and the canonical remote Rust
matrix rerun on the final integrated head.

## Final evidence

The exact final head, merge base, remote Rust results, full Console results, CI
checks, PR readback, and clean/ahead/behind state are recorded in the Draft PR
and REVIEW_READY handoff after final integration. This file intentionally does
not predict those results.
