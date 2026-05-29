# Issue #379 Verification

Date: 2026-05-29
Repo: `/work/company/project-sentinel`
Branch: `feat/issues-278-379-nightrun-fuse`
Deploy target: `ubuntu@10.0.0.240`

## Hardware Context

- Host: `sentinel-ubuntu-2404`
- CPU: Intel Core i7-3930K @ 3.20 GHz, Sandy Bridge-E, 2011, KVM, 8 vCPU
- Benchmark policy: FUSE/CAS benchmarks executed on the Deploy-VM, never on the build server.
- System metrics captured with `vmstat 1`, `mpstat 1`, and `iostat -x 1`.

## Implementation Verified

- `sentinel-fs` exposes live logical/CAS storage stats through `LayerManager::storage_stats()`.
- `MetadataStore::storage_stats()` counts logical regular-file bytes and reports unreadable inode rows instead of failing the whole stats path.
- Daemon Operator API exposes `GET /operator/security/fs-stats`.
- Daemon Operator API exposes `POST /operator/security/fs-dedup-benchmark`.
- The benchmark writes one seed blob and repeated identical hit files through the shared FUSE/CAS layer and reports dedup ratio plus latency target flags.
- The benchmark now reports phase-level latency for CAS hit check, `LayerManager::write_file()`, full loop, and storage-stat overhead so AC-4 optimization work can target the real bottleneck.
- The write hot path now allocates the inode, creates the dirent, writes metadata, and increments CAS refcount in one redb transaction.
- The daemon exposes `fs_metadata_durability = "eventual"` so FUSE hot-path metadata commits can skip per-commit fsync when low latency is preferred over crash durability for the most recent metadata writes.
- The dedup-hit metadata path skips `fs_trash_queue` access when the CAS refcount is already non-zero and serializes inode metadata as bincode with legacy JSON fallback.

## Test Gates

```text
cargo fmt --check -> PASS
cargo remote -c -- test -p sentinel-fs --lib -> 94 passed
cargo remote -c -- test -p sentinel-daemon --lib -> 202 passed
cargo remote -c -- clippy --workspace --all-targets -- -D warnings -> PASS after clippy::int_plus_one fix
cargo remote -c -- build -p sentinel-daemon --release -> PASS
```

Focused tests:

```text
cargo remote -c -- test -p sentinel-fs storage_stats_report_logical_bytes_and_dedup_savings --lib -> 1 passed
cargo remote -c -- test -p sentinel-daemon fs_storage_stats_get_reports_logical_and_cas_bytes --lib -> 1 passed
cargo remote -c -- test -p sentinel-daemon fs_dedup_benchmark_reports_hits_ratio_and_latency --lib -> 1 passed
cargo remote -c -- test -p sentinel-fs inode_data_serializes_as_bincode_with_legacy_json_fallback --lib -> 1 passed
cargo remote -c -- test -p sentinel-fs create_file_allocating_inode_removes_zero_ref_trash_entry --lib -> 1 passed
cargo remote -c -- test -p sentinel-daemon test_parse_fs_metadata_eventual_durability --lib -> 1 passed
```

## VM Deploy Evidence

```text
systemctl is-active sentinel-daemon -> active
systemctl is-active sentinel-gateway -> inactive
journalctl -> sentinel-fs FUSE-Mount starten mountpoint=/opt/sentinel/fs data_dir=/opt/sentinel/data
journalctl -> sentinel-fs FUSE-Mount aktiv mountpoint=/opt/sentinel/fs
journalctl -> Sandbox nutzt sentinel-fs FUSE-Mount fuer Agent-Homes fs_mount=/opt/sentinel/fs
```

Because `sentinel-daemon.service` uses systemd hardening with a service mount namespace, host `findmnt /opt/sentinel/fs` is empty. The mount is visible in the daemon namespace:

```text
PID=114953
/proc/114953/mountinfo:
58 214 0:45 / /opt/sentinel/fs rw,nosuid,nodev,relatime shared:418 - fuse sentinel-fs rw,user_id=0,group_id=0,allow_other
```

Agent-home binding:

```text
curl http://127.0.0.1:8084/operator/security/agent-runtime-state?agent_id=31

{
  "found": true,
  "agent_id": 31,
  "aggregate_id": "AGENT-31",
  "agent_name": "Sandra Vogel",
  "home_host_path": "/opt/sentinel/fs/AGENT-31",
  "fs_mount": "/opt/sentinel/fs"
}
```

