# Issue #472 ORC lifecycle correction evidence

Date: 2026-07-23

Scope: source and remote-build validation for the findings in the ORC review of
PR #700. No service, VM, installed binary, configuration, or runtime data was
accessed or changed during this correction pass.

## Review finding mapping

| Finding | Correction | Failure and lifecycle coverage |
|---|---|---|
| Default bwrap snapshots | Default bwrap snapshots now persist a compatibility payload containing the bound workload specification and command. Restore creates a fresh runtime incarnation from that binding. It does not claim process, memory, or complete filesystem-state capture. Only the new CAS-manifest mode from #548 remains default-off. | `default_bwrap_snapshot_is_reproducible_recreate_without_cas_manifest_claim`; `default_bwrap_compatibility_snapshot_supports_world_restore_without_cas_manifest`; the complete workspace suite exercises manual/periodic world snapshot, pre-restore, and pre-config-apply call sites. |
| Config-apply safety snapshot | A runtime-changing apply requires its pre-apply safety snapshot. A missing store or failed adapter snapshot aborts before runtime, ECS, or projection mutation. | Functional config-apply tests exercise the staged transition and fail-closed stop/replacement paths. |
| Persistent config recovery | `runtime_config_recovery` is an owner-fenced SQLite table. The daemon atomically creates a `transitioning` marker before stopping the exact old runtime. Incomplete rollback changes it to `recovery_required`; startup reads and reconciles markers before API/readiness/ECS/runtime publication, and spawn/reconcile reject blocked agents. A marker is cleared only after verified adapter cleanup/reconcile. | `runtime_config_recovery_survives_restart_and_blocks_startup_until_reconciled`; `recovery_block_rejects_spawn_instead_of_reconciler_resurrection`; stop and replacement failure tests. |
| Adapter-owned cleanup | bwrap and microVM implement typed abandoned-runtime reconciliation. microVM fails closed when only unowned durable sockets remain; it does not kill by an unverified PID or delete unowned state. | `abandoned_reconcile_is_verified_or_fails_closed_on_unowned_socket`; bwrap marker/setup/process failure-injection and retry tests in the workspace suite. |
| Ownership barrier | The raw registry and concrete adapters are held only by the private `runtime_lifecycle` module. Productive call sites receive `RuntimeAdapterOwner` and can mutate lifecycle state only through its typed, exact-handle methods. Direct PID/cgroup fallback cleanup was removed. | Functional startup, shift, config-apply, restore, shutdown, control, reconciler, cgroup/eBPF, and failure-injection tests. `ast_inventory_finds_no_raw_adapter_owner_outside_lifecycle_boundary` is a supplementary AST inventory, not a compile-time proof. |

## Remote Rust gates

All Rust commands ran through `cargo remote -c --` on the configured build
host and reported `rustc 1.97.1`. Per-command `CARGO_TARGET_DIR` and serialized
build settings were scoped to this PR; the global cargo-remote configuration
was not changed. Build duration and host load are not runtime evidence.

| Gate | Command | Result |
|---|---|---|
| Format | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472 CARGO_BUILD_JOBS=1' -c -- fmt --all -- --check` | PASS |
| Workspace check | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472 CARGO_BUILD_JOBS=1' -c -- check --workspace --all-targets -j1` | PASS |
| Config recovery failures | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1' -c -- test -p sentinel-daemon --lib runtime_config_ -j1` | PASS, 3 passed |
| Default bwrap daemon path | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1' -c -- test -p sentinel-daemon --lib default_bwrap_compatibility_snapshot_supports_world_restore_without_cas_manifest -j1` | PASS, 1 passed |
| Ownership boundary | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1' -c -- test -p sentinel-daemon --test nano_runtime_registry -j1` | PASS, 2 passed |
| microVM abandoned recovery | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1' -c -- test -p sentinel-microvm abandoned_reconcile_is_verified_or_fails_closed_on_unowned_socket -j1` | PASS, 1 passed |
| bwrap compatibility adapter | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1' -c -- test -p sentinel-sandbox default_bwrap_snapshot_is_reproducible_recreate_without_cas_manifest_claim -j1` | PASS, 1 passed |
| Workspace tests | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=1' -c -- test --workspace -j1` | PASS |
| Workspace Clippy | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472 CARGO_BUILD_JOBS=1' -c -- clippy --workspace --all-targets -j1 -- -D warnings` | PASS |
| Rustdoc | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472 CARGO_BUILD_JOBS=1 RUSTDOCFLAGS=-Dwarnings' -c -- doc --workspace --no-deps -j1` | PASS |
| Release build | `cargo remote -b 'CARGO_TARGET_DIR=/var/tmp/cdx3-target-472 CARGO_BUILD_JOBS=1' -c -- build -p sentinel-daemon --bin sentinel-daemon --release -j1` | IN PROGRESS |

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
| AC-2 | PASS (source) | Productive spawn, stop, config replacement, world restore, reconciliation, rollback, control, and shutdown use typed registry/adapter APIs. Functional failure tests and the supplementary AST inventory are listed above. |
| AC-3 | NOT TESTED live | Source tests cover bwrap lifecycle ownership, cgroup/eBPF ordering, retry retention, and stop-before-publication. No VM was accessed. |
| AC-4 | PASS (source); NOT TESTED live | Default bwrap compatibility snapshot/restore supports the existing World Snapshot, Restore, and Time Machine paths by recreating a fresh bound workload. It makes no process-state claim. The separate CAS-manifest extension remains default-off for #548. |
| AC-5 | PASS (source) | DEV-007, the TOGAF gap, feature documentation, and the changelog state the typed ownership and #548 boundary without overclaiming. |
| AC-6 | NOT TESTED live | The required productive lifecycle integration belongs to the post-review single-node validation on `.240`. |

Benchmarks, deployment, and live VM validation are **NOT TESTED**. PR #700 is
not authorized for merge or deployment in this phase.
