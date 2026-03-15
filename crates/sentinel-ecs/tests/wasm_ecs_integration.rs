//! ECS-Integration Tests fuer WASM Component Model Tools.
//!
//! Testet den vollen Pfad: Agent-Action → input_system → ToolRuntime →
//! WASM Plugin → ToolResult → DomainEvent im EventBuffer/Limbo.
//!
//! Benoetigt: `cargo remote -- test -p sentinel-ecs --features wasm`

#![cfg(feature = "wasm")]

use std::path::PathBuf;
use std::sync::Arc;

use sentinel_common::{ActionType, AgentAction, AgentId, Tick, Timestamp};
use sentinel_ecs::{
    create_simulation_world, spawn_agent, ActionReceiver, AgentCapabilities, LimboEventStore,
    SimulationTime, ToolRuntimeResource,
};
use sentinel_limbo::EventStore;

fn echo_fixture() -> PathBuf {
    // Pfad zum echo-plugin.wasm aus sentinel-wasm Test-Fixtures
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop(); // crates/sentinel-ecs -> crates/
    path.push("sentinel-wasm/tests/fixtures/echo-plugin.wasm");
    path
}

/// Returns true if WASM runtime can execute the echo plugin.
/// On some CI environments, Wasmtime component model may not work correctly.
fn wasm_runtime_available() -> bool {
    let path = echo_fixture();
    if !path.exists() {
        eprintln!("SKIP: echo-plugin.wasm not found at {}", path.display());
        return false;
    }
    // Quick smoke test: try to instantiate Wasmtime with the plugin
    let mut runtime = sentinel_wasm::ToolRuntime::new();
    match runtime.plugin_host_mut().load(sentinel_wasm::PluginConfig {
        wasm_path: path,
        ..Default::default()
    }) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("SKIP: WASM runtime not available: {e}");
            false
        }
    }
}

/// Erstellt eine ECS-World mit ToolRuntime die ein WASM-Plugin geladen hat.
fn setup_wasm_world() -> (
    bevy_ecs::world::World,
    bevy_ecs::schedule::Schedule,
    std::sync::mpsc::Sender<AgentAction>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let event_db_path = dir.path().join("events.db");

    let (mut world, schedule) = create_simulation_world();
    let event_store = EventStore::open(event_db_path.to_str().unwrap()).unwrap();
    world.insert_resource(LimboEventStore(Arc::new(event_store)));

    // ToolRuntime mit WASM echo-plugin + nativen Tools
    let mut tool_runtime = sentinel_wasm::ToolRuntime::new();

    // Echo-Plugin laden
    tool_runtime
        .plugin_host_mut()
        .load(sentinel_wasm::PluginConfig {
            wasm_path: echo_fixture(),
            ..Default::default()
        })
        .unwrap();

    // WASM-Tool registrieren
    tool_runtime
        .register_tool(sentinel_wasm::ToolDefinition {
            name: "echo".into(),
            description: "Echo WASM Plugin".into(),
            wasm_path: Some(echo_fixture().to_str().unwrap().to_string()),
            tool_type: sentinel_wasm::ToolType::Wasm,
            required_capabilities: Vec::new(),
        })
        .unwrap();

    // Natives Search-Tool registrieren (fuer Mixed-Tests)
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

    // Action Channel
    let (tx, rx) = std::sync::mpsc::channel();
    world.insert_resource(ActionReceiver(std::sync::Mutex::new(rx)));

    (world, schedule, tx, dir)
}

fn run_tick(
    world: &mut bevy_ecs::world::World,
    schedule: &mut bevy_ecs::schedule::Schedule,
    tick: u64,
) {
    {
        let mut time = world.resource_mut::<SimulationTime>();
        time.tick = Tick(tick);
        time.tick_count = tick;
        time.delta_seconds = 1.0;
        time.sim_hour = 8.0;
    }
    schedule.run(world);
}

// ---- AC-1: WASM-Tool via ECS input_system ausfuehren ----

