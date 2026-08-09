# Issue #472 lifecycle and Config Apply correction evidence

Date: 2026-08-09

Scope: source validation for PR #700. The exact reviewed code and dependency
tree is `961e04a42829fb08dd1fc316dffccc48cc02f8ac`, based on
`d12290fbb4d8bd95d815d5dc17891853d67889d2`.

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
| Pre-gate directory durability | After the complete stale Agent TOML delete set, the production callsite fsyncs `agents/` before reporting persistence success. `rooms.toml` retains its own atomic write and parent fsync. | `stale_agent_delete_fsyncs_the_agents_directory_before_success` injects a recording directory-sync callsite, proves exactly the Agent directory is synced after a stale delete, and reads back the deletion. |
| Pre-gate projection cardinality | Config Apply identity mutation requires exactly one active `agent_live_view` row. Missing, inactive, or duplicate rows fail closed before the saga can commit. | `config_apply_projection_identity_requires_exactly_one_active_row` covers zero-row, inactive-row, two-row, and exact-one-row behavior. |
| Pre-gate participant payload integrity | The filesystem participant recomputes canonical SHA-256 over both old and staged agent/building payloads when staging, loading, and before publication. It then compares the validated digest identities with SQLite. Corrupt payload with unchanged digest fields is rejected before any config mutation; a self-consistent tampered payload/digest pair is rejected against SQLite; missing participants are still deterministically rematerialized from SQLite. | `filesystem_participant_rejects_payload_corruption_before_publication` and `config_apply_startup_rejects_corrupt_file_participant_before_mutation` cover direct publication and both restart mismatch classes while retaining the canonical rollback marker and old files. |
| P1-1 per-agent recovery obligations | Full compensation validates restored World, files, projection, logical runtime ownership, exact adapter handles, and instance resources before deleting the exact per-agent recovery rows and in-memory latches. Partial compensation leaves both durable and fail-closed. | `config_apply_compensation_restores_old_world_runtime_config_and_marker` creates a per-agent durable marker and latch, compensates, proves both are cleared only after validation, and proves a later exact spawn succeeds. `runtime_config_recovery_survives_restart_and_blocks_startup_until_reconciled` proves failed recovery remains blocked across restart. |
| P1-2 status-bound `Degraded` | One classifier consumes durable/logical runtime status and exact adapter observation. Only `Suspended` plus adapter `Degraded` is the expected healthy suspended state. `Active` plus `Degraded` is typed non-serving and enters repair/backoff. Snapshot counters, last status, and reconcile use the same classifier. | `degraded_adapter_semantics_are_runtime_independent_and_status_bound` covers ECS-native, WASM, and bwrap. Reconcile negatives prove suspended agents are not replaced and active degraded agents are not accepted as healthy. |
| P1-3 honest microVM boundary | The unused daemon dependency on `sentinel-microvm` is removed. Production registration contains three supported adapters and rejects microVM. Issue #472, CHANGELOG, DEV-007, gap Markdown, PR text, and this evidence use the same boundary. | Production registry/config negatives plus issue #775 and its Quality Gate. No microVM live claim is made. |
| P2-1 claim correction | Source documents describe the versioned saga and three-adapter phase without saying "all four adapters", "complete transactional", or claiming productive microVM/live completion. | Added-line claim scan, issue/PR body readback, and AC mapping below. |

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

All Rust commands used the issue-owned remote target with Rust `1.97.1`; local
Cargo/Rust was not used and the global cargo-remote configuration was not
changed. The command family was explicit throughout:

```text
cargo remote -t /work/tmp/project-sentinel/cdx2-472-final -c .rustc_info.json -- fmt --all -- --check
cargo remote -t /work/tmp/project-sentinel/cdx2-472-final -c .rustc_info.json -- check --workspace --all-targets
cargo remote -t /work/tmp/project-sentinel/cdx2-472-final -c .rustc_info.json -- test --workspace
cargo remote -t /work/tmp/project-sentinel/cdx2-472-final -c .rustc_info.json -- test -p sentinel-wasm
cargo remote -t /work/tmp/project-sentinel/cdx2-472-final -c .rustc_info.json -- test -p sentinel-wasm --features wasm
cargo remote -t /work/tmp/project-sentinel/cdx2-472-final -c .rustc_info.json -- clippy --workspace --all-targets -- -D warnings
cargo remote -b 'RUST_BACKTRACE=1 RUSTDOCFLAGS=-Dwarnings' -t /work/tmp/project-sentinel/cdx2-472-final -c .rustc_info.json -- doc --workspace --no-deps
cargo remote -t /work/tmp/project-sentinel/cdx2-472-final -c release/sentinel-daemon -- build --workspace --release
cargo remote -t /work/tmp/project-sentinel/cdx2-472-final -c .rustc_info.json -- deny check
cargo remote -t /work/tmp/project-sentinel/cdx2-472-final -c .rustc_info.json -- audit --ignore RUSTSEC-2023-0071
```

