# Issue #409 Verification - WASM/Wasmtime NanoRuntime Adapter

Issue: https://github.com/silentspike/project-sentinel/issues/409

## Scope

Implement the `NanoRuntime` contract for `sentinel-wasm` using the existing
Wasmtime tool runtime. Snapshot semantics are intentionally declarative:
ToolRuntime/input state plus ECS snapshot and deterministic re-execution. This
does not claim a bit-exact Wasmtime Store dump.

## AC Matrix

| AC | Evidence | Status |
| --- | --- | --- |
| AC-1 | `crates/sentinel-wasm/src/nano.rs` implements `NanoRuntime` for `WasmtimeNanoRuntime`. | PASS |
| AC-2 | WASM adapter satisfies the shared conformance harness. | PASS |
| AC-3 | Snapshot/restore semantics are documented as input+ECS re-execute, not native Store checkpointing. | PASS |
| AC-4 | Existing WASM tests and workspace tests still pass. | PASS |

## Focused Tests

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- test -p sentinel-wasm --features wasm --test nano_runtime_conformance
```

Observed:

```text
running 1 test
test wasmtime_runtime_satisfies_nano_runtime_conformance ... ok
test result: ok. 1 passed; 0 failed; 0 ignored
```

Workspace output also included the full WASM crate suite:

```text
sentinel-wasm unittests: 51 passed; 0 failed
sentinel-wasm acceptance tests: 62 passed; 0 failed
tests/nano_runtime_conformance.rs: 1 passed
```

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

Observed WASM rows:

```text
| wasm-wasmtime | spawn | 10 | 32545.50 | 32691 | 29948 | 35663 |
| wasm-wasmtime | exec | 10 | 430.30 | 448 | 362 | 531 |
| wasm-wasmtime | snapshot | 10 | 13.90 | 13 | 10 | 22 |
| wasm-wasmtime | restore | 10 | 32397.50 | 32508 | 30581 | 35668 |
roundtrip wasm-wasmtime: 10/10 restore(snapshot(x)) payload checks passed
snapshot_semantics wasm-wasmtime: input+ECS re-execute state, no Wasmtime Store dump
```

System metrics captured in the same artifact:

```text
vmstat: us 1 sy 0 id 98 wa 1 st 0
mpstat avg-cpu: %user 0.54 %system 0.23 %iowait 1.18 %steal 0.16 %idle 97.89
iostat sda: r/s 32.32 w/s 23.11 %util 5.40
```

## Semantic Non-Claims

- No Wasmtime Store memory/image checkpoint is implemented or claimed.
- Restore is a deterministic re-execute model over declared runtime input and
  ECS state.
- Fuel-limited execution remains the safety boundary for the WASM adapter.