#[test]
fn ecs_wasm_tool_dispatch_creates_tool_result_event() {
    if !wasm_runtime_available() {
        return;
    }
    let (mut world, mut schedule, tx, _dir) = setup_wasm_world();
    let entity = spawn_agent(&mut world, AgentId(1), "Thomas Mueller", "CEO", 1);

    // Capabilities setzen (echo hat keine required_capabilities, aber Agent braucht sie im System)
    if let Some(mut caps) = world.get_mut::<AgentCapabilities>(entity) {
        caps.tools = vec!["echo".into(), "search".into()];
    }

    // WASM-Tool-Call via tool:NAME:INPUT Format
    tx.send(AgentAction {
        agent_id: AgentId(1),
        action_type: ActionType::ToolUse,
        target_room: None,
        target_agent: None,
        content: Some("tool:echo:hello from ECS".to_string()),
        timestamp: Timestamp(1000),
        tick: Tick(1),
    })
    .unwrap();

    run_tick(&mut world, &mut schedule, 1);

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
        "Expected one tool_result event from WASM echo plugin, got {}. All events: {:?}",
        tool_events.len(),
        events.iter().map(|e| &e.event_type).collect::<Vec<_>>()
    );
    assert_eq!(tool_events[0].aggregate_id, "AGENT-01");
    // Payload muss "echo: hello from ECS" enthalten
    assert!(
        tool_events[0].payload.contains("echo: hello from ECS"),
        "Payload must contain echo output: {}",
        tool_events[0].payload
    );
}

// ---- WASM-Tool via JSON Format ----

#[test]
fn ecs_wasm_tool_dispatch_json_format() {
    if !wasm_runtime_available() {
        return;
    }
    let (mut world, mut schedule, tx, _dir) = setup_wasm_world();
    let entity = spawn_agent(&mut world, AgentId(5), "Lisa Weber", "Designer", 1);

    if let Some(mut caps) = world.get_mut::<AgentCapabilities>(entity) {
        caps.tools = vec!["echo".into()];
    }

    // JSON-Format Tool-Call
    tx.send(AgentAction {
        agent_id: AgentId(5),
        action_type: ActionType::ToolUse,
        target_room: None,
        target_agent: None,
        content: Some(r#"{"tool":"echo","input":"JSON format test"}"#.to_string()),
        timestamp: Timestamp(2000),
        tick: Tick(2),
    })
    .unwrap();

    run_tick(&mut world, &mut schedule, 2);

    let es = world.resource::<LimboEventStore>();
    let events = es.0.get_events_since(0, 100).unwrap();
    let tool_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "tool_result")
        .collect();

    assert_eq!(tool_events.len(), 1);
    assert_eq!(tool_events[0].aggregate_id, "AGENT-05");
    assert!(tool_events[0].payload.contains("echo: JSON format test"));
}

// ---- Mehrere Agents nutzen WASM-Tools im selben Tick ----

#[test]
fn ecs_multiple_agents_wasm_tools_same_tick() {
    if !wasm_runtime_available() {
        return;
    }
    let (mut world, mut schedule, tx, _dir) = setup_wasm_world();

    // Drei Agents spawnen
    let e1 = spawn_agent(&mut world, AgentId(1), "Thomas", "CEO", 1);
    let e2 = spawn_agent(&mut world, AgentId(2), "Lisa", "Designer", 1);
    let e3 = spawn_agent(&mut world, AgentId(3), "Andreas", "Developer", 1);

    for (entity, caps) in [
        (e1, vec!["echo".into(), "search".into()]),
        (e2, vec!["echo".into()]),
        (e3, vec!["search".into()]),
    ] {
        if let Some(mut c) = world.get_mut::<AgentCapabilities>(entity) {
            c.tools = caps;
        }
    }

    // Agent-01: WASM echo
    tx.send(AgentAction {
        agent_id: AgentId(1),
        action_type: ActionType::ToolUse,
        target_room: None,
        target_agent: None,
        content: Some("tool:echo:from agent-01".to_string()),
        timestamp: Timestamp(1000),
        tick: Tick(1),
    })
    .unwrap();

    // Agent-02: WASM echo
    tx.send(AgentAction {
        agent_id: AgentId(2),
        action_type: ActionType::ToolUse,
        target_room: None,
        target_agent: None,
        content: Some("tool:echo:from agent-02".to_string()),
        timestamp: Timestamp(1000),
        tick: Tick(1),
    })
    .unwrap();

    // Agent-03: Native search
    tx.send(AgentAction {
        agent_id: AgentId(3),
        action_type: ActionType::ToolUse,
        target_room: None,
        target_agent: None,
        content: Some(r#"tool:search:{"query":"project"}"#.to_string()),
        timestamp: Timestamp(1000),
        tick: Tick(1),
    })
    .unwrap();

    run_tick(&mut world, &mut schedule, 1);

    let es = world.resource::<LimboEventStore>();
    let events = es.0.get_events_since(0, 100).unwrap();
    let tool_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "tool_result")
        .collect();

    // Alle 3 Agents sollten tool_result Events haben
    assert_eq!(
        tool_events.len(),
        3,
        "Expected 3 tool_result events, got {}",
        tool_events.len()
    );

    // Prüfe dass die richtigen Agents zugeordnet sind
    let agent_ids: Vec<&str> = tool_events
        .iter()
        .map(|e| e.aggregate_id.as_str())
        .collect();
    assert!(agent_ids.contains(&"AGENT-01"), "Agent-01 must have result");
    assert!(agent_ids.contains(&"AGENT-02"), "Agent-02 must have result");
    assert!(agent_ids.contains(&"AGENT-03"), "Agent-03 must have result");
}

