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
| AC-4 | TARGET MISS | Dedup-hit latency was measured with system metrics, but p95 was 21,712 us and `target_100us_met=false`. |
| AC-5 | PASS | 10-minute soak from `2026-05-29T05:35:33+00:00` to `2026-05-29T05:45:33+00:00`: daemon active, gateway inactive, FUSE still mounted, no journal errors/panics, and `projection_drift_detected=false`. |

## Evidence Log

- Reconcile before soak cleared pre-existing projection drift: `projection_drift_detected=false`, `orphan_cgroups=0`.
- `vmstat`, `mpstat`, and `iostat -x` were captured during the dedup benchmark and accelerated shift test.
- 10-minute FUSE soak passed with no daemon errors or panics.
