# Issue #279 Verification

## Phase 0 - Issue-Repair und Baseline-Reset

Date: 2026-04-23  
Worktree: `/work/company/project-sentinel-279-review`  
Branch: `feat/issue-279-daemon-hardening-v2`  
Base: `origin/main @ 83ab01c8835804fd951e619aa048324b7ff76ddf`

### AC-1 - Fresh Pre-Reset VM Baseline

Command target: `ubuntu@10.0.0.240`

```text
timestamp=2026-04-23T09:46:15+00:00
kernel=6.17.0-20-generic
services_begin
active
active
active
active
services_end
api_agents=57
projection_agents=57
cgroup_dirs=59
runtime_health_http=200
runtime_health.expected_active_agents=26
runtime_health.runtime_agents=26
runtime_health.projection_agents=57
runtime_health.live_cgroup_dirs=29
runtime_health.stale_runtime_entries=0
runtime_health.orphan_cgroups=33
runtime_health.zombie_tracked_pids=0
runtime_health.projection_drift_detected=True
runtime_health.analysis_queue_depth=0
runtime_health.analysis_queue_dropped_total=3
runtime_health.analysis_queue_coalesced_total=0
panic_count_30m=0
drift_count_30m=0
error_count_30m=26
```

PASS: The VM was service-active but drifted. API and Projection exposed `57`, while the experimental runtime-health endpoint expected `26` and reported `33` orphan cgroups.

### AC-2 - GitHub Issue Body Repair

Command:

```bash
gh issue edit 279 --repo silentspike/project-sentinel --body-file docs/issue-279-body.md
gh issue view 279 --repo silentspike/project-sentinel --json state,labels,updatedAt,body
```

Observed:

```text
issue_url=https://github.com/silentspike/project-sentinel/issues/279
state=OPEN
updatedAt=2026-04-23T09:47:19Z
labels include status:in-progress
body now contains fresh 2026-04-23 baseline, clean-main branch rule, Haiku/model policy out-of-scope, and strict status:verified close rule
```

PASS: Stale `Blocked by #278`, old 2026-04-21 numbers and #264-stack assumptions were removed from the GitHub SSOT.

### AC-3 - Canonical Main Build And Deploy

Remote build commands:

```bash
cargo remote -c -- build -p sentinel-daemon --release --features fuse
cargo remote -c -- build -p sentinel-projection-service --release
```

Observed:

```text
sentinel-daemon: Finished release profile [optimized] target(s) in 12m 04s
sentinel-projection-service: Finished release profile [optimized] target(s) in 1m 37s
target/release/sentinel-daemon sha256=2b31820c751c6f3dd0eb8e1282e827d64c5b2d9b016bf56fcede4c765ee86883
target/release/sentinel-projection sha256=10b845881d24ed0c661ba5a18de4ebddad44d404d15f0231ef1ce83a83855ce3
```

ExecStart check:

```text
# /etc/systemd/system/sentinel-daemon.service
ExecStart=/opt/sentinel/bin/sentinel-daemon --config /opt/sentinel/config/daemon.toml
# /etc/systemd/system/sentinel-projection.service
ExecStart=/opt/sentinel/bin/sentinel-projection \
  --event-store /opt/sentinel/data/events.db \
  --projection-db /opt/sentinel/data/projection.db
```

Deploy observed:

```text
deploy_ts=20260423T100300Z
2b31820c751c6f3dd0eb8e1282e827d64c5b2d9b016bf56fcede4c765ee86883  /tmp/sentinel-daemon
10b845881d24ed0c661ba5a18de4ebddad44d404d15f0231ef1ce83a83855ce3  /tmp/sentinel-projection
services_begin
active
active
active
active
services_end
2b31820c751c6f3dd0eb8e1282e827d64c5b2d9b016bf56fcede4c765ee86883  /opt/sentinel/bin/sentinel-daemon
10b845881d24ed0c661ba5a18de4ebddad44d404d15f0231ef1ce83a83855ce3  /opt/sentinel/bin/sentinel-projection
```

PASS: Canonical `main` daemon and Projection artifacts were built through the remote Rust builder, installed at the exact systemd `ExecStart` paths, and restarted successfully.

### AC-4 - Post-Reset Baseline Without Runtime-Health

Command target: `ubuntu@10.0.0.240`

First sample:

```text
timestamp=2026-04-23T10:03:58+00:00
services_begin
active
active
active
active
services_end
api_agents=57
projection_agents=57
cgroup_dirs=59
cgroup_live_procs_dirs=29
cgroup_live_threads_dirs=29
runtime_health_http=404
{"error":"Endpoint unbekannt"}
daemon_pid=1473664
panic_count_since_deploy=0
drift_count_since_deploy=0
error_count_since_deploy=29
```

Second sample:

```text
timestamp=2026-04-23T10:04:19+00:00
api_agents=57
projection_agents=57
cgroup_dirs=59
cgroup_live_procs_dirs=29
runtime_health_http=404
projection_pid=1473941
```

PASS: The canonical-main reset is measurable without `/operator/runtime-health`, and `runtime_health_http=404` is expected for current `main`.

Finding: Canonical `main` does not self-heal the drift. API and Projection remain at `57`, while only `29` cgroup directories have live processes. Therefore the full #279 recovery scope remains required.

## Phase 1 - Donor-Audit und Clean Port

Date: 2026-04-23

### AC-1 - Donor Commits Visible And Classified

Commands:

```bash
git -C /work/company/project-sentinel-issue279 log --oneline --decorate --reverse origin/main..HEAD
git -C /work/company/project-sentinel-issue279-stack log --oneline --decorate --reverse origin/main..HEAD
git -C /work/company/project-sentinel-279-review range-diff origin/main...feat/issue-279-daemon-hardening-stack
git -C /work/company/project-sentinel-279-review diff --stat origin/main..feat/issue-279-daemon-hardening-stack
```

Observed donor candidates:

```text
old donor includes:
12b5138 Task [1]: Issue-Body-/Label-Repair vor Code fuer #279
1af0d39 Task [2]: Build runtime health snapshot and operator endpoint
d673603 Task [3]: Add runtime reconcile control path for #279
76a20fe Task [4]: Add fast-path stall recovery and test hook for #279
28348eb Task [5]: Add in-process worker supervision and panic test for #279
a7ee414 Task [6]: Bound analysis queues and add flood test for #279
9b50cb6 Task [7]: Add projection drift healing for #279
dbb1b8e Task [8]: Pin agent LLM requests to haiku for #279
dbab8fb Task [9]: Add #279 bench and soak harness
300ded0 Fix [279]: Restore live agent runtime under FUSE and Landlock
b71e1e1 Fix [279]: stabilize projection recovery and verify daemon hardening

stack donor includes:
3c596c1 Task [1]: Issue-Body-/Label-Repair vor Code fuer #279
29a0fb5 Task [2]: Build runtime health snapshot and operator endpoint
146ef43 Task [3]: Add runtime reconcile control path for #279
fdc40c7 Task [4]: Add fast-path stall recovery and test hook for #279
6919e66 Task [5]: Add in-process worker supervision and panic test for #279
389b5e6 Task [6]: Bound analysis queues and add flood test for #279
14560c4 Task [7]: Add projection drift healing for #279
ffca58b Task [9]: Add #279 bench and soak harness
6c31975 Fix [279]: restore runtime consistency after FUSE and Landlock recovery
b1e376b Fix [279]: stabilize projection recovery and verify daemon hardening
4600532 Task [11]: Refresh stack evidence for #279
```

