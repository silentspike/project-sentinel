# Issue #696 dependency-independent core evidence

This directory contains public-safe evidence for the dependency-independent
delivery core. It is not live acceptance evidence and does not authorize
deployment.

## Scope

- Runtime target class: `SINGLE_NODE`.
- Deploy, read-only, and benchmark targets for this phase: none.
- No VM, Proxmox, provider, customer-delivery, or runtime mutation occurred.
- Productive #694 workbench and #695 workflow/memory adapters remain gated.
- The Console view is an isolated public-DTO scaffold; it is neither reachable
  from `App` nor connected to an authenticated API route.

## Implemented proof surface

| Area | Dependency-independent evidence |
| --- | --- |
| Schemas | Unknown-field-rejecting, domain-separated versioned QA, candidate, review, finding, approval, manifest, release, delivery, feedback, acceptance, rollback and closeout records |
| State | Legal candidate, QA-run, release, and delivery transitions reject shortcuts and terminal reopen |
| Authority | Opaque current-authority receipt, exact principal/role/generation checks, TOCTOU revalidation and developer/QA/release/customer separation |
| Currentness | Exact candidate, run, assignment, invocation, evidence graph, gate, manifest, release, delivery, acceptance and closeout bindings |
| Durability | Test-only redb fixture behind separate #732 aggregate/append and #733 publication-state contracts; no productive store claim |
| Recovery | Test-fixture restart readback for candidate, QA lineage, publication, rollback and closeout |
| Effects | Stable QA/effect requests and sealed deterministic fakes; productive #694/#695/effect adapters remain deferred |
| Console | Unreachable public-DTO scaffold with fail-closed validation; no product-surface or browser-security claim |

## Acceptance boundary

AC-1 and dependency-independent portions of AC-2, AC-4, AC-5, AC-6, AC-7,
AC-8, AC-10, AC-12, AC-14, AC-15, AC-17, AC-18, and AC-20 are implemented.
AC-3 productive execution, AC-6 authenticated API/browser, AC-7 productive #695
work-item creation, AC-9 product Console, AC-10 NMDA publication, AC-11 Gaia,
AC-13 `.240`, AC-16 productive capability enforcement, AC-19 recovery
integration, and all productive CQRS/effect claims remain open.

No screenshot alone is claimed as AC-N5 evidence. No build-server timing is
reported as benchmark evidence.

## Current correction checks

The following focused results were observed before the required final
`origin/main` merge:

```text
remote compile-only focused delivery_core
  PASS
remote delivery_core
  PASS 20/20 before final receipt and crash-boundary fixture additions
remote digest golden-vector probe
  correctly found one stale expected vector; reproduced value applied, rerun pending
```

Earlier quota and fixture compile failures are historical diagnostics and are not
counted as passed gates. Final evidence replaces this section only after the
current main merge and the complete exact-head matrix.

## Final evidence

The exact final head, merge base, remote Rust results, full Console results, CI
checks, PR readback, and clean/ahead/behind state are recorded in the Draft PR
and REVIEW_READY handoff after final integration. This file intentionally does
not predict those results.
