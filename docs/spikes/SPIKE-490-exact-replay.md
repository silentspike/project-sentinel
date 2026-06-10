# SPIKE #490 — Exact replay (bounded, same-machine): Go/No-Go for Arbitrary Restore (TM-3)

**Status:** experimental spike (not production). Harness: `services/sentinel-daemon/src/bin/replay-spike.rs`.
**Verdict: GO for TM-3, conditional on the restore-completeness prerequisites below.**

## Question

TM-3 ("back to any point / any event of the first hour") assumes the simulation can be
**re-executed byte-/state-identically** from an anchor snapshot to a target tick on the **same
machine**. This spike proves or disproves that empirically and pins the determinism profile.

## Method

Genuine re-execution (not event-apply — `BioStateUpdated`/`RoomPhysicsUpdated` events are sampled
every 60 ticks, so the event stream alone cannot reconstruct a byte-identical intermediate state).
The harness builds its own world (`create_simulation_world` + 26 agents), feeds tick-pinned scripted
inputs through the daemon's channel resources, and compares canonicalized ECS state hashes. No
daemon, no redb, gateway/judge off (0 tokens). PSI is treated as a declared per-tick input.

**State hash (canonicalization):** N1 sort every component vec by agent/task id (Bevy allocation
order is not stable across restore); N2 `Position.transit_correlation_id := None` (a per-action
UUIDv4 — event identity, not sim state); N3 canonicalize the chaos/stimuli JSON (HashMap key order);
N4 nothing else (f32 stays a bit pattern via the legacy bincode codec). **STRICT** = full snapshot;
**CORE** = without `PerceptionState`/`EventQueue` (separates perception-text gaps from sim-core
divergence). Each normalization step is justified by an A/B run.

**Test ladder:** T1 two full live runs (engine determinism) · T2 live@target vs restore(anchor)+
replay (AC-2) · T3 two replays (AC-1) · T4 inputs reconstructed from the persisted event log ·
T5 per-tick trace (first divergence) · T6 order-probe (100×, schedule/HashMap/task order) ·
T7 negative control (PSI on vs off — the hash MUST react). Each `run-all` runs in-process; the
`live`/`replay`/`compare` subcommands repeat it **cross-process** (a fresh process gives a new
HashMap random-state and ASLR — the stronger same-machine proof). Scenario variants: **clean**
and **gap-probe** (residue laid across the anchor).

## Determinism profile (AC-4)

