# Issue #472 ORC lifecycle correction evidence

Date: 2026-07-29

Scope: source and remote-build validation for the bundled adversarial review of
PR #700 at reviewed head `f028fb6b4f5b2fcc8ee76d5d73b3216ddeff4b8d`.
No runtime service, deployment VM, installed binary, configuration, snapshot,
or runtime data was accessed or changed.

Current `origin/main@ae31ed303bf039c78b666ed1bf0a29e5ac334a93` is included
through merge commit `c6ee6b6c9dec2023401e0c1fb88b2ca691440c75`; the branch
was not rebased or force-pushed. The merge was conflict-free and added only the
two current-main research documents. The prior current-main merge
`c764988df2df96c87e09fa759990656c8d30ba18` remains in history. The
`llm_bridge` one-second circuit-breaker reset and direct stored-deadline
expiration remain unchanged.

## Finding-to-test map

| Finding | Source correction | Focused evidence |
|---|---|---|
| P1-1 adapter health truth | The private `RuntimeAdapterOwner` observes the exact instance-fenced `NanoHandle`, then calls typed `health` and `resources`. Missing or rewritten handles, `Stopped`, `Unavailable`, and observation errors are stale/fail-closed. `Degraded` is classified explicitly and cannot become healthy. Raw adapter ownership remains private. | `adapter_health_observation_fails_closed_for_stopped_and_rewritten_handles`; `healthy_non_bwrap_runtime` exercises ECS-native and WASM with exact observations; missing, mismatched, stopped, unavailable, error, and degraded cases are covered in `runtime_health`. Reconcile consumes the same snapshot truth. |
| P1-2 microVM guest contract | The repository still has no packaged/versioned guest launcher, canonical rootfs pipeline, or workload-bound guest readiness attestation. Production registration and selection were therefore removed/rejected fail-closed. No productive microVM claim is made. | `microvm_selection_fails_closed_without_guest_attestation_contract`; production selection test proves microVM is absent while ECS-native, bwrap, and feature-enabled WASM remain available. |
| P1-3 microVM crash recovery | Because durable launch ownership and verified restart reconciliation are not complete, the incomplete adapter is not exposed as a productive runtime. This avoids duplicate/unowned Firecracker instances rather than silently accepting the gap. | The same config and production-registry negatives hold this boundary. AC-1 and AC-4 remain BLOCKED pending the complete launcher/readiness/recovery contract. |
| P1-4 workload-affecting Config Apply | One canonical `NanoWorkloadSpec` projection compares runtime, name, role, favorite room, shift set, tool capabilities, runtime metadata, and the bound command. Any difference uses the durable exact-handle stop/replace/rollback path; personality/background-only edits remain ECS-only. | Field-by-field positive comparison tests plus `every_workload_affecting_field_fails_closed_when_exact_stop_is_rejected`. |
| P1-5 transactional Config Apply | An owner-fenced SQLite recovery marker and filesystem journal are persisted before the first stop. Confirmed stops/spawns are tracked; publication and config persistence happen only after successful replacement. Any stop, spawn, room/projection, or persistence failure compensates the full transaction from old config/building and exact runtime snapshots. Failed compensation persists `RecoveryRequired`, fences serving/readiness/spawn, and is reconciled before startup publication. | Stop/replacement negatives, `runtime_config_recovery_survives_restart_and_blocks_startup_until_reconciled`, `recovery_block_rejects_spawn_instead_of_reconciler_resurrection`, and `config_apply_compensation_restores_old_world_runtime_config_and_marker`. |
| P1-6 lifecycle ownership proof | Concrete adapters and the live registry remain in the private lifecycle module. Productive mutation is available only through typed owner methods; direct PID/cgroup fallback cleanup is absent. Compile-fail visibility is the language-level external bypass barrier; the AST inventory is supplementary only. | `runtime_lifecycle_visibility`; `ast_inventory_finds_no_raw_adapter_owner_outside_lifecycle_boundary`; functional startup, shift, removal, restore, config, rollback, shutdown, control, reconcile, cgroup/eBPF, and failure paths in the daemon suite. |
| P2-1 TOGAF HTML ownership | The worker-authored HTML delta was removed. The file matches `origin/main` byte-for-byte. DEV-007 and gap Markdown say source candidate and live pending. | `git diff --exit-code origin/main -- docs/architecture/togaf-architecture-guide.html`. |
| P2-2 temporary paths | New tests use `SENTINEL_TEST_TMP_ROOT` when supplied (the project convention is `/work/tmp/project-sentinel`) and otherwise use a writable checkout-local `target/test-tmp` fallback. No newly added retained `/tmp` or `/var/tmp` test path remains in the #472 delta. | Added-line scan over the owned delta, excluding unrelated integrated research documents; the GitHub-hosted-runner regression was fixed after its non-writable `/work/tmp` failure and the two WASM legacy-effect tests passed remotely with the fallback. |