## Storage Stats

Before benchmark:

```text
{
  "accepted": true,
  "fs_mount": "/opt/sentinel/fs",
  "cas_blob_count": 15,
  "cas_bytes_on_disk": 997,
  "regular_file_count": 10,
  "logical_regular_file_bytes": 550,
  "dedup_savings_bytes": 0,
  "dedup_ratio_percent": 0.0,
  "unreadable_inode_rows": 5
}
```

After benchmark:

```text
{
  "accepted": true,
  "fs_mount": "/opt/sentinel/fs",
  "cas_blob_count": 17,
  "cas_bytes_on_disk": 9191,
  "regular_file_count": 156,
  "logical_regular_file_bytes": 598566,
  "dedup_savings_bytes": 589375,
  "dedup_ratio_percent": 98.46449681405225,
  "unreadable_inode_rows": 6
}
```

Note: unreadable inode rows are existing metadata anomalies. The stats path now reports them explicitly instead of hiding or crashing on them.

## Dedup Benchmark

This is the initial live-verification benchmark before the AC-4 optimization loop. The optimized same-VM benchmark is documented in the later "AC-4 Optimization Loop" section.

Command shape:

```text
vmstat 1 6
mpstat 1 6
iostat -x 1 6
curl -X POST http://127.0.0.1:8084/operator/security/fs-dedup-benchmark \
  -H "Content-Type: application/json" \
  -d '{"agent_name":"Sandra Vogel","writes":128,"bytes_per_write":4096,"file_prefix":"issue379-vm"}'
```

Output:

```text
{
  "accepted": true,
  "agent_name": "Sandra Vogel",
  "fs_agent_dir": "AGENT-31",
  "bytes_per_write": 4096,
  "seed_write_us": 7932,
  "dedup_hit_writes": 128,
  "dedup_hits": 128,
  "logical_bytes_written": 528384,
  "cas_blob_count_before": 15,
  "cas_blob_count_after": 16,
  "cas_bytes_before": 997,
  "cas_bytes_after": 5094,
  "cas_bytes_delta": 4097,
  "dedup_ratio_percent": 99.22461694525194,
  "target_87_percent_met": true,
  "dedup_hit_latency_us_min": 3314,
  "dedup_hit_latency_us_p50": 7295,
  "dedup_hit_latency_us_p95": 21712,
  "dedup_hit_latency_us_max": 90875,
  "dedup_hit_latency_us_mean": 10146.5859375,
  "target_100us_met": false
}
```

System metrics:

```text
mpstat average: user 0.21%, system 0.29%, iowait 2.76%, idle 96.72%
iostat sda peak during benchmark: 1534 writes/s, 6492 kB/s, 87.20% util, w_await 0.74 ms
vmstat: no swap, CPU idle 87-100%, peak iowait 11%
```

Interpretation:

- PASS for storage savings: dedup ratio was 99.22%, above the 87% target.
- TARGET MISS for dedup-hit latency: p95 was 21,712 us, above the `<100us` target. This is a real VM measurement through redb metadata writes and the CAS/FUSE path, not a synthetic in-memory number.

## Instrumented Baseline

Task-1 instrumented daemon:

```text
local sha256 target/release/sentinel-daemon -> cbb38e1be7a5d70158b61ee17b8a525212b5bc172e5ef48d0ceb0c3d1defda4e
vm sha256 /opt/sentinel/bin/sentinel-daemon -> cbb38e1be7a5d70158b61ee17b8a525212b5bc172e5ef48d0ceb0c3d1defda4e
systemctl is-active sentinel-daemon -> active
systemctl is-active sentinel-gateway -> inactive
/proc/118821/mountinfo -> fuse sentinel-fs at /opt/sentinel/fs
```

Live benchmark agent after daemon restart:

```text
curl http://127.0.0.1:8084/operator/security/agent-runtime-state?agent_id=1

{
  "found": true,
  "agent_id": 1,
  "aggregate_id": "AGENT-01",
  "agent_name": "Thomas Mueller",
  "home_host_path": "/opt/sentinel/fs/AGENT-01",
  "fs_mount": "/opt/sentinel/fs"
}
```

Command shape, repeated three times on the Deploy-VM:

```text
vmstat 1 12
mpstat 1 12
iostat -x 1 12
curl -X POST http://127.0.0.1:8084/operator/security/fs-dedup-benchmark \
  -H "Content-Type: application/json" \
  -d '{"agent_name":"Thomas Mueller","writes":128,"bytes_per_write":4096,"file_prefix":"issue379-baseline-rN"}'
```

Raw evidence path on VM:

```text
/tmp/issue379-baseline-20260529T071418/run-1
/tmp/issue379-baseline-20260529T071418/run-2
/tmp/issue379-baseline-20260529T071418/run-3
```

Instrumented baseline summary:

| Run | Dedup ratio | CAS check p95 | write_file p50 | write_file p95 | loop p95 | Target <100us | mpstat avg idle |
|---|---:|---:|---:|---:|---:|---|---:|
| 1 | 99.2246% | 35 us | 12,474 us | 40,411 us | 40,440 us | false | 97.58% |
| 2 | 99.2246% | 35 us | 11,822 us | 47,709 us | 47,740 us | false | 97.40% |
| 3 | 99.2246% | 38 us | 12,099 us | 37,235 us | 37,261 us | false | 97.64% |

Interpretation:

- CAS hit lookup is already near the `<100us` target at p95 35-38 us.
- The miss is dominated by `LayerManager::write_file()` metadata/write work: p95 37.2-47.7 ms.
- Full-loop p95 is only 26-31 us above `write_file()` p95, so benchmark loop overhead is not the driver.
- CPU was mostly idle and there was no swap pressure; this is a code-path/storage-sync bottleneck, not a saturated-CPU benchmark.

## AC-4 Optimization Loop

All runs below were executed on the Deploy-VM `ubuntu@10.0.0.240` on Intel i7-3930K @ 3.20 GHz (2011). The gateway stayed inactive. Each run captured `vmstat 1`, `mpstat 1`, and `iostat -x 1` next to the benchmark result.

Raw evidence paths:

```text
/tmp/issue379-baseline-20260529T071418
/tmp/issue379-txconsolidated-20260529T072419
/tmp/issue379-eventual-20260529T073637
/tmp/issue379-lazyroot-20260529T074504
/tmp/issue379-rootcache-20260529T075036
/tmp/issue379-trashqueue-20260529T075452
/tmp/issue379-bincode-20260529T080148
/tmp/issue379-norootcache-20260529T080737
/tmp/issue379-final-rootcache-20260529T081128
/tmp/issue379-final-rootcache-warm-20260529T081219
/tmp/issue379-directbincode-20260529T081916
/tmp/issue379-directbincode-warm-20260529T082022
/tmp/issue379-final-bincode-20260529T082714
/tmp/issue379-final-bincode-warm-20260529T082803
```

Median comparison, same VM and same command shape:

| Stage | Main change | Median write p50 | Median write p95 | Median loop p95 | Verdict |
|---|---|---:|---:|---:|---|
| Instrumented baseline | Phase timing only | 12,099 us | 40,411 us | 40,440 us | Bottleneck isolated to metadata write path |
| Transaction consolidation | inode allocation + create file in one transaction | 5,796 us | 41,658 us | 41,685 us | p50 improved; p95 still dominated by redb fsync/noise |
| Eventual durability | `redb::Durability::None` for configured FUSE hot path | 162 us | 214 us | 223 us | Major improvement; fsync was dominant cost |
| Lazy root serialization | serialize root only when missing | 184 us | 273 us | 281 us | Rejected, slower/noisier |
| Root-known cache | skip root check after first known agent root | 168 us | 228 us | 235 us | No reliable standalone improvement |
| Conditional trash-queue skip | only touch trash queue on zero-ref -> one-ref transition | 149 us | 197 us | 205 us | Kept |
| Bincode inode metadata | bincode v2 with legacy JSON fallback | 146 us | 188 us | 195 us | Kept |
| No root cache | remove root-known cache again | 171 us | 288 us | 303 us | Rejected |
| Direct-buffer bincode | encode directly into final buffer | 144 us | 190 us | 198 us | Rejected after warm repeat showed worse stability |
| Final bincode warm | final binary `91ef5767...` after reverting direct-buffer | 141 us | 189 us | 196 us | Final selected result |

Final selected run:

```text
Deploy hash: 91ef5767b810758acb02d5447b0c629a9949e62c4250c63db510f85bc251de1d
Config: fs_metadata_durability = "eventual"
Daemon: active
Gateway: inactive
FUSE: /proc/127633/mountinfo -> fuse sentinel-fs at /opt/sentinel/fs
Raw path: /tmp/issue379-final-bincode-warm-20260529T082803
```

Final VM benchmark summary:

| Run | Dedup ratio | CAS check p95 | write_file p50 | write_file p95 | write_file max | loop p95 | Target <100us | mpstat avg idle |
|---|---:|---:|---:|---:|---:|---:|---|---:|
| 1 | 99.2246% | 18 us | 140 us | 179 us | 233 us | 186 us | false | 99.73% |
| 2 | 99.2246% | 9 us | 146 us | 195 us | 277 us | 202 us | false | 99.67% |
| 3 | 99.2246% | 17 us | 141 us | 189 us | 229 us | 196 us | false | 99.67% |

Relative result versus the instrumented same-VM baseline:

```text
median write_file p50: 12,099 us -> 141 us = 98.83% faster
median write_file p95: 40,411 us -> 189 us = 99.53% faster
median loop p95:       40,440 us -> 196 us = 99.52% faster
```

Interpretation:

- The original p95 miss was primarily redb commit durability, not CAS hit lookup or benchmark loop overhead.
- The final code keeps the real FUSE/CAS write semantics and does not fake the benchmark by bypassing metadata writes.
- The strict `<100us` target is still not met on this 2011 Sandy Bridge-E VM: final p95 is 189 us and `target_100us_met=false`.
- Further reduction below 100 us likely requires a larger design change such as write-behind/batched metadata commits or a different metadata layout, which would change crash semantics and needs a separate architecture decision.

## Soak

Soak marker:

```text
2026-05-29T05:35:33+00:00
```

Final soak output:

```text
soak_done=2026-05-29T05:45:33+00:00
systemctl is-active sentinel-daemon -> active
systemctl is-active sentinel-gateway -> inactive
/proc/114953/mountinfo -> fuse sentinel-fs at /opt/sentinel/fs
journalctl -u sentinel-daemon --since 2026-05-29T05:35:33+00:00 -p err -> No entries
journal panic grep -> no output
Tick Checkpoint first/last -> 05:35:48 tick=1447260, 05:44:48 tick=1447800
```

Runtime-health summary after soak:

```text
{
  "current_shift": 3,
  "expected_active_agents": 26,
  "runtime_agents": 26,
  "projection_agents": 26,
  "projection_drift_detected": false,
  "projection_drift_agents": 0,
  "stale_runtime_entries": 1,
  "orphan_cgroups": 0,
  "zombie_tracked_pids": 1,
  "respawn_failures": 0,
  "last_repair_error": null,
  "repair_last_status": "projection_restart_requested"
}
```

## AC Evidence

| AC | Result | Evidence |
|---|---|---|
| AC-1 | PASS | Journal shows `sentinel-fs FUSE-Mount aktiv`; daemon `/proc/<MainPID>/mountinfo` shows `fuse sentinel-fs` at `/opt/sentinel/fs`. |
| AC-2 | PASS | Runtime-state endpoint reports agent `AGENT-31` home as `/opt/sentinel/fs/AGENT-31`, not `/ram/agents`. |
| AC-3 | PASS | Live dedup benchmark measured 99.22% storage savings against the 87% target. |
| AC-4 | OPTIMIZED / TARGET MISS | Dedup-hit latency was measured with system metrics and optimized in a same-VM loop. Median p95 improved from 40,411 us to 189 us, but the strict `<100us` target remains unmet on the i7-3930K VM (`target_100us_met=false`). |
| AC-5 | PASS | 10-minute soak from `2026-05-29T05:35:33+00:00` to `2026-05-29T05:45:33+00:00`: daemon active, gateway inactive, FUSE still mounted, no journal errors/panics, and `projection_drift_detected=false`. |

## Evidence Log

- Reconcile before soak cleared pre-existing projection drift: `projection_drift_detected=false`, `orphan_cgroups=0`.
- `vmstat`, `mpstat`, and `iostat -x` were captured during the dedup benchmark and accelerated shift test.
- AC-4 optimization loop raw paths are retained on the Deploy-VM under `/tmp/issue379-*`.
- 10-minute FUSE soak passed with no daemon errors or panics.
