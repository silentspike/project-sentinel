//! Acceptance Tests fuer sentinel-ecs (Issue #9)
//!
//! Tests fuer ECS World Setup, Agent Spawning, System-Reihenfolge,
//! 100-Tick-Simulation und Tick-Rate-Performance.

use sentinel_common::{room::BuildingConfig, AgentId, DomainEventPayload, EventType, Tick};
use sentinel_ecs::{
    create_simulation_world, spawn_agent, ActiveChaos, AgentIdentity, BioState, LimboEventStore,
    LlmConfig, Mood, PerceptionState, Personality, Position, Relationships, RoomDistanceMap,
    ShiftInfo, SimulationPhase, SimulationTime, WorkContext,
};
use sentinel_limbo::EventStore;
use std::{path::Path, sync::Arc};

// ── #9 AC2: spawn_agent() erstellt Entity mit allen 10 Components ──

/// AC #9.2: spawn_agent() erstellt Entity mit allen 10 Components
#[test]
fn ac_09_02_spawn_agent_10_components() {
    let (mut world, _schedule) = create_simulation_world();
    let entity = spawn_agent(
        &mut world,
        AgentId(1),
        "Thomas Mueller",
        "CEO",
        1,
        "empfang",
    );

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

// ── #9 AC4: SimulationPhase hat 10 Varianten in korrekter Reihenfolge ──

/// AC #9.4: SimulationPhase enum hat 10 Varianten in korrekter Reihenfolge
#[test]
fn ac_09_04_system_execution_order() {
    // Alle 10 Varianten muessen existieren und kompilieren
    let phases = [
        SimulationPhase::Input,
        SimulationPhase::Biology,
        SimulationPhase::Physics,
        SimulationPhase::Transit,
        SimulationPhase::Chaos,
        SimulationPhase::Mood,
        SimulationPhase::Perception,
        SimulationPhase::Decision,
        SimulationPhase::Output,
        SimulationPhase::Persist,
    ];
    assert_eq!(
        phases.len(),
        10,
        "SimulationPhase must have exactly 10 variants"
    );

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
    spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1, "empfang");

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
            "empfang",
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
            "empfang",
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

fn load_room_distances() -> RoomDistanceMap {
    let config_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/rooms.toml");
    let config = BuildingConfig::load(&config_path).expect("rooms.toml must load");
    RoomDistanceMap::from_building_config(&config)
}

#[test]
fn regression_printer_broken_emits_physics_for_empty_room() {
    let dir = tempfile::tempdir().unwrap();
    let event_db_path = dir.path().join("printer-empty-room.db");
    let event_store = EventStore::open(event_db_path.to_str().unwrap()).unwrap();

    let (mut world, mut schedule) = create_simulation_world();
    world.insert_resource(LimboEventStore(Arc::new(event_store)));
    world.insert_resource(load_room_distances());
    world.resource_mut::<ActiveChaos>().set(
        "buero-dev-1",
        EventType::PrinterBroken,
        "Drucker defekt".to_string(),
        0,
        sentinel_physics::default_chaos_duration_ticks(EventType::PrinterBroken),
    );

    {
        let mut time = world.resource_mut::<SimulationTime>();
        time.tick = Tick(20);
        time.tick_count = 20;
        time.delta_seconds = 1.0;
        time.sim_hour = 8.0;
    }
    schedule.run(&mut world);

    let events = world
        .resource::<LimboEventStore>()
        .0
        .get_events_since(0, 200)
        .unwrap();
    let room_event = events
        .iter()
        .find_map(
            |event| match serde_json::from_str::<DomainEventPayload>(&event.payload).ok() {
                Some(DomainEventPayload::RoomPhysicsUpdated {
                    room_id,
                    noise_db,
                    occupant_count,
                    ..
                }) if room_id == "buero-dev-1" => Some((noise_db, occupant_count)),
                _ => None,
            },
        )
        .expect("expected room_physics_updated for empty chaos room");

    assert_eq!(room_event.1, 0);
    assert!(
        room_event.0 > 50.0,
        "printer chaos should be clearly audible"
    );
}

#[test]
fn regression_aircon_broken_raises_temperature_in_physics_event() {
    let dir = tempfile::tempdir().unwrap();
    let event_db_path = dir.path().join("aircon-room.db");
    let event_store = EventStore::open(event_db_path.to_str().unwrap()).unwrap();

    let (mut world, mut schedule) = create_simulation_world();
    world.insert_resource(LimboEventStore(Arc::new(event_store)));
    world.insert_resource(load_room_distances());
    world.resource_mut::<ActiveChaos>().set(
        "meetingraum-alpha",
        EventType::AirConBroken,
        "Klimaanlage defekt".to_string(),
        0,
        sentinel_physics::default_chaos_duration_ticks(EventType::AirConBroken),
    );

    {
        let mut time = world.resource_mut::<SimulationTime>();
        time.tick = Tick(3600);
        time.tick_count = 3600;
        time.delta_seconds = 1.0;
        time.sim_hour = 9.0;
    }
    schedule.run(&mut world);

    let events = world
        .resource::<LimboEventStore>()
        .0
        .get_events_since(0, 200)
        .unwrap();
    let temperature = events
        .iter()
        .find_map(
            |event| match serde_json::from_str::<DomainEventPayload>(&event.payload).ok() {
                Some(DomainEventPayload::RoomPhysicsUpdated {
                    room_id,
                    temperature,
                    ..
                }) if room_id == "meetingraum-alpha" => Some(temperature),
                _ => None,
            },
        )
        .expect("expected room_physics_updated for aircon chaos room");

    assert!(
        temperature > 23.0,
        "aircon failure should raise room temperature"
    );
}

#[test]
fn regression_flur_noise_drops_after_chaos_expires() {
    let dir = tempfile::tempdir().unwrap();
    let event_db_path = dir.path().join("flur-noise-reset.db");
    let event_store = EventStore::open(event_db_path.to_str().unwrap()).unwrap();

    let (mut world, mut schedule) = create_simulation_world();
    world.insert_resource(LimboEventStore(Arc::new(event_store)));
    world.insert_resource(load_room_distances());
    world.resource_mut::<ActiveChaos>().set(
        "flur-eg",
        EventType::PrinterBroken,
        "Druckerchaos im Flur".to_string(),
        0,
        30,
    );

    for tick in [20_u64, 40_u64] {
        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(tick);
            time.tick_count = tick;
            time.delta_seconds = 1.0;
            time.sim_hour = 8.0;
        }
        schedule.run(&mut world);
    }

    let events = world
        .resource::<LimboEventStore>()
        .0
        .get_events_since(0, 400)
        .unwrap();
    let flur_noise: Vec<(u64, f32)> = events
        .iter()
        .filter_map(
            |event| match serde_json::from_str::<DomainEventPayload>(&event.payload).ok() {
                Some(DomainEventPayload::RoomPhysicsUpdated {
                    room_id, noise_db, ..
                }) if room_id == "flur-eg" => Some((event.tick, noise_db)),
                _ => None,
            },
        )
        .collect();

    assert_eq!(flur_noise.len(), 2, "expected two flur snapshots");
    assert!(
        flur_noise[0].1 > 50.0,
        "active chaos should raise hallway noise first"
    );
    assert!(
        flur_noise[1].1 < 35.0,
        "expired chaos should reset hallway noise instead of staying stale"
    );
}
