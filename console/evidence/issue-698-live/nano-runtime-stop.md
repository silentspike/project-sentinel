# Issue #698 NanoRuntime stop evidence

Date: 2026-07-21

Scope: the shared `NanoRuntime` stop contract and the ECS-native,
WASM/Wasmtime, bwrap/Landlock, and microVM adapters. Runtime diagnostics were
run only on the canonical #650 single-node VM. No cluster node was contacted.

## Candidate and scope guard

- Source branch: `feat/issue-698-nano-stop`
- The final source gates below ran at the review candidate in PR #699 with
  `rustc 1.97.1`; the exact pushed commit is recorded by the PR head.
- No service, installed binary, configuration, or cluster node was changed by
  the final review delta.
- Earlier single-node diagnostics used executables produced by
  `cargo remote -c --` from the initial candidate. Their SHA-256 values were:
  - bwrap conformance executable:
    `c6f072be9826a1cbe93e31c2a5f00e9ec74a7c7fb79a11898a3aca9efcd8f40f`
  - ECS-native conformance executable:
    `35a7c0d61b6f9419f75dd64ec959862a3d9595be8350421b57e9298c42f117af`
- The issue #650 pre-deployment VM snapshot remained present throughout that
  diagnostic. These earlier runtime results are retained below as lineage
  evidence and are not represented as a final-head deployment.

## Static and remote build gates

All Rust commands executed on the configured remote build host. No local Rust
tool or Rust executable was run.

| Gate | Command | Result |
|---|---|---|
| Format | `cargo remote -c -- fmt --all -- --check` | PASS |
| Check | `cargo remote -c -- check -j1 -p sentinel-common -p sentinel-runtime -p sentinel-sandbox -p sentinel-microvm -p sentinel-wasm` | PASS |
| Default tests | `cargo remote -c -- test -j1 -p <crate>` for each of `sentinel-common`, `sentinel-runtime`, `sentinel-sandbox`, `sentinel-microvm`, and `sentinel-wasm` | PASS |
| WASM feature check | `cargo remote -c -- check -j1 -p sentinel-wasm --features wasm` | PASS |
| WASM feature tests | `cargo remote -c -- test -j1 -p sentinel-wasm --features wasm` | PASS |
| Default Clippy | `cargo remote -c -- clippy -j1 -p sentinel-common -p sentinel-runtime -p sentinel-sandbox -p sentinel-microvm -p sentinel-wasm --all-targets -- -D warnings` | PASS |
| WASM feature Clippy | `cargo remote -c -- clippy -j1 -p sentinel-wasm --features wasm --all-targets -- -D warnings` | PASS |
| M0 contract | `python3 scripts/product-acceptance/check_contract.py --matrix scripts/product-acceptance/m0-contract.toml` | PASS |
| M0 validator tests | `python3 -m unittest test_check_contract.py` from `scripts/product-acceptance/` | PASS, 18/18 |

The final exact-source Rust test runs included:

- shared contract and registry tests: 123 unit tests passed, plus all
  acceptance and snapshot suites;
- ECS-native: 25 unit tests passed (1 host metric test ignored), 8 acceptance
  tests passed, and 2 NanoRuntime conformance tests passed;
- microVM: 9 unit/fixture tests passed, including a real Unix socket fixture,
  process/socket retry cleanup, retained snapshot preservation, path-alias
  rejection, and restore-envelope identity rejection;
- sandbox unit tests: 50 passed, with 3 host-capability tests ignored; the new
  retry, duplicate-home, and restore-envelope negative tests passed;
- bwrap integration tests compiled and remained ignored on the build host for
  execution on the single-node VM;
- WASM with `--features wasm`: 57 unit tests, 62 acceptance tests, and 2
  NanoRuntime conformance tests passed. Tool-name collisions, shared-component
  reference cleanup, removed-source unload, duplicate workload ids, and
  restore-envelope identity rejection are covered.

## Prior single-node runtime diagnostics

The following diagnostics were captured from the initial PR candidate before
the final adversarial corrections. They remain useful operational lineage, but
the final candidate was not deployed or executed on a VM without ORC approval.

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
before the retained diagnostic above.

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
| AC-1 | PASS | Version-1 `NanoStopResult` uses a closed `stopped` / `already_stopped` outcome. Per-incarnation UUIDs reject stale, legacy-nil, and rewritten active handles; first stop, replay, and JSON wire tests pass. |
| AC-2 | PASS | All four adapters implement the shared two-workload contract and reject active duplicate ownership. Exact-source remote suites passed; initial-candidate bwrap and ECS-native diagnostics are separately identified above. |
| AC-3 | PASS | Exact-source unit coverage proves checked process reap, retry-safe CAS/cgroup/marker cleanup, ownership retention after partial failure, duplicate-home rejection, and second-workload preservation. The zero process/cgroup/marker readback above belongs to the initial candidate. |
| AC-4 | PASS | The exact-source fixture uses a real Unix socket; Firecracker is reaped, cleanup failures retain ownership for retry, API/vsock paths are removed, path aliases and mismatched restore envelopes fail closed, and retained snapshots survive stop. |
| AC-5 | PASS | ECS-native removes only addressed runtime/ECS state. WASM exact-feature tests prove last-reference tool/component unload, shared-reference preservation, duplicate and tool-collision rejection, and mismatched restore-envelope rejection. |
| AC-6 | NOT VERIFIED | The compile-time production-daemon registry integration belongs to #472 and is intentionally outside this patch. #698 must remain open until #472 supplies that evidence against the merged contract. |

No final-delta service restart, cluster access, deployment, or provider call
occurred.
