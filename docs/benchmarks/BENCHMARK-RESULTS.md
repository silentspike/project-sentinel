# Benchmark Results

## Issue 443 - Gaia Console Memory

Date: 2026-06-27

Scope:

- `crates/sentinel-gaia-memory`
- Gaia Console Memory graph insert/query/supersede paths
- Read-only rehydration context assembly with `events_replayed=0`, `event_rows_loaded=0`, and `event_copy_count=0`

Infrastructure:

- Build artifact: `cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- bench -p sentinel-gaia-memory --no-run`
- Benchmark VM: `ubuntu@10.0.0.241`
- Benchmark command: `./gaia_memory_bench --bench --noplot --format pretty`
- `.240` was not used for benchmark execution.
- `cargo-remote` was not used to execute benchmarks.

Results:

| Benchmark | Median | Range |
| --- | ---: | ---: |
| `gaia_console_memory.graph_insert_fact` | 6.5244 ms | 5.2763 ms - 8.1790 ms |
| `gaia_console_memory.graph_query_current_1k` | 5.2903 us | 5.2398 us - 5.3456 us |
| `gaia_console_memory.graph_supersede_fact` | 11.592 ms | 11.169 ms - 11.892 ms |
| `gaia_console_memory.rehydrate_readonly_zero_replay` | 554.99 us | 554.63 us - 555.42 us |

Rehydration benchmark invariant:

- Uses immutable read-only SQLite URI opens for `events.db` and `projection.db` so live verification does not create WAL/SHM side files under `/opt/sentinel/data`.
- Uses read-only Hippocampus access.
- Does not replay or copy event rows.
- Asserts `events_replayed=0`, `event_rows_loaded=0`, and `event_copy_count=0` inside the benchmark loop.

Evidence snippets and command output are committed under `console/evidence/issue-443-live/`.
