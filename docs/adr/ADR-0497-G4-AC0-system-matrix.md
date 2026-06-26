# ADR-0497 / G4 — AC-0: complete mutating-system matrix (per-container freeze)

**Status:** Accepted (companion to `ADR-0497-G4-snapshot-consistency.md`) · **Issue:** #497 (Part of #397) · **Date:** 2026-06-26
**Scope boundary:** production-quality code, experimental cross-node-foundation scope.

## Context

Per-container snapshot/restore (#497) freezes ONE agent while the global tick keeps running.
A snapshot taken while any system still mutates that agent's state is a **torn snapshot** — a
silent correctness break. AC-0 is therefore the gate: **every** ECS system in the tick
schedule, plus every per-agent mutation outside the schedule, is classified here against the
code (file:line, verified — not from memory). A missed mutating system = torn snapshot.

## Canonical schedule

One schedule is built in `crates/sentinel-ecs/src/world.rs:1160-1245`
(`create_simulation_world`); the daemon only calls `schedule.run(&mut world)`
(`orchestrator.rs:4626`) and registers no systems of its own. Replay and the replay-spike use
the same schedule. **16 systems**, 10 chained phases:
`Input → Biology → Physics → Transit → Chaos → Mood → Perception → Decision → Output → Persist`.

Agent entity = anything with `AgentIdentity`. Per-agent components
(`crates/sentinel-common/src/components.rs`): `AgentIdentity, Position, BioState, Personality,
Mood, PerceptionState, WorkContext, Relationships, LlmConfig, ShiftInfo, AgentCapabilities,
EventQueue` (+ `AutonomyCooldown`, `autonomy.rs:24`).

## Matrix — 16 scheduled systems

Classification: **DIRECT** = mutates per-agent ECS components → `Without<Frozen>` guard ·
**INDIRECT** = mutates per-agent state held in Resources/separate entities (NOT components) →
not catchable by a component filter, handled by the bounded-class exclusion · **NO** =
world-/room-level or read-only on agents → no guard needed.

| # | System | file:line | Class | Per-agent write (verified) | Freeze mechanism |
|---|--------|-----------|-------|----------------------------|------------------|
| 1 | input_system | systems.rs:65 | **DIRECT** | `&mut Position/WorkContext/BioState` (:72-74) | `Without<Frozen>` |
| 2 | operator_command_system | systems.rs:353 | **DIRECT** | `&mut Position/WorkContext` (:360-361) | `Without<Frozen>` |
| 3 | work_context_system | systems.rs:1177 | **DIRECT** | `&mut WorkContext` (:1178) | `Without<Frozen>` |
| 4 | bio_system | systems.rs:822 | **DIRECT** | `&mut BioState` (:825) | `Without<Frozen>` |
| 5 | physics_system | systems.rs:927 | NO | read-only `&Position`; writes only room Resources | none |
| 6 | transit_system | systems.rs:1084 | **DIRECT** | `&mut Position` (:1085) | `Without<Frozen>` |
| 7 | encounter_system | systems.rs:~2120 | **DIRECT** | `&mut Position` (:2126) | `Without<Frozen>` |
| 8 | chaos_system | systems.rs:1425 | **DIRECT** | `&mut WorkContext` (via `inject_chaos_event` :1323) | `Without<Frozen>` |
| 9 | smell_system | systems.rs:2224 | NO | read-only `&BioState/&Position`; writes `ActiveSmells` | none |
| 10 | mood_system | systems.rs:1553 | **DIRECT** | `&mut Mood` (:1553) | `Without<Frozen>` |
| 11 | perception_system | systems.rs:1599 | **DIRECT** | `&mut PerceptionState` (:1600) | `Without<Frozen>` |
| 12 | decision_system | decision.rs:31 | **DIRECT** | `&mut EventQueue` (:40) | `Without<Frozen>` |
| 13 | autonomy_system | autonomy.rs:33 | **DIRECT** | `&mut Position/BioState/AutonomyCooldown` (:37-38) | `Without<Frozen>` |
| 14 | task_progress_system | systems.rs:786 | **INDIRECT** | `TaskState` entities (agent-id-keyed via `assigned_to`) | bounded-class: active scheduled task → excluded |
| 15 | output_system | systems.rs:1806 | **INDIRECT** | `RoomChatBuffer` (ResMut), `GaiaBuffer.shorten_ttl` — agent-keyed Resources | bounded-class: active inbound/chat → NotMigratable |
| 16 | persist_system | systems.rs:2269 | NO | read-only agent query; drains `EventBuffer` | none |

→ **11 DIRECT (Without<Frozen>) · 2 INDIRECT (bounded-class) · 3 NO (no guard).**

## Per-agent state OUTSIDE ECS components (the torn-snapshot traps)

A component-only `Without<Frozen>` filter does NOT cover these — they are why AC-0 cannot be
"just add the marker". They are handled by the **bounded migration class** (a resting,
migratable container has none of these active by definition; an active one is a typed reject):

- **`RoomChatBuffer`** (`world.rs:650`, keyed by agent_name) — chat cooldowns/response-counts.
  Active inbound/chat → `NotMigratable` (V11 inbound-policy). Excluded, not frozen.
- **`GaiaBuffer`** (`world.rs:816`, keyed `AgentId`) — Voice-of-Gaia thought TTL. Same exclusion
  (a pending delayed effect → excluded).
- **`TaskState` entities** (agent-id-keyed via `assigned_to`) — active scheduled task →
  `NotMigratable` (V23 scheduled-work exclusion).
- **`ActiveAgentsThisTick`** (`world.rs:643`) — transient per-tick `Vec<AgentId>`, rebuilt every
  tick; not snapshot state (excluded by nature).

## Per-agent mutation OUTSIDE the schedule (daemon tick-loop)

These run in the daemon loop around `schedule.run()`, not in the ECS schedule, so `Without<Frozen>`
cannot reach them. They are fenced by the **owner-epoch fence (#496)** during the migration
window: a retiring `NanoContainer(agent)` scope rejects these writes (StaleEpochError), so the
daemon must not apply them to a frozen/retiring agent.

- **`ShiftInfo.is_on_duty`** — set at shift change (`orchestrator.rs:545/549/4320/4324/7120`); no
  system writes `ShiftInfo`. → migration window must exclude a shift-change for the frozen agent.
- **`apply_personality` / `apply_capabilities`** (`orchestrator.rs:732-733/4475-4476`,
  `world.rs:1374`) — live config-apply onto `Personality`/`AgentCapabilities`/`AgentIdentity`. →
  fenced by owner-epoch during migration (already gated by #425/#440 config-apply path).

## Freeze design (decision)

1. **`Frozen` marker component** (`sentinel-common/components.rs`) — added to the one migrating
   agent's entity; removed on unfreeze/abort.
2. **All 11 DIRECT systems gain `Without<Frozen>`** on their agent query → the frozen agent's
   components are bit-stable across N ticks while the global tick advances (AC-3).
3. **The 2 INDIRECT systems are NOT frozen** — their per-agent state lives in Resources/entities.
   Instead the **bounded migration class excludes** any container with active chat/inbound,
   active Gaia thought, or active scheduled task (typed `NotMigratable`/scheduled-excluded, never
   silent). A resting container has none active, so freezing its components is sufficient.
4. **The 3 NO systems need no guard** (read-only on agents / world-level).
5. **Daemon-loop mutations** are fenced by the #496 owner-epoch during the migration window.

## Negative test (AC-0 AC-2) — proves the guard catches torn snapshots

`crates/sentinel-ecs` test: spawn agents, freeze one, run N ticks, assert the frozen agent's
component bytes are identical before/after AND the global tick advanced AND a non-frozen agent
changed. A second test removes `Without<Frozen>` from ONE direct system (e.g. `bio_system`) and
asserts the freeze invariant now FAILS — proving the guard is load-bearing, not decorative.
A `check`-style enumeration test asserts the count of `Without<Frozen>`-guarded systems matches
this matrix (11), so a newly added direct system without a guard fails CI.

## Honest boundary

This matrix is verified against the schedule on `origin/main` 85f4782. A future system added to
`create_simulation_world` that mutates agent components MUST be added here + guarded; the
enumeration test is the mechanical backstop. Cross-node behavior (per-node sub-worlds, G0) is
out of #497 scope — #497 freezes within one world; the migration that moves the frozen snapshot
across nodes is #501.