PASS: The old donor and the stack donor are visible. The stack donor is the safer source because it already removed the Haiku commit, but it is still not mergeable as a branch.

### AC-2 - Out-of-Scope Material Excluded

Rejected for #279:

```text
dbb1b8e / llm_bridge.rs Haiku pin:
- out-of-scope per current #279 body
- violates gateway/model-policy separation for this issue

direct branch merge:
- rejected because the donor diff still spans 38 files and includes stacked/history artifacts

donor evidence and progress files:
- donor PROGRESS.md is stale
- donor test-279-verification.md is old VM evidence, not current proof
- test-264-verification.md deletion is not part of #279

broad non-#279 paths:
- .gitignore
- deploy/systemd/sentinel-daemon.service unless a concrete runtime need appears
- crates/sentinel-common/src/snapshot_codec.rs
- crates/sentinel-common/src/types.rs
- crates/sentinel-common/tests/snapshot_roundtrip.rs
- crates/sentinel-fs/src/metadata.rs
- crates/sentinel-sandbox/src/bin/breakout_helper.rs
- crates/sentinel-sandbox/src/bwrap.rs
- crates/sentinel-sandbox/src/enforcer.rs
- crates/sentinel-sandbox/tests/breakout.rs
```

Conditional, not rejected:

```text
6c31975 / former 300ded0:
- keep as Slice G candidate only
- allowed paths if needed:
  - crates/sentinel-fs/src/fuse.rs
  - crates/sentinel-sandbox/src/landlock.rs
  - services/sentinel-daemon/src/orchestrator.rs
```

PASS: Haiku and stale stacked scope are explicitly excluded before any port.

### AC-3 - Clean Port Order

Port order:

```text
Task 3 / Slice A:
  donor 29a0fb5, path-limited runtime health endpoint and snapshot only

Task 4 / Slice B:
  donor 146ef43, runtime_control and reconcile paths

Task 5 / Slice C:
  donor fdc40c7, fast stall recovery and deterministic test hook

Task 6 / Slice D:
  donor 6919e66, in-process worker supervision and panic-test

Task 7 / Slice E:
  donor 389b5e6, bounded analysis/recovery queues and flood-test

Task 8 / Slice F:
  donor 14560c4 plus relevant parts of b1e376b, projection convergence and rebuild request

Task 9 / Slice G:
  donor 6c31975 only if current main plus ported #279 still requires FUSE/Landlock runtime restore

Task 14 / Benchmarks:
  donor ffca58b plus updated current evidence, not old results
```

Clean-port rule:

```text
Do not merge or cherry-pick the donor branch wholesale.
Apply donor code path-limited by slice.
Do not port llm_bridge.rs Haiku changes.
Do not port stale donor PROGRESS/evidence as proof.
Refresh CHANGELOG and verification evidence in this branch only.
```

PASS: The implementation path is now deterministic and scope-clean.

## Phase 2 / Slice A - Runtime-Health-Read-Model

Date: 2026-04-23

### AC-1 - Runtime-Health Snapshot Model

Code scope:

```text
services/sentinel-daemon/src/runtime_health.rs
services/sentinel-daemon/src/orchestrator.rs
services/sentinel-daemon/src/service_health.rs
services/sentinel-daemon/Cargo.toml
Cargo.lock
```

Observed implementation:

```text
RuntimeHealthSnapshot fields include:
- current_shift
- expected_active_agents
- runtime_agents
- projection_agents
- security_runtime_entries
- sandbox_handles
- tracked_processes
- live_cgroup_dirs
- stale_runtime_entries
- orphan_cgroups
- zombie_tracked_pids
- worker_states
- analysis_queue_depth
- analysis_queue_dropped_total
- analysis_queue_coalesced_total
- reconcile counters
- operator_auth_required
- per-agent runtime/projection/cgroup/security details
```

PASS: The snapshot is read-only and composes runtime, Projection, cgroup, security-runtime and worker-state truth without adding repair behavior in Slice A.

### AC-2 - Operator Endpoint And Auth Wiring

Code scope:

```text
services/sentinel-daemon/src/operator_api.rs
services/sentinel-daemon/src/orchestrator.rs
```

Observed implementation:

```text
GET /operator/runtime-health
protected by is_protected_read_path()
uses existing shared-secret authorization rule when configured
returns RuntimeHealthSnapshot from SharedRuntimeHealthState
```

Conflict resolution:

```text
operator_api.rs donor conflict resolved by keeping current main open_fs_layer()
and adding only is_protected_read_path() for /operator/runtime-health.
Donor open_artifact_plane() was not ported into Slice A.
```

PASS: The endpoint follows the existing Operator API read-path/auth structure and does not introduce a parallel control path.

### AC-3 - Remote Tests And Release Build

Remote test command:

```bash
cargo remote -c -- test -p sentinel-daemon runtime_health -- --nocapture
```

Observed:

```text
Finished test profile [unoptimized + debuginfo] target(s) in 7.36s
running 3 tests
test runtime_health::tests::build_snapshot_marks_missing_projection_and_security_as_stale ... ok
test operator_api::tests::runtime_health_endpoint_returns_snapshot ... ok
test operator_api::tests::runtime_health_endpoint_requires_auth_when_secret_is_set ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 173 filtered out; finished in 10.28s
```

Initial warning found and fixed:

```text
unused import: crate::runtime_health::RuntimeHealthSnapshot
fix: moved RuntimeHealthSnapshot import into operator_api::tests
```

Remote release build command:

```bash
cargo remote -c -- build -p sentinel-daemon --release --features fuse
```

Observed:

```text
Finished release profile [optimized] target(s) in 1m 07s
7b5145a77da0a55a3e9e9da00b2d9036ce943df748d327b4095f87955b0034d2  target/release/sentinel-daemon
```

Static hygiene:

```bash
git diff --check
```

Observed:

```text
no output
```

PASS: Slice A tests and release build are green via remote Rust execution only.

## Phase 3 - Slice B Runtime-Control / Reconcile Evidence

