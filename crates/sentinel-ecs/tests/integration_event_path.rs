//! Integration Tests fuer Issue #9: Event-First Write-Path
//!
//! Beweist die 4 Acceptance Criteria:
//! AC1: Jede mutierende Aktion erzeugt ein Event mit event_id
//! AC2: Kein Direct-Write am Persist-Pfad vorbei
//! AC3: Event+Outbox atomar (via append_with_outbox, hier indirekt geprueft)
//! AC4: E2E: Action rein → Event in Limbo → State veraendert

use sentinel_common::{ActionType, AgentAction, AgentId, Tick, Timestamp};
use sentinel_ecs::{
    attach_redb_store, create_simulation_world, spawn_agent, ActionReceiver, BioState, EventBuffer,
    LimboEventStore, SimulationTime,
};
use sentinel_limbo::EventStore;
use sentinel_redb::StateStore;
use std::sync::Arc;

/// Helfer: World mit EventStore + redb + ActionChannel einrichten
fn setup_world_with_stores() -> (
    bevy_ecs::prelude::World,
    bevy_ecs::prelude::Schedule,
    std::sync::mpsc::Sender<AgentAction>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let event_db_path = dir.path().join("events.db");
    let redb_path = dir.path().join("state.redb");

    let (mut world, schedule) = create_simulation_world();

    // Event Store (Limbo) anbinden
    let event_store = EventStore::open(event_db_path.to_str().unwrap()).unwrap();
    world.insert_resource(LimboEventStore(Arc::new(event_store)));

    // redb State Store anbinden
    attach_redb_store(
        &mut world,
        StateStore::open(redb_path.to_str().unwrap()).unwrap(),
    );

    // Action Channel einrichten
    let (tx, rx) = std::sync::mpsc::channel();
    world.insert_resource(ActionReceiver(std::sync::Mutex::new(rx)));

    (world, schedule, tx, dir)
}

/// Helfer: Einen Tick ausfuehren
fn run_tick(
    world: &mut bevy_ecs::prelude::World,
    schedule: &mut bevy_ecs::prelude::Schedule,
    tick: u64,
) {
    let mut time = world.resource_mut::<SimulationTime>();
    time.tick = Tick(tick);
    time.tick_count = tick;
    time.delta_seconds = 1.0;
    time.sim_hour = 10.0;
    schedule.run(world);
}

// ── AC4 + AC1: E2E drink_coffee → State + 2 Events + Causation-Chain ──

/// E2E: ToolUse "drink_coffee" → BioState.caffeine_mg += 95.0
///      + 2 Events (AgentActionReceived + BioActionPerformed)
///      + Causation-Chain (BioActionPerformed.causation_id == AgentActionReceived.event_id)
///      + Gleiche correlation_id
#[test]
fn test_e2e_drink_coffee_state_and_events() {
    let (mut world, mut schedule, tx, _dir) = setup_world_with_stores();
    let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

    // Initialer Koffeinwert merken
    let initial_caffeine = world.get::<BioState>(entity).unwrap().caffeine_mg;

    // drink_coffee Action senden
    tx.send(AgentAction {
        agent_id: AgentId(1),
        action_type: ActionType::ToolUse,
        target_room: None,
        target_agent: None,
        content: Some("drink_coffee".to_string()),
        timestamp: Timestamp(1000),
        tick: Tick(1),
    })
    .unwrap();

    run_tick(&mut world, &mut schedule, 1);

    // AC4: State veraendert - Koffein muss um 95mg gestiegen sein
    let final_caffeine = world.get::<BioState>(entity).unwrap().caffeine_mg;
    let caffeine_delta = final_caffeine - initial_caffeine;
    assert!(
        caffeine_delta > 90.0 && caffeine_delta < 100.0,
        "drink_coffee should add ~95mg caffeine, delta was {caffeine_delta}"
    );

    // AC1: Events in Limbo pruefen
    let es = world.resource::<LimboEventStore>();
    let events = es.0.get_events_since(0, 100).unwrap();

    let action_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "agent_action_received")
        .collect();
    let bio_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "bio_action_performed")
        .collect();

    assert_eq!(
        action_events.len(),
        1,
        "Genau 1 AgentActionReceived erwartet, got {}",
        action_events.len()
    );
    assert_eq!(
        bio_events.len(),
        1,
        "Genau 1 BioActionPerformed erwartet, got {}",
        bio_events.len()
    );

    // Alle Events haben eindeutige event_ids
    assert!(!action_events[0].event_id.is_empty());
    assert!(!bio_events[0].event_id.is_empty());
    assert_ne!(action_events[0].event_id, bio_events[0].event_id);

    // Causation-Chain: BioActionPerformed.causation_id == AgentActionReceived.event_id
    assert_eq!(
        bio_events[0].causation_id.as_deref(),
        Some(action_events[0].event_id.as_str()),
        "BioActionPerformed.causation_id muss auf AgentActionReceived.event_id zeigen"
    );

    // Gleiche correlation_id (gleicher Vorgang)
    assert_eq!(
        action_events[0].correlation_id, bio_events[0].correlation_id,
        "Beide Events muessen die gleiche correlation_id haben"
    );
    assert!(
        !action_events[0].correlation_id.is_empty(),
        "correlation_id darf nicht leer sein"
    );
}