The completed matrix on the reviewed code head is PASS:

- focused Config Apply, recovery, authority, fence, and lifecycle tests;
- format, workspace check, and full workspace tests, including daemon
  `391 passed / 0 failed / 1 ignored`;
- separate WASM default tests (`39` unit plus `10` acceptance) and
  feature-enabled tests (`63` unit plus `62` acceptance plus `2` conformance);
- workspace Clippy with warnings denied and Rustdoc with warnings denied;
- workspace release build with Wasmtime and Wasmtime-WASI `46.0.2`;
- cargo-deny advisories, bans, licenses, and sources;
- cargo-audit with zero vulnerabilities and exactly three repository-approved
  unmaintained warnings;
- fenced-writer, unsafe-baseline, patch-registry, typos, diff, and scope gates.

The returned release daemon is `60,675,104` bytes with SHA-256
`3bd6efb78b3216c9060b05abd538563c8c5e189ffd9f85823dab7389ed5f017a`.

## GitHub contract readback

- Issue #472: OPEN, `status:review`, `quality:ready`; the fresh issue Quality
  Gate run `30473957347` passed after the material body rewrite. Body SHA-256:
  `4278e8f428c59aed53d27300172760388dadb0b6c2e2869a60fdf54c4424c11c`.
- Follow-up #775: OPEN, `status:blocked`, `quality:ready`; Quality Gate run
  `30473925851` passed. Body SHA-256:
  `22cc4fb1d6a4cf9f422883ec5aad15f2da98a55d6bb4454190e68905e0c45904`.
- PR #700 closing issue references: empty.
- The exact reviewed code head is
  `961e04a42829fb08dd1fc316dffccc48cc02f8ac`, based on
  `d12290fbb4d8bd95d815d5dc17891853d67889d2`; its final source gates PASS.
  This evidence-only commit follows that reviewed code head and does not
  invalidate its source, dependency, or Rust-gate results.
- Benchmark register: `/work/company/BENCHMARK-REGISTER.md`.
- Hard-coded live CPU/RAM claims: none.

## Acceptance mapping

| AC | Status | Evidence |
|---|---|---|
| AC-1 | PASS (source) | The production registry contains ECS-native, bwrap, and feature-enabled WASM; microVM is absent and fails closed. |
| AC-2 | PASS (source) | Private typed ownership, compile-fail visibility, functional lifecycle tests, and supplementary AST inventory cover productive mutation classes. |
| AC-3 | PARTIAL; live NOT TESTED | Source covers bwrap selection, cgroup/eBPF ordering, retry cleanup, shutdown, and recreate-style snapshot/restore. `.240` is still required. |
| AC-4 | PARTIAL; live NOT TESTED | Source covers non-bwrap daemon lifecycle without a Firecracker or process-memory claim. Real `.240` spawn/snapshot/restore/health/stop is still required. |
| AC-5 | PASS (source) | The canonical saga, rollback/forward startup recovery, exact runtime snapshots, event/outbox decision, projection, participant, and per-agent obligations have focused tests. |
| AC-6 | PARTIAL; live NOT TESTED | The merged #698 instance/stale-handle/retry contract is integrated; authorized SINGLE_NODE live evidence is still required. |
| AC-7 | PASS (source candidate) | Issue, CHANGELOG, DEV-007, gap Markdown, PR text, and evidence use the same three-adapter/fail-closed-microVM boundary. TOGAF HTML is not modified in this correction. |

Deployment, snapshots, live VM validation, and benchmarks are **NOT TESTED**.
PR #700 is not authorized for merge or deployment.
