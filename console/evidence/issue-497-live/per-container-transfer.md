# #497 — per-container snapshot/restore: 2-VM AC-5 + benchmark (live evidence)

**Date:** 2026-06-26 · **Test cluster:** node-0 `10.0.0.241` / node-1 `10.0.0.242` (VM1071/1072, i5-1235U-class, 2 vCPU, Ubuntu 24.04). **VM 1069 (prod) untouched.** Build only `cargo remote -c --` (.155); the harness binary is standalone (`crates/sentinel-ecs/src/bin/per_container_transfer.rs`), it builds its own fresh world and never touches the running daemon/redb.

## Scope (honest)

The plan separates **#497 (per-container snapshot/restore primitives)** from **#501 (live cross-node move saga over the #569 control stream)**. Per the issue's Out-of-Scope ("local + manual file copy for the 2-VM test") and Test Plan ("snapshot file copied"), AC-5 is verified by transferring the **serialized snapshot file** between the two VMs with `scp` — the exact bytes a real move would carry — then restoring it. The `scp` step **replaces #569**: this proves the snapshot/restore primitives transfer a container **faithfully**, NOT the live daemon move (#501).

## AC-5 — container-scoped state-hash B == A across two VMs

```
node-0 (.241):  per_container_transfer snapshot 1 /tmp/nano-a1.json
  -> snapshot agent=1 bytes=1830 state_hash=1b770ee17dd5a912        (hash_A)

  scp .241:/tmp/nano-a1.json  ->  local  ->  .242:/tmp/nano-a1.json  (1830 bytes, manual file copy = the transfer)

node-1 (.242):  per_container_transfer restore 1 /tmp/nano-a1.json
  -> restore  agent=1 state_hash=1b770ee17dd5a912                    (hash_B)

hash_A == hash_B  ==>  AC-5 PASS (container-scoped, G0 — not whole-world)
```

The state hash (`NanoContainerSnapshot::state_hash`) is **container-scoped** (only the one agent's ECS components + filtered redb rows, never world resources — G0) and **order-stable** (no `HashMap` iteration — the #439 lesson; `Vec`s keep order). It deliberately ignores envelope metadata (`captured_at_tick`/`cut`) so two snapshots of the same resting container hash equal. **Determinism boundary:** same-CPU-class (.241/.242 are the same class; AC-DX); a cross-class transfer stays correct because the state is **copied, not recomputed**.

A single-machine unit test (`tests/nano_container_snapshot.rs::state_hash_survives_serialize_restore_round_trip`) runs the same serialize→restore→re-hash path in CI.

## Benchmark — snapshot/restore latency + bytes/agent

`per_container_transfer bench` on **.241** (200 iters/size; never `cargo remote`, never .240):

| state (sweep) | bytes/agent | snapshot p50/p95/p99/max | restore p50/p95/p99/max |
|---|---|---|---|
| small (1 tick) | 1786 | 2 / 2 / 2 / 4 µs | 8 / 9 / 15 / 22 µs |
| medium (60 ticks) | 1835 | 2 / 2 / 2 / 10 µs | 7 / 8 / 14 / 19 µs |
| large (600 ticks) | 1834 | 2 / 2 / 2 / 10 µs | 7 / 8 / 15 / 16 µs |

Sidecar `vmstat` during the run: r≈0, us 0–2 %, id 98–100 % (the per-agent path is light). CPU `12th Gen i5-1235U`, 2 vCPU.

**Findings (honest):**
- **~1.8 KB/agent** — confirms the KB-thesis (the per-container snapshot is a small, bounded artifact).
- **µs-class** snapshot (p50 ≈ 2 µs) and restore (p50 ≈ 7–8 µs).
- **Size-stable across the tick sweep:** the ECS-native per-agent component set is *fixed-size*, so ticking drifts values but not size. A true memory-size sweep would require growing the bounded `EventQueue`/`Relationships`; for the Track-A bounded class these stay small. The realistic per-agent figure is ~1.8 KB.

Register: `sentinel-daemon-per-container-transfer (#497)` in `/work/company/BENCHMARK-REGISTER.md`.
