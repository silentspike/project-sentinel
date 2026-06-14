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

## Decision (Phase 2): Snapshot-on-Shift-Transition — bypass R1, don't rebuild the shift in replay

Rather than teaching the replay loop to reproduce the shift transition (surgery on the #491 core
restore path, with regression risk), **force an anchor snapshot immediately after every shift
transition** (post-shift state, same tick). Then:
- For any target ≥ the shift tick, the nearest anchor ≤ target is the **post-shift** snapshot, so the
  replay window `(anchor, target]` is **always within a single shift** → #491's engine is already
  byte-exact there (it proved exactly that). R1 is **bypassed** (the replay never crosses a shift);
  R2 (wall-clock trigger tick) is **moot** (the anchor carries the post-shift state directly).
- This satisfies the TOGAF mechanism *"nearest anchor ≤ target + bounded replay (anchor, target]"*
  literally, by guaranteeing the window stays in scope. Additive, couples to #250 Tiered Retention.

Implemented (this PR):
- `SnapshotManager::mark_shift_snapshot_pending()` + `shift_snapshot_pending` flag; `should_create_snapshot`
  returns `true` while pending (interval logic otherwise unchanged); `create_and_store` clears it.
  (`services/sentinel-daemon/src/snapshot.rs`)
- The daemon tick loop calls `mark_shift_snapshot_pending()` right after a completed shift transition
  (despawn + respawn), so the periodic snapshot block later in the same tick captures the post-shift
  state. (`orchestrator.rs`, end of the shift block ~5185)
- `select_anchor_snapshot`: `tick < target_tick` → `tick <= target_tick` so a snapshot exactly at the
  target tick (the forced post-shift anchor for `target == shift_tick`) yields an empty replay window =
  exact post-shift state. The #528 guard (a snapshot with `tick > target` is rejected) is preserved.

Why R1 (replay can't reproduce a shift) makes this the clean answer rather than "rebuild it": the shift
is orchestrated outside the ECS schedule, so the correct contract is "never replay across it", enforced
structurally by the anchor placement. The R1 measurement above is kept as the permanent invariant test
that justifies the policy.

Replay cap unaffected: a within-shift window needs ≤ the hourly anchor distance (≤ 3600 ticks) <
`REPLAY_TICK_CAP` (14 400, `orchestrator.rs:2975`).
