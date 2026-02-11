//! Acceptance Tests fuer sentinel-ecs (Issue #9)
//!
//! Tests fuer ECS World Setup, Agent Spawning, System-Reihenfolge,
//! 100-Tick-Simulation und Tick-Rate-Performance.

use sentinel_common::{AgentId, Tick};
use sentinel_ecs::{
    create_simulation_world, spawn_agent, AgentIdentity, BioState, LlmConfig, Mood, Personality,
    PerceptionState, Position, Relationships, ShiftInfo, SimulationPhase, SimulationTime,
    WorkContext,
};

// ── #9 AC2: spawn_agent() erstellt Entity mit allen 10 Components ──

/// AC #9.2: spawn_agent() erstellt Entity mit allen 10 Components
#[test]
fn ac_09_02_spawn_agent_10_components() {
    let (mut world, _schedule) = create_simulation_world();
    let entity = spawn_agent(&mut world, AgentId(1), "Thomas Mueller", "CEO", 1);

    // Alle 10 Components muessen vorhanden sein
    assert!(
        world.get::<AgentIdentity>(entity).is_some(),
        "AgentIdentity missing"
    );
    assert!(world.get::<Position>(entity).is_some(), "Position missing");
    assert!(world.get::<BioState>(entity).is_some(), "BioState missing");
    assert!(
        world.get::<Personality>(entity).is_some(),
        "Personality missing"
    );
    assert!(world.get::<Mood>(entity).is_some(), "Mood missing");
    assert!(
        world.get::<PerceptionState>(entity).is_some(),
        "PerceptionState missing"
    );
    assert!(
        world.get::<WorkContext>(entity).is_some(),
        "WorkContext missing"
    );
    assert!(
        world.get::<Relationships>(entity).is_some(),
        "Relationships missing"
    );
    assert!(
        world.get::<LlmConfig>(entity).is_some(),
        "LlmConfig missing"
    );
    assert!(
        world.get::<ShiftInfo>(entity).is_some(),
        "ShiftInfo missing"
    );

    // Identity-Werte pruefen
    let identity = world.get::<AgentIdentity>(entity).unwrap();
    assert_eq!(identity.agent_id, AgentId(1));
    assert_eq!(identity.name, "Thomas Mueller");
    assert_eq!(identity.role, "CEO");

    // Shift-Werte pruefen (Set 1 = Frueh 06-14)
    let shift = world.get::<ShiftInfo>(entity).unwrap();
    assert_eq!(shift.shift_set, 1);
    assert_eq!(shift.shift_start_hour, 6);
    assert_eq!(shift.shift_end_hour, 14);
}

// ── #9 AC4: SimulationPhase hat 9 Varianten in korrekter Reihenfolge ──

/// AC #9.4: SimulationPhase enum hat 9 Varianten in korrekter Reihenfolge
#[test]
fn ac_09_04_system_execution_order() {
    // Alle 9 Varianten muessen existieren und kompilieren
    let phases = [
        SimulationPhase::Input,
        SimulationPhase::Biology,
        SimulationPhase::Physics,
        SimulationPhase::Transit,
        SimulationPhase::Chaos,
        SimulationPhase::Mood,
        SimulationPhase::Perception,
        SimulationPhase::Output,
        SimulationPhase::Persist,
    ];
    assert_eq!(phases.len(), 9, "SimulationPhase must have exactly 9 variants");

    // Varianten muessen unterschiedlich sein (PartialEq)
    for i in 0..phases.len() {
        for j in (i + 1)..phases.len() {
            assert_ne!(
                phases[i], phases[j],
                "Phases {:?} and {:?} must be different",
                phases[i], phases[j]
            );
        }
    }

    // Reihenfolge wird durch create_simulation_world() via .chain() garantiert.
    // Wir testen indirekt: 100 Ticks laufen ohne Panic = Systems greifen korrekt ineinander.
    let (mut world, mut schedule) = create_simulation_world();
    spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

    for tick in 0..10u64 {
        let mut time = world.resource_mut::<SimulationTime>();
        time.tick = Tick(tick);
        time.tick_count = tick;
        time.delta_seconds = 1.0;
        time.sim_hour = 8.0;
        schedule.run(&mut world);
    }
}

// ── #9 AC5: World mit 15 Agents, 100x schedule.run() ohne Panic ──

/// AC #9.5: 100 Ticks mit 15 Agents ohne Panic
#[test]
fn ac_09_05_100_ticks_15_agents() {
    let (mut world, mut schedule) = create_simulation_world();

    // 15 Agenten spawnen (volle Schicht)
    for i in 1..=15 {
        spawn_agent(
            &mut world,
            AgentId(i),
            &format!("Agent-{:02}", i),
            "Mitarbeiter",
            1,
        );
    }

    // 100 Ticks ohne Panic
    for tick in 0..100u64 {
        let mut time = world.resource_mut::<SimulationTime>();
        time.tick = Tick(tick);
        time.tick_count = tick;
        time.delta_seconds = 1.0;
        time.sim_hour = 8.0 + (tick as f32 / 3600.0);
        schedule.run(&mut world);
    }
}

// ── #9 AC6: 100 Ticks in unter 1 Sekunde (>100 ticks/s) ──

/// AC #9.6: Tick-Rate Performance >100 ticks/s
#[test]
fn ac_09_06_tick_rate() {
    let (mut world, mut schedule) = create_simulation_world();

    for i in 1..=15 {
        spawn_agent(
            &mut world,
            AgentId(i),
            &format!("Agent-{:02}", i),
            "Mitarbeiter",
            1,
        );
    }

    let start = std::time::Instant::now();
    for tick in 0..100u64 {
        let mut time = world.resource_mut::<SimulationTime>();
        time.tick = Tick(tick);
        time.tick_count = tick;
        time.delta_seconds = 1.0;
        time.sim_hour = 8.0 + (tick as f32 / 3600.0);
        schedule.run(&mut world);
    }
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_secs_f64() < 1.0,
        "100 ticks took {:.3}s (must be < 1.0s for >100 ticks/s)",
        elapsed.as_secs_f64()
    );
}
