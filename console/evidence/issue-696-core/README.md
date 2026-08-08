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
| Authority | Opaque current-authority receipt, stable principal/contract-generation/digest/issuer identity, exact receipt binding, TOCTOU revalidation and developer/QA/release/customer separation |
| Currentness | Exact candidate, run, assignment, invocation, strict plan/fixture/DataControl/independent full-tuple source/result graph, gate, manifest, full release reference, delivery, acceptance and closeout bindings |
| Durability | Test-only redb fixture behind separate #732 aggregate/append and #733 publication-state contracts; no productive store claim |
| Recovery | Test-fixture restart readback for candidate, QA lineage, publication, rollback and closeout |
| Effects | Distinct canonical #694 workbench and #710 delivery-effect saga readiness, authority-renewal-stable but lineage-sensitive operations/receipts, complete workbench ownership/cleanup evidence, exact rollback source+target receipts and durable deterministic fakes; productive #694/#695/#710 adapters remain deferred |
| Console | Unreachable public-DTO scaffold with fail-closed validation; no product-surface or browser-security claim |

## Acceptance boundary

AC-1 and dependency-independent portions of AC-2, AC-4, AC-5, AC-6, AC-7,
AC-8, AC-10, AC-12, AC-15, AC-17, AC-18, and AC-20 are implemented. AC-14 is
PARTIAL: plan, dataset, run, case, deterministic, flake, and gate core are
implemented, while model/calibration authority and productive import remain
#749/dependency deferred.
AC-3 productive execution, AC-6 authenticated API/browser, AC-7 productive #695
work-item creation, AC-9 product Console, AC-10 NMDA publication, AC-11 Gaia,
AC-13 `.240`, AC-16 productive capability enforcement, AC-19 recovery
integration, and all productive CQRS/effect claims remain open.

No screenshot alone is claimed as AC-N5 evidence. No build-server timing is
reported as benchmark evidence.

Every PASS case, required or optional, needs passing deterministic assertion evidence. Any populated
model record or grader reference is typed unavailable before its verdict is
evaluated; deterministic gates require absent model and calibration bindings
until #749 provides both authorities together. Focused negative coverage
includes zero plan inputs, same-ID fixture substitution, empty, malformed,
exactly duplicated, or per-inventory/graph-wide locator-conflicting source
tuples, relabeled/missing case slices, duplicate results for one required case,
any model evidence, fake aggregation-as-calibration, swapped/stale saga
contracts, incomplete workbench receipt schema/ownership/cleanup bindings,
unresolved or malformed finding evidence, missing/stale/wrong-owner flake
dispositions, and inconsistent retry declarations. Exact source reuse across
separate inventories and explicit no-retry/no-seed plans are valid. A technical
PASS may be recorded with review approval withheld; every unresolved finding
blocks approval, gate passage, and promotion.

## Focused correction-test inventory

The current source contains focused positive and negative tests for:

- receipt renewal stability and contract/principal/issuer lineage changes on
  both workbench and delivery-effect request identities;
- cross-run and cross-authority-lineage workbench receipt replay;
- wrong workbench schema, zero artifact ownership, and malformed or zero-digest
  cleanup references;
- exact effect-receipt authority identity and durable-saga conflict on lineage
  change;
- malformed, duplicate, locator-conflicting, unresolved, and falsely approved
  finding evidence through gate and promotion denial;
- required and optional PASS results without deterministic assertions;
- missing, expired, malformed, wrong-owner, and cross-tenant flake dispositions;
  and
- the previously accepted model-blocking, fixture/source, slice, retry/seed,
  distinct-saga, restart, idempotency, and publication-receipt matrices.

## Current local checks

The following results were observed on 2026-08-08 against committed base
`7e42e7700dc6e22264a4f56813fba8060ede0111` plus the exact seven-file local
correction diff:

```text
git diff --check
  PASS
typos <exact seven-file scope>
  PASS
ASCII and public secret-pattern scans over the exact seven-file scope
  PASS (no matches)
exact changed-file scope comparison
  PASS (seven owned files)
npm test -- --run tests/delivery-view.test.ts
  NOT RUN: local Console dependencies are absent (`vitest: not found`)
npm run typecheck
  NOT RUN: local Console dependencies are absent (`tsc: not found`)
cargo remote
  NOT RUN on this correction diff: ORC authorized the sequential remote window
  only after the correction commit and one current-main merge
```

The missing local Console toolchain is an environment blocker, not a product
failure and not a passed gate. Dependencies were not installed or changed.
Earlier remote results and the currently green checks on Draft PR #772 belong
to its stale remote head and are not counted for this correction diff. Final
evidence replaces this section only after the authorized current-main
integration, focused correction tests, complete remote matrix, Console gates,
push, and exact-head CI.

## Final evidence

The exact final head, merge base, remote Rust results, full Console results, CI
checks, PR readback, and clean/ahead/behind state are recorded in the Draft PR
and REVIEW_READY handoff after final integration. This file intentionally does
not predict those results.
