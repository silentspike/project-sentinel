# Issue #411 Verification - Runtime Registry And Workload Selection

Issue: https://github.com/silentspike/project-sentinel/issues/411

## Scope

Add explicit runtime-key selection and registry checks for the plural
NanoRuntime contract. Selection is workload/config-driven with explicit fallback
policy, not a hidden global default.

## AC Matrix

| AC | Evidence | Status |
| --- | --- | --- |
| AC-1 | `NanoRuntimeRegistry` registers and resolves runtime keys. | PASS |
| AC-2 | Runtime keys `ecs-native`, `wasm-wasmtime`, and `bwrap-landlock` exist in code and match DEV-007 docs. | PASS |
| AC-3 | Workload runtime selection uses explicit `runtime.nano_runtime` when present. | PASS |
| AC-4 | Missing runtime selection requires explicit fallback policy; no global default is injected. | PASS |
| AC-5 | Registry lookup benchmark ran on the deploy VM with system metrics. | PASS |

## Focused Tests

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- test -p sentinel-daemon --test nano_runtime_registry
```

Observed:

```text
running 1 test
test runtime_registry_routes_explicit_workload_keys ... ok
test result: ok. 1 passed; 0 failed; 0 ignored
```

The workspace test repeated this registry check:

```text
Running tests/nano_runtime_registry.rs
test runtime_registry_routes_explicit_workload_keys ... ok
```

## Build Gate For Benchmark Binary

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- build -p sentinel-daemon --bin nano_runtime_bench --release
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- build -p sentinel-sandbox --bin landlock-wrapper --release
```

Observed:

```text
build -p sentinel-daemon --bin nano_runtime_bench --release: exit 0
build -p sentinel-sandbox --bin landlock-wrapper --release: exit 0
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

Observed registry row:

```text
| registry | select_key | 10000 | 0.00 | 0 | 0 | 17 |
```

Runtime routing in the benchmark covered:

```text
ecs-native: spawn/exec/snapshot/restore roundtrip passed 10/10
wasm-wasmtime: spawn/exec/snapshot/restore roundtrip passed 10/10
bwrap-landlock: spawn/exec/snapshot/restore roundtrip passed 10/10
```

System metrics captured in the same artifact:

```text
vmstat: us 1 sy 0 id 98 wa 1 st 0
mpstat avg-cpu: %user 0.54 %system 0.23 %iowait 1.18 %steal 0.16 %idle 97.89
iostat sda: r/s 32.32 w/s 23.11 %util 5.40
```

## Token Safety

Gateway remained inactive on the deploy VM:

```text
sentinel-gateway.service loaded inactive dead
```