// ── AC4: E2E eat_meal → hunger=0 + Event ──

#[test]
fn test_e2e_eat_meal_resets_hunger() {
    let (mut world, mut schedule, tx, _dir) = setup_world_with_stores();
    let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

    // Hunger hochsetzen
    {
        let mut bio = world.get_mut::<BioState>(entity).unwrap();
        bio.hunger = 80.0;
    }

    tx.send(AgentAction {
        agent_id: AgentId(1),
        action_type: ActionType::ToolUse,
        target_room: None,
        target_agent: None,
        content: Some("eat_meal".to_string()),
        timestamp: Timestamp(1000),
        tick: Tick(1),
    })
    .unwrap();

    run_tick(&mut world, &mut schedule, 1);

    // State: hunger sollte nahe 0 sein (bio_system laeuft nach input_system,
    // aber bei dt=1s und hunger_rate=12.5/h ist der Anstieg vernachlaessigbar)
    let hunger = world.get::<BioState>(entity).unwrap().hunger;
    assert!(
        hunger < 5.0,
        "eat_meal should reset hunger near 0, got {hunger}"
    );

    // Event vorhanden
    let es = world.resource::<LimboEventStore>();
    let events = es.0.get_events_since(0, 100).unwrap();
    let bio_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "bio_action_performed")
        .collect();
    assert_eq!(bio_events.len(), 1);
    assert!(bio_events[0].payload.contains("eat_meal"));
}

// ── AC4: E2E use_bathroom → bladder=0 + Event ──

#[test]
fn test_e2e_use_bathroom_resets_bladder() {
    let (mut world, mut schedule, tx, _dir) = setup_world_with_stores();
    let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

    // Blase hochsetzen
    {
        let mut bio = world.get_mut::<BioState>(entity).unwrap();
        bio.bladder = 90.0;
    }

    tx.send(AgentAction {
        agent_id: AgentId(1),
        action_type: ActionType::ToolUse,
        target_room: None,
        target_agent: None,
        content: Some("use_bathroom".to_string()),
        timestamp: Timestamp(1000),
        tick: Tick(1),
    })
    .unwrap();

    run_tick(&mut world, &mut schedule, 1);

    // State: bladder sollte nahe 0 sein
    let bladder = world.get::<BioState>(entity).unwrap().bladder;
    assert!(
        bladder < 5.0,
        "use_bathroom should reset bladder near 0, got {bladder}"
    );

    // Event vorhanden
    let es = world.resource::<LimboEventStore>();
    let events = es.0.get_events_since(0, 100).unwrap();
    let bio_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "bio_action_performed")
        .collect();
    assert_eq!(bio_events.len(), 1);
    assert!(bio_events[0].payload.contains("use_bathroom"));
}

