# Issue #695 dependency-independent core evidence

Date: 2026-07-22

## Revision and boundary

- Branch: `feat/issue-695-company-workflow-core`
- Branch point: `13e87b663cc3b47223a2b0052db1cc6c274e66c8`
- Current `origin/main` during verification: `9d8bb2fc9cca1140867aff20280df7fb54b0a6f2`
- Runtime access: not performed
- Deployment: not performed
- Provider calls and spend: none
- Benchmarks: not run; issue benchmarks remain reserved for the separately authorized live phase
- Deferred dependency: the real `WorkExecutionPort` and `CompletionEvidencePort` adapters wait for #472 -> #701 -> #694
- Deferred acceptance: AC-12 and the live portions of AC-5, AC-6, AC-9, AC-10, and AC-11 wait for the dependency merge and explicit live authorization

## Implemented correction set

1. `web-project-v1` is loaded server-side from an exact-byte verified deployment file and an embedded release copy. Its ID, version, and SHA-256 digest are proposal-digest-bound and copied unchanged into Agreement and Project. The graph is checked against canonical roles, worker specialties, immutable artifacts, independent gates, and minimum topology.
2. Agents can request completion but cannot submit trusted output references or gate assertions. A sealed `CompletionEvidencePort` receipt proves the invocation, assignment, input digest, output bundle, artifact ownership, and independent profile-declared gate before completion.
3. Organization authority drift produces a typed durable `AuthorityConflict`, completes the affected outbox row, blocks the item for reassignment, and permits independent rows to continue. Authorized resolution deactivates the stale assignment without rearming the stale invocation.
4. Schema v1 to v2 migration executes in one immediate transaction, writes the version marker last, rolls back every destructive step on an injected failure, and retries deterministically from the intact v1 image.
5. `RecordDecision` validates an optional work-item ID against the authenticated tenant and target project.
6. `Cargo.lock` is intentionally changed because `sentinel-workflow` now uses the existing workspace `toml` dependency for the canonical profile.

## Automated verification

Every Rust command was dispatched through `cargo remote -c --`; no local Cargo, compiler, formatter, linter, rustdoc, or language-server process was used.

| Check | Command | Result |
|---|---|---|
| Workflow package | `cargo remote -c -- test -p sentinel-workflow` | PASS: 4 unit tests and 12 integration tests passed; doc-test target had 0 tests |
| Daemon workflow API | `cargo remote -c -- test -p sentinel-daemon --lib workflow_api::tests -- --nocapture` | PASS: 5 passed, 0 failed, 340 filtered out |
| Format | `cargo remote -c -- fmt --all -- --check` | PASS |
| Targeted compile | `cargo remote -c -- check -p sentinel-workflow -p sentinel-daemon --all-targets` | PASS, no diagnostics |
| Workspace compile | `cargo remote -c -- check --workspace --all-targets` | PASS after removal of three test-only unused imports, no remaining diagnostics |
| Workspace Clippy | `cargo remote -c -- clippy --workspace --all-targets -- -D warnings` | Lint subprocess PASS with no diagnostics; post-build full-target copy was canceled after it attempted to transfer about 40 GB, so the wrapper exit was 130. GitHub CI remains the authoritative complete wrapper-independent clean run. |
| Workspace tests | `cargo remote -c -- test --workspace -j1` | BUILDER RESOURCE BLOCKED: the daemon test binary link was terminated by SIGKILL (`clang: error: unable to execute command: Killed`); no test failure was reported |
| Workspace rustdoc | `RUSTDOCFLAGS="-D warnings" cargo remote -c -- doc --workspace --no-deps` | BUILDER RESOURCE BLOCKED: rustdoc for the unrelated `membership_spoof_probe` target was terminated by SIGKILL; no documentation diagnostic preceded the termination |
| Release build | `cargo remote -c -- build -j1 -p sentinel-daemon --release` | Build subprocess PASS: optimized profile completed; post-build full-target copy was canceled, and the remote release artifact was independently hashed |
| M0 contract | `python3 scripts/product-acceptance/check_contract.py --check` | PASS |
| M0 contract tests | `python3 -m unittest discover -s scripts/product-acceptance -p 'test_*.py'` | PASS: 18 passed |
| Typos | `typos .` | PASS |
| Patch integrity | `git diff --check` | PASS |

The two SIGKILL outcomes are build-host resource failures, not passing gates and not code/test failures. They are not benchmark evidence. Full workspace tests, Clippy, and rustdoc must pass on the pushed exact head in GitHub CI before ORC approval.