Additional lifecycle corrections retained from the earlier reviews:

- World Restore tracks every successfully stopped runtime. Any pre-store-commit
  failure compensates those exact snapshots before releasing the restore fence;
  a failpoint after the Nth stop proves partial teardown recovery.
- Default bwrap snapshots persist a compatibility payload containing the bound
  workload and command. Restore creates a fresh incarnation and makes no
  process-memory, CRIU, or complete filesystem-state claim. Only the #548
  CAS-manifest extension remains default-off.
- WASM schema v2 persists declarative binding plus an already-bound execution
  result. Restore never invokes stored input; legacy effectful input without a
  durable result/receipt fails closed before component load.
- Registry-owned handles participate in liveness and cleanup only through
  `NanoRuntime::stop`. Cgroup identity is captured before stop and eBPF state is
  removed only after confirmed stop.

## Remote Rust gates

Every Rust command used `cargo remote -c --`, a command-scoped
`CARGO_TARGET_DIR=/work/tmp/project-sentinel/cdx2-472-final-target`, and
`rustc 1.97.1`. The global cargo-remote configuration was not changed.
Build-host durations and load are not runtime evidence.

| Gate | Command | Result |
|---|---|---|
| Focused adapter-health tests | `cargo remote -b 'CARGO_TARGET_DIR=/work/tmp/project-sentinel/cdx2-472-final-target CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1' -c -- test -p sentinel-daemon runtime_health::tests -j2 -- --test-threads=1` | PASS, 6 passed; the exact stopped/rewritten-handle negative also passed in the final daemon/workspace run |
| Daemon lifecycle suite | same scoped environment, `test -p sentinel-daemon --lib -j2 -- --test-threads=1` | PASS, 368 passed, 1 VM-bound test ignored |
| Registry and AST inventory | same scoped environment, `test -p sentinel-daemon --test nano_runtime_registry -j2 -- --test-threads=1` | PASS, 3 passed |
| Compile-fail ownership boundary | same scoped environment, `test -p sentinel-daemon --test runtime_lifecycle_visibility -j2 -- --test-threads=1` | PASS, 1 passed |
| Limbo targeted reproduction | same scoped environment, `test -p sentinel-limbo -j2 -- --test-threads=1` | PASS, 55 unit and 12 acceptance |
| WASM feature check | `cargo remote -b 'CARGO_TARGET_DIR=/work/tmp/project-sentinel/cdx2-472-final-target CARGO_BUILD_JOBS=2' -c -- check -p sentinel-wasm --features wasm -j2` | PASS |
| WASM feature tests | same scoped test environment, `test -p sentinel-wasm --features wasm -j2 -- --test-threads=1` | PASS, 61 unit, 62 acceptance, 2 conformance |
| Hosted-runner temp-root regression | `cargo remote -b 'CARGO_TARGET_DIR=/work/tmp/project-sentinel/cdx2-472-ci-fix-target CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1' -c -- test -p sentinel-wasm --features wasm --lib legacy_effect -j2 -- --test-threads=1` | PASS, 2 passed, 59 filtered; no `SENTINEL_TEST_TMP_ROOT` override, exercising the checkout-local fallback |
| Format | same scoped build environment, `fmt --all -- --check` | PASS |
| Workspace check | same scoped build environment, `check --workspace --all-targets -j2` | PASS |
| Workspace tests | same scoped test environment, `test --workspace -j2 -- --test-threads=1` | PASS, including daemon 368 passed/1 VM-bound ignored, Limbo 55 unit/12 acceptance, registry 3, visibility 1, and WASM 61 unit/62 acceptance/2 conformance |
| Workspace Clippy | same scoped build environment, `clippy --workspace --all-targets -j2 -- -D warnings` | PASS |
| Rustdoc | same scoped build environment plus `RUSTDOCFLAGS=-Dwarnings`, `doc --workspace --no-deps -j2` | PASS |
| Release daemon build | same scoped build environment, `build -p sentinel-daemon --bin sentinel-daemon --release -j2` | PASS; returned binary size 56,712,376 bytes, SHA-256 `491a097ec9a49312ca279b2828a464baf350b7d4dedb005d6c11960c52fd47ee` |
| Cargo deny | same scoped build environment, `deny check` | PASS (`advisories`, `bans`, `licenses`, and `sources`); existing unmatched-allow/skip warnings only |

