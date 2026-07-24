# Issue #472 ORC lifecycle correction evidence

Date: 2026-07-24

Scope: source and remote-build validation for the findings in the ORC review of
PR #700. No runtime service, deployment VM, installed binary, configuration, or
runtime data was accessed or changed during this correction pass.

Current `origin/main@16c0e353861e29a9b4d181bebd9a9f4a432a49b3` is included
through merge commit `1e7e89a`; the feature branch was not rebased. The
`llm_bridge` conflict retains main's one-second circuit-breaker reset and
expires the stored deadline directly without a scheduler-dependent sleep.

## Review finding mapping

| Finding | Correction | Failure and lifecycle coverage |
|---|---|---|
| Default bwrap snapshots | Default bwrap snapshots now persist a compatibility payload containing the bound workload specification and command. Restore creates a fresh runtime incarnation from that binding. It does not claim process, memory, or complete filesystem-state capture. Only the new CAS-manifest mode from #548 remains default-off. | `default_bwrap_snapshot_is_reproducible_recreate_without_cas_manifest_claim` uses isolated temporary CAS/home roots while explicitly retaining the default-off CAS-manifest flag, so it also runs under an unprivileged hosted-runner filesystem; `default_bwrap_compatibility_snapshot_supports_world_restore_without_cas_manifest`; the complete workspace suite exercises manual/periodic world snapshot, pre-restore, and pre-config-apply call sites. |
| Config-apply safety snapshot | A runtime-changing apply requires its pre-apply safety snapshot. A missing store or failed adapter snapshot aborts before runtime, ECS, or projection mutation. | Functional config-apply tests exercise the staged transition and fail-closed stop/replacement paths. |
| Persistent config recovery | `runtime_config_recovery` is an owner-fenced SQLite table. The daemon atomically creates a `transitioning` marker before stopping the exact old runtime. Incomplete rollback changes it to `recovery_required`; startup reads and reconciles markers before API/readiness/ECS/runtime publication, and spawn/reconcile reject blocked agents. A marker is cleared only after verified adapter cleanup/reconcile. | `runtime_config_recovery_survives_restart_and_blocks_startup_until_reconciled`; `recovery_block_rejects_spawn_instead_of_reconciler_resurrection`; stop and replacement failure tests. |
| Adapter-owned cleanup | bwrap and microVM implement typed abandoned-runtime reconciliation. microVM fails closed when only unowned durable sockets remain; it does not kill by an unverified PID or delete unowned state. | `abandoned_reconcile_is_verified_or_fails_closed_on_unowned_socket`; bwrap marker/setup/process failure-injection and retry tests in the workspace suite. |
| Ownership barrier | The raw registry and concrete adapters are held in the orchestrator's private `runtime_lifecycle` submodule. Productive call sites receive `RuntimeAdapterOwner` and can mutate lifecycle state only through its typed, exact-handle methods. The live `DaemonNanoRuntimeRegistry` is private. Direct PID/cgroup fallback cleanup was removed. | `runtime_lifecycle_visibility` is a compile-fail test proving external production code cannot import either the typed owner or the live daemon registry. Functional startup, shift, config-apply, restore, shutdown, control, reconciler, cgroup/eBPF, and failure-injection tests cover productive call-site classes. `ast_inventory_finds_no_raw_adapter_owner_outside_lifecycle_boundary` remains supplementary AST drift detection, not a compile-time proof. |
| WASM restore effects | Version-two WASM snapshots persist declarative workload/tool binding plus an already-bound execution result. Restore never executes stored input. A legacy input without a bound result fails closed; any external-effect retry requires a separate durable idempotency key and receipt. | `snapshot_binds_completed_result_without_storing_a_replay_command`; `restore_never_replays_legacy_effectful_last_input` uses the real file-writing component and observes no file effect; `restore_rejects_legacy_effect_without_bound_result` rejects the unreceipted effect before loading the component. |

## Remote Rust gates

All Rust commands ran through `cargo remote -c --` on the configured build
host and reported `rustc 1.97.1`. Per-command `CARGO_TARGET_DIR` and serialized
build settings were scoped to this PR; the global cargo-remote configuration
was not changed. Build duration and host load are not runtime evidence.

| Gate | Command | Result |
|---|---|---|
| Format | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472-final CARGO_BUILD_JOBS=1' -c -- fmt --all -- --check` | PASS |
| WASM effect replay rejection | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472-final CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1' -c -- test -p sentinel-wasm --features wasm legacy_effect -j1` | PASS, 2 passed |
| WASM bound snapshot | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472-final CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1' -c -- test -p sentinel-wasm --features wasm snapshot_binds_completed_result_without_storing_a_replay_command -j1` | PASS, 1 passed |
| Compile-fail ownership boundary | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472-final CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1' -c -- test -p sentinel-daemon --test runtime_lifecycle_visibility -j1` | PASS, 1 passed |
| WASM feature check | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472-final CARGO_BUILD_JOBS=1' -c -- check -p sentinel-wasm --features wasm -j1` | PASS |
| WASM feature tests | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472-final CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1' -c -- test -p sentinel-wasm --features wasm -j1` | PASS, 61 unit, 62 acceptance, 2 conformance |
| Workspace check | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472-final CARGO_BUILD_JOBS=1' -c -- check --workspace --all-targets -j1` | PASS |
| Workspace tests | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472-final CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1' -c -- test --workspace -j1` | PASS |
| Workspace Clippy | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472-final CARGO_BUILD_JOBS=1' -c -- clippy --workspace --all-targets -j1 -- -D warnings` | PASS |
| Rustdoc | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472-final CARGO_BUILD_JOBS=1 RUSTDOCFLAGS=-Dwarnings' -c -- doc --workspace --no-deps -j1` | PASS |
| Release build | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472-final CARGO_BUILD_JOBS=1' -c -- build -p sentinel-daemon --bin sentinel-daemon --release -j1` | PASS |

## Non-Rust source gates

| Gate | Command | Result |
|---|---|---|
| Diff whitespace | `git diff --check` | PASS |
| Typos | `typos` | PASS |
| Fenced writers | `python3 scripts/check-fenced-writers.py` | PASS |
| Unsafe baseline | `python3 scripts/check-unsafe-baseline.py` | PASS, 26/26 |

## Issue acceptance mapping

| AC | Status | Evidence |
|---|---|---|
| AC-1 | PASS (source) | The production daemon constructs the typed lifecycle owner with all supported adapters and exact per-incarnation handles. |
| AC-2 | PASS (source) | Productive spawn, stop, config replacement, world restore, reconciliation, rollback, control, and shutdown use typed registry/adapter APIs. A compile-fail visibility test proves external code cannot import the productive owner/live registry; functional failure tests cover every productive call-site class. The AST inventory is supplementary only. |
| AC-3 | NOT TESTED live | Source tests cover bwrap lifecycle ownership, cgroup/eBPF ordering, retry retention, and stop-before-publication. No VM was accessed. |
| AC-4 | PASS (source); NOT TESTED live | Default bwrap compatibility snapshot/restore supports the existing World Snapshot, Restore, and Time Machine paths by recreating a fresh bound workload. It makes no process-state claim. The separate CAS-manifest extension remains default-off for #548. |
| AC-5 | PASS (source) | DEV-007, the TOGAF gap, feature documentation, and the changelog state the typed ownership, non-replaying WASM restore, and #548 boundary without overclaiming. |
| #698 AC-6 | NOT TESTED live | The required productive lifecycle integration belongs to the post-review single-node validation on `.240`. |

Benchmarks, deployment, and live VM validation are **NOT TESTED**. PR #700 is
not authorized for merge or deployment in this phase.
