# Issue #380 Verification - eBPF Kernel Mode

Date: 2026-05-29  
Branch: `feat/issue-380-ebpf-kernel-mode`  
Base: `1d5d1e7664f11e30cf1125d7f165ff7c1ba4f3cd`  
Deploy target: `ubuntu@10.0.0.240`  
Hardware label: `Intel i7-3930K @ 3.20 GHz (2011, KVM, 8 vCPU)`  
Runtime rule: benchmarks and live verification on Deploy-VM only; Rust build/test via `cargo remote -c --`.

## Scope

Issue #380: eBPF Kernel-Mode produktiv aktivieren + Daten ins Dashboard/Loop verdrahten.

Acceptance Criteria:

| AC | Requirement | Status |
|---|---|---|
| AC-1 | Daemon loads eBPF in Kernel-Mode (`mode=kernel`, not userspace fallback) | Baseline already PASS on VM; final post-deploy verification still required |
| AC-2 | fentry probes deliver live Agent-Health write-bytes, IOPS, TCP values | Code path PASS for I/O + TCP API exposure; final controlled VM deltas still required |
| AC-3 | Dashboard shows eBPF metrics with real values, no `N/A` | API/unit PASS for network counters; Playwright screenshot still required |
| AC-4 | Overhead stays below `<0.15%` tick budget | PENDING benchmark |
| AC-5 | Userspace fallback remains graceful without CAP_BPF | PENDING fallback proof |

## Task 1 - Issue Readiness And Baseline

### Context Reload

- `/start` skill was read from `/home/jan/.codex/skills/start/SKILL.md`.
- Hooks registered in project-local `.claude/settings.json`.
- Project rules read from `.claude/CLAUDE.md`, global rules from `/home/jan/.claude/CLAUDE.md`, workspace rules from `AGENTS.md` and `/work/company/AGENTS.md`.
- Relevant memory read from `/home/jan/.codex/memories/MEMORY.md` and Project Sentinel rollout summary.
- MainRag attempt failed:

```text
Command: mainrag search "sentinel ebpf kernel mode dashboard fallback" --source claude-conversations --limit 5
Output: localhost:3001 connection refused
```

### Issue Quality Gate Repair

Initial GitHub issue state:

```text
Issue #380: OPEN
Labels: status:triage, quality:needs-spec, type:feature, comp:daemon, comp:dashboard, comp:bio, comp:inference
Issue-quality comment: Missing sections: Benchmarks
```

Action:

```text
Added ## Benchmarks section to Issue #380.
Updated labels: status:in-progress, quality:ready.
Removed labels: status:triage, quality:needs-spec.
Verified: hasBenchmarks=true.
```

### VM Baseline

Command:

```bash
ssh ubuntu@10.0.0.240 'uname -a; systemctl is-active sentinel-daemon sentinel-projection sentinel-gateway || true'
```

Output:

```text
Linux sentinel-ubuntu-2404 6.17.0-22-generic #22~24.04.1-Ubuntu SMP PREEMPT_DYNAMIC Thu Mar 26 15:25:54 UTC 2 x86_64 x86_64 x86_64 GNU/Linux
sentinel-daemon: active
sentinel-projection: active
sentinel-gateway: inactive
```

Systemd paths and capability baseline:

```text
/etc/systemd/system/sentinel-daemon.service:ExecStart=/opt/sentinel/bin/sentinel-daemon --config /opt/sentinel/config/daemon.toml
/etc/systemd/system/sentinel-projection.service:ExecStart=/opt/sentinel/bin/sentinel-projection \
/etc/systemd/system/sentinel-gateway.service:ExecStart=/opt/sentinel/bin/cortex-gateway
CapabilityBoundingSet includes cap_bpf and cap_perfmon.
AmbientCapabilities=cap_sys_ptrace
NoNewPrivileges=yes
bpf fs mounted at /sys/fs/bpf
tracefs mounted at /sys/kernel/tracing
```

Daemon journal baseline:

```text
eBPF Publisher alive snapshots=60 mode=kernel stalled=1
eBPF Publisher alive snapshots=120 mode=kernel stalled=1
eBPF Publisher alive snapshots=180 mode=kernel stalled=1
```

Prometheus baseline:

```text
sentinel_ebpf_monitoring_mode{mode="kernel"} 1
sentinel_ebpf_collector_cycle_microseconds 985
sentinel_ebpf_ring_buffer_drops_total 0
sentinel_agent_stalled_total 1
sentinel_io_ops_total{...,direction="read"} > 0
sentinel_io_bytes_total{...,direction="read"} > 0
sentinel_llm_requests_total{destination="0.0.0.0:0"} 614 after controlled HTTPS curls
```

Dashboard API baseline:

