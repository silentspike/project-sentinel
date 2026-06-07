# Issue #276 Verification Evidence

## Scope

- Plan: `/home/jan/.claude/plans/robust-dancing-steele.md`
- Branch: `feat/issues-276-277-combined`
- Issue: `https://github.com/silentspike/project-sentinel/issues/276`
- Benchmark host: Deploy-VM `ubuntu@10.0.0.240`, host label `sentinel-ubuntu-2404`
- Hardware label: gemessen auf Intel i7-3930K @ 3.20 GHz (Sandy Bridge-E, 2011), KVM, 8 vCPU, `taskset -c 2`; Vergleich rein relativ vorher/nachher.
- Comparison rule: keine TOGAF-/Gap-Doc-Absolutwerte als Gate. Diese Baselines stammen von deutlich neuerer Hardware. #276 wird ueber same-VM deltas, Allokationsform und funktionale Gates bewertet.
- Build rule: Rust build/test/clippy via `cargo remote -c --`; Benchmarks nie auf dem Build-Server ausfuehren.
- Runtime rule: `sentinel-gateway` und `sentinel-health-monitor.timer` bleiben fuer Benchmarks/Smoke inaktiv.
- PR hygiene: final combined PR excludes unrelated commits `8e8ecf1` (Wasmtime audit), `bfac43c` (Go toolchain), and `d44f89e` (sandbox snapshot).

## AC Matrix

| AC | Requirement | Status | Evidence |
|---|---|---|---|
| AC-1 | `physics_system` no per-tick `HashMap`, reusable workspace, ECS tests green, physics before/after | PASS | `RoomPhysicsWorkspace`; VM benchmark 1,172,325 -> 857,440 ns/iter, 26.86% faster |
| AC-2 | Perception static text/buffer reuse, output unchanged, snapshots green, perception before/after | PASS | `generate_perception_into`; VM benchmark 149,127 -> 109,845 ns/iter, 26.34% faster |
| AC-3 | Persist batch writes events/outbox in one transaction, order/idempotency preserved | PASS | `append_with_outbox_batch`; e2e VM benchmark 40,377,309 -> 19,151,838 ns/iter, 52.57% faster |
| AC-4 | Overall `bio_tick_26_agents` no regression | PASS | VM benchmark 2,357,373 -> 1,951,265 ns/iter, 17.23% faster |
| AC-5 | Functional equivalence, existing tests green, daemon tick produces events | PASS | Remote ECS/limbo tests, workspace clippy, release daemon deploy, event count grew |
| AC-6 | VM smoke with gateway off: no panic/drift and event count grows | PASS | Final smoke: events 4,055,670 -> 4,055,725 in 60s, gateway inactive, health monitor inactive, `panic|drift=0` |

## Implementation Evidence

| Hotspot | Change | Allocation/behavior proof |
|---|---|---|
| Physics | `physics_system` uses `ResMut<RoomPhysicsWorkspace>` | Removed per-tick room/seed `HashMap` construction; workspace clears active prefix and reuses backing `Vec`/`String` capacity |
| Perception | `generate_perception_into(&mut PerceptionTexts, ...)` and reused `PerceptionState` buffers | Deterministic text uses static fragments; dynamic text writes into caller-owned buffers |
| Persist | `EventStore::append_with_outbox_batch` plus `PersistWorkspace` | One SQLite transaction for event/outbox batch; prebuilt benchmark isolates write-only path; event order and operation-id idempotency preserved |
| Deployment config | `daemon.platform_controlplane.monitored_services = ["sentinel-judge", "sentinel-projection"]` | Prevents daemon service-health self-heal from restarting `sentinel-gateway` during gateway-off benchmark/smoke runs |

## Final VM Benchmarks

Raw logs: `/tmp/issue-276-bench/logs` on `ubuntu@10.0.0.240`.

| Benchmark | Before | After | Delta |
|---|---:|---:|---:|
| `issue276.physics_system/tick_26_agents` | 1,172,325 ns/iter (+/- 35,864) | 857,440 ns/iter (+/- 24,774) | 26.86% faster |
| `issue276.generate_perception/texts_26_agents` | 149,127 ns/iter (+/- 3,788) | 109,845 ns/iter (+/- 4,944) | 26.34% faster |
| `issue276.persist_26_events_individual_tx` -> `issue276.persist_26_events_batch_tx` | 40,377,309 ns/iter (+/- 17,368,530) | 19,151,838 ns/iter (+/- 6,010,275) | 52.57% faster |
| `issue276.persist_26_events_individual_tx_prebuilt` -> `issue276.persist_26_events_batch_tx_prebuilt` | 34,143,782 ns/iter (+/- 12,614,457) | 4,454,866 ns/iter (+/- 1,560,601) | 86.95% faster |
| `room_phase2.bio_tick_26_agents` | 2,357,373 ns/iter (+/- 186,539) | 1,951,265 ns/iter (+/- 84,290) | 17.23% faster |

## System Metrics

Each benchmark class was run with concurrent `vmstat 1`, `mpstat 1`, and `iostat -x 1`.

