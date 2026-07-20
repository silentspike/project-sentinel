# Issue #698 NanoRuntime stop evidence

Date: 2026-07-21

Scope: the shared `NanoRuntime` stop contract and the ECS-native,
WASM/Wasmtime, bwrap/Landlock, and microVM adapters. Runtime diagnostics were
run only on the canonical #650 single-node VM. No cluster node was contacted.

## Candidate and rollback guard

- Source branch: `feat/issue-698-nano-stop`
- The live test executables were produced by `cargo remote -c --` from the
  candidate worktree. Their final SHA-256 values were:
  - bwrap conformance executable:
    `c6f072be9826a1cbe93e31c2a5f00e9ec74a7c7fb79a11898a3aca9efcd8f40f`
  - ECS-native conformance executable:
    `35a7c0d61b6f9419f75dd64ec959862a3d9595be8350421b57e9298c42f117af`
- The issue #650 pre-deployment VM snapshot remained present throughout the
  diagnostic. No service, installed binary, or configuration was replaced.

## Static and remote build gates

All Rust commands executed on the configured remote build host. No local Rust
tool or Rust executable was run.

| Gate | Command | Result |
|---|---|---|
| Format | `cargo remote -c -- fmt --all -- --check` | PASS |
| Check | `cargo remote -c -- check -p sentinel-common -p sentinel-runtime -p sentinel-sandbox -p sentinel-wasm -p sentinel-microvm` | PASS |
| Tests | `cargo remote -c -- test -p sentinel-common -p sentinel-runtime -p sentinel-sandbox -p sentinel-wasm -p sentinel-microvm` | PASS |
| Clippy | `cargo remote -c -- clippy -p sentinel-common -p sentinel-runtime -p sentinel-sandbox -p sentinel-wasm -p sentinel-microvm --all-targets -- -D warnings` | PASS |
| M0 contract | `python3 scripts/product-acceptance/check_contract.py --matrix scripts/product-acceptance/m0-contract.toml` | PASS |
| M0 validator tests | `python3 -m unittest test_check_contract.py` from `scripts/product-acceptance/` | PASS, 18/18 |

The final Rust test run included:

- shared contract and registry tests: 122 passed;
- microVM unit/fixture tests: 6 passed, including process/socket teardown and
  retained snapshot preservation;
- ECS-native adapter conformance: 2 passed;
- sandbox unit tests: 47 passed, with 3 host-capability tests ignored;
- bwrap integration tests compiled and remained ignored on the build host for
  execution on the single-node VM;
- WASM adapter and tool-runtime suites passed.

## Single-node runtime diagnostics

The bwrap diagnostic used `/usr/bin/agent-runtime`, the production-approved
Landlock entrypoint. The common conformance harness first required both
workloads to report `Healthy`, rejected a wrong-runtime handle, stopped A,
confirmed B remained healthy, replayed A's stop successfully, and stopped B.

```text
running 1 test
[landlock-wrapper] Landlock enforced for bwrap-stop-agent-a
agent-runtime: started (pid=2)
[landlock-wrapper] Landlock enforced for bwrap-stop-agent-b
test bwrap_stop_is_idempotent_and_workload_scoped ... ok
test result: ok. 1 passed; 0 failed
bwrap_elapsed_seconds=0.01
```

The first exploratory run used `/usr/bin/sleep`; Landlock correctly denied that
entrypoint. Its formal test result was discarded because the old harness did not
require a healthy pre-stop workload. The harness and fixture were corrected
before the final evidence above.

Post-stop readback on the single-node VM:

```text
process_count=0
cgroup_count=0
marker_count=0
NRestarts=0
ActiveState=active
SubState=running
```

The available non-bwrap runtime diagnostic also passed:

```text
test ecs_native_stop_is_idempotent_and_workload_scoped ... ok
test result: ok. 1 passed; 0 failed
ecs_elapsed_seconds=0.00
```

These elapsed values are operational diagnostics, not benchmark claims. The
temporary test executables and the two empty test agent-home directories were
removed after the negative readback; cleanup was verified.

## Acceptance mapping

| AC | Status | Evidence |
|---|---|---|
| AC-1 | PASS | Version-1 `NanoStopResult` uses a closed `stopped` / `already_stopped` outcome. Unit tests cover first stop, replay, JSON wire form, and wrong-runtime rejection. |
| AC-2 | PASS | All four adapters use the shared two-workload conformance contract. Remote suites passed; bwrap and ECS-native were additionally executed on the single-node VM. |
| AC-3 | PASS | Unit coverage proves process reap, CAS release, state removal, replay, and second-workload preservation. Live readback proves zero test process, cgroup, and runtime marker after stop. Cgroup cleanup errors now propagate. |
| AC-4 | PASS | A fixture Firecracker process is reaped; API/vsock paths are removed; a retained snapshot remains; the second workload remains healthy; replay is idempotent. |
| AC-5 | PASS | ECS-native removes the addressed orchestrator and ECS state. WASM removes only the addressed workload state. Both pass two-workload conformance. |
| AC-6 | NOT VERIFIED | The compile-time production-daemon registry integration belongs to #472 and is intentionally outside this patch. #698 must remain open until #472 supplies that evidence against the merged contract. |

No panic, service restart, cluster access, deployment, or provider call occurred.
