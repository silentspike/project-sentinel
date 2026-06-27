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
| `gaia_console_memory.graph_insert_fact` | 6.4297 ms | 5.5221 ms - 7.6797 ms |
| `gaia_console_memory.graph_query_current_1k` | 5.3586 us | 5.3151 us - 5.4137 us |
| `gaia_console_memory.graph_supersede_fact` | 11.756 ms | 11.415 ms - 11.993 ms |
| `gaia_console_memory.rehydrate_readonly_zero_replay` | 822.03 us | 821.46 us - 822.60 us |

Rehydration benchmark invariant:

- Uses existing read paths only: `EventStore::open_readonly`, `ReadModelStore::open_readonly`, and read-only Hippocampus access.
- Does not replay or copy event rows.
- Asserts `events_replayed=0`, `event_rows_loaded=0`, and `event_copy_count=0` inside the benchmark loop.

Raw output is committed under `console/evidence/issue-443-live/`.