## Repository policy gates

| Gate | Result |
|---|---|
| `python3 scripts/check-fenced-writers.py` | PASS |
| `python3 scripts/check-unsafe-baseline.py` | PASS, 26/26 |
| `python3 scripts/check-patch-registry.py` | PASS, 0 overrides, 0 registry entries, 4 direct Git dependencies |
| `python3 -m unittest scripts.tests.test_check_patch_registry` | PASS, 15 passed |
| `typos` | PASS |
| `git diff --check` | PASS |
| TOGAF HTML matches current main | PASS |
| New retained `/tmp` and `/var/tmp` scan | PASS |

## Acceptance mapping

| AC | Status | Evidence |
|---|---|---|
| AC-1 | BLOCKED | ECS-native, bwrap, and feature-enabled WASM are registered with exact per-incarnation ownership. microVM is intentionally non-selectable until the missing guest launcher/readiness/durable recovery contract exists. |
| AC-2 | PASS (source) | Compile-fail visibility plus functional lifecycle tests enforce productive mutation through typed ownership; AST inventory is supplementary. |
| AC-3 | PARTIAL; live NOT TESTED | Source tests cover bwrap lifecycle ownership, cgroup/eBPF ordering, retry retention, transactional apply, and stop-before-publication. |
| AC-4 | BLOCKED; live NOT TESTED | Default bwrap recreate restore and non-replaying WASM restore are source-tested, but productive microVM spawn/restore/health cannot be claimed. |
| AC-5 | PASS (source candidate) | DEV-007, gap Markdown, changelog, and this evidence state the implemented boundaries and live-pending status without modifying ORC-owned TOGAF HTML. |
| #698 AC-6 | NOT TESTED live | Source integration is present; authorized SINGLE_NODE validation on `.240` has not occurred. |

Benchmarks, deployment, snapshots, and live VM validation are **NOT TESTED**.
PR #700 is not authorized for merge or deployment.

After the release artifact and its matching remote/local SHA-256 were captured,
only the issue-owned remote target
`/work/tmp/project-sentinel/cdx2-472-final-target` was removed. It occupied
11 GiB. Build-host root capacity changed to 169 GiB used and 22 GiB available
of 190 GiB (89% used), with zero remaining #472 `cargo` or `rustc` processes.
No foreign target or process was changed.

The focused hosted-runner correction used a separate issue-owned remote target,
`/work/tmp/project-sentinel/cdx2-472-ci-fix-target`. It was removed after the
two feature-enabled WASM tests passed and occupied 851 MiB. Zero #472 `cargo`
or `rustc` processes remained. Subsequent foreign build activity reduced
build-host free capacity to 6.5 GiB; no foreign process or target was inspected,
signaled, or removed.