### AC-1 - Runtime-Control Types And Backoff

Code scope:

```text
services/sentinel-daemon/src/runtime_control.rs
services/sentinel-daemon/src/lib.rs
```

Observed implementation:

```text
RuntimeReconcileRequest { dry_run, projection_rebuild, respawn_missing }
RuntimeReconcileResponse with stale/orphan/respawn/projection counters
RuntimeControlCommand::Reconcile
RespawnBackoffTracker::new(3) with 1/2 tick backoff, then Blocked
```

PASS: Runtime-control is explicit, typed and bounded.

### AC-2 - Operator Runtime-Reconcile Endpoint

Code scope:

```text
services/sentinel-daemon/src/operator_api.rs
services/sentinel-daemon/src/orchestrator.rs
```

Observed implementation:

```text
POST /operator/runtime/reconcile
dispatches RuntimeControlCommand::Reconcile to the ECS thread
waits with recv_timeout(10s)
returns RuntimeReconcileResponse
```

PASS: The Operator API does not mutate runtime state directly; it dispatches into the ECS owner thread.

### AC-3 - Runtime Repair Semantics

Code scope:

```text
services/sentinel-daemon/src/orchestrator.rs
```

Observed implementation:

```text
remove_agent_runtime_fragments()
run_runtime_reconcile()
repair_blocked PlatformIntervention event
.projection-rebuild-request file via runtime_control::write_projection_rebuild_request()
```

PASS: Reconcile can clean unexpected runtime fragments, orphan cgroups and stale security snapshots, and it can respawn missing expected agents with bounded retries.

### AC-4 - Conflict Resolution

Observed conflict decision:

```text
orchestrator.rs donor conflict resolved by keeping the current suspend_pids()
implementation with 2s multi-PID verification from the #264/main lineage.
The donor's narrower 250ms tracked-PID-only check was not ported.
```

PASS: Slice B does not regress the stronger live-process stop verification.

### AC-5 - Remote Tests And Release Build

Remote test command:

```bash
cargo remote -c -- test -p sentinel-daemon runtime_control -- --nocapture
```

Observed:

```text
Finished test profile [unoptimized + debuginfo] target(s) in 26.37s
running 2 tests
test runtime_control::tests::respawn_backoff_tracker_applies_exponential_backoff_until_blocked ... ok
test runtime_control::tests::write_projection_rebuild_request_persists_request_file ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 177 filtered out; finished in 0.00s
```

Remote endpoint-dispatch test command:

```bash
cargo remote -c -- test -p sentinel-daemon runtime_reconcile -- --nocapture
```

Observed:

```text
Finished test profile [unoptimized + debuginfo] target(s) in 11.70s
running 1 test
test operator_api::tests::runtime_reconcile_is_forwarded_and_returns_response ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 178 filtered out; finished in 0.61s
```

Remote release build command:

```bash
cargo remote -c -- build -p sentinel-daemon --release --features fuse
```

Observed:

```text
Finished release profile [optimized] target(s) in 1m 00s
19ce8b228b5588142399a4f712afd4cd89b1caca2be43c1273355a3ad30fe724  target/release/sentinel-daemon
```

PASS: Slice B tests and release build are green via remote Rust execution only.

## Phase 4 - Slice C Fast Stall Recovery Evidence

Date: 2026-04-23

### AC-1 - Runtime Stall-Restart Command And Operator Hook

Code scope:

```text
services/sentinel-daemon/src/runtime_control.rs
services/sentinel-daemon/src/operator_api.rs
services/sentinel-daemon/src/orchestrator.rs
```

Observed implementation:

```text
RuntimeControlCommand::StallRestartTest
RuntimeStallRestartTestRequest { agent_id, mode, stall_secs }
RuntimeStallRestartTestResponse { pid_before, pid_after, runtime_present_after, security_runtime_present_after, note }
POST /operator/runtime/stall-restart-test
```

PASS: The test hook is typed, input-validated, routed through the existing Operator API path, and dispatched into the ECS owner thread.

### AC-2 - Fast-Restart Runtime Semantics

Code scope:

```text
services/sentinel-daemon/src/orchestrator.rs
```

Observed implementation:

```text
restart_agent_fast_path()
tracked_pid_for_agent()
PlatformSideEffect::RestartAgent => restart_agent_fast_path(...)
```

PASS: Restart now removes old runtime, sandbox, eBPF and security fragments before immediately respawning the configured agent in the same tick path.

### AC-3 - Formatting And Remote Tests

Remote format command:

```bash
cargo remote -c -- fmt --check
```

Observed:

```text
PASS after applying remote rustfmt diffs locally.
```

Remote Operator API test command:

```bash
cargo remote -c -- test -p sentinel-daemon runtime_stall_restart -- --nocapture
```

Observed:

```text
Finished test profile [unoptimized + debuginfo] target(s) in 21.36s
running 2 tests
test operator_api::tests::runtime_stall_restart_test_is_forwarded_and_returns_response ... ok
test operator_api::tests::runtime_stall_restart_test_rejects_invalid_mode ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 180 filtered out; finished in 1.49s
```

Remote fast-path unit test command:

```bash
cargo remote -c -- test -p sentinel-daemon restart_agent_fast_path -- --nocapture
```

Observed:

```text
Finished test profile [unoptimized + debuginfo] target(s) in 0.41s
running 1 test
bwrap: Can't find source path /work/company: No such file or directory
bwrap: Can't find source path /work/company: No such file or directory
test orchestrator::tests::test_restart_agent_fast_path_recreates_runtime_and_security_state ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 181 filtered out; finished in 0.14s
```

PASS: Slice C behavior is covered by remote Rust tests only. The `bwrap` messages are build-server fixture path warnings and did not fail the test.

### AC-4 - Release Build And Artifact Hash

Remote release build command:

```bash
cargo remote -c -- build -p sentinel-daemon --release --features fuse
```

Observed:

```text
Finished release profile [optimized] target(s) in 56.20s
3470152e8b897217e2d2e547549b8f6759b3e733ec53d8a3ea8e00745da8d2bd  target/release/sentinel-daemon
```

Whitespace check command:

```bash
git diff --check
```

Observed:

```text
PASS: no output, exit 0
```

PASS: Slice C builds with the FUSE feature and has a reproducible release artifact hash.

## Phase 5 - Slice D Worker-Supervision Evidence

Date: 2026-04-23

### AC-1 - In-Process Worker Supervision

Code scope:

```text
services/sentinel-daemon/src/service_health.rs
```

Observed implementation:

```text
ServiceHealthChecker::spawn()
panic::catch_unwind(AssertUnwindSafe(...))
ServiceHealthControl::PanicTest
ServiceHealthWorkerExit
restart_count
last_error
running
thread_name
```

