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
| Workflow core integration | `cargo remote -c -- test -j1 -p sentinel-workflow --test workflow_core -- --nocapture` | PASS, 7 passed, 0 failed |
| Workflow package | `cargo remote -c -- test -j1 -p sentinel-workflow` | PASS, integration and doc-test targets |
| Daemon workflow API | `cargo remote -c -- test -j1 -p sentinel-daemon workflow_api::tests --lib` | PASS, 2 passed, 0 failed, 340 filtered out |
| Format | `cargo remote -c -- fmt --all -- --check` | PASS |
| Compile | `cargo remote -c -- check -j1 -p sentinel-workflow -p sentinel-daemon` | PASS |
| Clippy | `cargo remote -c -- clippy -j1 -p sentinel-workflow -p sentinel-daemon --all-targets -- -D warnings` | PASS |
| Rustdoc | `RUSTDOCFLAGS='-D warnings' cargo remote -c -- doc -j1 -p sentinel-workflow -p sentinel-daemon --no-deps` | PASS |
| Release build | `cargo remote -c -- build -j1 -p sentinel-daemon --release` | PASS on retry, optimized profile completed in 14m34s |
| Typos | `typos .` | PASS |
| Patch integrity | `git diff --check` | PASS |

The first daemon API build attempt was terminated by the builder with rustc signal 9 while compiling `cranelift-codegen`. Repeating the same command serially with `-j1` passed. The first release attempt returned exit 101 after dependency compilation, but its wrapper output did not retain a precise diagnostic. The exact same serial command was repeated from the retained cache and completed successfully. These attempts are build diagnostics, not benchmark evidence.

Release artifact before commit:

- File: `target/release/sentinel-daemon`
- Size: `56629096` bytes
- SHA-256: `0bf1714f581f1fb544f61812fd26a84cc0047050333cc4b1666a482d8efa203b`
- Toolchain readback: `rustc 1.97.1 (8bab26f4f 2026-07-14)`

The artifact was not deployed. Final release provenance must be rebuilt from the merge revision before live acceptance.

## Acceptance-criterion mapping

| AC | Core status | Evidence | Remaining work |
|---|---|---|---|
| AC-1 | PASS | Versioned domain records, durable typed IDs, explicit state enums, transition documentation, invalid-transition tests | None for the independent core |
| AC-2 | PASS for core/API | Authenticated customer and operator routes; replay, stale version, wrong digest, expiry, reject, feedback, cancellation, tenant isolation, and unauthorized-role tests | Live API probe remains part of AC-12 |
| AC-3 | PASS | One SQLite immediate transaction binds the accepted proposal digest and immutable commercial terms to Agreement, Project, events, and projection; idempotency digest conflict tested | Live readback remains part of AC-12 |
| AC-4 | PASS for core | DAG validation rejects cycles, duplicate or missing dependencies, empty input/output/capability contracts, self-dependencies, and zero budgets; valid graph records stable IDs and gates | Full-workspace clean-run proof comes from PR CI |
| AC-5 | PASS for core policy | Participant, role, capability, reporting line, active state, workload, version, self-assignment, and cross-project checks are fail-closed; assignment snapshot is immutable | Effective live roster probe is deferred |
| AC-6 | PARTIAL | A claim creates a durable outbox request; only accepted execution advances to `InProgress`; digest-bound output and passing gate are required for `Done`; tick-only advancement is absent | Real #694 workbench adapter and live integration are deferred |
| AC-7 | PASS for core | Project rooms, decisions, action items, questions, handoffs, acknowledgements, and blockers are structured entities/events; `/operator/chat` is not a workflow route | Live API journey is deferred |
| AC-8 | PASS for core | Assignment/reassignment snapshots, blocker raise/escalate/role-bound resolve, independent QA approval, actors, before/after states, and reasons are persisted and tested | Executive live escalation probe is deferred |
| AC-9 | PASS for bounded core; live pending | Execution is explicit and outbox-driven; stable invocation IDs prevent replay duplication; retry stops after three attempts and creates a typed operator blocker; operator resolution re-arms one exact request | Blocked-project soak and metrics readback are deferred |
| AC-10 | PASS for token-free core | Immutable project/provider/work-item ceilings are checked before reservation; Gaia spend is denied; exhaustion commits a typed blocker; deterministic mock path spends no money | Capped provider proof belongs to #650; live cost readback is deferred |
| AC-11 | PASS for core restart tests; live pending | Restart tests recover agreements, graph, assignment, outbox, accepted execution, decisions, handoff acknowledgement, approval, completion evidence, spend, and projection without duplicate dispatch | Gateway/NATS/workbench live restart proof is deferred |
| AC-12 | NOT VERIFIED | No runtime mutation was authorized or performed | After #694 integration: snapshot, deploy, complete `.240` journey, stability scan, evidence, and snapshot cleanup |

## Final clean-run note

GitHub CI is the authoritative clean environment and runs `cargo test --workspace`, workspace Clippy, workspace rustdoc, format, lint, and eBPF jobs. The Draft PR must remain unmerged until all applicable checks complete and the deferred dependency boundary is reviewed. Issue #695 must remain open and must not receive `status:verified` before AC-6 integration and AC-12 live acceptance pass.