// ── AC1: Move-Action → TransitCompleted mit Correlation-Chain ──

#[test]
fn test_e2e_move_action_transit_with_correlation() {
    let (mut world, mut schedule, tx, _dir) = setup_world_with_stores();
    spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

    // Move-Action senden
    tx.send(AgentAction {
        agent_id: AgentId(1),
        action_type: ActionType::Move,
        target_room: Some("buero-dev-1".to_string()),
        target_agent: None,
        content: None,
        timestamp: Timestamp(1000),
        tick: Tick(1),
    })
    .unwrap();

    // Tick 1: input_system setzt Transit + erzeugt AgentActionReceived
    run_tick(&mut world, &mut schedule, 1);

    // Tick 2-4: transit_system zaehlt runter (3000ms bei 1s/tick)
    run_tick(&mut world, &mut schedule, 2);
    run_tick(&mut world, &mut schedule, 3);
    run_tick(&mut world, &mut schedule, 4);

    // Events pruefen
    let es = world.resource::<LimboEventStore>();
    let events = es.0.get_events_since(0, 100).unwrap();

    let action_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "agent_action_received")
        .collect();
    let transit_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "transit_completed")
        .collect();

    assert_eq!(
        action_events.len(),
        1,
        "Genau 1 AgentActionReceived erwartet"
    );
    assert!(
        !transit_events.is_empty(),
        "Mindestens 1 TransitCompleted erwartet"
    );

    // TransitCompleted sollte die correlation_id vom Move-Action haben
    // (gesetzt via Position.transit_correlation_id)
    let move_correlation = &action_events[0].correlation_id;
    let transit_correlation = &transit_events[0].correlation_id;
    assert_eq!(
        move_correlation, transit_correlation,
        "Move-Action und TransitCompleted muessen gleiche correlation_id haben"
    );
}

// ── AC1: Alle 5 ActionTypes erzeugen mindestens AgentActionReceived ──

#[test]
fn test_all_action_types_generate_events() {
    let (mut world, mut schedule, tx, _dir) = setup_world_with_stores();
    spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

    // 5 verschiedene ActionTypes senden
    let actions = vec![
        (ActionType::Chat, None, Some("Hallo".to_string())),
        (ActionType::Move, Some("kueche".to_string()), None),
        (ActionType::ToolUse, None, Some("drink_coffee".to_string())),
        (ActionType::Emote, None, Some("lacht".to_string())),
        (
            ActionType::PhoneCall,
            None,
            Some("Kunde anrufen".to_string()),
        ),
    ];

    for (i, (action_type, target_room, content)) in actions.into_iter().enumerate() {
        tx.send(AgentAction {
            agent_id: AgentId(1),
            action_type,
            target_room,
            target_agent: None,
            content,
            timestamp: Timestamp(i as u64 * 100),
            tick: Tick(1),
        })
        .unwrap();
    }

    run_tick(&mut world, &mut schedule, 1);

    // Alle 5 Actions muessen ein AgentActionReceived-Event haben
    let es = world.resource::<LimboEventStore>();
    let events = es.0.get_events_since(0, 100).unwrap();
    let action_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "agent_action_received")
        .collect();

    assert_eq!(
        action_events.len(),
        5,
        "Alle 5 ActionTypes muessen ein Event erzeugen, got {}",
        action_events.len()
    );

    // Alle event_ids eindeutig
    let unique_ids: std::collections::HashSet<_> =
        action_events.iter().map(|e| &e.event_id).collect();
    assert_eq!(unique_ids.len(), 5, "Alle event_ids muessen eindeutig sein");

    // ToolUse "drink_coffee" erzeugt zusaetzlich BioActionPerformed
    let bio_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "bio_action_performed")
        .collect();
    assert_eq!(
        bio_events.len(),
        1,
        "drink_coffee sollte genau 1 BioActionPerformed erzeugen"
    );
}