PASS: `service_health` panics are contained inside the worker thread, recorded in the worker snapshot, and followed by an in-process worker restart rather than a daemon-process exit.

### AC-2 - Runtime Panic-Test Operator Hook

Code scope:

```text
services/sentinel-daemon/src/runtime_control.rs
services/sentinel-daemon/src/operator_api.rs
services/sentinel-daemon/src/orchestrator.rs
```

Observed implementation:

```text
RuntimeControlCommand::PanicTest
RuntimePanicTestRequest { worker }
RuntimePanicTestResponse { accepted, worker, note }
POST /operator/runtime/panic-test
```

PASS: The hook is typed, input-validated, routed through the existing Operator API path, and dispatched into the ECS owner thread. Only `worker=service_health` is accepted.

### AC-3 - Remote Tests

Remote Operator API test command:

```bash
cargo remote -c -- test -p sentinel-daemon runtime_panic_test -- --nocapture
```

Observed:

```text
Finished test profile [unoptimized + debuginfo] target(s) in 21.56s
running 2 tests
test operator_api::tests::runtime_panic_test_rejects_invalid_worker ... ok
test operator_api::tests::runtime_panic_test_is_forwarded_and_returns_response ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 183 filtered out; finished in 0.07s
```

Remote worker-supervision test command:

```bash
cargo remote -c -- test -p sentinel-daemon panic_test_restarts_worker_in_process -- --nocapture
```

Observed:

```text
Finished test profile [unoptimized + debuginfo] target(s) in 0.42s
running 1 test
thread 'service-health-checker' panicked at services/sentinel-daemon/src/service_health.rs:163:17:
panic-test requested for service_health
test service_health::tests::panic_test_restarts_worker_in_process ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 184 filtered out; finished in 0.25s
```

PASS: The panic/backtrace is the intentional test stimulus. The test passes because `catch_unwind` records the panic and restarts the worker in-process.

### AC-4 - Release Build And Artifact Hash

Remote format command:

```bash
cargo remote -c -- fmt --check
```

Observed:

```text
PASS: no remaining rustfmt diff.
```

Remote release build command:

```bash
cargo remote -c -- build -p sentinel-daemon --release --features fuse
```

Observed:

```text
Finished release profile [optimized] target(s) in 57.79s
4400fcb9162fe242f3cbebda550f25175976abc8f444475ab201d0570e43c3d2  target/release/sentinel-daemon
```

Whitespace check command:

```bash
git diff --check
```

Observed:

```text
PASS: no output, exit 0
```

PASS: Slice D builds with the FUSE feature and has a reproducible release artifact hash.

## Phase 6 - Slice E Bounded Analysis-/Recovery-Pfade Evidence

Date: 2026-04-23

### AC-1 - Bounded Platform-Controlplane Trigger Queue

Code scope:

```text
services/sentinel-daemon/src/platform_controlplane/mod.rs
config/daemon.toml
services/sentinel-daemon/src/config.rs
```

Observed implementation:

```text
llm_trigger_queue_capacity = 16
queued_analysis_triggers: VecDeque<QueuedAnalysisTrigger>
analysis_queue_dropped_total
analysis_queue_coalesced_total
enqueue_analysis_trigger()
analysis_queue_stats()
```

Remote test command:

```bash
cargo remote -c -- test -p sentinel-daemon trigger_queue -- --nocapture
```

Observed:

```text
running 2 tests
test platform_controlplane::tests::test_manual_trigger_queue_coalesces_duplicates ... ok
test platform_controlplane::tests::test_test_trigger_queue_drops_when_capacity_is_exceeded ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 188 filtered out
```

PASS: The pre-worker trigger queue is bounded, coalesces duplicate triggers and drops oldest entries when capacity is exceeded.

### AC-2 - Bounded LLM Analyzer Channel

Code scope:

```text
services/sentinel-daemon/src/platform_controlplane/llm_analyzer.rs
```

Observed implementation:

```text
llm_analysis_channel_capacity = 16
PlatformLlmAnalyzerHandle::queue_stats()
try_send()
TrySendError::Full
dropped_total
depth
```

Remote test command:

```bash
cargo remote -c -- test -p sentinel-daemon enqueue_drops_when_bounded_queue_is_full -- --nocapture
```

Observed:

```text
running 1 test
test platform_controlplane::llm_analyzer::tests::enqueue_drops_when_bounded_queue_is_full ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 189 filtered out
```

PASS: Analyzer enqueue uses bounded `try_send()` semantics and records drops when the bounded channel is full.

### AC-3 - Runtime-Health Queue Stats

Code scope:

```text
services/sentinel-daemon/src/runtime_health.rs
services/sentinel-daemon/src/orchestrator.rs
```

Observed implementation:

```text
publish_runtime_health_snapshot(..., analysis_queue_stats)
analysis_queue_depth
analysis_queue_dropped_total
analysis_queue_coalesced_total
```

PASS: Runtime-health snapshots receive merged Platform-Controlplane and LLM-Analyzer queue stats, so VM verification can read current depth/drop/coalescing state from the runtime truth surface after deploy.

### AC-4 - Deterministic Analysis Flood Test Hook

Code scope:

```text
services/sentinel-daemon/src/runtime_control.rs
services/sentinel-daemon/src/operator_api.rs
services/sentinel-daemon/src/orchestrator.rs
```

Observed implementation:

```text
RuntimeControlCommand::AnalysisFloodTest
RuntimeAnalysisFloodTestRequest { count }
RuntimeAnalysisFloodTestResponse { accepted, requested, queue_depth, dropped_total, coalesced_total, note }
POST /operator/runtime/analysis-flood-test
```

Remote Operator API test command:

```bash
cargo remote -c -- test -p sentinel-daemon runtime_analysis_flood_test -- --nocapture
```

Observed:

```text
running 2 tests
test operator_api::tests::runtime_analysis_flood_test_rejects_zero_count ... ok
test operator_api::tests::runtime_analysis_flood_test_is_forwarded_and_returns_response ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 188 filtered out; finished in 0.06s
```

PASS: The flood hook is typed, validates `count > 0`, dispatches through Runtime-Control into the ECS owner thread and returns queue-pressure counters.

### AC-5 - Format, Clippy, Build, Artifact And Scope Guard

Remote config test command:

```bash
cargo remote -c -- test -p sentinel-daemon test_parse_config -- --nocapture
```

Observed:

```text
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 188 filtered out
```

Remote clippy command:

```bash
cargo remote -c -- clippy -p sentinel-daemon --all-targets --features fuse -- -D warnings
```

Observed:

```text
Finished dev profile [unoptimized + debuginfo] target(s) in 2m 59s
```

Remote release build command:

