# Issue #650 Single-Node Product Acceptance Evidence

Status: `BLOCKED ON DELIVERY DEPENDENCIES`

Claim boundary: this evidence can establish **Single-Node Product Ready** only. It
does not establish Cluster GA, N-node HA, single-survivor continuity, native-host
readiness, or support for project profiles other than `web-project-v1`.

## Target Contract

| Item | Value |
| --- | --- |
| Runtime target class | `SINGLE_NODE` |
| Deploy/read-only/operational-baseline target | Canonical single-node VM |
| Forbidden targets | Cluster seed and member VMs |
| Rollback | Issue-specific hypervisor snapshot plus pre-deploy artifact hashes/backups |
| Provider budget | Maximum USD 1.00 total for the single approved real-provider acceptance leg |

Private host identities, addresses, VM identifiers, credentials, and
credential-derived values are intentionally excluded from this public evidence
index.

## M0 Journey

The bounded product profile is `web-project-v1`. The acceptance journey requires a
real static-website project from customer request through explicit customer
acceptance:

1. authenticated customer intake;
2. Sales clarification and qualification;
3. a versioned proposal with explicit scope, deliverables, exclusions, acceptance
   criteria, and cost ceiling;
4. explicit customer acceptance and a durable agreement;
5. governed project and acyclic work graph creation;
6. authorized work by Project Management, technical leadership, and at least two
   specialist roles in isolated workspaces;
7. independent QA of the exact candidate digest;
8. immutable release promotion, preview, delivery receipt, and customer decision;
9. one governed rework generation;
10. durable source-linked organizational memory and background/oversight readback;
    and
11. restart-safe, cost-bounded, lineage-complete execution without duplicate
    external effects.

Agent counts, chat, seeded task rows, screenshots, mocks, or task-status changes
alone cannot satisfy this journey.

## Delivery Dependency Gate

The live issue contract makes every row below a hard prerequisite for deployment
and final acceptance. The M0 lead must not substitute preflight evidence for these
deliverables.

| Issue | Delivery surface | Live state at preflight | Gate |
| --- | --- | --- | --- |
| #693 | Versioned work-execution contract and fail-closed conformance matrix | Open, ready | `BLOCKED` |
| #75 | Sandbox network isolation | Open, in progress | `BLOCKED` |
| #472 | Production `NanoRuntimeRegistry` selection path | Open, triage | `BLOCKED` |
| #694 | Capability-scoped isolated agent workbench | Open, ready | `BLOCKED` |
| #695 | Customer intake, governance, and multi-agent workflow | Open, ready | `BLOCKED` |
| #696 | Independent QA, release, delivery, Console lineage, and memory closeout | Open, ready | `BLOCKED` |

Final deployment may begin only after all six issues are closed with
`status:verified`, their target-runtime evidence is linked, and the #693
machine-readable matrix contains no `not_tested` or `blocked` M0 entry.

## Acceptance Matrix

