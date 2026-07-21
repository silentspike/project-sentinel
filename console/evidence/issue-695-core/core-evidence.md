# Issue #695 dependency-independent core evidence

Date: 2026-07-21

## Revision and boundary

- Branch: `feat/issue-695-company-workflow-core`
- Base revision: `13e87b663cc3b47223a2b0052db1cc6c274e66c8`
- Runtime access: not performed
- Deployment: not performed
- Provider calls and spend: none
- Benchmarks: not run; issue benchmarks are reserved for the authorized `.240` live phase
- Deferred dependency: the real `WorkExecutionPort` adapter and workbench result integration wait for #472 -> #701 -> #694
- Deferred acceptance: AC-12 and the live portions of AC-5, AC-6, AC-9, AC-10, and AC-11 wait for the dependency merge and explicit live authorization

## Automated verification

All Rust commands were dispatched through `cargo remote -c --`.

| Check | Command | Result |
|---|---|---|
| Workflow package | `cargo remote -c -- test -j1 -p sentinel-workflow` | PASS: 1 store/migration test and 10 integration tests passed; doc-test target had 0 tests |
| Daemon workflow API | `cargo remote -c -- test -j1 -p sentinel-daemon --lib workflow_api::tests -- --nocapture` | PASS: 4 passed, 0 failed, 340 filtered out |
| Format | `cargo remote -c -- fmt --all -- --check` | PASS |
| Compile | `cargo remote -c -- check -j1 -p sentinel-workflow -p sentinel-daemon` | PASS |
| Clippy | `cargo remote -c -- clippy -j1 -p sentinel-workflow -p sentinel-daemon --lib --tests -- -D warnings` | PASS, no diagnostics |
| Rustdoc | `RUSTDOCFLAGS='-D warnings' cargo remote -c -- doc -j1 -p sentinel-workflow -p sentinel-daemon --no-deps` | PASS |
| Release build | `cargo remote -c -- build -j1 -p sentinel-daemon --release` | PASS, optimized profile completed in 7m05s |
| M0 contract | `python3 scripts/product-acceptance/check_contract.py --check` | PASS |
| M0 contract tests | `TMPDIR=<workspace-temp> python3 -m unittest discover -s scripts/product-acceptance -p 'test_*.py'` | PASS: 18 passed; temporary directory removed |
| Typos | `typos .` | PASS |
| Patch integrity | `git diff --check` | PASS |

An earlier non-library daemon test command attempted to link unrelated binary targets and failed while linking `cluster_fail_closed_probe`: `clang: error: unable to execute command: Killed` (cargo-remote exit 254). The correctly scoped `--lib workflow_api::tests` command then completed with all four tests passing. The failed link is retained as a builder-resource diagnostic, not a code result or benchmark.

The first M0 validator-unit-test invocation could not create its temporary fixture under the quota-limited system `/tmp` and returned `OSError: [Errno 122] Disk quota exceeded`. Repeating the unchanged tests with a dedicated workspace temporary directory passed all 18 tests; the directory was removed afterward.

Release artifact before commit:

- File: `target/release/sentinel-daemon`
- Size: `56716936` bytes
- SHA-256: `a9ef641520522280755e82e7a3d0a4d76b0bb88d3a38c243e4a4fd0f80852203`
- Toolchain readback: `rustc 1.97.1 (8bab26f4f 2026-07-14)`

The artifact was not deployed. Final release provenance must be rebuilt from the merge revision before live acceptance.

## Acceptance-criterion mapping

| AC | Core status | Evidence | Remaining work |
|---|---|---|---|
| AC-1 | PASS | Versioned domain records, durable typed IDs, explicit state enums, transition documentation, invalid-transition tests | None for the independent core |
| AC-2 | PASS for core/API | Customer, operator, and agent routes resolve server-owned credential bindings to typed principals; payload authority fields are rejected; replay, stale version, wrong digest, expiry, reject, feedback, cancellation, tenant isolation, route-kind isolation, and unauthorized-role tests pass | Live API probe remains part of AC-12 |
| AC-3 | PASS | One SQLite immediate transaction binds the accepted proposal digest, governance profile/policy, owner, participants, and immutable commercial terms to Agreement, Project, events, and projection; caller-supplied acceptance authority was removed; scoped idempotency conflicts are tested | Live readback remains part of AC-12 |
| AC-4 | PASS for core | DAG validation rejects cycles, duplicate or missing dependencies, empty input/output/capability contracts, self-dependencies, and zero budgets; valid graph records stable IDs and gates | Full-workspace clean-run proof comes from PR CI |
| AC-5 | PASS for core policy | Assignment resolves the assignee through the authoritative organization port and persists its generation/digest; claim and dispatch revalidate the exact snapshot; participant, role, capability, reporting line, active state, workload, version, self-assignment, tenant, and cross-project checks fail closed | Effective live roster probe is deferred |
| AC-6 | PARTIAL | A claim creates a tenant/principal-bound durable outbox request; organization TOCTOU changes prevent provider dispatch; only accepted execution advances to `InProgress`; digest-bound output and passing gate are required for `Done`; tick-only advancement is absent | Real #694 workbench adapter and live integration are deferred |
| AC-7 | PASS for core | Project rooms, decisions, action items, questions, handoffs, acknowledgements, and blockers are structured entities/events; `/operator/chat` is not a workflow route | Live API journey is deferred |
| AC-8 | PASS for core | Assignment/reassignment snapshots, blocker raise/escalate/role-bound resolve, independent QA approval, actors, before/after states, and reasons are persisted and tested | Executive live escalation probe is deferred |
| AC-9 | PASS for bounded core; live pending | Execution is explicit and outbox-driven; stable invocation IDs prevent replay duplication; retry stops after three attempts and creates a typed operator blocker; operator resolution re-arms one exact request | Blocked-project soak and metrics readback are deferred |
| AC-10 | PASS for token-free core | Immutable project/provider/work-item ceilings are checked before reservation; Gaia spend is denied; exhaustion commits a typed blocker; deterministic mock path spends no money | Capped provider proof belongs to #650; live cost readback is deferred |
| AC-11 | PASS for core restart tests; live pending | Schema-v2 stores preserve append-only entity history, tenant-filtered events, scoped idempotency records, projection checkpoints, and outbox state; verified SQLite backup/restore checks hash, watermarks, state/history linkage, and entity/operation/outbox/projection counts while rejecting manifest mismatch; restart and projection rebuild recover state without duplicate dispatch | Gateway/NATS/workbench live restart proof is deferred |
| AC-12 | NOT VERIFIED | No runtime mutation was authorized or performed | After #694 integration: snapshot, deploy, complete `.240` journey, stability scan, evidence, and snapshot cleanup |

## Final clean-run note

GitHub CI is the authoritative clean environment and runs `cargo test --workspace`, workspace Clippy, workspace rustdoc, format, lint, and eBPF jobs. The Draft PR must remain unmerged until all applicable checks complete and the deferred dependency boundary is reviewed. Issue #695 must remain open and must not receive `status:verified` before AC-6 integration and AC-12 live acceptance pass.
