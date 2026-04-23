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
