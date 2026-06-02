# Issue #410 Verification - bwrap+Landlock NanoRuntime Adapter

Issue: https://github.com/silentspike/project-sentinel/issues/410

## Scope

Implement the `NanoRuntime` contract for `sentinel-sandbox` using the existing
bubblewrap, cgroup, and Landlock sandbox primitives. Snapshot semantics are
workload-level: bwrap config, isolation metadata, and agent-home filesystem
state. This does not claim process-RAM checkpointing or CRIU migration.

## AC Matrix

| AC | Evidence | Status |
| --- | --- | --- |
| AC-1 | `crates/sentinel-sandbox/src/nano.rs` implements `NanoRuntime` for `BwrapNanoRuntime`. | PASS |
| AC-2 | bwrap adapter satisfies the shared conformance harness on a host with bwrap/user namespaces. | PASS |
| AC-3 | Landlock entrypoint execution is explicitly allowed once, preventing wrapper self-denial while keeping root writes denied. | PASS |
| AC-4 | Teardown terminates/reaps spawned processes and restores agent-home state cleanly. | PASS |
| AC-5 | Snapshot semantics are documented as config+FS restore, not RAM checkpointing. | PASS |

## Focused Tests

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- test -p sentinel-sandbox --test nano_runtime_conformance -- --ignored
```

Observed:

```text
running 1 test
test bwrap_runtime_satisfies_nano_runtime_conformance ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; finished in 0.05s
```

Workspace output also included the sandbox crate suite:

```text
sentinel-sandbox unittests: 43 passed; 0 failed; 3 ignored
sentinel-sandbox acceptance tests: 14 passed; 0 failed; 2 ignored
sentinel-sandbox breakout tests: 3 passed; 0 failed; 9 ignored
tests/nano_runtime_conformance.rs: ignored by default, passed in the explicit host-capability run above
```

## Runtime Anomalies Found And Fixed

The first bwrap benchmark/smoke exposed two host-realism issues:

- Host bind paths may be absent on `.155`; bwrap config now prunes missing
  default host binds for existing-host test runs.
- Landlock wrapper initially denied the sandbox entrypoint itself; the ruleset
  now allows the explicit entrypoint executable while preserving root-write
  denial.

Final focused conformance and deploy-VM benchmark passed after these fixes.

## Deploy-VM Benchmark Evidence

Command:

```bash
ssh ubuntu@10.0.0.240 'timeout 90s /tmp/nano_runtime_bench 10'
```

Benchmark artifact:

```text
/tmp/nano-runtime-bench-407-411-20260529-204251
hardware: Intel(R) Core(TM) i7-3930K CPU @ 3.20GHz
gateway: not used
benchmark_note: deployment VM evidence only; no TOGAF absolute latency gate
```

Observed bwrap rows:

```text
| bwrap-landlock | spawn | 10 | 536.60 | 529 | 389 | 698 |
| bwrap-landlock | exec | 10 | 3.80 | 4 | 3 | 5 |
| bwrap-landlock | snapshot | 10 | 95.30 | 79 | 49 | 277 |
| bwrap-landlock | restore | 10 | 2020.80 | 2106 | 1593 | 2524 |
roundtrip bwrap-landlock: 10/10 restore(snapshot(x)) payload checks passed
snapshot_semantics bwrap-landlock: config+agent-home filesystem state, no RAM/CRIU checkpoint
```

System metrics captured in the same artifact:

```text
vmstat: us 1 sy 0 id 98 wa 1 st 0
mpstat avg-cpu: %user 0.54 %system 0.23 %iowait 1.18 %steal 0.16 %idle 97.89
iostat sda: r/s 32.32 w/s 23.11 %util 5.40
```

## Semantic Non-Claims

- No process RAM checkpoint is implemented or claimed.
- No CRIU or live process migration is implemented or claimed.
- Restore means re-spawn plus agent-home/config/isolation metadata restoration.
