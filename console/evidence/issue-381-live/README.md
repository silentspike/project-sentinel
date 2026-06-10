# Issue #381 — Live Evidence (Phase-Timing → Profiling View)

## Benchmark evidence (committed, measured 2026-06-10 on the deploy VM)

Measured on the deploy VM (Intel i7-3930K, virtio, simulation active at ~13-16% base load
per `mpstat-*.txt`), criterion release binaries built via cargo-remote and executed on the VM:

| File | Content |
|---|---|
| `bench-telemetry.txt` | `phase_histogram` group: observe ns/record sweep 4/8/16 buckets (27.5 / 28.0 / 30.0 ns) + `record_all_10_phases_per_tick` (280 ns) |
| `bench-phasetiming.txt` | AC-4: `full_tick_baseline_26_agents` 129.78 µs vs `full_tick_with_phase_timing_26_agents` 129.54 µs (CIs overlap → delta within noise, < 0.1% tick budget) |
| `vmstat-*.txt` / `mpstat-*.txt` | System monitoring sidecars captured during each run (mandatory benchmark rule) |

Sweep result: **16 buckets** fixed as `PHASE_DURATION_BOUNDARIES_MS` (+2 ns/observe buys
double quantile resolution; percentiles quantize to bucket boundaries).

## Live verification (MAIN SESSION, after deploy)

- **AC-1:** `curl -s http://localhost:9090/metrics | grep sentinel_phase_duration_ms`
  on the VM → lines with `phase="input"…"persist"` and values > 0 (after warmup;
  the lazy filter hides phase histograms until the first tick was recorded).
- **AC-3:** run `smoke-381.mjs` ON the VM (playwright/chromium there, pattern of
  `issue-433-pr-d/smoke-pr-d.mjs`), then review the screenshots **visually** and
  operate the panel functionally. Screenshots get committed into this directory.

```bash
# on the VM:
DASHBOARD_KEY=<operator-key> node smoke-381.mjs
```