```bash
cargo remote -c -- build -p sentinel-daemon --release --features fuse
```

Observed:

```text
Finished release profile [optimized] target(s) in 56.44s
39bf79442190541ec03f11a66c4e8be437f6a8bd1f2e2366611a0d9ad0366cb2  target/release/sentinel-daemon
```

Whitespace check command:

```bash
git diff --check
```

Observed:

```text
PASS: no output, exit 0
```

Scope guard command:

```bash
rg -n "AGENT_MODEL_HAIKU|model: AGENT|model: String::new\\(\\).*Gateway" services/sentinel-daemon/src cmd crates || true
```

Observed:

```text
services/sentinel-daemon/src/llm_bridge.rs:612:            model: String::new(), // Gateway waehlt default
```

PASS: Slice E builds, lints and keeps model selection out of the Daemon. The only runtime model-related match is the intended Gateway-default path.

## Phase 7 - Slice F Projection/API-Konvergenz Evidence

Date: 2026-04-23

### AC-1 - Projection Rebuild Request File

Code scope:

```text
crates/sentinel-projection/src/config.rs
crates/sentinel-projection/src/worker.rs
services/sentinel-projection/src/main.rs
```

Observed implementation:

```text
ProjectionConfig::rebuild_request_path
ProjectionConfig::rebuild_request_poll_interval
ProjectionWorker::handle_rebuild_request_if_present()
```

Remote test command:

```bash
cargo remote -c -- test -p sentinel-projection rebuild_request -- --nocapture
```

Observed:

```text
test worker::tests::rebuild_request_file_triggers_full_rebuild_and_is_removed ... ok
test result: ok. 1 passed; 0 failed
```

PASS: The Projection worker consumes the request file, triggers a full rebuild and removes the request file after successful processing.

### AC-2 - Read-Only Projection Reads For Runtime-Health

Code scope:

```text
crates/sentinel-projection/src/store.rs
services/sentinel-daemon/src/runtime_health.rs
```

Observed implementation:

```text
ReadModelStore::open_readonly(path)
OpenFlags::SQLITE_OPEN_READ_ONLY
build_snapshot() uses ReadModelStore::open_readonly()
```

Remote test command:

```bash
cargo remote -c -- test -q -p sentinel-projection -- --nocapture
```

Observed:

```text
test store::tests::test_open_readonly_reads_existing_projection_without_writes ... ok
test result: ok. 7 passed; 0 failed
test result: ok. 6 passed; 0 failed
```

PASS: Runtime-health can read Projection state through a read-only connection without running migrations or startup cleanup.

### AC-3 - Projection Batch Error Rollback

Code scope:

```text
crates/sentinel-projection/src/worker.rs
```

Observed implementation:

```text
txn.rollback()
Handler error, aborting batch
handler_error_rolls_back_batch_and_returns_err
```

Remote test command:

```bash
cargo remote -c -- test -q -p sentinel-projection -- --nocapture
```

Observed:

```text
test worker::tests::handler_error_rolls_back_batch_and_returns_err ... ok
```

PASS: Handler failures abort and rollback the batch instead of partially applying events.

### AC-4 - Runtime-Health Projection Drift Detection

Code scope:

```text
services/sentinel-daemon/src/runtime_health.rs
```

Observed implementation:

```text
projection_drift_detected
projection_drift_agents
ReadModelStore::open_readonly()
```

Remote test command:

```bash
cargo remote -c -- test -q -p sentinel-daemon runtime_health -- --nocapture
```

Observed:

```text
test runtime_health::tests::build_snapshot_marks_missing_projection_and_security_as_stale ... ok
test runtime_health::tests::build_snapshot_prefers_latest_service_health_restart_count ... ok
test result: ok. 4 passed; 0 failed
```

PASS: Runtime-health reports Projection drift against runtime/security/cgroup truth and preserves latest worker restart state.

### AC-5 - Reconcile Requests Rebuild Without Restart Storm

Code scope:

```text
services/sentinel-daemon/src/runtime_control.rs
services/sentinel-daemon/src/orchestrator.rs
services/sentinel-daemon/src/service_health.rs
```

Observed implementation:

```text
write_projection_rebuild_request(data_dir, tick, reason)
projection_drift_before
projection_restart_attempted
projection_restart_succeeded
projection service active -> no restart
```

Remote test commands:

```bash
cargo remote -c -- test -q -p sentinel-daemon runtime_control -- --nocapture
cargo remote -c -- test -q -p sentinel-daemon test_runtime_reconcile_skips_projection_restart_when_rebuild_can_run_in_place -- --nocapture
```

Observed:

```text
runtime_control: test result: ok. 2 passed; 0 failed
test orchestrator::tests::test_runtime_reconcile_skips_projection_restart_when_rebuild_can_run_in_place ... ok
test result: ok. 1 passed; 0 failed
```

PASS: Runtime-reconcile writes a rebuild request for Projection drift and deliberately avoids restarting an already active Projection service.

### AC-6 - Format, Clippy, Build, Artifact And Scope Guard

Remote quality gate commands:

```bash
cargo remote -c -- fmt --check
cargo remote -c -- clippy -q -p sentinel-projection --all-targets -- -D warnings
cargo remote -c -- clippy -q -p sentinel-projection-service --all-targets -- -D warnings
cargo remote -c -- clippy -q -p sentinel-daemon --all-targets --features fuse -- -D warnings
cargo remote -c -- build -q -p sentinel-daemon --release --features fuse
cargo remote -c -- build -q -p sentinel-projection-service --release
```

Observed:

```text
fmt: PASS
sentinel-projection clippy: PASS exit 0
sentinel-projection-service clippy: PASS exit 0
sentinel-daemon clippy --features fuse: PASS exit 0
sentinel-daemon release build --features fuse: PASS exit 0
sentinel-projection-service release build: PASS exit 0
3a05fc872c061ae973e3d820e0a1aaa2d0a909b201d61e9b0c8c84b080c08425  target/release/sentinel-daemon
d0da3c42b11397e1c954777397c4b3622cfeeb4365fff2adebbded9adacb13b4  target/release/sentinel-projection
```

Scope guard command:

```bash
rg -n "AGENT_MODEL_HAIKU|model: AGENT|model: String::new\\(\\).*Gateway" services/sentinel-daemon/src cmd crates || true
```

Expected/observed allowed match:

```text
services/sentinel-daemon/src/llm_bridge.rs:612:            model: String::new(), // Gateway waehlt default
```

PASS: Slice F builds, lints and keeps model selection out of the Daemon. The Projection-side release artifact is explicitly built for later VM deploy evidence.

## Phase 8 - Slice G Conditional FUSE/Landlock Runtime Restore Evidence

