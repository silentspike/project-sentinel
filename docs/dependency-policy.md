# Dependency Intervention and Ownership Policy

Status: normative

This policy governs how Project Sentinel configures, retains, wraps, patches,
temporarily forks, or replaces open source dependencies. It is the implementation
contract for the ownership decisions produced by
[#705](https://github.com/silentspike/project-sentinel/issues/705), based on the
reachability evidence from
[#631](https://github.com/silentspike/project-sentinel/issues/631).

Related contracts:

- [#617](https://github.com/silentspike/project-sentinel/issues/617) governs the
  safe-Rust and unsafe-boundary audit.
- [#621](https://github.com/silentspike/project-sentinel/issues/621) is the parent
  dependency-governance epic.
- [#656](https://github.com/silentspike/project-sentinel/issues/656) consumes this
  policy for agent-operated Cargo, Go, and npm upgrades.
- [The patch registry](dependency-patches.md) records every active Cargo patch,
  source replacement, and temporary fork.

## 1. Authority and Goals

The component owner owns the decision. Automation and LLM-assisted analysis may
collect evidence, implement an approved change, and run gates. They may not assign
an ownership decision, create a fork, replace a dependency, waive a security
obligation, or remove the rollback path without an approved issue.

The objective is the smallest total dependency and ownership surface that preserves
or improves correctness, security, deterministic behavior, compatibility, incident
response, and delivery speed. A smaller package count is not an objective by itself.
Sentinel-owned code is an owned dependency: Project Sentinel assumes every defect,
advisory response, compatibility obligation, and future update.

The following are never sufficient evidence:

- dependency count or lockfile membership;
- estimated or generated line count;
- the ability of an LLM to generate an implementation;
- successful compilation;
- a unit test without compatibility and failure evidence;
- build-server duration as a performance measurement.

## 2. Required Evidence Before Intervention

Every proposed intervention starts with one approved implementation issue and the
following pinned evidence:

1. The exact repository revision and the applicable #631 and #705 rows.
2. Release roots, enabled features, direct and transitive reachability, and the exact
   public API surface Sentinel uses.
3. The reproducible functional, security, reliability, or target-runtime baseline
   that motivates the change.
4. Unsafe, FFI, cryptographic, identity, authority, kernel, wire-format,
   storage-format, and failure-semantics impact.
5. Upstream version or commit, maintenance state, advisory state, and the complete
   transitive closure affected by the intervention.
6. A named component owner, reviewers, review triggers, rollback owner, and incident
   response responsibility.
7. Required conformance, property, fuzz, model, failure-injection, security,
   migration, observability, runtime, and benchmark gates.
8. An exit condition that is observable and can be checked in CI or during a
   scheduled review.

Performance claims require an issue-specific workload on the authorized target
hardware. Cargo build time on a shared build server is never benchmark evidence.

## 3. Intervention Ladder

Use the lowest stage that can satisfy the approved objective.

| Stage | Name | Allowed action | Entry condition | Exit condition |
| --- | --- | --- | --- | --- |
| 0 | Configure | Prune features, remove unused declarations, or align compatible versions. | Reachability and feature evidence proves the change is lossless. | Normal build, test, security, and target acceptance remain green. |
| 1 | Build | Change profile, LTO, codegen, linker, or platform choices. | The release-profile epic owns the hypothesis and target baseline. | Reproducible release artifact and target-hardware acceptance satisfy that epic. |
| 2 | Integrate | Add a Sentinel wrapper, carry a bounded upstream-first patch, or use an exceptional temporary fork. | Stage 0 and Stage 1 cannot satisfy the requirement; exact API and failure boundaries are known. | Upstream absorbs the change, the wrapper remains justified, or the temporary fork is removed before expiry. |
| 3 | Own | Add a minimal or strategic in-repository implementation. | An approved #705 decision and dedicated implementation issue prove that ownership has lower total risk and cost. | Replacement lifecycle, target-runtime acceptance, rollback proof, and old-dependency removal criteria are complete. |

Skipping a stage requires the implementation issue to explain why every lower stage
is insufficient.

## 4. Ownership Decision Taxonomy

Every #705 row has exactly one decision. The decision is not implementation
authorization; actionable decisions require a separate approved issue.

| Decision | Entry criteria | Mandatory proof and ownership | Exit or review condition |
| --- | --- | --- | --- |
| `REMOVE_UNUSED` | The declaration or package has no required repository or release-root contract. | Reachability proof, source/import search, feature comparison, lockfile delta, and component-owner review. | All affected gates pass and the package is absent from the intended graph. |
| `USE_STD` | Stable standard-library behavior completely covers the used API and semantics. | Conformance tests for edge cases, platform support, error semantics, and a smaller total owned surface. | Old dependency is absent after target acceptance; review on MSRV or semantics change. |
| `KEEP` | Upstream remains the best correctness, security, compatibility, and maintenance boundary. | Used API/features, upstream health, advisory ownership, update gates, and any format/security obligations are recorded. | Reopen when a review trigger changes the total-cost decision. |
| `WRAP` | Upstream remains valuable but its churn, authority, failure semantics, or data model must not leak into Sentinel. | Minimal Sentinel-owned interface, contract tests, error mapping, observability, and no unreviewed bypass. | Review when the used API, wrapper leakage, or upstream contract materially changes. |
| `PATCH_UPSTREAM` | A bounded defect or missing capability is reproducible and cannot be handled safely by configuration or a wrapper alone. | Upstream issue or PR first, minimal diff, conformance and regression tests, patch registry row, named owner, expiry, advisory mapping, and rollback. | Remove the patch after an accepted upstream release or at the registry deadline. |
| `FORK_TEMPORARY` | A production-critical or security-critical need cannot wait for upstream and no safer bounded option exists. | Incident or implementation issue, exact upstream basis, minimal diff, upstream tracking link, full security review, active registry row, hard expiry, owner, migration, and rollback. | Upstream convergence, replacement, or removal before the expiry date; renewal requires a new approval. |
| `OWN_MINIMAL` | Sentinel uses a narrow, stable surface that can be owned with lower total risk than the dependency. | Sentinel-owned boundary, conformance harness, property/fuzz tests where applicable, failure injection, security review, compatibility and migration proof, observability, target acceptance, rollback, and maintainer ownership. | Remove the old dependency only after all replacement gates pass; review on surface growth. |
| `OWN_STRATEGIC` | The mechanism is core product differentiation and Sentinel-specific semantics justify long-term ownership. | Everything required for `OWN_MINIMAL`, plus architecture approval, capacity plan, model or state-machine evidence, operational SLOs, incident playbook, and sustained ownership budget. | Continuous ownership review; removal of upstream occurs only after full cutover and rollback proof. |

### 4.1 Decision Constraints

- `REMOVE_UNUSED` and `USE_STD` must reduce total ownership cost, not just package
  count.
- `PATCH_UPSTREAM` must be upstream-first. A local patch without an upstream tracking
  link cannot become active.
- `FORK_TEMPORARY` is an exception, never a default upgrade strategy.
- `OWN_MINIMAL` cannot grow beyond its approved used surface without reopening #705.
- `OWN_STRATEGIC` requires architecture and component-owner approval.
- Uncertainty resolves to `KEEP` or a typed investigation, not a speculative rewrite.

## 5. Safe Replacement Lifecycle

Every `OWN_MINIMAL` or `OWN_STRATEGIC` implementation, and every stateful `USE_STD`
replacement, follows this order:

1. **Sentinel-owned boundary.** Isolate the old dependency behind the smallest
   interface that expresses Sentinel semantics. Direct bypasses are removed or
   fail-closed in CI.
2. **Conformance harness.** Freeze the required behavior, error model, ordering,
   determinism, resource limits, and compatibility vectors against the old
   implementation.
3. **Replacement implementation.** Implement only the approved surface with explicit
   security, failure, observability, and maintenance ownership.
4. **Shadow, A/B, or dual-read.** Compare results without granting the replacement
   sole authority. If this mode is not applicable, the issue records why and uses an
   equivalent offline or replay harness.
5. **Format or wire migration.** Version formats, preserve downgrade or export paths,
   test mixed-version behavior, and prove crash-safe recovery. No silent in-place
   reinterpretation is allowed.
6. **Target-runtime acceptance.** Run functional, failure, security, soak, and
   issue-specific performance gates on the authorized runtime target with required
   sidecars.
7. **Rollback proof.** Demonstrate the exact switch-back, data compatibility, and
   restoration path before changing authority.
8. **Old-dependency removal.** Remove the old dependency only after all acceptance
   criteria pass, rollback remains available for the approved window, and the owner
   accepts the new advisory and maintenance burden.

No step may infer compatibility from semantic versioning, compilation, or generated
code alone.

## 6. Proof Obligations for Owned Code

The implementation issue selects applicable gates and explains every omission.

| Obligation | Required evidence |
| --- | --- |
| Conformance | Golden vectors and differential tests against the old boundary. |
| Property, fuzz, or model | Invariants, state transitions, parsers, and malformed inputs exercised at the right abstraction. |
| Failure injection | I/O errors, partial writes, timeouts, cancellation, exhaustion, restart, and recovery where applicable. |
| Security | Threat model, unsafe/FFI review, authority checks, dependency and advisory mapping, and hostile input tests. |
| Compatibility | API, wire, storage, ordering, determinism, and mixed-version behavior. |
| Migration | Versioned forward path, restart safety, validation, and recovery from interruption. |
| Observability | Metrics and logs that distinguish old, shadow, replacement, fallback, and rollback paths. |
| Runtime | Acceptance on the target class named by the implementation issue. |
| Rollback | Rehearsed command or control path plus data compatibility proof. |
| Maintenance | Named owner, review cadence, advisory response, update process, and incident playbook. |

## 7. Security and High-Risk Defaults

Cryptography, TLS, QUIC internals, compression, hashing, language or runtime engines,
storage engines, kernel interfaces, and security boundaries default to `KEEP` or
`WRAP`. A different decision requires a separate bounded issue with domain review and
all applicable replacement obligations.

The default applies even when the used API looks small. These components carry hidden
interoperability, side-channel, crash-consistency, protocol, unsafe, or adversarial
input obligations. A compile-only implementation is not evidence.

Any in-house replacement is tracked like an external dependency:

- advisories and upstream developments are monitored;
- compatibility and format versions are explicit;
- owners and review triggers are recorded;
- SBOM and provenance identify the owned component;
- update and incident obligations remain active after the old package is removed.

## 8. Patch and Temporary-Fork Rules

Every active Cargo patch, `[replace]` entry, or Cargo source replacement must have one
exact row in [docs/dependency-patches.md](dependency-patches.md). CI compares the
declared override key, manifest, package, and normalized source in both directions.

An active row must contain the upstream basis, bounded diff size, reason and evidence,
upstream issue or PR, owner, status, expiry, advisory mapping, revisit condition, and
rollback. The gate fails on:

- an unregistered override;
- a registry row with no active override;
- a missing or mismatched field;
- an expired patch or temporary fork.

Historical rows do not stay in the active registry. Their removal commit and upstream
or rollback outcome remain in Git history and the implementation issue.

Ordinary dependencies sourced directly from an official upstream Git repository are
not patch/fork rows, but every declaration must match the bidirectional direct-Git
allowlist in the patch registry. New, removed, or changed Git sources fail closed.
They remain dependencies governed by #705 and #656. A fork must be represented
through a recognized override mechanism; changing a dependency URL to hide a fork is
prohibited.

## 9. Upgrade and Renovate Playbook

Renovate and other update agents must classify the ownership mode before selecting
gates. Critical classes never blind-auto-merge.

| Ownership mode | Upgrade behavior |
| --- | --- |
| `KEEP` | Compare version, features, API, advisories, licenses, transitive graph, unsafe/FFI, and formats. Run contract-selected gates. |
| `WRAP` | Do the `KEEP` checks plus wrapper conformance and bypass checks. Reopen #705 if the wrapper surface or failure mapping changes. |
| `PATCH_UPSTREAM` | Rebase the minimal patch onto the new upstream basis, rerun conformance/security gates, update registry source and expiry, and check whether the upstream release makes the patch removable. No auto-merge. |
| `FORK_TEMPORARY` | Block automatic upgrade. Owner reviews upstream divergence, advisories, diff growth, expiry, migration, and rollback. Renewal requires explicit approval. |
| `OWN_MINIMAL` | Track changes in the former upstream behavior and the owned surface. Reopen the decision if compatibility obligations or surface area expand. |
| `OWN_STRATEGIC` | Use the owned component release process, architecture review triggers, security gates, and provenance. Upstream changes are intelligence, not automatic code changes. |

For all modes:

1. Preserve the current ownership decision until a maintainer approves a change.
2. Reopen #705 when used API surface, format semantics, upstream health, advisory
   state, patch basis, closure size, or ownership cost materially changes.
3. Regenerate SBOM and provenance when source or ownership changes.
4. Include exact gates, migration impact, and rollback in the update PR.
5. Follow the cross-ecosystem `DependencyContract` in #656 when available.

### Duplicate-version resolution

Renovate and manual Cargo updates must run the Bans gate before merge. If an update
introduces a duplicate version, resolve it in the same PR by aligning the direct
requirement, feature set, or forcing upstream dependency. The update cannot be split
into a temporarily red intermediate merge.

If same-PR alignment is proven impossible, the component owner may add one exact
`crate@version` skip in that update PR. The adjacent comments must state the complete
forcing chain, why alignment is currently unsafe, an ISO expiry date, and the concrete
upstream release or graph-removal condition that removes the skip. Broad name-only
skips and `skip-tree` are prohibited. The PR must update the structural duplicate
count and cannot use blind auto-merge. At expiry, either remove the duplicate and skip
or obtain a new explicit maintainer decision with refreshed graph evidence.

Current Renovate defaults do not override this policy. A patch, fork, wrapper,
security boundary, format-bearing dependency, or owned replacement must not gain
blind auto-merge merely because its version change is minor or patch-level.

## 10. Worked Review Examples

These examples demonstrate the review method. They are not active #705 decisions and
do not authorize dependency changes.

### 10.1 Narrow Helper Candidate

Scenario: a helper dependency is used only to normalize one ASCII suffix before a
file is written.

1. Pin the #631 row and prove the exact call sites and platform behavior.
2. Compare the helper against stable standard-library path and string behavior,
   including empty input, invalid encoding policy, separators, and platform cases.
3. Assign `USE_STD` only if the standard library completely covers the frozen
   behavior and the owned code is materially smaller.
4. Add a Sentinel boundary and golden/property tests before implementation.
5. Run all release-root gates and target acceptance required by the issue.
6. Prove rollback by restoring the old adapter and preserving stored output.
7. Remove the dependency only after graph and target acceptance pass.

If behavior is ambiguous or the replacement grows beyond the frozen helper surface,
the decision is `KEEP`, not a speculative rewrite.

### 10.2 Stateful, Format-Bearing Dependency

Scenario: a storage engine is proposed for replacement while existing data must
survive restart, migration, and rollback.

1. Default to `KEEP` or `WRAP` and identify transactions, durability, locking,
   ordering, corruption handling, read-only access, and file-format obligations.
2. Put both engines behind a Sentinel-owned store interface.
3. Build conformance, model, crash, partial-write, corruption, and recovery tests.
4. Version the new format and implement resumable migration with validation.
5. Use dual-read or shadow comparison while the old engine retains authority.
6. Exercise mixed-version, interrupted migration, downgrade/export, and rollback.
7. Run target-runtime fault injection, soak, resource, security, and performance
   acceptance with system sidecars.
8. Transfer advisory, incident, and maintenance ownership before removing the old
   engine.

The old engine remains until data migration, target acceptance, and rollback are
proven. Package count, line count, LLM generation, and successful compilation have no
bearing on that decision.

## 11. Review and Enforcement

- Component owners review entries on their stated date and whenever a trigger fires.
- Security reviewers approve interventions at security, crypto, identity, kernel,
  protocol, storage, and unsafe/FFI boundaries.
- Architecture approval is required for `OWN_STRATEGIC`.
- CI runs `python3 scripts/check-patch-registry.py` and its negative tests in the
  always-running lint job.
- The PR that adds or changes an override must update the registry in the same commit.
- PR rollback is the default rollback for this policy and checker. Each future source
  intervention also records its own runtime and data rollback.
