# Issue 443 Live Evidence - Gaia Console Memory

Date: 2026-06-27

Scope:

- New standalone crate: `crates/sentinel-gaia-memory`
- Binary: `sentinel-gaia-memory`
- Live VM verification: `ubuntu@10.0.0.240`
- Benchmark VM: `ubuntu@10.0.0.241`
- No daemon restart, no daemon hook, no token, no write to `/opt/sentinel/data`
- No TOGAF HTML edits in this worker PR

## Remote Build And Test

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- test -p sentinel-gaia-memory
```

Relevant output:

```text
Finished `test` profile [unoptimized + debuginfo] target(s) in 11.69s
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 62.47s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- test -p sentinel-hippocampus readonly
```

Relevant output:

```text
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 107 filtered out; finished in 0.17s
```

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- clippy --workspace --all-targets -- -D warnings
```

Relevant output:

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 27.02s
```

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- build -p sentinel-gaia-memory --release
```

Relevant output:

```text
Finished `release` profile [optimized] target(s) in 54.52s
```

## Benchmarks

Build command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- bench -p sentinel-gaia-memory --no-run
```

Relevant output:

```text
Finished `bench` profile [optimized] target(s) in 27.88s
Executable benches/gaia_memory_bench.rs (target/release/deps/gaia_memory_bench-25080a1cb72cfa0a)
```

Execution command on `.241`:

```bash
scp target/release/deps/gaia_memory_bench-25080a1cb72cfa0a ubuntu@10.0.0.241:/tmp/issue-443-gaia-memory-bench-bin
ssh ubuntu@10.0.0.241 'rm -rf /tmp/issue-443-gaia-memory-bench-run && mkdir -p /tmp/issue-443-gaia-memory-bench-run && cd /tmp/issue-443-gaia-memory-bench-run && chmod +x /tmp/issue-443-gaia-memory-bench-bin && /tmp/issue-443-gaia-memory-bench-bin --bench --noplot --format pretty'
```

Relevant output, units normalized to ASCII `us`:

```text
gaia_console_memory.graph_insert_fact
                        time:   [5.2763 ms 6.5244 ms 8.1790 ms]
gaia_console_memory.graph_query_current_1k
                        time:   [5.2398 us 5.2903 us 5.3456 us]
gaia_console_memory.graph_supersede_fact
                        time:   [11.169 ms 11.592 ms 11.892 ms]
gaia_console_memory.rehydrate_readonly_zero_replay
                        time:   [554.63 us 554.99 us 555.42 us]
```

Benchmark execution did not use `.240` and did not use `cargo-remote`.

## Live CLI Verification

Before and after the live run, the daemon process was unchanged:

```text
$ systemctl show -p ActiveState -p MainPID sentinel-daemon --no-pager
MainPID=236888
ActiveState=active
...
$ systemctl show -p ActiveState -p MainPID sentinel-daemon --no-pager
MainPID=236888
ActiveState=active
```

Confirmation gate for mutating commands:

```text
$ /tmp/issue-443-gaia-memory-live/sentinel-gaia-memory --json memory append --data-dir /tmp/issue-443-gaia-memory-live/data-a --section notes --timestamp-ms 1770000000000 --text requires\ confirmation
exit_code=2
{"error":"policy: 'mutate' action requires confirmation; pass --confirm or set SENTINEL_GAIA_MEMORY_ASSUME_YES=1 (no mutation performed)","ok":false}
```

Memory file append/read in an isolated temp data dir:

```text
$ /tmp/issue-443-gaia-memory-live/sentinel-gaia-memory --json --confirm memory append --data-dir /tmp/issue-443-gaia-memory-live/data-a --section notes --timestamp-ms 1770000000001 --text Issue\ 443\ live\ memory\ append\ in\ tmp\ data_dir
{"action":"memory.append","ok":true,"path":"/tmp/issue-443-gaia-memory-live/data-a/gaia-memory.md","section":"notes","timestamp_ms":1770000000001}

$ /tmp/issue-443-gaia-memory-live/sentinel-gaia-memory --json memory read --data-dir /tmp/issue-443-gaia-memory-live/data-a --max-bytes 4096
{"action":"memory.read","bytes":150,"contents":"# Gaia Console Memory\n\n## Setup Decisions\n\n## Open Tasks\n\n## User Preferences\n\n## Notes\n- 1770000000001: Issue 443 live memory append in tmp data_dir\n","ok":true,"path":"/tmp/issue-443-gaia-memory-live/data-a/gaia-memory.md"}
```

Bi-temporal graph insert, current query, supersede, and historical stale query:

```text
$ /tmp/issue-443-gaia-memory-live/sentinel-gaia-memory --json --confirm fact insert --data-dir /tmp/issue-443-gaia-memory-live/data-a --subject project/aurora --relation status --literal scouting --valid-from-ms 1770000000003 --tx-ms 1770000000003 --confidence 0.80 --note initial\ live\ AC\ fact
{"action":"fact.insert","fact":{"confidence":0.800000011920929,"fact_id":"019f0816-91cf-7063-ab16-bc950eff4053","note":"initial live AC fact","object":{"type":"literal","value":"scouting"},"relation":"status","source":{"evidence_ref":null,"kind":"manual","uri":null},"subject":"project/aurora","tx_from_ms":1770000000003,"tx_to_ms":null,"valid_from_ms":1770000000003,"valid_to_ms":null},"ok":true}