Date: 2026-04-23
Host: remote build server via `cargo remote -c --`
Scope: `services/sentinel-daemon/src/orchestrator.rs`, `crates/sentinel-sandbox/src/landlock.rs`

### AC-1 - FUSE Mount Is Not Trusted Until Active

Code scope:

```text
mountinfo_contains_mountpoint()
mountpoint_is_active()
wait_for_fuse_mount()
active_fs_mount
```

Remote test command:

```bash
cargo remote -c -- test -q -p sentinel-daemon mountinfo_contains_mountpoint_matches_exact_sentinel_fuse_mount --features fuse -- --nocapture
```

Observed:

```text
PASS: targeted FUSE mountinfo test passed
```

PASS: The daemon recognizes only an exact `sentinel-fs` FUSE mount as active, not a configured path by itself.

### AC-2 - Inactive FUSE Falls Back To `/ram/agents`

Code scope:

```text
active_fs_mount: Option<String>
sandbox.set_fs_mount(active_fs_mount)
operator_api::start_server(..., active_fs_mount.clone(), ...)
ecs_fs_mount = active_fs_mount.clone()
```

Observed implementation:

```text
if wait_for_fuse_mount(...) { active_mount = Some(fs_mount.clone()) }
else { warn!("sentinel-fs FUSE-Mount nicht aktiv, fallback auf /ram/agents") }
```

PASS: Sandbox, Operator-API and ECS receive `fs_mount` only when FUSE is active; otherwise the runtime keeps the default `/ram/agents` path.

### AC-3 - Landlock Execute Scope Stays Narrow

Remote test command:

```bash
cargo remote -c -- test -q -p sentinel-sandbox ruleset_for_agent_paths -- --nocapture
```

Observed:

```text
PASS: targeted Landlock ruleset test passed
```

PASS: The ruleset still rejects broad `/usr` execute allowlisting and allows only the runtime binaries plus loader paths.

### AC-4 - Full Sandbox Test Matrix

Remote test command:

```bash
cargo remote -c -- test -q -p sentinel-sandbox -- --nocapture
```

Observed:

```text
44 tests: 41 passed, 0 failed, 3 ignored
16 tests: 14 passed, 0 failed, 2 ignored
12 tests: 3 passed, 0 failed, 9 ignored
```

PASS: Existing sandbox behavior remains green after the Landlock loader-path change.

### AC-5 - sentinel-fs Fuse Feature Gate

Remote test command:

```bash
cargo remote -c -- test -q -p sentinel-fs --features fuse-tests -- --nocapture
```

Observed:

```text
93 tests: 93 passed, 0 failed
19 tests: 19 passed, 0 failed
2 tests: 0 passed, 0 failed, 2 ignored
```

PASS: The `sentinel-fs` crate builds and tests successfully with the FUSE feature path enabled; kernel-dependent FUSE tests remain explicitly ignored.

### AC-6 - Format, Clippy, Builds, Artifact And Scope Guard

Remote quality gate commands:

```bash
cargo remote -c -- fmt --check
cargo remote -c -- clippy -q -p sentinel-sandbox --all-targets -- -D warnings
cargo remote -c -- clippy -q -p sentinel-daemon --all-targets --features fuse -- -D warnings
cargo remote -c -- clippy -q -p sentinel-fs --all-targets --features fuse-tests -- -D warnings
cargo remote -c -- build -q -p sentinel-daemon --release --features fuse
cargo remote -c -- build -q -p sentinel-sandbox --release --bins
```

Observed:

```text
fmt: PASS
sentinel-sandbox clippy: PASS exit 0
sentinel-daemon clippy --features fuse: PASS exit 0
sentinel-fs clippy --features fuse-tests: PASS exit 0
sentinel-daemon release build --features fuse: PASS exit 0
sentinel-sandbox release bin build: PASS exit 0
517a9c6a14da0a4d4cd761b74cd7e37d1e2aaa9f24ec4329a7585f0c45638338  target/release/sentinel-daemon
a3d35bdf5261a617546a19a380f731dba89dc6f24327f4dca1eff4783a05a31d  target/release/landlock-wrapper
03abe24e93222b540bf36bbedf0d5c259780bea0f53c79386086c44d22e40be8  target/release/breakout-helper
```

Local non-build guards:

```bash
git diff --check
rg -n "AGENT_MODEL_HAIKU|model: AGENT|model: String::new\\(\\).*Gateway" services/sentinel-daemon/src cmd crates
```

Observed:

```text
git diff --check: PASS
services/sentinel-daemon/src/llm_bridge.rs:612:            model: String::new(), // Gateway waehlt default
```

PASS: Slice G builds, lints, keeps model selection out of the Daemon, and produces the release artifacts needed for VM deploy evidence.

## Phase 9 - Full Remote Quality Matrix Before VM Deploy

Date: 2026-04-23
Host: remote build server via `cargo remote -c --`
Scope: all #279 runtime-change crates plus conditional FUSE/Landlock slice

### AC-1 - Daemon Full Test Suite

Remote test command:

```bash
cargo remote -c -- test -q -p sentinel-daemon -- --nocapture
```

Observed:

```text
running 193 tests
test result: ok. 193 passed; 0 failed; 0 ignored; finished in 10.29s
expected controlled panic-test output:
panic-test requested for service_health
```

PASS: The full Daemon test suite is green. The panic-test output is expected and is caught by the worker-supervision path.

### AC-2 - Projection Test Suites

Remote test commands:

```bash
cargo remote -c -- test -q -p sentinel-projection -- --nocapture
cargo remote -c -- test -q -p sentinel-projection-service -- --nocapture
```

Observed:

```text
sentinel-projection:
test result: ok. 7 passed; 0 failed
test result: ok. 6 passed; 0 failed

sentinel-projection-service:
test result: ok. 0 passed; 0 failed
```

PASS: Projection library and service test targets are green before deploy.

### AC-3 - Clippy Matrix

Remote clippy commands:

```bash
cargo remote -c -- clippy -q -p sentinel-daemon --all-targets --features fuse -- -D warnings
cargo remote -c -- clippy -q -p sentinel-projection --all-targets -- -D warnings
cargo remote -c -- clippy -q -p sentinel-projection-service --all-targets -- -D warnings
cargo remote -c -- clippy -q -p sentinel-fs --all-targets --features fuse-tests -- -D warnings
cargo remote -c -- clippy -q -p sentinel-sandbox --all-targets -- -D warnings
```

Observed:

```text
sentinel-daemon clippy --features fuse: PASS exit 0
sentinel-projection clippy: PASS exit 0
sentinel-projection-service clippy: PASS exit 0
sentinel-fs clippy --features fuse-tests: PASS exit 0
sentinel-sandbox clippy: PASS exit 0
```

PASS: All touched Rust crates pass Clippy with `-D warnings`.

### AC-4 - Release Build Matrix

