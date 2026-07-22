# Issue #472 ORC P1 correction evidence

Date: 2026-07-22

Scope: source and remote-build validation for the four findings in the ORC
review of PR #700 at `70b6861661fbfa49077cd6326d22df9c7c2d88c7`.
No service, VM, installed binary, configuration, or runtime data was accessed or
changed during this correction pass.

## Review finding mapping

| Finding | Correction | Failure and lifecycle coverage |
|---|---|---|
| World restore | The pre-commit teardown records only exact runtime incarnations whose adapter stop succeeded. Every later pre-commit failure compensates those recorded snapshots in reverse stop order, and the restore fence opens only after complete compensation. | `world_restore_nth_stop_failure_compensates_only_successfully_stopped_runtimes`; the `world_restore_` filtered suite passed 3/3. |
| Config apply | A runtime-changing config remains staged until the exact old handle confirms stop. ECS, projection, and logical identity are published only after stop; replacement failure restores the old configuration and starts a new incarnation of the old runtime. An incomplete rollback removes serving surfaces and persists `runtime_config_recovery_required` with `serving=false`. | `runtime_config_change_stop_failure_preserves_old_name_role_runtime_and_projection`; `runtime_config_replacement_failure_restores_old_config_and_runtime`; the `runtime_config_` filtered suite passed 2/2. |
| Runtime reconciler | Registry handles are part of liveness. Missing logical state is rebuilt around the exact registry-owned incarnation. Direct PID/cgroup removal is prohibited for registry-owned resources; cleanup remains adapter-owned through `NanoRuntime::stop`. | `runtime_reconcile_recovers_registry_handle_missing_from_logical_map_without_replacement` passed 1/1. The source lifecycle gate also checks this ownership barrier. |
| bwrap/CAS snapshot | Productive bwrap world snapshot and restore are default-off behind `SENTINEL_BWRAP_CAS_WORLD_SNAPSHOT_ENABLED`. The flag defaults to `false`; enabling it is reserved for #548 after durable CAS ownership, GC-safe new-before-old pin transfer, retention, delete, restore, and failed-walk handling exist. | `productive_bwrap_world_snapshot_and_restore_are_default_off_until_issue_548` passed 1/1. No productive bwrap snapshot/restore claim is made by #472. |

## Remote Rust gates

All Rust commands ran through `cargo remote -c --` on the configured build
host and reported `rustc 1.97.1`. Test-profile debug information was disabled
for the large test links; command duration and build-host load are not runtime
evidence.

| Gate | Command | Result |
|---|---|---|
| Format | `cargo remote -c debug/.cargo-lock -- fmt --all -- --check` | PASS |
| Daemon check | `cargo remote -c debug/.cargo-lock -- check -p sentinel-daemon -j1` | PASS |
| World-restore failures | `cargo remote -b 'RUST_BACKTRACE=1 CARGO_PROFILE_TEST_DEBUG=0' -c debug/.cargo-lock -- test -p sentinel-daemon --lib world_restore_ -j1` | PASS, 3 passed |
| Config-apply failures | `cargo remote -b 'RUST_BACKTRACE=1 CARGO_PROFILE_TEST_DEBUG=0' -c debug/.cargo-lock -- test -p sentinel-daemon --lib runtime_config_ -j1` | PASS, 2 passed |
| Reconciler ownership | `cargo remote -b 'RUST_BACKTRACE=1 CARGO_PROFILE_TEST_DEBUG=0' -c debug/.cargo-lock -- test -p sentinel-daemon --lib runtime_reconcile_recovers_registry_handle_missing_from_logical_map_without_replacement -j1` | PASS, 1 passed |
| bwrap gate | `cargo remote -b 'RUST_BACKTRACE=1 CARGO_PROFILE_TEST_DEBUG=0' -c debug/.cargo-lock -- test -p sentinel-daemon --lib productive_bwrap_world_snapshot_and_restore_are_default_off_until_issue_548 -j1` | PASS, 1 passed |
| Lifecycle source barrier | `cargo remote -b 'RUST_BACKTRACE=1 CARGO_PROFILE_TEST_DEBUG=0' -c debug/.cargo-lock -- test -p sentinel-daemon --test nano_runtime_registry productive_lifecycle_call_sites_remain_registry_owned -j1` | PASS, 1 passed |
| Daemon library | `cargo remote -b 'RUST_BACKTRACE=1 CARGO_PROFILE_TEST_DEBUG=0' -c debug/.cargo-lock -- test -p sentinel-daemon --lib -j1` | PASS, 360 passed, 1 ignored |
| WASM feature check | `cargo remote -c debug/.cargo-lock -- check -p sentinel-wasm --features wasm -j1` | PASS |
| WASM feature tests | `cargo remote -b 'RUST_BACKTRACE=1 CARGO_PROFILE_TEST_DEBUG=0 RUST_TEST_THREADS=1' -c debug/.cargo-lock -- test -p sentinel-wasm --features wasm -j1` | PASS, 58 unit, 62 acceptance, 2 conformance |
| Workspace tests | `cargo remote -b 'RUST_BACKTRACE=1 CARGO_PROFILE_TEST_DEBUG=0 RUST_TEST_THREADS=1' -c debug/.cargo-lock -- test --workspace -j1` | PASS |
| Workspace Clippy | `cargo remote -c debug/.cargo-lock -- clippy --workspace --all-targets -j1 -- -D warnings` | PASS |
| Release build | `cargo remote -c release/.cargo-lock -- build -p sentinel-daemon --bin sentinel-daemon --release -j1` | PASS |

Earlier WASM attempts that exceeded pre-existing sub-second test timeouts while
the shared build host was contended were discarded. The table records the
subsequent complete serialized reruns only; no timeout or production setting
was changed.

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
| AC-1 | PASS (source) | The production daemon constructs `NanoRuntimeRegistry` with all four adapters and the daemon depends on `sentinel-microvm`. |
| AC-2 | PASS (source) | Productive spawn, removal, config replacement, world-restore replacement, reconciliation, rollback, and shutdown are registry-owned and use exact per-incarnation handles. Failure-injection and lifecycle source gates are listed above. |
| AC-3 | NOT TESTED live | Source tests cover bwrap lifecycle ownership, cgroup/eBPF ordering, retry retention, and the corrected stop-before-publication paths. No VM was accessed in this pass. |
| AC-4 | BLOCKED by #548 for bwrap snapshot/restore; NOT TESTED live | Non-bwrap productive live spawn remains a #472 live gate after code approval. Productive bwrap CAS world snapshot/restore is explicitly default-off until #548 supplies the ownership/pin contract. |
| AC-5 | PASS (source) | DEV-007, the TOGAF gap, and the changelog state registry use and the #548 boundary without a productive bwrap snapshot overclaim. |

Benchmarks, deployment, and live VM validation are **NOT TESTED**. PR #700 is
not authorized for merge or deployment in this phase.
