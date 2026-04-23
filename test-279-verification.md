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