Remote build commands:

```bash
cargo remote -c -- build -q -p sentinel-daemon --release --features fuse
cargo remote -c -- build -q -p sentinel-projection-service --release
cargo remote -c -- build -q -p sentinel-sandbox --release --bins
```

Observed:

```text
sentinel-daemon release build --features fuse: PASS exit 0
sentinel-projection-service release build: PASS exit 0
sentinel-sandbox release bin build: PASS exit 0
```

Artifact hashes:

```text
517a9c6a14da0a4d4cd761b74cd7e37d1e2aaa9f24ec4329a7585f0c45638338  target/release/sentinel-daemon
d0da3c42b11397e1c954777397c4b3622cfeeb4365fff2adebbded9adacb13b4  target/release/sentinel-projection
a3d35bdf5261a617546a19a380f731dba89dc6f24327f4dca1eff4783a05a31d  target/release/landlock-wrapper
03abe24e93222b540bf36bbedf0d5c259780bea0f53c79386086c44d22e40be8  target/release/breakout-helper
```

PASS: Deployable release artifacts exist locally after remote builds.

### AC-5 - Conditional FUSE/Landlock Test Matrix

Remote test commands:

```bash
cargo remote -c -- test -q -p sentinel-fs --features fuse-tests -- --nocapture
cargo remote -c -- test -q -p sentinel-sandbox -- --nocapture
```

Observed:

```text
sentinel-fs:
93 tests: 93 passed, 0 failed
19 tests: 19 passed, 0 failed
2 tests: 0 passed, 0 failed, 2 ignored

sentinel-sandbox:
44 tests: 41 passed, 0 failed, 3 ignored
16 tests: 14 passed, 0 failed, 2 ignored
12 tests: 3 passed, 0 failed, 9 ignored
```

PASS: The conditional FUSE/Landlock slice remains green after the complete #279 port.

### AC-6 - Whitespace And Model-Scope Guards

Local guard commands:

```bash
git diff --check
rg -n "AGENT_MODEL_HAIKU|model: AGENT|model: String::new\\(\\).*Gateway" services/sentinel-daemon/src cmd crates
```

Observed:

```text
git diff --check: PASS
services/sentinel-daemon/src/llm_bridge.rs:612:            model: String::new(), // Gateway waehlt default
```

PASS: No whitespace/conflict errors. No Daemon-side Haiku/model-policy change was introduced.

## Phase 10 - VM Deploy, Projection Convergence And Cgroup Orphan Repair

Date: 2026-04-23
Command target: `ubuntu@10.0.0.240`
Scope: #279 runtime deploy after clean-port plus hotfix for stopped orphan cgroups found during live verification

### AC-1 - ExecStart And Artifact Deploy

ExecStart preflight:

```bash
ssh ubuntu@10.0.0.240 "systemctl cat sentinel-daemon | grep -n '^ExecStart'; systemctl cat sentinel-projection | grep -n '^ExecStart'; systemctl is-active sentinel-daemon sentinel-projection"
```

Observed:

```text
13:ExecStart=/opt/sentinel/bin/sentinel-daemon --config /opt/sentinel/config/daemon.toml
13:ExecStart=/opt/sentinel/bin/sentinel-projection \
active
active
```

Final deploy command:

```bash
ssh ubuntu@10.0.0.240 "sudo mkdir -p /opt/sentinel/backups/issue-279-20260423T142544Z && sudo cp /opt/sentinel/bin/sentinel-daemon /opt/sentinel/backups/issue-279-20260423T142544Z/sentinel-daemon && sudo systemctl stop sentinel-daemon"
scp target/release/sentinel-daemon ubuntu@10.0.0.240:/tmp/sentinel-daemon.issue279
ssh ubuntu@10.0.0.240 "sudo install -m 0755 /tmp/sentinel-daemon.issue279 /opt/sentinel/bin/sentinel-daemon && sudo systemctl start sentinel-daemon && sleep 2 && systemctl is-active sentinel-daemon && sha256sum /opt/sentinel/bin/sentinel-daemon"
```

Observed:

```text
active
d6c30a324d85cbb3ec9d919a14e5f5744482cda75e59984e6133944289129d86  /opt/sentinel/bin/sentinel-daemon
backup=/opt/sentinel/backups/issue-279-20260423T142544Z
```

PASS: The final Daemon artifact was installed at the exact systemd `ExecStart` path and restarted successfully.

### AC-2 - Projection-Only Drift Repair

Initial post-deploy runtime-health before reconcile:

```bash
ssh ubuntu@10.0.0.240 "/tmp/opcurl -s http://127.0.0.1:8084/operator/runtime-health | python3 -c 'import sys,json; h=json.load(sys.stdin); print(\"expected\",h.get(\"expected_active_agents\"),\"runtime\",h.get(\"runtime_agents\"),\"projection\",h.get(\"projection_agents\"),\"cgroups\",h.get(\"live_cgroup_dirs\"),\"stale\",h.get(\"stale_runtime_entries\"),\"orphans\",h.get(\"orphan_cgroups\"),\"zombies\",h.get(\"zombie_tracked_pids\"),\"drift\",h.get(\"projection_drift_detected\"))'"
```

Observed before the first reconcile round:

```text
expected 26 runtime 26 projection 57 cgroups 29 stale 31 orphans 3 zombies 0 drift True
api_agents=57
projection_active_rows=57
```

Reconcile command:

```bash
ssh ubuntu@10.0.0.240 "/tmp/opcurl -s -X POST http://127.0.0.1:8084/operator/runtime/reconcile -H 'Content-Type: application/json' -d '{\"dry_run\":false,\"projection_rebuild\":true,\"respawn_missing\":true}' | python3 -m json.tool"
```

Observed first reconcile:

```text
"accepted": true
"stale_agents_before": 31
"stale_agents_after": 0
"orphan_cgroups_before": 3
"unexpected_runtime_removed": 3
"projection_drift_before": true
"projection_drift_after": false
"projection_rebuild_requested": true
"repair_last_status": "projection_rebuild_requested"
```

PASS: Projection-only ghost agents are now visible to Runtime-Health and reconciled through `agent_despawned` plus Projection rebuild instead of being hidden behind an aggregate count.

### AC-3 - Stopped Orphan Cgroup Finding And Fix

Runtime gate after the first reconcile still failed on live Cgroups:

```text
expected 26 runtime 26 projection 26 cgroups 29 stale 0 orphans 3 zombies 0 drift False api_agents=26
```

Orphan inspection:

```bash
ssh ubuntu@10.0.0.240 "python3 - <<'PY'
import os
root='/sys/fs/cgroup/sentinel'
for name in sorted(os.listdir(root)):
    procs=open(os.path.join(root,name,'cgroup.procs')).read().strip().split()
    if procs:
        print(name, ','.join(procs))
PY"
ssh ubuntu@10.0.0.240 "ps -o pid,stat,cmd -p 1291960,1314755,1324475 || true"
```