Release artifact after the current release command:

- File: `target/release/sentinel-daemon`
- Size: `57113992` bytes
- SHA-256: `2583e55e29aa195c68404badc5d0754af685173081a82b4689aa424fb1ce869f`
- Toolchain readback: `rustc 1.97.1 (8bab26f4f 2026-07-14)`

The artifact was not deployed. Final release provenance must be rebuilt from the eventual merge revision before live acceptance.

## Acceptance-criterion mapping

| AC | Core status | Evidence | Remaining work |
|---|---|---|---|
| AC-1 | PASS | Versioned domain records, durable typed IDs, explicit state enums, transition documentation, invalid-transition tests, and crash-atomic v1-to-v2 migration with failpoint recovery | None for the independent core |
| AC-2 | PASS for core/API | Customer, operator, and agent routes resolve server-owned credential bindings to typed principals; payload authority fields are rejected; replay, stale version, wrong digest, expiry, reject, feedback, cancellation, tenant isolation, route-kind isolation, forged completion evidence, self-attested gates, and unauthorized-role tests pass | Live API probe remains part of AC-12 |
| AC-3 | PASS | One SQLite immediate transaction binds the accepted proposal digest, canonical profile ID/version/digest, governance policy, owner, participants, and immutable commercial terms to Agreement, Project, events, and projection; scoped idempotency conflicts are tested | Live readback remains part of AC-12 |
| AC-4 | PASS for core | Canonical profile and DAG validation reject unknown or modified profiles, digest mismatch, missing roles/specialties/artifacts/gates, insufficient topology, cycles, duplicate or missing dependencies, empty contracts, self-dependencies, zero budgets, and a one-work-item shortcut to `DeliveryCandidate` | Full-workspace clean-run proof comes from PR CI |
| AC-5 | PASS for core policy | Assignment resolves the assignee through the authoritative organization port and persists its generation/digest; claim and dispatch revalidate the exact snapshot; participant, role, capability, reporting line, active state, workload, version, self-assignment, tenant, and cross-project checks fail closed | Effective live roster probe is deferred |
| AC-6 | PARTIAL | Claim creates a tenant/principal-bound durable outbox request; only accepted execution advances to `InProgress`; completion requires a sealed independent receipt bound to invocation, assignment, input, output, artifact ownership, and a canonical gate; authority drift is recoverable without queue poisoning or busy loops | Real #694 execution/evidence adapters and live integration are deferred |
| AC-7 | PASS for core | Project and team rooms, decisions, action items, questions, handoffs, acknowledgements, and blockers are structured entities/events; optional decision work-item scope is project/tenant checked; `/operator/chat` is not a workflow route | Live API journey is deferred |
| AC-8 | PASS for core | Assignment/reassignment snapshots, typed authority conflict, blocker raise/escalate/role-bound resolve, independent QA approval, actors, before/after states, and reasons are persisted and tested | Executive live escalation probe is deferred |
| AC-9 | PASS for bounded core; live pending | Execution is explicit and outbox-driven; stable invocation IDs prevent replay duplication; retry stops after three attempts and creates a typed operator blocker; authority conflicts terminalize their row and authorized resolution creates one fresh assignment/dispatch | Blocked-project soak and metrics readback are deferred |
| AC-10 | PASS for token-free core | Immutable project/provider/work-item ceilings are checked before reservation; Gaia spend is denied; exhaustion commits a typed blocker; deterministic fake paths spend no money | Capped provider proof belongs to #650; live cost readback is deferred |
| AC-11 | PASS for core restart tests; live pending | Schema-v2 stores preserve append-only history, tenant-filtered events, scoped idempotency, projection checkpoints, outbox state, and typed authority conflicts; verified backup/restore and restart/projection tests reject manifest mismatch and prevent duplicate dispatch | Gateway/NATS/workbench live restart proof is deferred |
| AC-12 | NOT VERIFIED | No runtime mutation was authorized or performed | After #694 integration: snapshot, deploy, complete the designated single-node journey, stability scan, evidence, and snapshot cleanup |

## Final clean-run requirement

GitHub CI is the authoritative clean environment and runs workspace tests, Clippy, rustdoc, format, lint, and eBPF jobs. Draft PR #725 must remain unmerged until every applicable exact-head check completes and the deferred dependency boundary is reviewed. Issue #695 remains open and must not receive `status:verified` before AC-6 adapter integration and AC-12 live acceptance pass.