Same binary, rustc 1.94.0 (`rust-toolchain.toml`), release profile, no `RUSTFLAGS`/`target-cpu`
overrides (no `.cargo/config.toml`), x86-64 baseline (no FMA contraction), Bevy schedule effectively
totally ordered, single machine (deploy VM, Intel i7-3930K), inputs tick-pinned, PSI a declared
input, `SENTINEL_CHAOS_ENABLED=true` (reported per run). **Not claimed:** cross-arch (#406),
cross-binary, debug-vs-release.

## Results (deploy VM, i7-3930K; raw JSON + `vmstat`/`mpstat` in `console/evidence/issue-490-replay-spike/`)

Matrix {100, 1000, 10000} × {clean, gap-probe} × {scripted PSI, zero PSI}:

| Test | Result |
| --- | --- |
| **T1** engine determinism | **PASS** for every window/variant/PSI (two live runs → identical hash) |
| **T3** two replays | **PASS** everywhere (AC-1) |
| **T7** negative control | **PASS** everywhere (PSI-on ≠ PSI-off → the hash reacts to a real input) |
| **T6** order-probe | **PASS** — identical anchor, 1 tick, 100× → constant hash (no Bevy/HashMap/`task_progress_system` order nondeterminism observed) |
| **T2** restore-vs-live (AC-2) | **PASS** for **zero-PSI at all windows incl. 10 000** (in- and cross-process); **FAIL** for **scripted-PSI at window ≥ 1000** |

**AC-1 / AC-2 (window 1000, zero PSI), quoted hashes:**

```
t1_hash_a = t1_hash_b = fdd662c8a1fcecdfe106c6c7237bd6801c687c003d5a5c578c20c1f21ad2933b   (engine determinism)
t2_live.strict   = t2_replay.strict   = fdd662c8a1fcecdfe106c6c7237bd6801c687c003d5a5c578c20c1f21ad2933b   (AC-2)
t2_live.core     = t2_replay.core     = d3e167b69c0a31a294ac6b03428d38909393c89addbcf87a6fc8f4920d111735
```

**Cross-process (separate process, fresh HashMap seed/ASLR):** `compare` → `equal: true`, identical
to the in-process hash above. Same-machine re-execution is byte-stable across processes.

## AC-3 — nondeterminism source matrix

| # | Source | Behaviour | Test | Classification |
| --- | --- | --- | --- | --- |
| 1 | UUIDv4 `transit_correlation_id` leaking into `Position` | per-action UUID in ECS state | N2 + T2 | normalized away (event identity, not sim state) |
| 2 | `SystemTime`/UUID in event metadata | events only, no component field | code audit | documented, no measure |
| 3 | **PSI → `apply_psi_stress`** | real per-tick state input (thresholds) | T7 (on/off), variants | declared input; PSI-driven pre-anchor autonomy exposes the restore gap (#6) |
| 4 | HashMap iteration (room aggregation) | state effects commutative | T6 + cross-process | not observed as state divergence |
| 5 | Bevy query / entity allocation order | unstable across restore | N1 + T2 (zero-PSI PASS) | normalized away by N1 |
| 6 | **Restore gap: `AutonomyCooldown` + `ActiveSmells`/`RoomChatBuffer`/`GaiaBuffer`/`BroadcastBuffer`** not in `EcsSnapshot` | lost at the anchor | scripted-PSI T2 FAIL; T5 first divergence tick 426 (clean) / 401 (gap-probe), anchor 400 | **predicted gap → TM-3 prerequisite** |
| 7 | `task_progress_system` unconstrained in the Decision set | potential ResMut order ambiguity | T6 (100× stable) | not observed (no task entities in the scenario; revisit with tasks) |
| 8 | f32 `exp`/`ln`/`sin` (bio) | same-binary/same-machine | T1/T3 over 10 000 ticks bit-exact | stable under profile; cross-arch = #406 |
| 9 | `delta_seconds` / `sim_hour` | constant / `f(tick)` | constructive + T1 | deterministic |
| 10 | chaos/encounter splitmix64 | `f(tick, ids)` | T1 | deterministic |
| 11 | channel arrival timing (daemon Zenoh→mpsc) | tick-pinned in the spike | analysis | **key TM-3 insight: `AgentActionReceived.tick` pins the arrival assignment — the event log resolves this source** |

## T4 — is the event log a sufficient input log?

`event-replay` (window 1000, clean, scripted): **78/78 agent actions reconstructed** from the
persisted `agent_action_received` events, and the log-reconstructed replay hash **matched** the
script-based replay (`match: true`). Internally-derived actions (`content == "autonomy:bio_emergency"`)
are excluded (re-derived during replay). **The event log is a faithful input log for agent actions.**
The one operator chaos in the scenario was pre-anchor (its effect is captured in the snapshot via
`ActiveChaos`), so its non-reconstructability did not bite here — but it is a real gap: `ChaosTriggered`
lacks `duration_ticks`, and `AgentActionReceived` lacks a `source` (external vs. derived) field. That
is a TM-3 prerequisite for *post-anchor* operator events.

## Benchmarks (break-even)

| Window | Live compute | Replay (anchor 40%) | Snapshot |
| --- | --- | --- | --- |
| 100 | 42.2 ms | 31.5 ms | 13.5 KB |
| 1000 | 298.8 ms | 177.3 ms | 16.6 KB |
| 10000 | 3043.6 ms | 1858.5 ms | 14.6 KB |

Replay cost ≈ `(target − anchor)/window × live-compute` (replay only re-executes the suffix). The
snapshot is tiny (~15 KB) and cheap to encode, so **anchor + replay beats a full re-run whenever the
anchor is at/after ~0% and the suffix is shorter than the whole window** — i.e., denser anchors trade
~15 KB of storage for a linear reduction in replay latency. For a slider to "any point", the practical
design is periodic anchors + bounded replay of the suffix.

## Go/No-Go

**GO for TM-3, conditional.** The engine is deterministic on the same machine (T1/T3/T6/T7 pass
universally; cross-process identical), and bounded replay is **byte-identical to the live run when the
anchor snapshot captures the complete state** (zero-PSI T2 perfect through 10 000 ticks, in- and
cross-process). Every observed failure is an **enumerated, predicted snapshot-coverage gap**, traced
by T5 to the exact post-anchor divergence — not engine nondeterminism.

**TM-3 prerequisites (decided by this spike):**
1. **Extend `EcsSnapshot`** to include `AutonomyCooldown`, `ActiveSmells`, `RoomChatBuffer`,
   `GaiaBuffer`, `BroadcastBuffer` (the resources `restore_ecs_state` currently drops). This is the
   single cause of the scripted-PSI T2 failures.
2. **`AgentActionReceived.tick` pins the channel-arrival assignment** — the event log is the input
   log; build the TM-3 input replay on it (relevant to #491).
3. **Harden operator event fields** for post-anchor reconstruction: `ChaosTriggered.duration_ticks`,
   an explicit `source` (external vs. derived) on `AgentActionReceived`.
4. Order `task_progress_system` with `.after(...)` before TM-3 ships (not observed here, but it is the
   one unconstrained system in the Decision set).
5. Keep the determinism profile (same binary/rustc/no-RUSTFLAGS); cross-arch is out of scope (#406).