| Criterion | Current status | Required final evidence |
| --- | --- | --- |
| AC-1 Cluster-disabled local readiness | `BLOCKED` | Snapshot-backed deployment; fresh restart; service/listener/health/dependency and local owner/write readback for every required component |
| AC-2 Identity and runtime consistency | `BLOCKED` | Configured, scheduled, resident, event-store, runtime, and projection set reconciliation; explicit profile selection; no stale row, duplicate owner, drift, or ECS-only fallback |
| AC-3 Customer intake and agreement | `BLOCKED` | Authenticated request; Sales clarification; proposal digest; explicit customer acceptance; durable agreement IDs |
| AC-4 Governed project graph | `BLOCKED` | Acyclic work graph spanning required roles, dependencies, outputs, budgets, and authorities; unauthorized-assignment negative test |
| AC-5 Structured collaboration authority | `BLOCKED` | Decisions, handoffs, acknowledgements, blockers, and escalation; chat-only mutation and hierarchy-bypass negative probes |
| AC-6 Isolated real work | `BLOCKED` | Assigned file/tool/test work through the approved runtime and bwrap; durable artifact manifests; unassigned-agent, egress, traversal, and fallback negative probes |
| AC-7 Evidence-gated state transitions | `BLOCKED` | Authorized claim and completion-evidence event sequence; tick-only advancement regression probe |
| AC-8 Independent QA | `BLOCKED` | QA of exact candidate digest; failed, stale, missing, self-approved, and mismatched-digest promotion negatives |
| AC-9 Immutable release and customer delivery | `BLOCKED` | Provenance-complete release, preview, receipt, explicit acceptance, and governed rework-generation evidence |
| AC-10 Durable closeout knowledge | `BLOCKED` | Source-linked memory query, one background cycle, Cortex/Gaia observation, and unauthorized-mutation negative test |
| AC-11 Real inference within spend ceiling | `BLOCKED` | One real-provider leg at or below USD 1.00; provider/model/tier/usage/cost/caller/request lineage; redaction scan; token-free repeatability/failure injection |
| AC-12 Boundary restart and duplicate-effect safety | `BLOCKED` | Restart/failure injection at each named durable boundary; stable request/outbox/event IDs; no duplicate provider call, action, artifact, release, or acceptance |
| AC-13 Console/API/event/artifact agreement | `BLOCKED` | Playwright screenshots plus matching API/event/artifact data for the complete journey; secret, browser-error, overflow, and focus checks |
| AC-14 Release rollback and restoration | `BLOCKED` | Live rollback to prior approved release and restore to accepted generation with manifest, preview, event, and audit readback |
| AC-15 Stability soak | `BLOCKED` | At least 600 advancing ticks and 60 minutes; unchanged restart counters; no panic/fatal/drift/stale runtime/duplicate effect/unresolved blocker; resource sidecars |
| AC-16 Recoverable deployment | `IN PROGRESS` | Snapshot and pre-deploy artifact backup are verified; retain through all acceptance work; delete only after every AC passes |
| AC-17 Honest milestone name | `PASS (wording gate)` | Final evidence uses only `Single-Node Product Ready` and repeats the excluded claims |

## Negative Criteria

| Criterion | Status | Enforcement |
| --- | --- | --- |
| AC-N1 Counts/chat/status alone are insufficient | `ENFORCED` | AC-3 through AC-10 require one correlated durable product journey |
| AC-N2 Mocks alone are insufficient | `ENFORCED` | AC-11 requires one separately capped real-provider leg after token-free tests |
| AC-N3 No Cluster-12 branch or VM | `ENFORCED` | Candidate lineage and runtime target remain single-node only |
| AC-N4 No insecure runtime fallback | `ENFORCED` | AC-2 and AC-6 fail closed on host or ECS-only fallback for tool-bearing work |
| AC-N5 No authority impersonation | `ENFORCED` | Customer acceptance, QA, and release authorities require distinct authenticated paths |
| AC-N6 Screenshots alone are insufficient | `ENFORCED` | AC-13 requires matching durable API/event/artifact evidence |
| AC-N7 No build-host performance evidence | `ENFORCED` | Operational timings and sidecars run only on the single-node target |
| AC-N8 No incomplete contract row at closure | `ENFORCED` | #693 matrix must be entirely evidenced and green |

## Read-Only Preflight

Timestamp: 2026-07-20 UTC.

Repository baseline:

- the issue branch starts at `origin/main` commit
  `dade246e244bf1809200da5c0464e80bc79c5cdf`;
- the branch contains no Cluster-12 experimental implementation;
- Rust execution remains remote-only through `cargo remote -c --`;
- no deployment or service restart has occurred for #650; and
- an independent worktree already owns #693, so this branch does not edit that
  issue's implementation surface.

Pre-deployment verification:

- remote Rust formatting and the full-workspace all-target check completed
  successfully with the checked-in Rust toolchain;
- targeted remote tests passed for the daemon (`338/338`) and the common,
  projection, projection-service, dashboard-backend, Gaia, Gaia-loop, control CLI,
  and agent-runtime packages;
- targeted remote Clippy passed for both the daemon and the non-daemon M0 component
  set with warnings denied;