// ── AC2: Bio-State aendert sich NUR mit zugehoerigem Event ──

#[test]
fn test_no_bio_mutation_without_event() {
    let (mut world, mut schedule, tx, _dir) = setup_world_with_stores();
    let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

    let initial_caffeine = world.get::<BioState>(entity).unwrap().caffeine_mg;

    // ToolUse mit unbekanntem Content → KEIN Bio-Action, nur WorkContext
    tx.send(AgentAction {
        agent_id: AgentId(1),
        action_type: ActionType::ToolUse,
        target_room: None,
        target_agent: None,
        content: Some("check_email".to_string()),
        timestamp: Timestamp(1000),
        tick: Tick(1),
    })
    .unwrap();

    run_tick(&mut world, &mut schedule, 1);

    // Koffein darf sich NICHT geaendert haben (kein drink_coffee)
    let final_caffeine = world.get::<BioState>(entity).unwrap().caffeine_mg;
    // bio_system Decay ist minimal bei 1s: C(1) = C(0) * e^(-ln2/20520 * 1) ≈ C(0) * 0.99997
    // Bei initial=0 bleibt es 0
    assert!(
        (final_caffeine - initial_caffeine).abs() < 1.0,
        "Koffein sollte sich nicht signifikant aendern ohne drink_coffee: initial={initial_caffeine}, final={final_caffeine}"
    );

    // Kein BioActionPerformed-Event
    let es = world.resource::<LimboEventStore>();
    let events = es.0.get_events_since(0, 100).unwrap();
    let bio_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "bio_action_performed")
        .collect();
    assert_eq!(
        bio_events.len(),
        0,
        "check_email darf kein BioActionPerformed erzeugen"
    );

    // Aber AgentActionReceived muss trotzdem existieren
    let action_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "agent_action_received")
        .collect();
    assert_eq!(
        action_events.len(),
        1,
        "AgentActionReceived muss auch fuer nicht-bio ToolUse erzeugt werden"
    );
}

// ── AC3: Events landen in Limbo (Outbox indirekt via persist_system) ──

#[test]
fn test_events_persisted_to_limbo_via_persist_system() {
    let (mut world, mut schedule, tx, _dir) = setup_world_with_stores();
    spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

    // 3 verschiedene Bio-Actions
    for (i, action_name) in ["drink_coffee", "eat_meal", "use_bathroom"]
        .iter()
        .enumerate()
    {
        tx.send(AgentAction {
            agent_id: AgentId(1),
            action_type: ActionType::ToolUse,
            target_room: None,
            target_agent: None,
            content: Some(action_name.to_string()),
            timestamp: Timestamp(i as u64 * 100),
            tick: Tick(i as u64 + 1),
        })
        .unwrap();
    }

    // Alle Actions in einem Tick verarbeiten
    run_tick(&mut world, &mut schedule, 1);

    // EventBuffer sollte nach persist_system leer sein (Events wurden geflusht)
    let buffer = world.resource::<EventBuffer>();
    assert_eq!(
        buffer.events.len(),
        0,
        "EventBuffer sollte nach persist_system leer sein"
    );

    // Alle 6 Events (3x AgentActionReceived + 3x BioActionPerformed) in Limbo
    let es = world.resource::<LimboEventStore>();
    let events = es.0.get_events_since(0, 100).unwrap();

    let action_count = events
        .iter()
        .filter(|e| e.event_type == "agent_action_received")
        .count();
    let bio_count = events
        .iter()
        .filter(|e| e.event_type == "bio_action_performed")
        .count();

    assert_eq!(action_count, 3, "3 AgentActionReceived erwartet");
    assert_eq!(bio_count, 3, "3 BioActionPerformed erwartet");
}