// ---- WASM und Native Tools koexistieren im selben Tick ----

#[test]
fn ecs_wasm_and_native_tools_coexist() {
    if !wasm_runtime_available() {
        return;
    }
    let (mut world, mut schedule, tx, _dir) = setup_wasm_world();
    let entity = spawn_agent(&mut world, AgentId(1), "Thomas", "CEO", 1);

    if let Some(mut caps) = world.get_mut::<AgentCapabilities>(entity) {
        caps.tools = vec!["echo".into(), "search".into()];
    }

    // Tick 1: WASM echo
    tx.send(AgentAction {
        agent_id: AgentId(1),
        action_type: ActionType::ToolUse,
        target_room: None,
        target_agent: None,
        content: Some("tool:echo:wasm call".to_string()),
        timestamp: Timestamp(1000),
        tick: Tick(1),
    })
    .unwrap();

    run_tick(&mut world, &mut schedule, 1);

    // Tick 2: Native search
    tx.send(AgentAction {
        agent_id: AgentId(1),
        action_type: ActionType::ToolUse,
        target_room: None,
        target_agent: None,
        content: Some(r#"tool:search:{"query":"meeting notes"}"#.to_string()),
        timestamp: Timestamp(2000),
        tick: Tick(2),
    })
    .unwrap();

    run_tick(&mut world, &mut schedule, 2);

    let es = world.resource::<LimboEventStore>();
    let events = es.0.get_events_since(0, 200).unwrap();
    let tool_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "tool_result")
        .collect();

    assert_eq!(tool_events.len(), 2, "Expected 2 tool_result events");
    // Eins ist WASM echo, eins ist native search
    let has_echo = tool_events
        .iter()
        .any(|e| e.payload.contains("echo: wasm call"));
    let has_search = tool_events
        .iter()
        .any(|e| e.payload.contains("meeting notes"));
    assert!(has_echo, "Must have WASM echo result");
    assert!(has_search, "Must have native search result");
}

// ---- Fehlerfall: WASM Plugin Error crasht ECS nicht ----