Observed:

```text
Carla Mendez pids=1291960
Jonas Weber pids=1314755
Oliver Brandt pids=1324475

1291960 T /usr/bin/python3 -c ... /opt/sentinel/data/security-write-anomaly/AGENT-17/.issue264-write-anomaly.bin ...
1314755 T /usr/bin/python3 -c ... /opt/sentinel/data/security-write-anomaly/AGENT-22/.issue264-write-anomaly.bin ...
1324475 T /usr/bin/python3 -c ... /opt/sentinel/data/security-write-anomaly/AGENT-26/.issue264-write-anomaly.bin ...
```

Code fix:

```text
crates/sentinel-sandbox/src/cgroups.rs:
  kill_cgroup_processes(name) uses cgroup.kill when available and SIGKILL fallback otherwise.

services/sentinel-daemon/src/orchestrator.rs:
  runtime reconcile empties orphan cgroups before remove_cgroup().
```

Post-fix remote gates:

```bash
cargo remote -c -- fmt --all
cargo remote -c -- test -q -p sentinel-sandbox kill_nonexistent_cgroup_is_noop -- --nocapture
cargo remote -c -- test -q -p sentinel-daemon test_runtime_reconcile_skips_projection_restart_when_rebuild_can_run_in_place -- --nocapture
cargo remote -c -- test -q -p sentinel-daemon -- --nocapture
cargo remote -c -- test -q -p sentinel-sandbox -- --nocapture
cargo remote -c -- clippy -q -p sentinel-sandbox --all-targets -- -D warnings
cargo remote -c -- clippy -q -p sentinel-daemon --all-targets --features fuse -- -D warnings
cargo remote -c -- build -q -p sentinel-daemon --release --features fuse
cargo remote -c -- build -q -p sentinel-projection-service --release
```

Observed:

```text
sentinel-sandbox targeted test: 1 passed; 0 failed
sentinel-daemon targeted reconcile test: 1 passed; 0 failed
sentinel-daemon full test: 194 passed; 0 failed
sentinel-sandbox full test: 42 passed; 0 failed; 3 ignored
sentinel-sandbox full test: 14 passed; 0 failed; 2 ignored
sentinel-sandbox full test: 3 passed; 0 failed; 9 ignored
sentinel-sandbox clippy: PASS exit 0
sentinel-daemon clippy --features fuse: PASS exit 0
sentinel-daemon release build --features fuse: PASS exit 0
sentinel-projection-service release build: PASS exit 0
d6c30a324d85cbb3ec9d919a14e5f5744482cda75e59984e6133944289129d86  target/release/sentinel-daemon
67670ad90e9e72ac9e856ca88770e23fb3b5762115941ea5f26b3f94bf61c46c  target/release/sentinel-projection
a3d35bdf5261a617546a19a380f731dba89dc6f24327f4dca1eff4783a05a31d  target/release/landlock-wrapper
03abe24e93222b540bf36bbedf0d5c259780bea0f53c79386086c44d22e40be8  target/release/breakout-helper
```

PASS: Reconcile no longer leaves stopped writer processes in orphan cgroups.

### AC-4 - Final Runtime Health Gate

Final reconcile command:

```bash
ssh ubuntu@10.0.0.240 "/tmp/opcurl -s -X POST http://127.0.0.1:8084/operator/runtime/reconcile -H 'Content-Type: application/json' -d '{\"dry_run\":false,\"projection_rebuild\":true,\"respawn_missing\":true}' | python3 -m json.tool"
```

Observed:

```text
"accepted": true
"stale_agents_before": 0
"stale_agents_after": 0
"orphan_cgroups_before": 3
"orphan_cgroups_after": 0
"orphan_cgroups_removed": 3
"respawned_agents": 0
"projection_drift_before": false
"projection_drift_after": false
"projection_rebuild_requested": true
"errors": []
```

Projection rebuild completion:

```text
Full rebuild complete total=3562240
Projection-Rebuild-Request abgearbeitet path=/opt/sentinel/data/.projection-rebuild-request events=3562240
```

Final runtime-health:

```bash
ssh ubuntu@10.0.0.240 "/tmp/opcurl -s http://127.0.0.1:8084/operator/runtime-health | python3 -c 'import sys,json; h=json.load(sys.stdin); print(\"expected\",h[\"expected_active_agents\"],\"runtime\",h[\"runtime_agents\"],\"projection\",h[\"projection_agents\"],\"cgroups\",h[\"live_cgroup_dirs\"],\"stale\",h[\"stale_runtime_entries\"],\"orphans\",h[\"orphan_cgroups\"],\"zombies\",h[\"zombie_tracked_pids\"],\"drift\",h[\"projection_drift_detected\"],\"repair\",h.get(\"repair_last_status\"))'"
```

Observed:

```text
expected 26 runtime 26 projection 26 cgroups 26 stale 0 orphans 0 zombies 0 drift False repair projection_rebuild_requested
```

Supporting checks:

```bash
ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8000/api/agents | python3 -c 'import sys,json; print(len(json.load(sys.stdin)))'"
ssh ubuntu@10.0.0.240 "sqlite3 /opt/sentinel/data/projection.db \"SELECT status, COUNT(*) FROM agent_live_view GROUP BY status ORDER BY status;\""
ssh ubuntu@10.0.0.240 "ps -o pid,stat,cmd -p 1291960,1314755,1324475 || true"
ssh ubuntu@10.0.0.240 "journalctl -u sentinel-daemon --since '5 min ago' --no-pager | grep -Ei 'panic|drift' || true"
```

Observed:

```text
api_agents=26
active|26
despawned|34
PID STAT CMD
no daemon panic/drift output
```

PASS: Runtime, Projection, Dashboard API and cgroups converge to the canonical active shift size `26`; stale, orphan and zombie counters are all zero.

### AC-5 - Local Scope Guards After Deploy

Commands:

```bash
git diff --check
rg -n "AGENT_MODEL_HAIKU|model: AGENT|model: String::new\\(\\).*Gateway" services/sentinel-daemon/src cmd crates
gh -R silentspike/project-sentinel issue view 279 --json number,title,state,labels,url
```

Observed:

```text
git diff --check: PASS
services/sentinel-daemon/src/llm_bridge.rs:612:            model: String::new(), // Gateway waehlt default
state=OPEN
labels include status:in-progress and quality:needs-spec
url=https://github.com/silentspike/project-sentinel/issues/279
```

PASS: No Daemon-side model policy change was introduced, and #279 remains open for the AC matrix, benchmarks, PR and verified close sequence.
