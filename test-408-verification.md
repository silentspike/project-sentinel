# Issue #408 Verification - NanoRuntime Trait, ECS-native Reference, Harness

Issue: https://github.com/silentspike/project-sentinel/issues/408

## Scope

Introduce the shared NanoRuntime contract in `sentinel-common`, an ECS-native
reference implementation in `sentinel-runtime`, and a reusable conformance
harness that downstream adapters must satisfy.

## AC Matrix

| AC | Evidence | Status |
| --- | --- | --- |
| AC-1 | `crates/sentinel-common/src/nano_runtime.rs` defines the runtime contract, operation result types, explicit runtime keys, and registry. | PASS |
| AC-2 | The trait exposes spawn, exec, snapshot, restore, health, isolate, and a default migrate composition. | PASS |
| AC-3 | `crates/sentinel-runtime/src/nano.rs` implements the ECS-native reference runtime over `RuntimeOrchestrator` plus ECS world snapshot codec. | PASS |
| AC-4 | The conformance harness verifies spawn -> health -> exec -> snapshot -> restore and restore(snapshot(x)) payload invariants. | PASS |
| AC-5 | The new `AgentConfig.runtime.nano_runtime` field is optional and does not inject a hidden global default. | PASS |

## Focused Tests

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- test -p sentinel-common
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- test -p sentinel-runtime --test nano_runtime_conformance
```

Observed:

```text
sentinel-common: 55 passed; 0 failed; 0 ignored
nano_runtime::conformance::conformance_harness_checks_roundtrip ... ok
nano_runtime::conformance::registry_requires_explicit_runtime_or_fallback ... ok
ecs_native_runtime_satisfies_nano_runtime_conformance ... ok
```

## Workspace Gates

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- fmt --check
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- clippy --workspace --all-targets -- -D warnings
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- test --workspace
```

Observed:

```text
fmt --check: exit 0
clippy --workspace --all-targets -- -D warnings: exit 0
test --workspace: exit 0
sentinel-runtime tests/nano_runtime_conformance.rs: 1 passed
sentinel-daemon tests/nano_runtime_registry.rs: 1 passed
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

Observed ECS-native rows:

```text
| ecs-native | spawn | 10 | 26.30 | 21 | 19 | 62 |
| ecs-native | exec | 10 | 0.10 | 0 | 0 | 1 |
| ecs-native | snapshot | 10 | 22.30 | 20 | 19 | 44 |
| ecs-native | restore | 10 | 36.60 | 27 | 25 | 126 |
roundtrip ecs-native: 10/10 restore(snapshot(x)) payload checks passed
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