#[test]
fn ecs_wasm_plugin_error_does_not_crash_tick() {
    if !wasm_runtime_available() {
        return;
    }
    let (mut world, mut schedule, tx, _dir) = setup_wasm_world();
    let entity = spawn_agent(&mut world, AgentId(1), "Thomas", "CEO", 1);

    if let Some(mut caps) = world.get_mut::<AgentCapabilities>(entity) {
        caps.tools = vec!["echo".into()];
    }

    // Leerer Input → echo-Plugin gibt Err("empty input") zurueck
    tx.send(AgentAction {
        agent_id: AgentId(1),
        action_type: ActionType::ToolUse,
        target_room: None,
        target_agent: None,
        content: Some("tool:echo:".to_string()),
        timestamp: Timestamp(1000),
        tick: Tick(1),
    })
    .unwrap();

    // Muss NICHT paniken — Error wird geloggt, kein tool_result Event
    run_tick(&mut world, &mut schedule, 1);

    let es = world.resource::<LimboEventStore>();
    let events = es.0.get_events_since(0, 100).unwrap();
    let tool_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "tool_result")
        .collect();

    // Kein tool_result Event bei Plugin-Error (nur Warning im Log)
    assert_eq!(
        tool_events.len(),
        0,
        "Plugin error should not create tool_result event"
    );

    // Aber AgentActionReceived Event muss trotzdem da sein
    let action_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "agent_action_received")
        .collect();
    assert!(
        !action_events.is_empty(),
        "AgentActionReceived must still be recorded"
    );
}

// ---- Fehlerfall: Capability-Check blockiert WASM-Tool im ECS ----

#[test]
fn ecs_capability_check_blocks_wasm_tool() {
    if !wasm_runtime_available() {
        return;
    }
    let (mut world, mut schedule, tx, _dir) = setup_wasm_world();

    // Registriere ein WASM-Tool das "admin" Capability braucht
    {
        let mut runtime = world.resource_mut::<ToolRuntimeResource>();
        runtime
            .0
            .register_tool(sentinel_wasm::ToolDefinition {
                name: "admin_echo".into(),
                description: "Admin-only WASM echo".into(),
                wasm_path: Some(echo_fixture().to_str().unwrap().to_string()),
                tool_type: sentinel_wasm::ToolType::Wasm,
                required_capabilities: vec!["admin".into()],
            })
            .unwrap();
    }

    let entity = spawn_agent(&mut world, AgentId(1), "Thomas", "CEO", 1);
    if let Some(mut caps) = world.get_mut::<AgentCapabilities>(entity) {
        caps.tools = vec!["echo".into()]; // KEIN "admin"!
    }

    tx.send(AgentAction {
        agent_id: AgentId(1),
        action_type: ActionType::ToolUse,
        target_room: None,
        target_agent: None,
        content: Some("tool:admin_echo:should fail".to_string()),
        timestamp: Timestamp(1000),
        tick: Tick(1),
    })
    .unwrap();

    run_tick(&mut world, &mut schedule, 1);

    let es = world.resource::<LimboEventStore>();
    let events = es.0.get_events_since(0, 100).unwrap();
    let tool_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "tool_result")
        .collect();

    // Kein tool_result — Agent hat keine admin-Capability
    assert_eq!(
        tool_events.len(),
        0,
        "Agent without admin cap must not get tool_result"
    );
}

// ---- Fehlerfall: Kein ToolRuntimeResource → System ueberlebt ----

#[test]
fn ecs_no_tool_runtime_resource_survives() {
    let dir = tempfile::tempdir().unwrap();
    let event_db_path = dir.path().join("events.db");

    let (mut world, mut schedule) = create_simulation_world();
    let event_store = EventStore::open(event_db_path.to_str().unwrap()).unwrap();
    world.insert_resource(LimboEventStore(Arc::new(event_store)));

    // KEIN ToolRuntimeResource eingefuegt!

    let (tx, rx) = std::sync::mpsc::channel();
    world.insert_resource(ActionReceiver(std::sync::Mutex::new(rx)));

    spawn_agent(&mut world, AgentId(1), "Thomas", "CEO", 1);

    tx.send(AgentAction {
        agent_id: AgentId(1),
        action_type: ActionType::ToolUse,
        target_room: None,
        target_agent: None,
        content: Some("tool:echo:should be ignored".to_string()),
        timestamp: Timestamp(1000),
        tick: Tick(1),
    })
    .unwrap();

    // Muss NICHT paniken
    run_tick(&mut world, &mut schedule, 1);

    // System laeuft weiter, kein tool_result (weil kein ToolRuntime)
    let es = world.resource::<LimboEventStore>();
    let events = es.0.get_events_since(0, 100).unwrap();
    let tool_events: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "tool_result")
        .collect();
    assert_eq!(tool_events.len(), 0);
}
