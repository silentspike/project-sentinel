//! #497 AC-0 / AC-3 — per-container freeze invariant + negative control.
//!
//! AC-0 gate (`docs/adr/ADR-0497-G4-AC0-system-matrix.md`): every per-agent-mutating system
//! carries `Without<Frozen>`. These tests are the mechanical backstop:
//!
//! 1. `frozen_agent_is_bit_stable_across_ticks` — runs the REAL schedule with one agent frozen;
//!    the frozen agent's components are byte-identical over N ticks while the global tick advances
//!    and a non-frozen agent changes. If a future direct system loses its `Without<Frozen>` guard,
//!    it would mutate the frozen agent and this test FAILS — so the guard is load-bearing, not
//!    decorative.
//! 2. `unguarded_system_mutates_frozen_agent` — the negative control: a system WITHOUT the guard
//!    DOES mutate a frozen agent, proving the bit-stability check in (1) actually detects an
//!    unguarded mutation (without it, (1) could pass vacuously).

use bevy_ecs::prelude::*;
use sentinel_common::components::{
    AgentIdentity, BioState, EventQueue, Frozen, Mood, PerceptionState, Position, WorkContext,
};
use sentinel_common::{AgentId, Tick};
use sentinel_ecs::{create_simulation_world, spawn_agent, SimulationTime};

/// Serialize the full per-agent mutable component set of one agent. Every one of the 11 DIRECT
/// systems writes at least one of these (Position, BioState, WorkContext, Mood, PerceptionState,
/// EventQueue — autonomy's writes to Position/BioState are covered too), so a byte-diff here
/// catches any unguarded per-agent mutation.
fn agent_state_json(world: &World, e: Entity) -> String {
    let bio = world.entity(e).get::<BioState>().expect("BioState");
    let pos = world.entity(e).get::<Position>().expect("Position");
    let mood = world.entity(e).get::<Mood>().expect("Mood");
    let work = world.entity(e).get::<WorkContext>().expect("WorkContext");
    let perc = world
        .entity(e)
        .get::<PerceptionState>()
        .expect("PerceptionState");
    let events = world.entity(e).get::<EventQueue>().expect("EventQueue");
    serde_json::json!({
        "bio": bio, "pos": pos, "mood": mood,
        "work": work, "perc": perc, "events": events,
    })
    .to_string()
}

#[test]
fn frozen_agent_is_bit_stable_across_ticks() {
    let (mut world, mut schedule) = create_simulation_world();
    let frozen = spawn_agent(&mut world, AgentId(1), "Frozen", "Tester", 1, "buero-dev-1");
    let live = spawn_agent(&mut world, AgentId(2), "Live", "Tester", 1, "buero-dev-1");

    // Freeze exactly one agent (the per-container snapshot subject).
    world.entity_mut(frozen).insert(Frozen);

    let frozen_before = agent_state_json(&world, frozen);
    let live_before = agent_state_json(&world, live);

    let start = world.resource::<SimulationTime>().tick.0;
    for t in 1..=30u64 {
        world.resource_mut::<SimulationTime>().tick = Tick(start + t);
        schedule.run(&mut world);
    }
    let end = world.resource::<SimulationTime>().tick.0;

    let frozen_after = agent_state_json(&world, frozen);
    let live_after = agent_state_json(&world, live);

    assert!(end > start, "global tick must advance ({start} -> {end})");
    assert_eq!(
        frozen_before, frozen_after,
        "FROZEN agent must be byte-stable across {} ticks (a missing Without<Frozen> guard breaks this)",
        end - start
    );
    assert_ne!(
        live_before, live_after,
        "non-frozen agent must change — otherwise the schedule did not actually run"
    );
}

/// Negative control: a system WITHOUT `Without<Frozen>` mutates a frozen agent. This proves the
/// byte-stability assertion above is load-bearing — a forgotten guard is a real, detectable bug.
fn unguarded_bio_mutator(mut q: Query<&mut BioState>) {
    for mut bio in &mut q {
        bio.energy -= 1.0;
    }
}

#[test]
fn unguarded_system_mutates_frozen_agent() {
    let mut world = World::new();
    let e = world
        .spawn((
            AgentIdentity {
                agent_id: AgentId(1),
                name: "Frozen".into(),
                role: "Tester".into(),
            },
            BioState {
                hunger: 50.0,
                energy: 50.0,
                caffeine_mg: 0.0,
                bladder: 0.0,
                stress: 0.0,
                social_need: 0.0,
                comfort: 50.0,
            },
            Frozen,
        ))
        .id();

    let mut schedule = Schedule::default();
    schedule.add_systems(unguarded_bio_mutator);

    let before = world.entity(e).get::<BioState>().unwrap().energy;
    schedule.run(&mut world);
    let after = world.entity(e).get::<BioState>().unwrap().energy;

    assert_ne!(
        before, after,
        "an UNGUARDED system mutates even a frozen agent — this is exactly the torn-snapshot bug \
         that the Without<Frozen> guards prevent and that frozen_agent_is_bit_stable detects"
    );
}
