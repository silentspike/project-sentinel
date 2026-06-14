# SPIKE-529 — Cross-Nightrun/Shift exact replay: source characterization (AC-1)

Status: **AC-1 GATE — characterized.** This decides the #529 implementation (Phase 2).

## Question (AC-1)
What exactly makes the canonical ECS state-hash diverge between two passes when a bounded
replay window crosses a nightrun / shift-transition? (Pin the element before designing a fix.)

## Method
- **Discriminator 1 (cheapest, first): STRICT vs CORE.** `sentinel-ecs::hash::state_hashes` returns
  STRICT (full `EcsSnapshot`) and CORE (minus `perception_states`/`event_queues`). If only STRICT
  diverged, cross-shift CORE would already be exact (cosmetic narration only).
- **Discriminator 2: phase/tick-offset of the whole shift vs a single field's content.**
- **Deterministic in-process measurement** (0 token, 0 production risk, preferred per plan) rather
  than re-running the live daemon: `replay::tests::replay_does_not_reproduce_shift_transition_r1`.
- Code trace of the live shift path vs the replay loop.

## Finding — two compounding root causes (NOT the originally-assumed evolution result)

### R1 (structural, dominant): the replay loop runs only the ECS schedule; the shift transition is daemon-loop orchestration
- `run_bounded_replay` → `replay_loop` (`services/sentinel-daemon/src/replay.rs:366-405`) per tick does:
  input injection + `SimulationTime`/`sim_hour` update + PSI band + **`schedule.run(world)`** + drains.
  It advances `sim_hour` itself but **never checks a shift boundary and never fires a shift transition.**
- The shift/nightrun transition (despawn old-shift agents, spawn new-shift agents) is
  `RuntimeOrchestrator::shift_transition` (`crates/sentinel-runtime/src/lib.rs:351`), invoked from the
  **daemon tick loop** (`orchestrator.rs:4731-4744`, `if new_shift != current_shift { runtime_orch.shift_transition(new_shift) … }`)
  — it is **NOT an ECS system in the schedule** (grep of `crates/sentinel-ecs` finds only the
  `focus_hours_since_shift_start` helper, no shift-transition system).
- ⇒ A bounded replay spanning a shift boundary **does not reproduce the despawn/respawn at all**, so the
  active agent population (and thus identities/positions/bio/… of the whole set) diverges. This is a
  **CORE-level** divergence, not a STRICT-only perception artifact.

**Measured (in-process, deterministic):** `cargo remote -c -- test -p sentinel-daemon --lib replay_does_not_reproduce_shift_transition_r1 -- --nocapture`
```
[R1] live ids=[1, 2, 3, 5]  replay ids=[1, 2, 3, 4]
[R1] STRICT live=041c1920886ca0d63f66347fd5dd5a53705f9b1745954492af95ac6f47cc4287 replay=553827af2f878e23369763ce7de1d0e349b770273ad7cd455686da6ef701c6f5
[R1] CORE   live=b25edd7b355aac564a23d47b3c84050ff22e36da05133569c4beb71035cba206 replay=a548e730e402483e8d5486e8480b3baba392b63cd2e47bac0b781c7aa9e38f61
test result: ok. 1 passed
```
The live world performed a shift (despawn agent 4, spawn agent 5, done outside the ECS schedule);
the replay world ran only the schedule and kept the pre-shift set `[1,2,3,4]`. **Both STRICT and CORE
diverge**, and the diff is localized to the active agent set (identities). (The real shift transition
also runs consolidation + queues the async evolution job, but the structural gap — replay runs only
the schedule — is the same regardless of those details.)

### R2 (H5, trigger non-determinism): the live shift fires at a wall-clock-determined tick
- `detect_current_shift()` = `chrono::Local::now().hour()` (`services/sentinel-daemon/src/shift.rs:10-12`),
  used at `time_scale == 1.0` (`orchestrator.rs:4731-4734`). So the *tick* at which the shift fires is
  determined by wall-clock alignment, not sim state — two live passes cross the boundary at different
  ticks. The tick is, however, recorded in the event log as `shift_transition_completed` at that tick.
- Note (measurement trap): at `time_scale != 1.0` the trigger is `detect_shift_from_sim_hour(sim_hour)`
  = deterministic. So a forced-shift spike at `time_scale=600` would NOT exhibit R2 — R2 must be argued
  from the production path / event log, not a forced-shift run.

### What it is NOT
- **NOT the evolution result.** `drain_evolution_results` (`orchestrator.rs:2566`) writes **redb only**
  (`set_evolution_batch`), never the ECS world; `hash.rs::canonicalize` hashes the `EcsSnapshot`, which
  contains no redb/evolution; `apply_personality` loads from the TOML `PersonalityConfig` (`world.rs:1374`),
  not redb. With the gateway off there is no ECS feedback path. So recording the evolution result would
  not fix the divergence. (This is why the original "✅ DECIDED record-evolution-result" was wrong — the
  premise was corrected on the issue.)
- **NOT Branch B (STRICT-only).** CORE diverges (measured above), so cross-shift CORE replay is *not*
  already exact — there is real sim-core divergence (the agent set).

## Cross-reference (prior empirical reproduction)
`console/evidence/issue-491-live/AC-note-nightrun-boundary.txt`: on the live VM the post-restore forward
trajectory matched ground truth byte-for-byte until exactly the nightrun tick (2785860→2785861), then
diverged — consistent with R1+R2.

## Implication for Phase 2 (decided by this gate)
The fix must make the replay reproduce the shift/nightrun transition deterministically at its
**recorded** tick:
1. During replay, fire the shift transition (despawn/respawn for the new shift) at the tick of the
   recorded `shift_transition_completed` event — i.e., the replay path must include the daemon-loop
   shift orchestration, driven by the event log, not by `detect_current_shift()` (which would read
   replay-time wall-clock). (Branch H5 + the structural R1 extension.)
2. Gate any async re-trigger during replay (no re-queue of the evolution background task), analogous to
   the `source == "autonomy"` suppression (`replay.rs:79`) and the store removals in the RAII guard.
3. The recordable "input" is the **shift transition (trigger tick + new shift set)**, already present in
   the event log — not a new evolution-content field. Confirm during implementation whether the
   despawn/respawn alone closes the gap or whether consolidation side-effects also need recording
   (re-measure with the harness after wiring the shift into the replay path).

Replay cap unaffected: a window straddling one shift boundary needs ≤ the hourly anchor distance
(≤ 3600 ticks) < `REPLAY_TICK_CAP` (14 400, `orchestrator.rs:2975`).
