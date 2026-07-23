# Issue #695 dependency-independent core evidence

Date: 2026-07-23

## Revision and boundary

- Branch: `feat/issue-695-company-workflow-core`
- Branch point: `13e87b663cc3b47223a2b0052db1cc6c274e66c8`
- Merged upstream: `origin/main@16c0e353861e29a9b4d181bebd9a9f4a432a49b3`
- Verified code commit: `85a339e42fb6979688d7dcdb1e3e620cc0643286`
- Runtime target class: `SINGLE_NODE`
- Runtime access: not performed
- Deployment: not performed
- Provider calls and spend: none
- Benchmarks: not run; issue benchmarks remain reserved for the separately authorized live phase
- Deferred dependency: production `WorkExecutionPort` and completion-authority adapters wait for #472 -> #701 -> #694
- Deferred acceptance: AC-12 and the live portions of AC-5, AC-6, AC-9, AC-10, and AC-11 wait for dependency integration and explicit ORC authorization

## Implemented correction set

1. The company workflow is deployment-safe and default OFF. A disabled daemon starts without touching workflow storage, profiles, or principal bindings, while workflow routes return typed `workflow_unavailable` responses. Explicit enablement remains fail closed for missing or modified profiles and missing principals.
2. `web-project-v1` is loaded server-side from exact verified bytes and is the workflow profile SSOT. Its ID, version, and digest are proposal-digest-bound and copied unchanged into Agreement and Project. Canonical roles, specialties, artifacts, independent gates, and minimum topology are validated.
3. Completion evidence no longer runs inside a SQLite writer transaction. Transaction 1 durably records a digest-bound evidence request with project, item, assignment, and authority versions. The external authority is called without a writer lock. Transaction 2 performs a complete CAS/TOCTOU recheck before committing completion.
4. Completion receipts are opaque authority results, not publicly reproducible hashes. Receipt claims bind schema, request digest, invocation, project and item versions, assignment generation, authority generation and digest, issuer, validity window, artifacts, gates, and replay domain. The issuer must be an active independent QA or Release Manager participant in the authoritative organization snapshot.
5. The completion outbox has bounded timeout/failure handling and restart-safe duplicate semantics. Crash, timeout, duplicate, replay, stale authority, forged role, non-participant issuer, modified output, self-attested gate, and cross-version cases are covered.
6. Organization authority drift produces a typed durable `AuthorityConflict`, completes the affected outbox attempt, blocks the item for reassignment, and permits independent rows to continue. Authorized resolution deactivates the stale assignment without rearming its invocation.
7. Schema v1 to v2 migration executes in one immediate transaction, writes the version marker last, rolls back every destructive step on an injected failure, and retries deterministically from the intact v1 image.
8. `RecordDecision` validates an optional work-item ID against the authenticated tenant and target project.
9. `Cargo.lock` changes because `sentinel-workflow` uses the existing workspace `toml` dependency for canonical profile loading.

## Automated verification

Every Rust command was dispatched through `cargo remote -c --`; no local Cargo, compiler, formatter, linter, rustdoc, or language-server process was used.

| Check | Command | Result |
|---|---|---|
| Workflow package | `cargo remote -c -- test -p sentinel-workflow -j1` | PASS on the verified code commit: 4 unit tests and 18 integration tests passed; doc-test target had 0 tests |
| Daemon workflow API | `cargo remote -c -- test -j1 -p sentinel-daemon --lib workflow_api::tests -- --nocapture` | PASS on the verified code commit: 9 passed, 0 failed, 340 filtered out |
| Format | `cargo remote -c -- fmt --all -- --check` | PASS on the verified code commit |
| Workspace compile | `cargo remote -c -- check --workspace --all-targets -j1` | PASS on the verified code commit, no diagnostics |
| Workspace Clippy | `cargo remote -c -- clippy --workspace --all-targets -j1 -- -D warnings` | PASS on the verified code commit |
| Workspace tests | `cargo remote -c -- test --workspace -j1` | PASS on the verified code commit; the daemon library reported 348 passed, 0 failed, and 1 deploy-VM-only test ignored, and all remaining workspace unit, integration, and doc-test targets completed successfully |
| Workspace rustdoc | `RUSTDOCFLAGS="-D warnings" cargo remote -c -- doc --workspace --no-deps -j1` | PASS; Cargo emitted the pre-existing Projection lib/bin output-filename collision warning, while rustdoc completed with `-D warnings` |
| Release build | `cargo remote -c -- build -j1 -p sentinel-daemon --release` | PASS: optimized release profile completed |
| M0 contract | `python3 scripts/product-acceptance/check_contract.py --check` | PASS |
| M0 contract tests | `python3 -m unittest discover -s scripts/product-acceptance -p 'test_*.py'` | PASS: 18 passed |
| Typos | `typos .` | PASS |
| Patch integrity | `git diff --check` | PASS |