- Console Vitest (`71/71`), TypeScript typecheck, and production build completed
  successfully;
- Gateway, Judge, and NATS bridge Go test suites and builds completed successfully;
- two attempted full-workspace Rust test runs were invalidated by build-host resource
  exhaustion: the persistent build filesystem returned `Disk quota exceeded (os
  error 122)`, and an isolated memory-backed retry terminated the daemon compiler
  with `SIGKILL` under memory pressure; and
- build-host timings and resource behavior are not runtime or benchmark evidence.

The resource-invalidated runs are not recorded as product failures or passes. Relevant
package gates run in bounded isolated targets, while pull-request CI remains the
authoritative clean-workspace result.

Runtime baseline:

- core daemon, gateway, projection, dashboard backend, NATS bridge, NATS server, and
  judge units are active with zero recorded restarts since their current starts;
- gateway health/readiness return HTTP 200 on the token-free `local-loop` provider;
- 60 canonical agent TOML files exist and 26 agent-runtime processes are resident;
- the event store is advancing and contains hierarchy-aware v2 usage events;
- the projection database contains agent, room, task, model-tier, hierarchy-tier,
  cost, and watermark tables;
- no warning-or-higher journal entries were returned for the inspected core services
  during the initial 60-minute window;
- the Gaia readiness-loop unit and binary are absent from the live deployment even
  though they are part of the current repository deployment manifest;
- the health-monitor timer is installed but inactive;
- the historical projection/API port is not listening; the dashboard backend is on
  its current TLS/WebTransport listener; and
- hypervisor inventory identifies the canonical running single-node VM and confirms
  that issue-specific snapshots are supported.

These observations are baseline facts, not AC passes. Because the hard delivery
dependencies are incomplete, none of the missing M0 paths may be classified as a
deployment-only drift or exercised by a premature production deployment.

## Blocker Register

The product blockers are already materialized as the hard dependency issues listed
above. New issues are created only for a reproducible, narrower defect not covered by
their accepted scope; duplicate issues are forbidden.

| Candidate observation | Current disposition |
| --- | --- |
| Live Gaia loop absent | Deployment drift candidate; re-evaluate only after dependency gate closes |
| Health-monitor timer inactive | Provisioning/observability candidate; re-evaluate only after dependency gate closes |
| M0 customer-to-acceptance path unavailable | Covered by #693 through #696, #75, and #472 |
| Projection/API access mismatch | Validate against the final #696 Console/API contract before filing anything new |

## Snapshot Lifecycle

| Step | Status | Evidence |
| --- | --- | --- |
| Live VM and hypervisor placement identified | `PASS` | Read-only host inventory plus guest-agent identity match |
| Existing snapshots inventoried | `PASS` | Read-only snapshot tree captured privately |
| `pre-650-20260720T215926Z` created | `PASS` | Created before any deployment or service restart |
| Snapshot read back | `PASS` | Snapshot tree shows the issue snapshot as the immediate parent of `current` |
| Pre-deploy artifacts and hashes backed up | `PASS` | Root-only same-VM backup; archive SHA-256 `4d96ccb12e7e579962ede7d700cfed8aeb35760ee39b137dcc9faff8b7c5bfc7`; manifest SHA-256 `e37b7a514c86697b8a2f31ef95254b489957b6ead6f7b2c9d72c7ea319d4c961` |
| Dependency gate closed | `BLOCKED` | All hard dependency issues must be verified before the final epic deployment |
| Snapshot retained through all ACs | `IN PROGRESS` | Required on every failure or partial result |
| Snapshot deleted after complete success | `NOT STARTED` | Allowed only after AC-1 through AC-17 pass and final runtime stability readback |

## Evidence Rules

Every final AC row links an artifact containing:

- UTC timestamp and deployed revision;
- exact public-safe command, request, or browser interaction;
- exit code or HTTP status;
- meaningful output with secrets and private infrastructure removed;
- durable IDs/digests and matching data-source readbacks;
- explicit pass/fail decision; and
- anything not tested.

The issue remains open and this index remains blocked until the dependency gate
closes and every acceptance row passes.