```bash
curl -fsS http://127.0.0.1:8000/api/ebpf/status
curl -fsS http://127.0.0.1:8000/api/ebpf/metrics
```

Output:

```json
{"mode":"kernel"}
```

```json
{
  "available": true,
  "mode": "kernel",
  "stalled_count": 1,
  "stalled_agents": [{"agent": "Stefan Huber", "seconds": 110}],
  "collection_cycle_us": 985,
  "ring_buffer_drops": 0,
  "io_read_bytes": 12490959,
  "io_write_bytes": 226,
  "avg_stress": 0
}
```

### Task 1 Findings

- The issue text was stale: the live VM already runs eBPF `mode=kernel`.
- The dashboard frontend uses `/api/ebpf/metrics`; `/api/metrics/ebpf/*` is not a valid route.
- TCP metrics are present in Prometheus as `sentinel_llm_requests_total`, but the dashboard eBPF JSON currently does not expose network request counters. This is a Task 3 gap.
- AC-4 and AC-5 still require fresh VM evidence.

## Task 2 - Loader And Fallback Hardening

Change:

- Extracted CAP_BPF effective-capability parsing into a pure helper.
- Added tests for CAP_BPF present, CAP_BPF absent, malformed `CapEff`, and missing `CapEff`.
- Runtime behavior is unchanged on privileged systems; the change makes the no-CAP fallback decision explicit and covered.

Checks:

```text
cargo fmt --check
PASS
```

```text
git diff --check
PASS
```

```text
cargo remote -c -- test -p sentinel-ebpf --lib
running 65 tests
test result: ok. 64 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out
```

```text
cargo remote -c -- clippy -p sentinel-ebpf --all-targets -- -D warnings
PASS
```

## Task 3 - Network Metrics API And Dashboard Exposure

Change:

- Added Prometheus export for `sentinel_llm_bytes_total` with `sent` and `received` directions.
- Parsed eBPF TCP request/error/latency/byte counters from Prometheus in `/api/ebpf/metrics`.
- Exposed dashboard JSON fields:
  `network_request_count`, `network_error_count`, `network_avg_latency_ms`,
  `network_bytes_sent`, `network_bytes_received`, `network_destinations`.
- Rendered TCP cards and top destinations in the eBPF dashboard section without `N/A` placeholders.
- Added API coverage using mocked Prometheus text with kernel mode, I/O, PSI, and TCP counters.

Checks:

```text
git diff --check
PASS
```

```text
cargo fmt --check
PASS
```

```text
cd dashboard && bun test
75 pass
0 fail
661 expect() calls
```

```text
cd dashboard && bun run typecheck
PASS
```

```text
cargo remote -c -- test -p sentinel-ebpf --lib
running 65 tests
test result: ok. 64 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out
```

```text
cargo remote -c -- clippy -p sentinel-ebpf --all-targets -- -D warnings
PASS
```

## Task 4 - PSI Coupling Into Bio, Mood, And Perception

Decision:

- Kernel eBPF remains an observability and judge signal path.
- CPU and memory pressure enter the diegetic simulation through PSI, not by
  feeding technical probe stalls directly into agent physiology.
- Existing runtime path:
  `/proc/pressure` -> `AdaptiveTickRate` -> daemon ECS `PsiMetrics` ->
  `bio_system` -> `sentinel_bio::apply_psi_stress` -> Mood -> Perception.
- Existing judge path:
  eBPF publisher/NATS -> `sentinel-judge` eBPF consumer -> drift score
  enrichment.

Change:

- Added `test_psi_pressure_flows_into_bio_mood_and_perception` in
  `crates/sentinel-ecs/src/lib.rs`.
- The test runs identical baseline and pressured worlds, then asserts:
  stress increases by at least 25 points, comfort drops by at least 10 points,
  mood arousal rises by more than 0.1, and body perception contains the raised
  stress state.

Checks:

```text
cargo fmt --check
PASS
```

```text
git diff --check
PASS
```

```text
cargo remote -c -- test -p sentinel-ecs test_psi_pressure_flows_into_bio_mood_and_perception
running 1 test
test tests::test_psi_pressure_flows_into_bio_mood_and_perception ... ok
test result: ok. 1 passed; 0 failed
```

```text
cargo remote -c -- clippy -p sentinel-ecs --all-targets -- -D warnings
PASS
```

## Benchmark Method

Final #380 benchmark evidence must compare only same-VM runs:

1. Kernel-mode collector active.
2. Controlled fallback/no-CAP run.
3. Same sampling window, same load trigger.
4. Parallel `vmstat 1`, `mpstat 1`, `iostat -x 1`.
5. Report collector cycle, tick duration, CPU, IO, and service error state.

No benchmark result from `cargo remote` is valid runtime evidence.