$ /tmp/issue-443-gaia-memory-live/sentinel-gaia-memory --json --confirm fact supersede --data-dir /tmp/issue-443-gaia-memory-live/data-a --subject project/aurora --relation status --literal active --valid-from-ms 1770000000004 --tx-ms 1770000000004 --confidence 0.95 --note supersedes\ initial\ live\ AC\ fact
{"action":"fact.supersede","fact":{"confidence":0.949999988079071,"fact_id":"019f0816-9260-7083-88d7-f475633d7e86","note":"supersedes initial live AC fact","object":{"type":"literal","value":"active"},"relation":"status","source":{"evidence_ref":null,"kind":"manual","uri":null},"subject":"project/aurora","tx_from_ms":1770000000004,"tx_to_ms":null,"valid_from_ms":1770000000004,"valid_to_ms":null},"ok":true}

$ /tmp/issue-443-gaia-memory-live/sentinel-gaia-memory --json fact query --data-dir /tmp/issue-443-gaia-memory-live/data-a --subject project/aurora --relation status --current-only
{"action":"fact.query","count":1,"facts":[{"fact":{"confidence":0.949999988079071,"fact_id":"019f0816-9260-7083-88d7-f475633d7e86","note":"supersedes initial live AC fact","object":{"type":"literal","value":"active"},"relation":"status","source":{"evidence_ref":null,"kind":"manual","uri":null},"subject":"project/aurora","tx_from_ms":1770000000004,"tx_to_ms":null,"valid_from_ms":1770000000004,"valid_to_ms":null},"is_current":true,"stale_reason":null}],"ok":true}

$ /tmp/issue-443-gaia-memory-live/sentinel-gaia-memory --json fact query --data-dir /tmp/issue-443-gaia-memory-live/data-a --subject project/aurora --relation status --valid-at-ms 1770000000003 --as-of-tx-ms 1770000000005 --include-stale
{"action":"fact.query","count":1,"facts":[{"fact":{"confidence":0.800000011920929,"fact_id":"019f0816-91cf-7063-ab16-bc950eff4053","note":"initial live AC fact","object":{"type":"literal","value":"scouting"},"relation":"status","source":{"evidence_ref":null,"kind":"manual","uri":null},"subject":"project/aurora","tx_from_ms":1770000000004,"tx_to_ms":null,"valid_from_ms":1770000000003,"valid_to_ms":1770000000004},"is_current":false,"stale_reason":"validity_closed"}],"ok":true}
```

## Backup Roundtrip

Command/output:

```text
$ /tmp/issue-443-gaia-memory-live/sentinel-gaia-memory --json --confirm backup export --data-dir /tmp/issue-443-gaia-memory-live/data-a --output /tmp/issue-443-gaia-memory-live/gaia-memory.bundle.json --timestamp-ms 1770000000010 --overwrite
{"action":"backup.export","boundary":"crate-local-backup-not-simulation-snapshot","exported_at_ms":1770000000010,"format_version":1,"graph_file":{"name":"gaia_console_memory.redb","sha256":"6d995873e33a8f7372fd344b90bf1f42acf04b7341553202eae02319a5af448f","size_bytes":557056},"memory_file":{"name":"gaia-memory.md","sha256":"4bed7a567d1454b9f872dd49373dacddf2240d04838d33f81bf71ff8e3afbab4","size_bytes":150},"ok":true,"output":{"path":"/tmp/issue-443-gaia-memory-live/gaia-memory.bundle.json","sha256":"a3f900f2df185a241a6d4ab658fc78c23e55637308e640dc1ed3f6274e1b244b","size_bytes":557398}}

