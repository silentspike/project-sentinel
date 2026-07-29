# Issue #472 lifecycle and Config Apply correction evidence

Date: 2026-07-29

Scope: source validation for PR #700 after the bundled ORC review at
`fed35f0b0e23d0e99db6fd3310da8e3a56823b68`.

No daemon, deployment VM, installed binary, configuration, snapshot, runtime
data, or benchmark target was accessed or changed. Live acceptance on `.240`
and every deployment claim remain **NOT TESTED**. `.241` and `.242` remain
forbidden for this issue.

## Product boundary

The productive daemon runtime set delivered by #472 is:

- ECS-native;
- bwrap plus Landlock;
- WASM/Wasmtime when the daemon is built with the `wasm` feature.

The daemon does not register `sentinel-microvm`. Explicit `microvm` selection
fails closed because the repository does not yet package a versioned guest
launcher and canonical rootfs, attest the requested agent identity inside the
guest, or durably reconcile Firecracker launch ownership after a crash.
SINGLE_NODE productization is tracked only by
[#775](https://github.com/silentspike/project-sentinel/issues/775) under #650
after #687. Cross-node and deep microVM migration remain owned by #553 and
#554.

## Finding-to-test map

| Finding | Source correction | Focused evidence |
|---|---|---|
| P0-1 canonical Config Apply decision | SQLite is the only durable decision authority. A schema-v2 marker binds `op_id`, old/staged digests, both configurations/buildings, the pre-apply snapshot, exact runtime snapshots, confirmed stops/spawns, phase, and rollback/forward decision. The phases are `Prepared`, `RuntimesApplied`, `CommittedPendingFinalize`, `RecoveryRequired`, and `Finalized`. The completion event, outbox row, and forward decision commit in one SQLite transaction. The filesystem journal is an idempotent participant; no cross-store atomicity is claimed. | `config_apply_decision_and_completion_event_survive_restart` covers phase transitions, restart readback, idempotent finalization, and an injected event-identity conflict that leaves the rollback decision and omits the outbox row. `filesystem_participant_replays_partial_publication_from_canonical_direction` simulates crash residue after a rename, stale-file deletion, and incomplete temp write, then proves rollback and forward replay converge from the canonical decision. |
| P0-2 startup recovery | Startup validates the schema, digests, and participant binding before any serving. Before the forward decision it restores the pre-apply World snapshot, ECS/redb rows, projection, old files, and exact pre-runtime snapshots. After the decision it publishes staged files and restores the recorded applied runtime snapshots. Ordinary spawn/readiness stays fenced until exact handle, resource incarnation, configuration, building, and projection validation completes. A marker is never cleared merely after process cleanup. | `config_apply_startup_rolls_back_and_restores_exact_runtime_before_finalizing` injects a crash after staged file publication and proves old file/redb/projection readback, a retained recovery fence, exact runtime restore, and final marker/participant completion. `config_apply_decision_and_completion_event_survive_restart` proves the durable forward decision and event/outbox survive restart. |
| P1-1 per-agent recovery obligations | Full compensation validates restored World, files, projection, logical runtime ownership, exact adapter handles, and instance resources before deleting the exact per-agent recovery rows and in-memory latches. Partial compensation leaves both durable and fail-closed. | `config_apply_compensation_restores_old_world_runtime_config_and_marker` creates a per-agent durable marker and latch, compensates, proves both are cleared only after validation, and proves a later exact spawn succeeds. `runtime_config_recovery_survives_restart_and_blocks_startup_until_reconciled` proves failed recovery remains blocked across restart. |
| P1-2 status-bound `Degraded` | One classifier consumes durable/logical runtime status and exact adapter observation. Only `Suspended` plus adapter `Degraded` is the expected healthy suspended state. `Active` plus `Degraded` is typed non-serving and enters repair/backoff. Snapshot counters, last status, and reconcile use the same classifier. | `degraded_adapter_semantics_are_runtime_independent_and_status_bound` covers ECS-native, WASM, and bwrap. Reconcile negatives prove suspended agents are not replaced and active degraded agents are not accepted as healthy. |
| P1-3 honest microVM boundary | The unused daemon dependency on `sentinel-microvm` is removed. Production registration contains three supported adapters and rejects microVM. Issue #472, CHANGELOG, DEV-007, gap Markdown, PR text, and this evidence use the same boundary. | Production registry/config negatives plus issue #775 and its Quality Gate. No microVM live claim is made. |
| P2-1 claim correction | Source documents describe the versioned saga and three-adapter phase without saying “all four adapters”, “complete transactional”, or claiming productive microVM/live completion. | Added-line claim scan, issue/PR body readback, and AC mapping below. |

Additional lifecycle corrections retained from the earlier reviews:

- Adapter health is observed through the private lifecycle owner using the
  exact `NanoHandle` and typed adapter health/resources. Missing, rewritten,
  stopped, unavailable, and observation-error states fail closed.
- World Restore retains every confirmed runtime stop and compensates the exact
  pre-restore runtime snapshots before releasing its fence.
- One canonical workload projection decides Config Apply replacement for
  runtime, name, role, favorite room, shift, tool capabilities, runtime
  metadata, and the bound command.
- Default bwrap restore creates a fresh incarnation from its bound workload
  specification. It does not claim CRIU, process-memory, or complete
  filesystem-state capture. Only #548 CAS-manifest behavior is default-off.
- WASM restore binds declarative state and already-bound results. It never
  replays stored input; external effects require a separate durable receipt
  contract.
- Registry-owned resources are cleaned only through typed `NanoRuntime::stop`.
  Cgroup identity is captured before stop and eBPF deregistration occurs only
  after confirmed stop.
- Compile-fail visibility is the language-level external ownership barrier.
  The AST inventory is supplementary drift detection, not a compile-time
  proof.

## Remote Rust evidence

All successful Rust results predating this bundled correction were run through
`cargo remote -c --` with Rust `1.97.1`; local Cargo/Rust was not used and the
global cargo-remote configuration was not changed.

The new saga/health delta invalidates those earlier final-gate claims. The first
focused rerun reached two test-code compile errors before executing tests:

```text
error[E0599]: no method named `append` found for `EventStore`
error[E0382]: borrow of moved value: `old_config`
```

Both test-only defects were corrected to use the public `append_event` API and
retain the later config value. A second focused rerun was stopped before tests
when the build-host capacity window was reassigned. Therefore the current
focused and final remote gates are **PENDING**, not PASS, until they execute on
the final main-integrated head.

The issue-owned target
`/work/tmp/project-sentinel/cdx2-472-saga-focused` was then removed locally and
from the build host. It occupied 1.3 GiB remotely. Build-host readback:
190 GiB total, 168 GiB used, 23 GiB available, with zero matching #472
Cargo/Rust processes. No foreign target or process was changed.

Required final reruns:

- focused Config Apply phase/failure/restart and three-runtime health tests;
- remote format and workspace check/tests;
- separate feature-enabled WASM check/tests;
- workspace Clippy with warnings denied and rustdoc with warnings denied;
- release daemon build;
- fenced-writer, unsafe-baseline, patch-registry, cargo-deny, typos,
  diff/scope, and exact-head GitHub checks.

## GitHub contract readback

- Issue #472: OPEN, `status:review`, `quality:ready`; the fresh issue Quality
  Gate passed after the material body rewrite.
- Follow-up #775: OPEN, `status:blocked`, `quality:ready`; Quality Gate run
  `30473925851` passed.
- PR #700 closing issue references: empty.
- Benchmark register: `/work/company/BENCHMARK-REGISTER.md`.
- Hard-coded live CPU/RAM claims: none.

## Acceptance mapping

| AC | Status | Evidence |
|---|---|---|
| AC-1 | PASS (source candidate); final gates pending | The production registry contains ECS-native, bwrap, and feature-enabled WASM; microVM is absent and fails closed. |
| AC-2 | PASS (source candidate); final gates pending | Private typed ownership, compile-fail visibility, functional lifecycle tests, and supplementary AST inventory cover productive mutation classes. |
| AC-3 | PARTIAL; live NOT TESTED | Source covers bwrap selection, cgroup/eBPF ordering, retry cleanup, shutdown, and recreate-style snapshot/restore. `.240` is still required. |
| AC-4 | PARTIAL; live NOT TESTED | Source covers non-bwrap daemon lifecycle without a Firecracker or process-memory claim. Real `.240` spawn/snapshot/restore/health/stop is still required. |
| AC-5 | PASS (source candidate); final gates pending | The canonical saga, rollback/forward startup recovery, exact runtime snapshots, event/outbox decision, projection, participant, and per-agent obligations have focused tests. |
| AC-6 | PARTIAL; live NOT TESTED | The merged #698 instance/stale-handle/retry contract is integrated; authorized SINGLE_NODE live evidence is still required. |
| AC-7 | PASS (source candidate) | Issue, CHANGELOG, DEV-007, gap Markdown, PR text, and evidence use the same three-adapter/fail-closed-microVM boundary. TOGAF HTML is not modified in this correction. |

Deployment, snapshots, live VM validation, and benchmarks are **NOT TESTED**.
PR #700 is not authorized for merge or deployment.