Build and transfer durations are diagnostic only and are not benchmark evidence. GitHub CI remains the authoritative clean full-workspace run for the pushed exact head.

Release artifact from the verified code commit:

- File: `target/release/sentinel-daemon`
- Size: `57184176` bytes
- SHA-256: `acc45eb674631806859e38e482be0ce757d81d693e2f65761e9b41c58c72c43f`
- Toolchain readback: `rustc 1.97.1 (8bab26f4f 2026-07-14)`

The artifact was not deployed. Final release provenance must be rebuilt from the eventual merge revision before live acceptance.

## Acceptance-criterion mapping

| AC | Core status | Evidence | Remaining work |
|---|---|---|---|
| AC-1 | PASS | Versioned domain records, durable typed IDs, explicit state enums, transition documentation, invalid-transition tests, and crash-atomic v1-to-v2 migration with failpoint recovery | None for the independent core |
| AC-2 | PASS for core/API | Default-OFF daemon startup, typed unavailable routes, server-owned credential-to-principal bindings, payload authority rejection, replay, stale version, wrong digest, expiry, rejection, feedback, cancellation, tenant isolation, route-kind isolation, forged evidence, self-attested gates, and unauthorized-role tests | Live API probe remains part of AC-12 |
| AC-3 | PASS | One SQLite immediate transaction binds the accepted proposal digest, canonical profile ID/version/digest, governance policy, owner, participants, and immutable commercial terms to Agreement, Project, events, and projection; scoped idempotency conflicts are tested | Live readback remains part of AC-12 |
| AC-4 | PASS for core | Canonical profile and DAG validation reject unknown or modified profiles, digest mismatch, missing roles/specialties/artifacts/gates, insufficient topology, cycles, duplicate or missing dependencies, empty contracts, self-dependencies, zero budgets, and a one-work-item shortcut to `DeliveryCandidate`; the complete remote workspace test passed | Exact-head GitHub CI remains required |
| AC-5 | PASS for core policy | Assignment resolves the assignee through the authoritative organization port and persists its generation/digest; claim, dispatch, and completion revalidate the exact snapshot; participant, role, capability, reporting line, active state, workload, version, self-assignment, tenant, and cross-project checks fail closed | Effective live roster probe is deferred |
| AC-6 | PARTIAL | Claim creates a tenant/principal-bound durable execution request. Completion uses a durable evidence request, an external opaque authority receipt outside the writer transaction, and a second full CAS transaction. Request, invocation, project/item/assignment/authority versions, replay domain, output ownership, and an independent profile gate are verified. Restart, crash, timeout, duplicate, forged, stale, and replay paths pass | Production #694 execution and completion-authority adapters plus live integration are deferred |
| AC-7 | PASS for core | Project and team rooms, decisions, action items, questions, handoffs, acknowledgements, and blockers are structured entities/events; optional decision work-item scope is project/tenant checked; `/operator/chat` is not a workflow route | Live API journey is deferred |
| AC-8 | PASS for core | Assignment/reassignment snapshots, typed authority conflict, blocker raise/escalate/role-bound resolve, independent QA approval, actors, before/after states, and reasons are persisted and tested | Executive live escalation probe is deferred |
| AC-9 | PASS for bounded core; live pending | Execution and completion are explicit and outbox-driven; stable request/invocation IDs prevent duplicate commits; retries stop after three attempts; terminal authority conflicts do not poison independent rows; authorized resolution creates one fresh assignment/dispatch | Blocked-project soak and metrics readback are deferred |
| AC-10 | PASS for token-free core | Immutable project/provider/work-item ceilings are checked before reservation; Gaia spend is denied; exhaustion commits a typed blocker; deterministic fake paths spend no money | Capped provider proof belongs to #650; live cost readback is deferred |
| AC-11 | PASS for core restart tests; live pending | Schema-v2 stores preserve append-only history, tenant-filtered events, scoped idempotency, projection checkpoints, execution and completion outboxes, evidence requests, and typed authority conflicts; verified backup/restore and restart/projection tests reject manifest mismatch and prevent duplicate dispatch/completion | Gateway/NATS/workbench live restart proof is deferred |
| AC-12 | NOT VERIFIED | No runtime mutation was authorized or performed | After dependency integration and ORC authorization: snapshot, deploy to the designated single node, run the complete journey and stability scan, collect evidence, and clean up the snapshot |

## Final clean-run requirement

GitHub CI is the authoritative clean environment and runs workspace tests, Clippy, rustdoc, format, lint, and eBPF jobs. Draft PR #725 must remain unmerged until every applicable exact-head check completes and the deferred dependency boundary is reviewed. Issue #695 remains open and must not receive `status:verified` before AC-6 adapter integration and AC-12 live acceptance pass.
