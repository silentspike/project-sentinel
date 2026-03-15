//! ECS world simulation core using bevy_ecs.
//!
//! Definiert 11 Components, 10 Systems (in strikter Reihenfolge via SimulationPhase),
//! und die World-Setup-Logik fuer die Agent-Simulation.
//!
//! Components liegen in sentinel-common::components (Re-Export hier).
//! Systems rufen sentinel-bio und sentinel-physics fuer echte Berechnungen auf.

pub mod autonomy;
pub mod components;
pub mod decision;
pub mod perception;
pub mod systems;
pub mod world;

pub use components::*;
pub use decision::format_impulse_from_queue;
pub use perception::{format_injection, generate_perception, PerceptionTexts, SmellEvent};
pub use systems::SimulationPhase;
pub use world::{
    apply_capabilities, apply_personality, attach_redb_store, create_simulation_world,
    despawn_agent_from_world, spawn_agent, ActionReceiver, ActiveChaos, ActiveChaosEvent,
    ActiveRoomStimuli, ActiveSmell, ActiveSmells, EventBuffer, LimboEventStore,
    OperatorCommandReceiver, PerceptionSender, PersistTelemetry, PsiMetrics, RedbStateStore,
    RoomDistanceMap, RoomPhysicsSnapshot, RoomPhysicsState, SimulationTime,
    ToolRuntimeResource, ZenohFanoutSender,
};

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::{ActionType, AgentAction, AgentId, RoomStimulusType, Tick, Timestamp};
    use sentinel_limbo::EventStore;
    use sentinel_redb::StateStore;
    use std::sync::Arc;

    #[test]
    fn test_single_agent_spawn() {
        let (mut world, _schedule) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Thomas Mueller", "CEO", 1);

        // Alle 11 Components muessen vorhanden sein
        assert!(world.get::<AgentIdentity>(entity).is_some());
        assert!(world.get::<Position>(entity).is_some());
        assert!(world.get::<BioState>(entity).is_some());
        assert!(world.get::<Personality>(entity).is_some());
        assert!(world.get::<Mood>(entity).is_some());
        assert!(world.get::<PerceptionState>(entity).is_some());
        assert!(world.get::<WorkContext>(entity).is_some());
        assert!(world.get::<Relationships>(entity).is_some());
        assert!(world.get::<LlmConfig>(entity).is_some());
        assert!(world.get::<ShiftInfo>(entity).is_some());
        assert!(world.get::<EventQueue>(entity).is_some());

        // Shift-Werte pruefen (Set 1 = Frueh 06-14)
        let shift = world.get::<ShiftInfo>(entity).unwrap();
        assert_eq!(shift.shift_set, 1);
        assert_eq!(shift.shift_start_hour, 6);
        assert_eq!(shift.shift_end_hour, 14);

        // Identity pruefen
        let identity = world.get::<AgentIdentity>(entity).unwrap();
        assert_eq!(identity.agent_id, AgentId(1));
        assert_eq!(identity.name, "Thomas Mueller");
    }

    #[test]
    fn test_full_shift_15_agents() {
        let (mut world, _schedule) = create_simulation_world();
        let mut entities = Vec::new();

        for i in 1..=15 {
            let entity = spawn_agent(
                &mut world,
                AgentId(i),
                &format!("Agent-{:02}", i),
                "Mitarbeiter",
                1,
            );
            entities.push(entity);
        }

        // 15 einzigartige Entities
        assert_eq!(entities.len(), 15);
        let unique: std::collections::HashSet<_> = entities.iter().collect();
        assert_eq!(unique.len(), 15);

        // Alle haben AgentIdentity
        for entity in &entities {
            assert!(world.get::<AgentIdentity>(*entity).is_some());
        }
    }

    #[test]
    fn test_100_ticks_no_panic() {
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

        // 100 Ticks ohne Panic - SimulationTime wird pro Tick aktualisiert
        for tick in 0..100u64 {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = sentinel_common::Tick(tick);
            time.tick_count = tick;
            time.delta_seconds = 1.0;
            time.sim_hour = 8.0 + (tick as f32 / 3600.0);
            schedule.run(&mut world);
        }
    }

    #[test]
    fn test_tick_rate_performance() {
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
            time.tick = sentinel_common::Tick(tick);
            time.tick_count = tick;
            time.delta_seconds = 1.0;
            time.sim_hour = 8.0 + (tick as f32 / 3600.0);
            schedule.run(&mut world);
        }
        let elapsed = start.elapsed();

        // >100 ticks/s = 100 ticks in unter 1 Sekunde
        assert!(
            elapsed.as_secs_f64() < 1.0,
            "100 ticks took {:.3}s (must be < 1.0s for >100 ticks/s)",
            elapsed.as_secs_f64()
        );
    }

    #[test]
    fn test_bio_system_updates_hunger() {
        let (mut world, mut schedule) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        // Initialer Hunger
        let initial_hunger = world.get::<BioState>(entity).unwrap().hunger;

        // 60 Ticks a 60 Sekunden = 1 Stunde
        for tick in 0..60u64 {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = sentinel_common::Tick(tick);
            time.delta_seconds = 60.0; // 1 Minute pro Tick
            time.sim_hour = 10.0;
            schedule.run(&mut world);
        }

        let final_hunger = world.get::<BioState>(entity).unwrap().hunger;
        // 12.5/h → nach 1h sollte Hunger um ~12.5 gestiegen sein
        assert!(
            final_hunger > initial_hunger + 10.0,
            "Hunger should increase: initial={}, final={}",
            initial_hunger,
            final_hunger
        );
    }

    #[test]
    fn test_mood_system_calculates_valence() {
        let (mut world, mut schedule) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        // Einen Tick ausfuehren
        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
        }
        schedule.run(&mut world);

        // Mood sollte berechnet worden sein (nicht mehr Default 0.2)
        let mood = world.get::<Mood>(entity).unwrap();
        // Bei Energy=80, Stress=15 → positiver Valenz
        assert!(
            mood.valence > 0.0,
            "Valence should be positive for healthy agent, got {}",
            mood.valence
        );
    }

    #[test]
    fn test_perception_generates_text() {
        let (mut world, mut schedule) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        // Einen Tick ausfuehren
        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = sentinel_common::Tick(1);
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
        }
        schedule.run(&mut world);

        let perception = world.get::<PerceptionState>(entity).unwrap();
        // Environment-Text sollte gesetzt sein
        assert!(
            !perception.environment_text.is_empty(),
            "Environment text should not be empty"
        );
        assert!(
            perception.environment_text.contains("Empfang"),
            "Should mention Empfangsbereich, got: {}",
            perception.environment_text
        );
        assert_eq!(perception.last_updated, sentinel_common::Tick(1));
    }

    #[test]
    fn test_transit_system_completes_movement() {
        let (mut world, mut schedule) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        // Agent in Transit setzen
        {
            let mut pos = world.get_mut::<Position>(entity).unwrap();
            pos.in_transit = true;
            pos.transit_target = Some("kueche".to_string());
            pos.transit_remaining_ms = 2000; // 2 Sekunden
        }

        // 3 Ticks a 1 Sekunde (= 3000ms > 2000ms Transit)
        for tick in 0..3u64 {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = sentinel_common::Tick(tick);
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
            schedule.run(&mut world);
        }

        let pos = world.get::<Position>(entity).unwrap();
        assert!(!pos.in_transit, "Transit should be complete");
        assert_eq!(pos.room_id, "kueche", "Should be in target room");
    }

    #[test]
    fn test_redb_store_default_persist_interval_is_20() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("default-interval.redb");
        let store = StateStore::open(db_path.to_str().unwrap()).unwrap();

        let mut world = bevy_ecs::prelude::World::new();
        attach_redb_store(&mut world, store);

        let state = world.resource::<RedbStateStore>();
        assert_eq!(
            state.persist_every_n_ticks, 20,
            "default persist interval should be 20 ticks"
        );
    }

    #[test]
    fn test_persist_telemetry_records_flush_and_batch_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("persist-telemetry.redb");
        let store = StateStore::open(db_path.to_str().unwrap()).unwrap();

        let (mut world, mut schedule) = create_simulation_world();
        attach_redb_store(&mut world, store);
        world.resource_mut::<RedbStateStore>().persist_every_n_ticks = 1;

        for i in 1..=3 {
            spawn_agent(
                &mut world,
                AgentId(i),
                &format!("Agent-{i:02}"),
                "Mitarbeiter",
                1,
            );
        }

        for tick in 0..5u64 {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = sentinel_common::Tick(tick);
            time.tick_count = tick;
            time.delta_seconds = 1.0;
            time.sim_hour = 8.0 + (tick as f32 / 3600.0);
            schedule.run(&mut world);
        }

        let telemetry = world.resource::<PersistTelemetry>();
        assert!(telemetry.enabled, "persist telemetry should be enabled");
        assert_eq!(telemetry.interval_ticks, 1, "persist interval mismatch");
        assert!(
            telemetry.flush_attempts >= 5,
            "expected at least one flush per tick"
        );
        assert_eq!(
            telemetry.flush_attempts, telemetry.flush_success,
            "all test flushes should succeed"
        );
        assert_eq!(telemetry.flush_failures, 0, "unexpected flush failures");
        assert_eq!(
            telemetry.batch_size_last, 3,
            "batch size should match agent count"
        );
        assert_eq!(
            telemetry.batch_size_max, 3,
            "max batch size should be stable"
        );
        assert_eq!(
            telemetry.queue_depth_current, 0,
            "queue depth should stay zero"
        );
        assert_eq!(telemetry.drop_count, 0, "drop count should stay zero");
        assert_eq!(
            telemetry.coalesce_count, 0,
            "coalesce count should stay zero"
        );
        assert!(
            telemetry.avg_flush_latency_us() >= 0.0,
            "flush latency average must be non-negative"
        );
        assert!(
            telemetry.avg_batch_size() >= 3.0,
            "average batch size should reflect full snapshots in this test"
        );
    }

    // ── Event-First Integration Tests ──────────────

    /// E2E: AgentAction via Channel → input_system → EventBuffer → persist_system → Event in Limbo
    #[test]
    fn test_e2e_action_to_event_to_limbo() {
        let dir = tempfile::tempdir().unwrap();
        let event_db_path = dir.path().join("events.db");
        let redb_path = dir.path().join("state.redb");

        let (mut world, mut schedule) = create_simulation_world();

        // Event Store und redb anbinden
        let event_store = EventStore::open(event_db_path.to_str().unwrap()).unwrap();
        world.insert_resource(LimboEventStore(Arc::new(event_store)));
        attach_redb_store(
            &mut world,
            StateStore::open(redb_path.to_str().unwrap()).unwrap(),
        );

        // Agent spawnen
        spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        // Action Channel einrichten
        let (tx, rx) = std::sync::mpsc::channel();
        world.insert_resource(ActionReceiver(std::sync::Mutex::new(rx)));

        // AgentAction senden
        tx.send(AgentAction {
            agent_id: AgentId(1),
            action_type: ActionType::Chat,
            target_room: None,
            target_agent: None,
            content: Some("Guten Morgen!".to_string()),
            timestamp: Timestamp(1000),
            tick: Tick(1),
        })
        .unwrap();

        // Einen Tick ausfuehren
        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(1);
            time.tick_count = 1;
            time.delta_seconds = 1.0;
            time.sim_hour = 8.0;
        }
        schedule.run(&mut world);

        // Verifiziere: Event ist in Limbo
        let es = world.resource::<LimboEventStore>();
        let events = es.0.get_events_since(0, 100).unwrap();
        assert!(
            !events.is_empty(),
            "Events should have been written to Limbo"
        );

        // Mindestens ein AgentActionReceived Event
        let action_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "agent_action_received")
            .collect();
        assert_eq!(
            action_events.len(),
            1,
            "Expected exactly one agent_action_received event"
        );
        assert_eq!(action_events[0].aggregate_id, "AGENT-01");
    }

    /// AC1 (Issue #9): 100+ mutierende Aktionen → alle haben event_id
    #[test]
    fn test_100_actions_all_have_event_ids() {
        let dir = tempfile::tempdir().unwrap();
        let event_db_path = dir.path().join("events-100.db");

        let (mut world, mut schedule) = create_simulation_world();

        let event_store = EventStore::open(event_db_path.to_str().unwrap()).unwrap();
        world.insert_resource(LimboEventStore(Arc::new(event_store)));

        // 5 Agenten spawnen
        for i in 1..=5 {
            spawn_agent(
                &mut world,
                AgentId(i),
                &format!("Agent-{i:02}"),
                "Tester",
                1,
            );
        }

        // Action Channel
        let (tx, rx) = std::sync::mpsc::channel();
        world.insert_resource(ActionReceiver(std::sync::Mutex::new(rx)));

        // 100 Actions senden (20 pro Agent)
        for i in 0..100 {
            let agent_id = AgentId((i % 5 + 1) as u16);
            tx.send(AgentAction {
                agent_id,
                action_type: ActionType::Chat,
                target_room: None,
                target_agent: None,
                content: Some(format!("Nachricht {i}")),
                timestamp: Timestamp(i as u64 * 100),
                tick: Tick(i as u64),
            })
            .unwrap();
        }

        // Einen Tick ausfuehren (alle 100 Actions werden im selben Tick verarbeitet)
        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(1);
            time.tick_count = 1;
            time.delta_seconds = 1.0;
            time.sim_hour = 8.0;
        }
        schedule.run(&mut world);

        // Verifiziere: 100 Events in Limbo, alle mit eindeutiger event_id
        let es = world.resource::<LimboEventStore>();
        let events = es.0.get_events_since(0, 200).unwrap();
        let action_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "agent_action_received")
            .collect();

        assert_eq!(
            action_events.len(),
            100,
            "Expected 100 action events, got {}",
            action_events.len()
        );

        // Alle event_ids sind eindeutig
        let unique_ids: std::collections::HashSet<_> =
            action_events.iter().map(|e| &e.event_id).collect();
        assert_eq!(unique_ids.len(), 100, "All 100 event_ids should be unique");
    }

    /// output_system sendet Perception via Channel
    #[test]
    fn test_output_system_sends_perceptions() {
        let (mut world, mut schedule) = create_simulation_world();
        spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        // Perception Channel (bound=64)
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        world.insert_resource(PerceptionSender(tx));

        // Einen Tick ausfuehren
        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(1);
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
        }
        schedule.run(&mut world);

        // Mindestens eine Perception empfangen
        let perception = rx.try_recv();
        assert!(
            perception.is_ok(),
            "Should have received a Perception message"
        );
        let perception = perception.unwrap();
        assert_eq!(perception.agent_id, AgentId(1));
        assert!(!perception.environment_text.is_empty());
    }

    #[test]
    fn test_output_system_includes_room_physics_and_presence() {
        let (mut world, mut schedule) = create_simulation_world();
        let entity_a = spawn_agent(&mut world, AgentId(1), "Lisa Bergmann", "Design", 1);
        let entity_b = spawn_agent(&mut world, AgentId(2), "Thomas Mueller", "Entwicklung", 1);

        world.get_mut::<Position>(entity_a).unwrap().room_id = "buero-design-1".to_string();
        world.get_mut::<Position>(entity_b).unwrap().room_id = "buero-design-1".to_string();
        world.resource_mut::<ActiveRoomStimuli>().set(
            "buero-design-1",
            RoomStimulusType::Temperature,
            6.0,
            "Temperaturreiz +6.0 °C".to_string(),
            1,
            120,
        );
        world.resource_mut::<ActiveRoomStimuli>().set(
            "buero-design-1",
            RoomStimulusType::Co2,
            1300.0,
            "CO2-Reiz +1300 ppm".to_string(),
            1,
            120,
        );
        world.resource_mut::<ActiveRoomStimuli>().set(
            "buero-design-1",
            RoomStimulusType::Noise,
            42.0,
            "Laermreiz +42 dB".to_string(),
            1,
            120,
        );
        world.resource_mut::<ActiveSmells>().add(
            "buero-design-1",
            "coffee".to_string(),
            0.8,
            1,
            20,
        );

        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        world.insert_resource(PerceptionSender(tx));

        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(1);
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
        }
        schedule.run(&mut world);

        let perceptions: Vec<_> = rx.try_iter().collect();
        let perception = perceptions
            .iter()
            .find(|msg| msg.agent_id == AgentId(1))
            .expect("perception for Lisa must exist");

        assert!(
            perception.environment_text.contains("warm")
                || perception.environment_text.contains("zu warm"),
            "environment should mention warmth, got: {}",
            perception.environment_text
        );
        assert!(
            perception.environment_text.to_lowercase().contains("stickig"),
            "environment should mention CO2 discomfort, got: {}",
            perception.environment_text
        );
        assert!(
            perception.environment_text.contains("Kaffeeduft"),
            "environment should mention coffee smell, got: {}",
            perception.environment_text
        );
        assert!(
            perception.acoustic_text.to_lowercase().contains("laut"),
            "acoustic text should mention loudness, got: {}",
            perception.acoustic_text
        );
        assert!(
            perception.presence_text.contains("Thomas Mueller"),
            "presence should list other agents, got: {}",
            perception.presence_text
        );
    }

    /// Transit-Completion erzeugt DomainEvent
    #[test]
    fn test_transit_completion_creates_event() {
        let dir = tempfile::tempdir().unwrap();
        let event_db_path = dir.path().join("transit-events.db");

        let (mut world, mut schedule) = create_simulation_world();
        let event_store = EventStore::open(event_db_path.to_str().unwrap()).unwrap();
        world.insert_resource(LimboEventStore(Arc::new(event_store)));

        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        // Agent in Transit setzen
        {
            let mut pos = world.get_mut::<Position>(entity).unwrap();
            pos.in_transit = true;
            pos.transit_target = Some("kueche".to_string());
            pos.transit_remaining_ms = 1000; // 1 Sekunde
        }

        // 2 Ticks a 1 Sekunde
        for tick in 0..2u64 {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(tick);
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
            schedule.run(&mut world);
        }

        // Verifiziere: TransitCompleted Event in Limbo
        let es = world.resource::<LimboEventStore>();
        let events = es.0.get_events_since(0, 100).unwrap();
        let transit_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "transit_completed")
            .collect();

        assert_eq!(
            transit_events.len(),
            1,
            "Expected one transit_completed event"
        );
        assert_eq!(transit_events[0].aggregate_id, "AGENT-01");
    }

    /// ToolUse-Dispatch: tool:NAME:INPUT → ToolRuntime → tool_result Event
    #[test]
    fn test_tool_dispatch_creates_tool_result_event() {
        let dir = tempfile::tempdir().unwrap();
        let event_db_path = dir.path().join("tool-events.db");

        let (mut world, mut schedule) = create_simulation_world();
        let event_store = EventStore::open(event_db_path.to_str().unwrap()).unwrap();
        world.insert_resource(LimboEventStore(Arc::new(event_store)));

        let entity = spawn_agent(&mut world, AgentId(1), "Dev Agent", "Developer", 1);

        // Capabilities setzen (Agent benoetigt "search" fuer den Tool-Call)
        if let Some(mut caps) = world.get_mut::<AgentCapabilities>(entity) {
            caps.tools = vec!["search".into()];
        }

        // ToolRuntime mit search registrieren
        let mut tool_runtime = sentinel_wasm::ToolRuntime::new();
        tool_runtime
            .register_tool(sentinel_wasm::ToolDefinition {
                name: "search".into(),
                description: "Suche".into(),
                wasm_path: None,
                tool_type: sentinel_wasm::ToolType::Search,
                required_capabilities: vec!["search".into()],
            })
            .unwrap();
        world.insert_resource(ToolRuntimeResource(tool_runtime));

        // Action Channel einrichten
        let (tx, rx) = std::sync::mpsc::channel();
        world.insert_resource(ActionReceiver(std::sync::Mutex::new(rx)));

        // ToolUse-Action im tool:NAME:INPUT Format senden
        tx.send(AgentAction {
            agent_id: AgentId(1),
            action_type: ActionType::ToolUse,
            target_room: None,
            target_agent: None,
            content: Some(r#"tool:search:{"query":"project status"}"#.to_string()),
            timestamp: Timestamp(1000),
            tick: Tick(1),
        })
        .unwrap();

        // Einen Tick ausfuehren
        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(1);
            time.tick_count = 1;
            time.delta_seconds = 1.0;
            time.sim_hour = 8.0;
        }
        schedule.run(&mut world);

        // Verifiziere: tool_result Event in Limbo
        let es = world.resource::<LimboEventStore>();
        let events = es.0.get_events_since(0, 100).unwrap();
        let tool_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "tool_result")
            .collect();

        assert_eq!(
            tool_events.len(),
            1,
            "Expected one tool_result event, got {}",
            tool_events.len()
        );
        assert_eq!(tool_events[0].aggregate_id, "AGENT-01");
    }

    /// ToolUse-Dispatch: JSON Format {"tool":"NAME","input":"..."} → tool_result
    #[test]
    fn test_tool_dispatch_json_format() {
        let dir = tempfile::tempdir().unwrap();
        let event_db_path = dir.path().join("tool-json.db");

        let (mut world, mut schedule) = create_simulation_world();
        let event_store = EventStore::open(event_db_path.to_str().unwrap()).unwrap();
        world.insert_resource(LimboEventStore(Arc::new(event_store)));

        let entity = spawn_agent(&mut world, AgentId(5), "Dev Agent", "Developer", 1);

        // Capabilities setzen
        if let Some(mut caps) = world.get_mut::<AgentCapabilities>(entity) {
            caps.tools = vec!["chat".into()];
        }

        // ToolRuntime mit chat registrieren
        let mut tool_runtime = sentinel_wasm::ToolRuntime::new();
        tool_runtime
            .register_tool(sentinel_wasm::ToolDefinition {
                name: "chat".into(),
                description: "Chat".into(),
                wasm_path: None,
                tool_type: sentinel_wasm::ToolType::Chat,
                required_capabilities: vec!["chat".into()],
            })
            .unwrap();
        world.insert_resource(ToolRuntimeResource(tool_runtime));

        let (tx, rx) = std::sync::mpsc::channel();
        world.insert_resource(ActionReceiver(std::sync::Mutex::new(rx)));

        // JSON-Format ToolUse
        tx.send(AgentAction {
            agent_id: AgentId(5),
            action_type: ActionType::ToolUse,
            target_room: None,
            target_agent: None,
            content: Some(r#"{"tool":"chat","input":"{\"target\":\"AGENT-02\",\"message\":\"Hallo Team!\"}"}"#.to_string()),
            timestamp: Timestamp(2000),
            tick: Tick(2),
        })
        .unwrap();

        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(2);
            time.tick_count = 2;
            time.delta_seconds = 1.0;
            time.sim_hour = 8.0;
        }
        schedule.run(&mut world);

        let es = world.resource::<LimboEventStore>();
        let events = es.0.get_events_since(0, 100).unwrap();
        let tool_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "tool_result")
            .collect();

        assert_eq!(
            tool_events.len(),
            1,
            "Expected one tool_result from JSON format"
        );
        assert_eq!(tool_events[0].aggregate_id, "AGENT-05");
    }

    /// Transit-Dauer variiert mit Raum-Distanz (nicht hardcoded!)
    #[test]
    fn test_transit_duration_varies_with_distance() {
        let dir = tempfile::tempdir().unwrap();
        let event_db_path = dir.path().join("transit-distance.db");
        let event_store = Arc::new(EventStore::open(event_db_path.to_str().unwrap()).unwrap());

        let (mut world, mut schedule) = create_simulation_world();
        world.insert_resource(LimboEventStore(event_store.clone()));

        let rooms_toml = include_str!("../../../config/rooms.toml");
        let building_config: sentinel_common::room::BuildingConfig =
            toml::from_str(rooms_toml).expect("rooms.toml parse error");
        world.insert_resource(RoomDistanceMap::from_building_config(&building_config));

        spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        let (tx, rx) = std::sync::mpsc::channel();
        world.insert_resource(ActionReceiver(std::sync::Mutex::new(rx)));

        // Move zu nahem Raum: empfang → flur-eg (1 hop → 2300ms)
        tx.send(AgentAction {
            agent_id: AgentId(1),
            action_type: ActionType::Move,
            target_room: Some("flur-eg".to_string()),
            target_agent: None,
            content: None,
            timestamp: Timestamp(1000),
            tick: Tick(1),
        })
        .unwrap();

        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(1);
            time.tick_count = 1;
            time.delta_seconds = 1.0;
            time.sim_hour = 8.0;
        }
        schedule.run(&mut world);

        let events = event_store.get_events_since(0, 100).unwrap();
        let transit_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "transit_started")
            .collect();
        assert_eq!(transit_events.len(), 1);

        let payload1: serde_json::Value = serde_json::from_str(&transit_events[0].payload).unwrap();
        let duration_near = payload1["duration_ms"].as_u64().unwrap();
        assert_eq!(duration_near, 2300, "1-hop transit should be 2300ms");

        // Transit abschliessen (manuell Position resetten)
        {
            let entity_id = world
                .query_filtered::<bevy_ecs::prelude::Entity, bevy_ecs::prelude::With<AgentIdentity>>()
                .iter(&world)
                .next()
                .unwrap();
            let mut pos = world.get_mut::<Position>(entity_id).unwrap();
            pos.in_transit = false;
            pos.transit_target = None;
            pos.transit_remaining_ms = 0;
            pos.room_id = "empfang".to_string();
        }

        // Move zu fernem Raum: empfang → buero-ceo (4 hops → 4700ms)
        tx.send(AgentAction {
            agent_id: AgentId(1),
            action_type: ActionType::Move,
            target_room: Some("buero-ceo".to_string()),
            target_agent: None,
            content: None,
            timestamp: Timestamp(2000),
            tick: Tick(2),
        })
        .unwrap();

        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(2);
            time.tick_count = 2;
            time.delta_seconds = 1.0;
            time.sim_hour = 8.0;
        }
        schedule.run(&mut world);

        let events = event_store.get_events_since(0, 100).unwrap();
        let transit_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "transit_started")
            .collect();
        assert_eq!(transit_events.len(), 2);

        let payload2: serde_json::Value = serde_json::from_str(&transit_events[1].payload).unwrap();
        let duration_far = payload2["duration_ms"].as_u64().unwrap();
        assert_eq!(duration_far, 4700, "4-hop transit should be 4700ms");

        assert!(
            duration_far > duration_near,
            "Far room ({duration_far}ms) must take longer than near room ({duration_near}ms)"
        );
    }

    /// Transit-Dauer bleibt immer in [2000, 5000]ms Bounds
    #[test]
    fn test_transit_duration_within_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let event_db_path = dir.path().join("transit-bounds.db");
        let event_store = Arc::new(EventStore::open(event_db_path.to_str().unwrap()).unwrap());

        let (mut world, mut schedule) = create_simulation_world();
        world.insert_resource(LimboEventStore(event_store.clone()));

        let rooms_toml = include_str!("../../../config/rooms.toml");
        let building_config: sentinel_common::room::BuildingConfig =
            toml::from_str(rooms_toml).expect("rooms.toml parse error");
        world.insert_resource(RoomDistanceMap::from_building_config(&building_config));

        spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        let (tx, rx) = std::sync::mpsc::channel();
        world.insert_resource(ActionReceiver(std::sync::Mutex::new(rx)));

        let targets = [
            "flur-eg",        // 1 hop
            "kueche",         // 2 hops
            "treppenhaus",    // 2 hops
            "buero-ceo",      // 4 hops
            "buero-design-1", // 4 hops
        ];

        for (i, target) in targets.iter().enumerate() {
            {
                let entity = world
                    .query_filtered::<bevy_ecs::prelude::Entity, bevy_ecs::prelude::With<AgentIdentity>>()
                    .iter(&world)
                    .next()
                    .unwrap();
                let mut pos = world.get_mut::<Position>(entity).unwrap();
                pos.in_transit = false;
                pos.transit_target = None;
                pos.transit_remaining_ms = 0;
                pos.room_id = "empfang".to_string();
            }

            tx.send(AgentAction {
                agent_id: AgentId(1),
                action_type: ActionType::Move,
                target_room: Some(target.to_string()),
                target_agent: None,
                content: None,
                timestamp: Timestamp(i as u64 * 1000),
                tick: Tick(i as u64 + 10),
            })
            .unwrap();

            {
                let mut time = world.resource_mut::<SimulationTime>();
                time.tick = Tick(i as u64 + 10);
                time.tick_count = i as u64 + 10;
                time.delta_seconds = 1.0;
                time.sim_hour = 8.0;
            }
            schedule.run(&mut world);
        }

        let events = event_store.get_events_since(0, 200).unwrap();
        let transit_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "transit_started")
            .collect();

        assert_eq!(transit_events.len(), targets.len());

        for event in &transit_events {
            let payload: serde_json::Value = serde_json::from_str(&event.payload).unwrap();
            let duration = payload["duration_ms"].as_u64().unwrap();
            assert!(
                (2000..=5000).contains(&duration),
                "Duration {duration}ms outside bounds [2000, 5000]"
            );
        }
    }

    /// Room-ID nach Transit ist der echte Raumname (nicht "ROOM-1")
    #[test]
    fn test_room_id_correct_after_transit() {
        let (mut world, mut schedule) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        // Agent manuell in Transit nach "kueche" setzen
        {
            let mut pos = world.get_mut::<Position>(entity).unwrap();
            pos.in_transit = true;
            pos.transit_target = Some("kueche".to_string());
            pos.transit_remaining_ms = 1000;
        }

        // 2 Ticks a 1s (1000ms Transit → abgeschlossen)
        for tick in 0..2u64 {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(tick);
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
            schedule.run(&mut world);
        }

        let pos = world.get::<Position>(entity).unwrap();
        assert!(!pos.in_transit);
        assert_eq!(
            pos.room_id, "kueche",
            "Room ID must be real room name, not ROOM-X format"
        );
        assert!(
            !pos.room_id.starts_with("ROOM-"),
            "Room ID must not use ROOM-X format"
        );
    }

    /// Encounter-System erzeugt HallwayEncounterDetected Events
    #[test]
    fn test_encounter_system_generates_events() {
        let dir = tempfile::tempdir().unwrap();
        let event_db_path = dir.path().join("encounter.db");
        let event_store = Arc::new(EventStore::open(event_db_path.to_str().unwrap()).unwrap());

        let (mut world, mut schedule) = create_simulation_world();
        world.insert_resource(LimboEventStore(event_store.clone()));

        // 3 Agents spawnen, alle manuell in Transit setzen
        for i in 1..=3 {
            let entity = spawn_agent(
                &mut world,
                AgentId(i),
                &format!("Agent-{i:02}"),
                "Tester",
                1,
            );
            let mut pos = world.get_mut::<Position>(entity).unwrap();
            pos.in_transit = true;
            pos.transit_target = Some("kueche".to_string());
            pos.transit_remaining_ms = 600_000; // 10min — lang genug damit Transit nicht endet
        }

        // Mehrere Ticks laufen (encounter_system laeuft alle 3 Ticks)
        // Bei 30% Wahrscheinlichkeit und 3 Paaren pro Check: ~0.9 Events pro Check
        // 30 Ticks = 10 Checks → statistisch ~9 Events
        for tick in 1..=30u64 {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(tick);
            time.tick_count = tick;
            time.delta_seconds = 1.0;
            time.sim_hour = 8.0;
            schedule.run(&mut world);
        }

        let events = event_store.get_events_since(0, 500).unwrap();
        let encounter_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "hallway_encounter_detected")
            .collect();

        assert!(
            !encounter_events.is_empty(),
            "Encounter system must generate HallwayEncounterDetected events \
             when multiple agents are in transit (got 0 events after 30 ticks \
             with 3 agents in transit)"
        );
    }

    /// Move-Action mit echtem Raumnamen → korrekte Transit-Duration via RoomDistanceMap
    #[test]
    fn test_move_action_uses_room_distance_map() {
        let dir = tempfile::tempdir().unwrap();
        let event_db_path = dir.path().join("move-rdm.db");
        let event_store = Arc::new(EventStore::open(event_db_path.to_str().unwrap()).unwrap());

        let (mut world, mut schedule) = create_simulation_world();
        world.insert_resource(LimboEventStore(event_store.clone()));

        let rooms_toml = include_str!("../../../config/rooms.toml");
        let building_config: sentinel_common::room::BuildingConfig =
            toml::from_str(rooms_toml).expect("rooms.toml parse error");
        let rdm = RoomDistanceMap::from_building_config(&building_config);

        // Verifiziere: Distanzen sind nicht alle gleich
        let d1 = rdm.distance("empfang", "flur-eg"); // 1 hop
        let d2 = rdm.distance("empfang", "kueche"); // 2 hops
        let d3 = rdm.distance("empfang", "buero-ceo"); // 4 hops
        assert_eq!(d1, 1);
        assert_eq!(d2, 2);
        assert_eq!(d3, 4);

        world.insert_resource(rdm);

        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        let (tx, rx) = std::sync::mpsc::channel();
        world.insert_resource(ActionReceiver(std::sync::Mutex::new(rx)));

        // Move nach kueche (2 hops → 3100ms)
        tx.send(AgentAction {
            agent_id: AgentId(1),
            action_type: ActionType::Move,
            target_room: Some("kueche".to_string()),
            target_agent: None,
            content: None,
            timestamp: Timestamp(1000),
            tick: Tick(1),
        })
        .unwrap();

        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(1);
            time.tick_count = 1;
            time.delta_seconds = 1.0;
            time.sim_hour = 8.0;
        }
        schedule.run(&mut world);

        // Position muss in Transit sein
        let pos = world.get::<Position>(entity).unwrap();
        assert!(pos.in_transit, "Agent should be in transit");
        assert_eq!(pos.transit_target, Some("kueche".to_string()));

        // TransitStarted Event muss korrekte Duration haben
        let events = event_store.get_events_since(0, 100).unwrap();
        let transit_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "transit_started")
            .collect();
        assert_eq!(transit_events.len(), 1);

        let payload: serde_json::Value = serde_json::from_str(&transit_events[0].payload).unwrap();
        assert_eq!(
            payload["duration_ms"].as_u64().unwrap(),
            3100,
            "empfang→kueche = 2 hops → 1500+1600=3100ms"
        );
        assert_eq!(payload["to_room"].as_str().unwrap(), "kueche");
        assert_eq!(payload["from_room"].as_str().unwrap(), "empfang");
    }

    /// ToolUse ohne ToolRuntime: Fallback auf WorkContext
    #[test]
    fn test_tool_dispatch_fallback_without_runtime() {
        let (mut world, mut schedule) = create_simulation_world();
        spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);

        // Kein ToolRuntimeResource eingefuegt!

        let (tx, rx) = std::sync::mpsc::channel();
        world.insert_resource(ActionReceiver(std::sync::Mutex::new(rx)));

        tx.send(AgentAction {
            agent_id: AgentId(1),
            action_type: ActionType::ToolUse,
            target_room: None,
            target_agent: None,
            content: Some("tool:search:test query".to_string()),
            timestamp: Timestamp(1000),
            tick: Tick(1),
        })
        .unwrap();

        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(1);
            time.tick_count = 1;
            time.delta_seconds = 1.0;
            time.sim_hour = 8.0;
        }
        schedule.run(&mut world);

        // Ohne ToolRuntime: Content landet im WorkContext
        let mut query = world.query::<&WorkContext>();
        let work_ctx = query.single(&world).unwrap();
        assert_eq!(
            work_ctx.current_task,
            Some("tool:search:test query".to_string()),
            "Without ToolRuntime, tool content should fall back to WorkContext"
        );
    }

    // ── SmellEvent End-to-End Tests (Issue #195) ─────

    /// Coffee E2E: drink_coffee → SmellEventTriggered in Limbo + ActiveSmells Resource
    #[test]
    fn test_smell_coffee_e2e_event_and_resource() {
        let dir = tempfile::tempdir().unwrap();
        let event_db_path = dir.path().join("smell-coffee.db");
        let event_store = Arc::new(EventStore::open(event_db_path.to_str().unwrap()).unwrap());

        let (mut world, mut schedule) = create_simulation_world();
        world.insert_resource(LimboEventStore(event_store.clone()));

        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);
        // Agent in Kueche platzieren
        world.get_mut::<Position>(entity).unwrap().room_id = "kueche".to_string();

        let (tx, rx) = std::sync::mpsc::channel();
        world.insert_resource(ActionReceiver(std::sync::Mutex::new(rx)));

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

        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(1);
            time.tick_count = 1;
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
        }
        schedule.run(&mut world);

        // SmellEventTriggered muss in Limbo sein
        let events = event_store.get_events_since(0, 200).unwrap();
        let smell_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "smell_event_triggered")
            .collect();
        assert_eq!(
            smell_events.len(),
            1,
            "Expected 1 smell_event_triggered, got {}",
            smell_events.len()
        );
        let payload: serde_json::Value = serde_json::from_str(&smell_events[0].payload).unwrap();
        assert_eq!(payload["smell_type"], "coffee");
        assert_eq!(payload["room_id"], "kueche");

        // ActiveSmells Resource muss coffee in kueche haben
        let active = world.resource::<world::ActiveSmells>();
        let smells = active.get_active("kueche", 1);
        assert_eq!(smells.len(), 1, "ActiveSmells should have 1 entry");
        assert_eq!(smells[0].smell_type, "coffee");
    }

    /// Perception Injection: ActiveSmells coffee → environment_text "Kaffeeduft"
    #[test]
    fn test_smell_perception_injection() {
        let (mut world, mut schedule) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);
        world.get_mut::<Position>(entity).unwrap().room_id = "kueche".to_string();

        // Manuell einen Smell in ActiveSmells einfuegen
        world.resource_mut::<world::ActiveSmells>().add(
            "kueche",
            "coffee".to_string(),
            0.8,
            0,
            200,
        );

        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(1);
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
        }
        schedule.run(&mut world);

        let perception = world.get::<PerceptionState>(entity).unwrap();
        assert!(
            perception.environment_text.contains("Kaffeeduft"),
            "Perception should contain 'Kaffeeduft' when coffee smell active, got: {}",
            perception.environment_text
        );
    }

    /// Smell Decay: abgelaufener Smell wird nicht mehr wahrgenommen
    #[test]
    fn test_smell_decay_removes_from_perception() {
        let (mut world, mut schedule) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);
        world.get_mut::<Position>(entity).unwrap().room_id = "kueche".to_string();

        // Smell mit duration_ticks=5, erstellt bei Tick 0
        world
            .resource_mut::<world::ActiveSmells>()
            .add("kueche", "coffee".to_string(), 0.8, 0, 5);

        // Tick 3: Smell noch aktiv
        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(3);
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
        }
        schedule.run(&mut world);
        let perception = world.get::<PerceptionState>(entity).unwrap();
        assert!(
            perception.environment_text.contains("Kaffeeduft"),
            "At tick 3 smell should still be active"
        );

        // Tick 6: Smell abgelaufen (0 + 5 = 5, tick 6 > 5)
        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(6);
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
        }
        schedule.run(&mut world);
        let perception = world.get::<PerceptionState>(entity).unwrap();
        assert!(
            !perception.environment_text.contains("Kaffeeduft"),
            "At tick 6 smell should have decayed, got: {}",
            perception.environment_text
        );
    }

    /// Kein Smell in fremdem Raum: coffee in kueche, Agent in empfang
    #[test]
    fn test_smell_not_in_different_room() {
        let (mut world, mut schedule) = create_simulation_world();
        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);
        // Agent im Empfang, Smell in Kueche
        world.get_mut::<Position>(entity).unwrap().room_id = "empfang".to_string();

        world.resource_mut::<world::ActiveSmells>().add(
            "kueche",
            "coffee".to_string(),
            0.8,
            0,
            200,
        );

        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(1);
            time.delta_seconds = 1.0;
            time.sim_hour = 10.0;
        }
        schedule.run(&mut world);

        let perception = world.get::<PerceptionState>(entity).unwrap();
        assert!(
            !perception.environment_text.contains("Kaffeeduft"),
            "Agent in empfang should NOT smell coffee from kueche, got: {}",
            perception.environment_text
        );
    }

    /// Food E2E: eat_meal → SmellEventTriggered mit "food" in Limbo
    #[test]
    fn test_smell_food_e2e() {
        let dir = tempfile::tempdir().unwrap();
        let event_db_path = dir.path().join("smell-food.db");
        let event_store = Arc::new(EventStore::open(event_db_path.to_str().unwrap()).unwrap());

        let (mut world, mut schedule) = create_simulation_world();
        world.insert_resource(LimboEventStore(event_store.clone()));

        let entity = spawn_agent(&mut world, AgentId(1), "Test Agent", "Tester", 1);
        world.get_mut::<Position>(entity).unwrap().room_id = "kueche".to_string();

        let (tx, rx) = std::sync::mpsc::channel();
        world.insert_resource(ActionReceiver(std::sync::Mutex::new(rx)));

        // eat_meal Action senden
        tx.send(AgentAction {
            agent_id: AgentId(1),
            action_type: ActionType::ToolUse,
            target_room: None,
            target_agent: None,
            content: Some("eat_meal".to_string()),
            timestamp: Timestamp(2000),
            tick: Tick(1),
        })
        .unwrap();

        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(1);
            time.tick_count = 1;
            time.delta_seconds = 1.0;
            time.sim_hour = 12.0;
        }
        schedule.run(&mut world);

        // SmellEventTriggered mit "food" muss in Limbo sein
        let events = event_store.get_events_since(0, 200).unwrap();
        let smell_events: Vec<_> = events
            .iter()
            .filter(|e| e.event_type == "smell_event_triggered")
            .collect();
        assert_eq!(
            smell_events.len(),
            1,
            "Expected 1 food smell_event_triggered, got {}",
            smell_events.len()
        );
        let payload: serde_json::Value = serde_json::from_str(&smell_events[0].payload).unwrap();
        assert_eq!(payload["smell_type"], "food");
        assert_eq!(payload["room_id"], "kueche");

        // ActiveSmells Resource muss food in kueche haben
        let active = world.resource::<world::ActiveSmells>();
        let smells = active.get_active("kueche", 1);
        assert_eq!(smells.len(), 1);
        assert_eq!(smells[0].smell_type, "food");
    }
}