$ /tmp/issue-443-gaia-memory-live/sentinel-gaia-memory --json --confirm backup restore --data-dir /tmp/issue-443-gaia-memory-live/data-restore --input /tmp/issue-443-gaia-memory-live/gaia-memory.bundle.json --overwrite
{"action":"backup.restore","boundary":"crate-local-backup-not-simulation-snapshot","format_version":1,"input":"/tmp/issue-443-gaia-memory-live/gaia-memory.bundle.json","ok":true,"restore":{"data_dir":"/tmp/issue-443-gaia-memory-live/data-restore","graph_redb":{"path":"/tmp/issue-443-gaia-memory-live/data-restore/gaia_console_memory.redb","sha256":"6d995873e33a8f7372fd344b90bf1f42acf04b7341553202eae02319a5af448f","size_bytes":557056},"memory_markdown":{"path":"/tmp/issue-443-gaia-memory-live/data-restore/gaia-memory.md","sha256":"4bed7a567d1454b9f872dd49373dacddf2240d04838d33f81bf71ff8e3afbab4","size_bytes":150}}}
```

The backup boundary is crate-local and separate from simulation snapshots: no `WorldSnapshot`, no schema bump, no daemon snapshot routine.

## Read-only Rehydration Against `/opt/sentinel/data`

Command/output:

```text
$ strace -f -o /tmp/issue-443-gaia-memory-live/rehydrate.strace -e trace=openat,openat2,creat,truncate /tmp/issue-443-gaia-memory-live/sentinel-gaia-memory --json rehydrate --data-dir /opt/sentinel/data --agent Thomas --fact-key facts/projects/aurora
{"action":"rehydrate","context":{"data_dir":"/opt/sentinel/data","event_copy_count":0,"event_rows_loaded":0,"event_store":{"event_count":1647,"latest_event_id":13470170,"notes":["metadata-only immutable SQLite read; no event rows loaded or replayed","schema assumption: events(id) exists; failure degrades to unavailable notes"],"path":"/opt/sentinel/data/events.db","status":"ok"},"events_replayed":0,"hippocampus":{"agent_name":"Thomas","archived_episode_summaries":[],"facts":[],"live_episode_summaries":[],"narrative_summary":null,"notes":["read-only open failed: Failed to open hippocampus.redb read-only at /opt/sentinel/data/hippocampus.redb: Database already open. Cannot acquire lock."],"path":"/opt/sentinel/data/hippocampus.redb","status":"unavailable"},"memory_file":{"bytes_returned":0,"contents":"","notes":["/opt/sentinel/data/gaia-memory.md is missing"],"path":"/opt/sentinel/data/gaia-memory.md","status":"missing"},"notes":["read-only rehydration: metadata/read-model reads only; no event replay","events_replayed=0 and event_rows_loaded=0 by design","event store rows are referenced through source metadata; they are not copied into Gaia Console Memory","task_kanban open-task context is skipped in this crate: no public projection read API exists yet, so #438 data remains optional and graceful"],"projection":{"active_agent_count":0,"active_agents":[],"notes":["projection read used immutable SQLite to avoid WAL side-file writes","schema assumption: agent_live_view active-agent columns exist; failure degrades to unavailable notes"],"path":"/opt/sentinel/data/projection.db","status":"ok"}},"ok":true}
```

Assertion output:

```text
events_replayed=0
event_rows_loaded=0
event_copy_count=0
event_store_status=ok
projection_status=ok
hippocampus_status=unavailable
```

`hippocampus_status=unavailable` is an expected graceful degradation when the live daemon already holds the redb lock. The rehydration command still did not write or replay events.

The projection read uses immutable SQLite. That can ignore live WAL-only rows, so `active_agent_count=0` is not a latest-freshness claim; it is the deliberate no-write live verification path.

Strace proof for `/opt/sentinel/data`:

```text
$ grep /opt/sentinel/data /tmp/issue-443-gaia-memory-live/rehydrate.strace
238177 openat(AT_FDCWD, "/opt/sentinel/data/events.db", O_RDONLY|O_NOFOLLOW|O_CLOEXEC) = 3
238177 openat(AT_FDCWD, "/opt/sentinel/data/projection.db", O_RDONLY|O_NOFOLLOW|O_CLOEXEC) = 3
238177 openat(AT_FDCWD, "/opt/sentinel/data/hippocampus.redb", O_RDONLY|O_CLOEXEC) = 3

$ grep -E /opt/sentinel/data.*O_\(WRONLY\|RDWR\|CREAT\|TRUNC\) /tmp/issue-443-gaia-memory-live/rehydrate.strace
(no output)
```

Files created by the live verification were all under `/tmp/issue-443-gaia-memory-live`:

```text
/tmp/issue-443-gaia-memory-live/data-a/gaia-memory.md 150
/tmp/issue-443-gaia-memory-live/data-a/gaia_console_memory.redb 557056
/tmp/issue-443-gaia-memory-live/data-restore/gaia-memory.md 150
/tmp/issue-443-gaia-memory-live/data-restore/gaia_console_memory.redb 557056
/tmp/issue-443-gaia-memory-live/gaia-memory.bundle.json 557398
/tmp/issue-443-gaia-memory-live/no_confirm.err 150
/tmp/issue-443-gaia-memory-live/no_confirm.out 0
/tmp/issue-443-gaia-memory-live/rehydrate.json 1625
/tmp/issue-443-gaia-memory-live/rehydrate.strace 754
/tmp/issue-443-gaia-memory-live/sentinel-gaia-memory 4156200
```