| Run | CPU / iowait summary | Storage summary |
|---|---|---|
| `clean-baseline-tick` | mpstat usr 12.53%, iowait 0.01%, idle 87.43% | sda util 0.28% |
| `clean-after2-tick` | mpstat usr 12.52%, iowait 0.01%, idle 87.44% | sda util 0.23% |
| `clean-baseline-bio` | mpstat usr 12.49%, iowait 0.01%, idle 87.43% | sda util 0.36% |
| `clean-after2-bio` | mpstat usr 12.50%, iowait 0.01%, idle 87.45% | sda util 0.47% |
| `clean-baseline-persist-individual` | mpstat usr 3.88%, sys 0.38%, iowait 8.25% | w/s 405.88, await 54.06 ms, util 64.78% |
| `clean-after2-persist-batch` | mpstat usr 4.03%, sys 0.37%, iowait 8.04% | w/s 616.90, await 61.06 ms, util 63.15% |
| `clean2-after2-persist-individual-prebuilt` | mpstat usr 4.10%, sys 0.44%, iowait 7.94% | w/s 396.26, await 68.89 ms, util 62.39% |
| `clean2-after2-persist-batch-prebuilt` | mpstat usr 5.50%, sys 0.58%, iowait 6.37% | w/s 795.81, await 90.34 ms, util 50.41% |

Interpretation:

- Physics, perception, and full tick are CPU-pinned single-core runs; low whole-system CPU is expected because the VM has 8 vCPU and the benchmark is pinned to core 2.
- Persist benchmarks are storage-bound on this VM. The relative deltas are valid; absolute SQLite latency is noisy and not a production-baseline gate on this 2011 CPU/storage stack.

## Optimization Loop And Anomalies

Accepted loop:

- Task 1 established the benchmark harness and VM baseline from `e3e960a`.
- Task 2 replaced room physics hot-path maps with reusable workspace storage.
- Task 3 replaced avoidable perception string churn with static fragments and caller-owned buffers.
- Task 4 batched Limbo event/outbox writes.
- Task 4b added `PersistWorkspace` and prebuilt write-only benchmarks to separate store-write cost from event construction/UUID overhead.

Rejected loop:

- `EventStore::prepare_cached` plus `TransactionBehavior::Immediate` was tested and reverted after mixed/noisy A/B results.
- `prepare_cached` alone improved one e2e persist run but worsened the prebuilt write-only path; no clear write-path win, so it was not kept.

Runtime anomalies discovered and resolved:

- Initial benchmark/smoke attempts were contaminated when `sentinel-health-monitor.timer` or daemon service-health restarted `sentinel-gateway`.
- Gateway activity produced LLM/provider failures and extra CPU/noise; those runs are anomaly evidence only and not used for final benchmark deltas.
- Root cause for final smoke: `daemon.platform_controlplane.monitored_services` default included `sentinel-gateway`. Deployment config now explicitly monitors only `sentinel-judge` and `sentinel-projection`, allowing gateway-off validation.

## Verification Commands

Remote build/test gates:

```bash
cargo fmt --all
cargo remote -c -- test -p sentinel-ecs
cargo remote -c -- test -p sentinel-limbo
cargo remote -c -- build -p sentinel-ecs --benches
cargo remote -c -- build -p sentinel-limbo --benches
cargo remote -c -- clippy --workspace --all-targets -- -D warnings
cargo remote -c -- build -p sentinel-daemon --release --features fuse
```

Results:

- `cargo fmt --all`: PASS.
- `cargo remote -c -- test -p sentinel-ecs`: PASS.
- `cargo remote -c -- test -p sentinel-limbo`: PASS.
- `cargo remote -c -- build -p sentinel-ecs --benches`: PASS.
- `cargo remote -c -- build -p sentinel-limbo --benches`: PASS.
- `cargo remote -c -- clippy --workspace --all-targets -- -D warnings`: PASS.
- `cargo remote -c -- build -p sentinel-daemon --release --features fuse`: PASS.

Dashboard/documentation gates:

```bash
cd dashboard
bun test
bun run typecheck
```

Results:

- `bun test`: PASS, 65 tests passed.
- `bun run typecheck`: PASS.

Deploy VM smoke:

```bash
scp target/release/sentinel-daemon ubuntu@10.0.0.240:/tmp/sentinel-daemon-276
ssh ubuntu@10.0.0.240
sudo install -m755 /tmp/sentinel-daemon-276 /opt/sentinel/bin/sentinel-daemon
sudo systemctl restart sentinel-daemon
sudo systemctl stop sentinel-health-monitor.timer sentinel-health-monitor.service sentinel-gateway
```

Final state:

- Deployed daemon SHA-256: `74314e9c71d04d79409cfa08a51fac237cce09104a04f8ba54687235723fb459`.
- Final 60s smoke: events `4,055,670 -> 4,055,725`, delta `55`.
- Services after smoke: `sentinel-daemon active`, `sentinel-judge active`, `nats-server active`, `sentinel-projection active`, `sentinel-gateway inactive`, `sentinel-health-monitor.timer inactive`, `sentinel-health-monitor.service inactive`.
- Journal checks: `panic|drift=0`, `gateway_restart_count=0`.

## Conclusion

Issue #276 is verified on the Deploy-VM with same-machine relative benchmarks. The final accepted implementation improves all measured hot paths and the full 26-agent tick, documents the old hardware context explicitly, keeps benchmark comparisons relative-only, and leaves the gateway inactive for VM smoke validation.
