//! Acceptance Tests fuer Issue #19: sentinel-wasm
//!
//! Testet ToolRuntime mit Sandbox-Isolation, Capability-Checks,
//! DomainEvent-Output, und WASM-Ausfuehrung.

use sentinel_wasm::{ExecutionContext, SandboxConfig, ToolDefinition, ToolRuntime, ToolType};
use std::io::Write;

fn make_tool(name: &str, tool_type: ToolType) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: format!("Test tool {name}"),
        wasm_path: None,
        tool_type,
        required_capabilities: Vec::new(),
    }
}

fn ctx_with_sandbox(sandbox: SandboxConfig) -> ExecutionContext {
    ExecutionContext {
        agent_id: "AGENT-01".to_string(),
        agent_capabilities: vec!["file_read".to_string(), "file_write".to_string()],
        sandbox,
        correlation_id: "test-corr".to_string(),
        tick: 1,
        #[cfg(feature = "wasm")]
        agent_snapshot: None,
        #[cfg(feature = "wasm")]
        rooms: None,
    }
}

// AC-7: Bestehende native FileRead/FileWrite Handler funktionieren weiterhin
#[test]
fn ac_19_07_file_read_works() {
    let mut runtime = ToolRuntime::new();
    runtime
        .register_tool(make_tool("file_read", ToolType::FileRead))
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let mut temp_file = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
    let expected = "Hello from acceptance test!";
    temp_file.write_all(expected.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let sandbox = SandboxConfig::with_paths(vec![dir.path().to_path_buf()]);
    let ctx = ctx_with_sandbox(sandbox);
    let path = temp_file.path().to_str().unwrap();
    let result = runtime.execute("file_read", path, &ctx).unwrap();

    assert_eq!(
        result.output, expected,
        "file_read should return exact file content"
    );
    assert!(result.success);
    assert_eq!(result.agent_id, "AGENT-01");
}

// AC-7: FileWrite
#[test]
fn ac_19_07_file_write_works() {
    let mut runtime = ToolRuntime::new();
    runtime
        .register_tool(make_tool("file_write", ToolType::FileWrite))
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test_output.txt");
    let content = "Written by acceptance test";
    let input = format!("{}\n{}", file_path.display(), content);

    let sandbox = SandboxConfig::with_paths(vec![dir.path().to_path_buf()]);
    let ctx = ctx_with_sandbox(sandbox);
    let result = runtime.execute("file_write", &input, &ctx).unwrap();

    let read_back = std::fs::read_to_string(&file_path).unwrap();
    assert_eq!(read_back, content);
    assert!(result.output.contains("Written"));
}

// AC-2: Sandbox-Violation — Pfad ausserhalb allowed_paths wird blockiert
#[test]
fn ac_19_02_sandbox_blocks_outside_paths() {
    let mut runtime = ToolRuntime::new();
    runtime
        .register_tool(make_tool("file_read", ToolType::FileRead))
        .unwrap();

    let allowed_dir = tempfile::tempdir().unwrap();
    let other_dir = tempfile::tempdir().unwrap();
    let secret_file = other_dir.path().join("secret.txt");
    std::fs::write(&secret_file, "top secret").unwrap();

    let sandbox = SandboxConfig::with_paths(vec![allowed_dir.path().to_path_buf()]);
    let ctx = ctx_with_sandbox(sandbox);

    let result = runtime.execute("file_read", secret_file.to_str().unwrap(), &ctx);
    assert!(result.is_err(), "Reading outside sandbox must fail");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Sandbox violation"),
        "Error must mention sandbox violation"
    );
}

// AC-3: Tool-Ergebnisse als DomainEvent mit event_type = "tool_result"
#[test]
fn ac_19_03_domain_event_output() {
    let mut runtime = ToolRuntime::new();
    runtime
        .register_tool(make_tool("file_read", ToolType::FileRead))
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("event_test.txt");
    std::fs::write(&file, "event data").unwrap();

    let sandbox = SandboxConfig::with_paths(vec![dir.path().to_path_buf()]);
    let ctx = ctx_with_sandbox(sandbox);
    let result = runtime
        .execute("file_read", file.to_str().unwrap(), &ctx)
        .unwrap();

    let event = result.to_domain_event("corr-42", 100);
    assert_eq!(event.event_type, "tool_result");
    assert_eq!(event.aggregate_id, "AGENT-01");
    assert_eq!(event.correlation_id, "corr-42");
    assert_eq!(event.tick, 100);
    assert!(event.payload.contains("file_read"));
    assert!(event.payload.contains("event data"));
}

// AC-5: Capability-Check verhindert unauthorisierten Tool-Zugriff
#[test]
fn ac_19_05_capability_check_denies() {
    let mut runtime = ToolRuntime::new();
    let tool = ToolDefinition {
        name: "admin_tool".to_string(),
        description: "Needs admin".to_string(),
        wasm_path: None,
        tool_type: ToolType::FileRead,
        required_capabilities: vec!["admin".to_string()],
    };
    runtime.register_tool(tool).unwrap();

    let sandbox = SandboxConfig::restrictive();
    // Agent hat file_read + file_write, aber NICHT admin
    let ctx = ctx_with_sandbox(sandbox);
    let result = runtime.execute("admin_tool", "/dev/null", &ctx);

    assert!(result.is_err(), "Agent without admin cap must be denied");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("lacks required capabilities"),
        "Error must mention missing capabilities"
    );
}

// AC-5: Agent mit passenden Capabilities darf Tool nutzen
#[test]
fn ac_19_05_capability_check_allows() {
    let mut runtime = ToolRuntime::new();
    let tool = ToolDefinition {
        name: "reader".to_string(),
        description: "Needs file_read".to_string(),
        wasm_path: None,
        tool_type: ToolType::FileRead,
        required_capabilities: vec!["file_read".to_string()],
    };
    runtime.register_tool(tool).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("cap_test.txt");
    std::fs::write(&file, "allowed").unwrap();

    let sandbox = SandboxConfig::with_paths(vec![dir.path().to_path_buf()]);
    let ctx = ctx_with_sandbox(sandbox);
    let result = runtime.execute("reader", file.to_str().unwrap(), &ctx);
    assert!(result.is_ok(), "Agent with correct capability must succeed");
}

// AC-6: ToolType::Wasm nutzt wasm_path — ohne wasm-Feature gibt's einen Fehler
#[test]
fn ac_19_06_wasm_tool_type_exists() {
    let mut runtime = ToolRuntime::new();
    let tool = ToolDefinition {
        name: "wasm_tool".to_string(),
        description: "A WASM tool".to_string(),
        wasm_path: Some("/nonexistent/module.wasm".to_string()),
        tool_type: ToolType::Wasm,
        required_capabilities: Vec::new(),
    };
    runtime.register_tool(tool).unwrap();

    let found = runtime.get_tool("wasm_tool").unwrap();
    assert_eq!(found.tool_type, ToolType::Wasm);
    assert_eq!(found.wasm_path.as_deref(), Some("/nonexistent/module.wasm"));

    // Ausfuehrung scheitert (kein wasm-Feature oder Datei nicht gefunden)
    let sandbox = SandboxConfig::restrictive();
    let ctx = ctx_with_sandbox(sandbox);
    let result = runtime.execute("wasm_tool", "", &ctx);
    assert!(result.is_err());
}

// AC-N1: Duplikat-Registrierung schlaegt fehl
#[test]
fn ac_19_n1_duplicate_registration_fails() {
    let mut runtime = ToolRuntime::new();
    runtime
        .register_tool(make_tool("dupe", ToolType::FileRead))
        .unwrap();
    let result = runtime.register_tool(make_tool("dupe", ToolType::FileRead));
    assert!(result.is_err(), "Duplicate registration must fail");
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("already registered"));
}

// AC-7: Register + Lookup (3 Tools, list_tools().len()==3)
#[test]
fn ac_19_07_register_lookup() {
    let mut runtime = ToolRuntime::new();
    runtime
        .register_tool(make_tool("file_read", ToolType::FileRead))
        .unwrap();
    runtime
        .register_tool(make_tool("file_write", ToolType::FileWrite))
        .unwrap();
    runtime
        .register_tool(make_tool("chat", ToolType::Chat))
        .unwrap();

    assert_eq!(runtime.list_tools().len(), 3);
    assert!(runtime.get_tool("file_read").is_some());
    assert!(runtime.get_tool("chat").is_some());
    assert!(runtime.get_tool("nonexistent").is_none());
}

// AC-2: Restrictive Sandbox blockiert ALLE Pfade
#[test]
fn ac_19_02_restrictive_sandbox_blocks_all() {
    let mut runtime = ToolRuntime::new();
    runtime
        .register_tool(make_tool("file_read", ToolType::FileRead))
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "data").unwrap();

    // Restrictive = keine allowed_paths
    let sandbox = SandboxConfig::restrictive();
    let ctx = ctx_with_sandbox(sandbox);
    let result = runtime.execute("file_read", file.to_str().unwrap(), &ctx);
    assert!(result.is_err());
}

// Wasm Component Model Tests — nur mit wasm-Feature kompilierbar
#[cfg(feature = "wasm")]
mod wasm_tests {
    use super::*;
    use sentinel_wasm::{PluginConfig, PluginHost};
    use std::path::PathBuf;

    // AC-1: PluginHost erstellt Engine + Linker (Component Model Pipeline)
    #[test]
    fn ac_19_01_plugin_host_creates() {
        let host = PluginHost::new();
        assert!(host.is_ok(), "PluginHost::new() must succeed");
        let host = host.unwrap();
        assert_eq!(host.cached_count(), 0);
    }

    // AC-1: ToolRuntime enthält PluginHost (Component Model integriert)
    #[test]
    fn ac_19_01_runtime_has_plugin_host() {
        let runtime = ToolRuntime::new();
        assert_eq!(runtime.plugin_host().cached_count(), 0);
    }

    // AC-1: Ungeladenes Plugin gibt klaren Fehler via ToolRuntime.execute()
    #[test]
    fn ac_19_01_unloaded_plugin_error() {
        let mut runtime = ToolRuntime::new();
        let tool = ToolDefinition {
            name: "wasm_unloaded".to_string(),
            description: "Unloaded wasm tool".to_string(),
            wasm_path: Some("/some/plugin.wasm".to_string()),
            tool_type: ToolType::Wasm,
            required_capabilities: Vec::new(),
        };
        runtime.register_tool(tool).unwrap();

        let sandbox = SandboxConfig::with_paths(vec![PathBuf::from("/tmp")]);
        let ctx = ctx_with_sandbox(sandbox);
        let result = runtime.execute("wasm_unloaded", "test input", &ctx);
        assert!(result.is_err(), "Unloaded plugin must fail");
        assert!(
            result.unwrap_err().to_string().contains("not loaded"),
            "Error must mention 'not loaded'"
        );
    }

    // AC-2: PluginConfig-Defaults sind korrekt (64MB Memory, 10M Fuel)
    #[test]
    fn ac_19_02_plugin_config_defaults() {
        let config = PluginConfig::default();
        assert_eq!(config.memory_limit_bytes, 64 * 1024 * 1024, "Default 64MB");
        assert_eq!(config.fuel_limit, 10_000_000, "Default 10M instructions");
        assert!(config.allowed_paths.is_empty());
    }

    // AC-4: Laden einer nicht-existenten Component-Datei schlaegt fehl
    #[test]
    fn ac_19_04_load_nonexistent_component() {
        let mut runtime = ToolRuntime::new();
        let config = PluginConfig {
            wasm_path: PathBuf::from("/nonexistent/plugin.wasm"),
            ..Default::default()
        };
        let result = runtime.plugin_host_mut().load(config);
        assert!(result.is_err(), "Loading nonexistent .wasm must fail");
    }

    // AC-6: Fehlender wasm_path gibt klaren Fehler
    #[test]
    fn ac_19_06_wasm_missing_path() {
        let mut runtime = ToolRuntime::new();
        let tool = ToolDefinition {
            name: "no_path".to_string(),
            description: "Wasm without path".to_string(),
            wasm_path: None, // Fehlt!
            tool_type: ToolType::Wasm,
            required_capabilities: Vec::new(),
        };
        runtime.register_tool(tool).unwrap();

        let sandbox = SandboxConfig::restrictive();
        let ctx = ctx_with_sandbox(sandbox);
        let result = runtime.execute("no_path", "", &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no wasm_path"));
    }

    // AC-6: Ungueltiger wasm_path gibt klaren Fehler
    #[test]
    fn ac_19_06_wasm_invalid_path() {
        let mut runtime = ToolRuntime::new();
        let tool = ToolDefinition {
            name: "bad_path".to_string(),
            description: "Wasm with bad path".to_string(),
            wasm_path: Some("/nonexistent/module.wasm".to_string()),
            tool_type: ToolType::Wasm,
            required_capabilities: Vec::new(),
        };
        runtime.register_tool(tool).unwrap();

        let sandbox = SandboxConfig::restrictive();
        let ctx = ctx_with_sandbox(sandbox);
        let result = runtime.execute("bad_path", "", &ctx);
        assert!(result.is_err(), "Nonexistent wasm file must fail");
        assert!(
            result.unwrap_err().to_string().contains("not loaded"),
            "Error must indicate plugin is not loaded"
        );
    }

    // AC-7: Native Tools und Wasm-Tools koexistieren in derselben Registry
    #[test]
    fn ac_19_07_native_and_wasm_coexist() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("file_read", ToolType::FileRead))
            .unwrap();
        runtime
            .register_tool(ToolDefinition {
                name: "wasm_tool".to_string(),
                description: "A WASM Component tool".to_string(),
                wasm_path: Some("/some/plugin.wasm".to_string()),
                tool_type: ToolType::Wasm,
                required_capabilities: Vec::new(),
            })
            .unwrap();

        assert_eq!(runtime.list_tools().len(), 2);

        // Native FileRead funktioniert weiterhin neben registriertem Wasm-Tool
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("coexist.txt");
        std::fs::write(&file, "native works").unwrap();

        let sandbox = SandboxConfig::with_paths(vec![dir.path().to_path_buf()]);
        let ctx = ctx_with_sandbox(sandbox);
        let result = runtime
            .execute("file_read", file.to_str().unwrap(), &ctx)
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "native works");
    }

    // AC-5: Capability-Check greift auch fuer Wasm-Tools
    #[test]
    fn ac_19_05_wasm_capability_check() {
        let mut runtime = ToolRuntime::new();
        let tool = ToolDefinition {
            name: "wasm_admin".to_string(),
            description: "Wasm tool needing admin".to_string(),
            wasm_path: Some("/some/admin-plugin.wasm".to_string()),
            tool_type: ToolType::Wasm,
            required_capabilities: vec!["admin".to_string()],
        };
        runtime.register_tool(tool).unwrap();

        let sandbox = SandboxConfig::restrictive();
        // Agent hat file_read + file_write, aber NICHT admin
        let ctx = ctx_with_sandbox(sandbox);
        let result = runtime.execute("wasm_admin", "", &ctx);
        assert!(result.is_err(), "Agent without admin cap must be denied");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("lacks required capabilities"),
            "Error must mention missing capabilities"
        );
    }
}

// E2E Tests mit echten .wasm Component Fixtures
#[cfg(feature = "wasm")]
mod wasm_e2e {
    use super::*;
    use sentinel_wasm::{AgentSnapshot, PluginConfig, RoomSnapshot};
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// Pfad zum echo-plugin.wasm Test-Fixture (relativ zum Workspace-Root).
    fn echo_fixture() -> PathBuf {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/echo-plugin.wasm");
        path
    }

    /// Pfad zum loop-plugin.wasm Test-Fixture.
    fn loop_fixture() -> PathBuf {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/loop-plugin.wasm");
        path
    }

    fn make_agent_snapshot() -> AgentSnapshot {
        AgentSnapshot {
            agent_id: "AGENT-01".to_string(),
            name: "Thomas Mueller".to_string(),
            role: "CEO".to_string(),
            hunger: 0.3,
            energy: 0.7,
            stress: 0.2,
            social_need: 0.5,
            caffeine: 0.4,
            bladder: 0.1,
            room_id: "buero-ceo".to_string(),
        }
    }

    fn make_rooms() -> HashMap<String, RoomSnapshot> {
        let mut rooms = HashMap::new();
        rooms.insert(
            "buero-ceo".to_string(),
            RoomSnapshot {
                room_id: "buero-ceo".to_string(),
                name: "CEO Buero".to_string(),
                floor: 1,
                temperature: 22.0,
                noise_db: 35.0,
                occupant_count: 1,
            },
        );
        rooms
    }

    // ---- AC-1: Component laden + call_execute() → ToolResult ----

    #[test]
    fn e2e_load_echo_plugin() {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();
        let config = PluginConfig {
            wasm_path: echo_fixture(),
            ..Default::default()
        };
        let result = host.load(config);
        assert!(result.is_ok(), "Echo plugin must load: {:?}", result.err());
        assert_eq!(host.cached_count(), 1);
        assert!(host.is_loaded(&echo_fixture()));
    }

    #[test]
    fn e2e_execute_echo_plugin() {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();
        let config = PluginConfig {
            wasm_path: echo_fixture(),
            ..Default::default()
        };
        host.load(config).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let result = host
            .execute(
                &echo_fixture(),
                "hello world",
                make_agent_snapshot(),
                make_rooms(),
                42,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_ok(), "Execute must return Ok: {:?}", result.err());
        assert_eq!(result.unwrap(), "echo: hello world");
    }

    #[test]
    fn e2e_echo_plugin_empty_input_returns_error() {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();
        host.load(PluginConfig {
            wasm_path: echo_fixture(),
            ..Default::default()
        })
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let result = host
            .execute(
                &echo_fixture(),
                "",
                make_agent_snapshot(),
                make_rooms(),
                1,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "empty input");
    }

    // ---- AC-10: query_meta() gibt tool-name und tool-description ----

    #[test]
    fn e2e_query_meta() {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();
        host.load(PluginConfig {
            wasm_path: echo_fixture(),
            ..Default::default()
        })
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let meta = host
            .query_meta(&echo_fixture(), dir.path().to_path_buf())
            .unwrap();

        assert_eq!(meta.tool_name, "echo");
        assert!(meta.tool_description.contains("Echoes input"));
    }

    // ---- AC-13: Mehrfach-Ausfuehrung (Store pro Call) ----

    #[test]
    fn e2e_multiple_executions() {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();
        host.load(PluginConfig {
            wasm_path: echo_fixture(),
            ..Default::default()
        })
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        for i in 0..20 {
            let input = format!("call-{i}");
            let result = host
                .execute(
                    &echo_fixture(),
                    &input,
                    make_agent_snapshot(),
                    make_rooms(),
                    i,
                    dir.path().to_path_buf(),
                )
                .unwrap();
            assert_eq!(result.unwrap(), format!("echo: call-{i}"));
        }
    }

    // ---- AC-4: Fuel-Exhaustion mit loop-plugin ----

    #[test]
    fn e2e_fuel_exhaustion() {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();
        host.load(PluginConfig {
            wasm_path: loop_fixture(),
            fuel_limit: 100_000, // 100K fuel — Endlosschleife soll daran scheitern
            ..Default::default()
        })
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let result = host.execute(
            &loop_fixture(),
            "trigger",
            make_agent_snapshot(),
            HashMap::new(),
            1,
            dir.path().to_path_buf(),
        );

        // Fuel-Exhaustion kommt als wasmtime Trap (Err im aeusseren Result)
        assert!(
            result.is_err(),
            "Infinite loop must be stopped by fuel exhaustion, got: {result:?}"
        );
        // Prüfe Error-Chain: wasmtime meldet "all fuel consumed by WebAssembly"
        let err = result.unwrap_err();
        let full_chain = format!("{err:#}");
        assert!(
            full_chain.contains("fuel") || full_chain.contains("Fuel"),
            "Error chain must mention fuel: {full_chain}"
        );
    }

    // ---- AC-7: ToolRuntime E2E mit echtem Plugin ----

    #[test]
    fn e2e_tool_runtime_full_path() {
        let mut runtime = ToolRuntime::new();

        // Plugin laden
        runtime
            .plugin_host_mut()
            .load(PluginConfig {
                wasm_path: echo_fixture(),
                ..Default::default()
            })
            .unwrap();

        // Tool registrieren
        runtime
            .register_tool(ToolDefinition {
                name: "echo".to_string(),
                description: "Echo tool".to_string(),
                wasm_path: Some(echo_fixture().to_str().unwrap().to_string()),
                tool_type: ToolType::Wasm,
                required_capabilities: Vec::new(),
            })
            .unwrap();

        // Auch natives Tool registrieren
        runtime
            .register_tool(make_tool("file_read", ToolType::FileRead))
            .unwrap();
        assert_eq!(runtime.list_tools().len(), 2);

        // WASM-Tool ausfuehren
        let dir = tempfile::tempdir().unwrap();
        let sandbox = SandboxConfig::with_paths(vec![dir.path().to_path_buf()]);
        let ctx = ExecutionContext {
            agent_id: "AGENT-01".to_string(),
            agent_capabilities: vec!["file_read".to_string()],
            sandbox,
            correlation_id: "e2e-test".to_string(),
            tick: 100,
            agent_snapshot: Some(make_agent_snapshot()),
            rooms: Some(make_rooms()),
        };

        let result = runtime.execute("echo", "test input", &ctx).unwrap();
        assert!(result.success);
        assert_eq!(result.output, "echo: test input");
        assert_eq!(result.agent_id, "AGENT-01");
        assert_eq!(result.tool_name, "echo");

        // DomainEvent pruefen
        let event = result.to_domain_event("corr-e2e", 100);
        assert_eq!(event.event_type, "tool_result");
        assert_eq!(event.aggregate_id, "AGENT-01");
        assert!(event.payload.contains("echo: test input"));
    }

    // ---- AC-7: Native + WASM Tools parallel ----

    #[test]
    fn e2e_native_and_wasm_side_by_side() {
        let mut runtime = ToolRuntime::new();

        runtime
            .plugin_host_mut()
            .load(PluginConfig {
                wasm_path: echo_fixture(),
                ..Default::default()
            })
            .unwrap();

        runtime
            .register_tool(make_tool("file_read", ToolType::FileRead))
            .unwrap();
        runtime
            .register_tool(make_tool("file_write", ToolType::FileWrite))
            .unwrap();
        runtime
            .register_tool(ToolDefinition {
                name: "echo".to_string(),
                description: "Echo WASM".to_string(),
                wasm_path: Some(echo_fixture().to_str().unwrap().to_string()),
                tool_type: ToolType::Wasm,
                required_capabilities: Vec::new(),
            })
            .unwrap();

        assert_eq!(runtime.list_tools().len(), 3);

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "native content").unwrap();

        let sandbox = SandboxConfig::with_paths(vec![dir.path().to_path_buf()]);
        let ctx = ExecutionContext {
            agent_id: "AGENT-05".to_string(),
            agent_capabilities: vec!["file_read".to_string(), "file_write".to_string()],
            sandbox,
            correlation_id: "mixed".to_string(),
            tick: 50,
            agent_snapshot: Some(make_agent_snapshot()),
            rooms: Some(make_rooms()),
        };

        // Native FileRead
        let native_result = runtime
            .execute("file_read", file.to_str().unwrap(), &ctx)
            .unwrap();
        assert_eq!(native_result.output, "native content");

        // WASM Echo
        let wasm_result = runtime.execute("echo", "wasm input", &ctx).unwrap();
        assert_eq!(wasm_result.output, "echo: wasm input");
    }

    // ---- AC-2: Multiple Plugins laden ----

    #[test]
    fn e2e_load_multiple_plugins() {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();

        host.load(PluginConfig {
            wasm_path: echo_fixture(),
            ..Default::default()
        })
        .unwrap();

        host.load(PluginConfig {
            wasm_path: loop_fixture(),
            fuel_limit: 100_000,
            ..Default::default()
        })
        .unwrap();

        assert_eq!(host.cached_count(), 2);
        assert!(host.is_loaded(&echo_fixture()));
        assert!(host.is_loaded(&loop_fixture()));
    }

    // ---- AC-11: Host-Function Roundtrip (Agent-Info, Tick, Logging) ----

    #[test]
    fn e2e_host_function_roundtrip_agent_info() {
        // Echo-Plugin ruft get_agent_info() + get_tick() + log() auf.
        // Wenn die Host-Functions nicht korrekt implementiert sind, panicked das Plugin.
        let mut host = sentinel_wasm::PluginHost::new().unwrap();
        host.load(PluginConfig {
            wasm_path: echo_fixture(),
            ..Default::default()
        })
        .unwrap();

        let agent = AgentSnapshot {
            agent_id: "AGENT-42".to_string(),
            name: "Kira Nakamura".to_string(),
            role: "Senior Developer".to_string(),
            hunger: 0.85,
            energy: 0.15,
            stress: 0.9,
            social_need: 0.6,
            caffeine: 0.2,
            bladder: 0.7,
            room_id: "buero-dev-1".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        let result = host
            .execute(
                &echo_fixture(),
                "host roundtrip",
                agent,
                make_rooms(),
                9999,
                dir.path().to_path_buf(),
            )
            .unwrap();

        // Plugin hat get_agent_info() aufgerufen und den Namen im Log benutzt.
        // Wenn der Roundtrip funktioniert, kommt das Echo zurueck.
        assert_eq!(result.unwrap(), "echo: host roundtrip");
    }

    // ---- AC-4: Fuel-Konfigurierbarkeit (verschiedene Limits) ----

    #[test]
    fn e2e_fuel_configurable_per_plugin() {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();

        // Echo-Plugin mit 10M Fuel (Default) — muss funktionieren
        host.load(PluginConfig {
            wasm_path: echo_fixture(),
            fuel_limit: 10_000_000,
            ..Default::default()
        })
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let result = host
            .execute(
                &echo_fixture(),
                "fuel test",
                make_agent_snapshot(),
                make_rooms(),
                1,
                dir.path().to_path_buf(),
            )
            .unwrap();
        assert_eq!(result.unwrap(), "echo: fuel test");
    }

    // ---- AC-2: Memory-Limit (StoreLimits) ----

    #[test]
    fn e2e_memory_limit_too_small_fails() {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();

        // 64KB Memory-Limit — viel zu klein fuer Component-Instantiation
        host.load(PluginConfig {
            wasm_path: echo_fixture(),
            memory_limit_bytes: 64 * 1024,
            ..Default::default()
        })
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let result = host.execute(
            &echo_fixture(),
            "should fail",
            make_agent_snapshot(),
            make_rooms(),
            1,
            dir.path().to_path_buf(),
        );
        assert!(
            result.is_err(),
            "64KB memory limit must prevent plugin execution"
        );
    }

    #[test]
    fn e2e_memory_limit_sufficient_works() {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();

        // 4MB Memory-Limit — ausreichend fuer Echo-Plugin
        host.load(PluginConfig {
            wasm_path: echo_fixture(),
            memory_limit_bytes: 4 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let result = host
            .execute(
                &echo_fixture(),
                "memory ok",
                make_agent_snapshot(),
                make_rooms(),
                1,
                dir.path().to_path_buf(),
            )
            .unwrap();
        assert_eq!(result.unwrap(), "echo: memory ok");
    }

    // ---- AC-7: ToolRuntime mit mehreren verschiedenen Agents ----

    #[test]
    fn e2e_multiple_agents_use_same_tool() {
        let mut runtime = ToolRuntime::new();
        runtime
            .plugin_host_mut()
            .load(PluginConfig {
                wasm_path: echo_fixture(),
                ..Default::default()
            })
            .unwrap();
        runtime
            .register_tool(ToolDefinition {
                name: "echo".to_string(),
                description: "Echo tool".to_string(),
                wasm_path: Some(echo_fixture().to_str().unwrap().to_string()),
                tool_type: ToolType::Wasm,
                required_capabilities: Vec::new(),
            })
            .unwrap();

        let dir = tempfile::tempdir().unwrap();

        // Agent-01 nutzt das Tool
        let ctx1 = ExecutionContext {
            agent_id: "AGENT-01".to_string(),
            agent_capabilities: vec![],
            sandbox: SandboxConfig::with_paths(vec![dir.path().to_path_buf()]),
            correlation_id: "agent01-call".to_string(),
            tick: 100,
            agent_snapshot: Some(AgentSnapshot {
                agent_id: "AGENT-01".to_string(),
                name: "Thomas Mueller".to_string(),
                ..Default::default()
            }),
            rooms: Some(make_rooms()),
        };
        let r1 = runtime.execute("echo", "von Agent-01", &ctx1).unwrap();
        assert_eq!(r1.agent_id, "AGENT-01");
        assert_eq!(r1.output, "echo: von Agent-01");

        // Agent-42 nutzt dasselbe Tool (anderer Store, frischer State)
        let ctx2 = ExecutionContext {
            agent_id: "AGENT-42".to_string(),
            agent_capabilities: vec![],
            sandbox: SandboxConfig::with_paths(vec![dir.path().to_path_buf()]),
            correlation_id: "agent42-call".to_string(),
            tick: 101,
            agent_snapshot: Some(AgentSnapshot {
                agent_id: "AGENT-42".to_string(),
                name: "Kira Nakamura".to_string(),
                ..Default::default()
            }),
            rooms: Some(make_rooms()),
        };
        let r2 = runtime.execute("echo", "von Agent-42", &ctx2).unwrap();
        assert_eq!(r2.agent_id, "AGENT-42");
        assert_eq!(r2.output, "echo: von Agent-42");

        // Events haben unterschiedliche Agent-IDs
        let event1 = r1.to_domain_event("corr-1", 100);
        let event2 = r2.to_domain_event("corr-2", 101);
        assert_eq!(event1.aggregate_id, "AGENT-01");
        assert_eq!(event2.aggregate_id, "AGENT-42");
    }

    // ---- AC-3: ToolResult korrekte Felder ----

    #[test]
    fn e2e_tool_result_fields_complete() {
        let mut runtime = ToolRuntime::new();
        runtime
            .plugin_host_mut()
            .load(PluginConfig {
                wasm_path: echo_fixture(),
                ..Default::default()
            })
            .unwrap();
        runtime
            .register_tool(ToolDefinition {
                name: "echo".to_string(),
                description: "Echo tool".to_string(),
                wasm_path: Some(echo_fixture().to_str().unwrap().to_string()),
                tool_type: ToolType::Wasm,
                required_capabilities: Vec::new(),
            })
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let ctx = ExecutionContext {
            agent_id: "AGENT-07".to_string(),
            agent_capabilities: vec![],
            sandbox: SandboxConfig::with_paths(vec![dir.path().to_path_buf()]),
            correlation_id: "corr-result".to_string(),
            tick: 500,
            agent_snapshot: Some(make_agent_snapshot()),
            rooms: Some(make_rooms()),
        };

        let result = runtime.execute("echo", "field check", &ctx).unwrap();

        // Alle ToolResult-Felder pruefen
        assert_eq!(result.tool_name, "echo");
        assert_eq!(result.agent_id, "AGENT-07");
        assert_eq!(result.output, "echo: field check");
        assert!(result.success);
        assert!(result.duration_ms < 10_000, "Should finish in under 10s");

        // Serialisierung korrekt
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"tool_name\":\"echo\""));
        assert!(json.contains("\"agent_id\":\"AGENT-07\""));
        assert!(json.contains("\"success\":true"));
    }

    // ---- AC-4: Plugin-Error wird korrekt propagiert ----

    #[test]
    fn e2e_plugin_error_propagates_through_runtime() {
        let mut runtime = ToolRuntime::new();
        runtime
            .plugin_host_mut()
            .load(PluginConfig {
                wasm_path: echo_fixture(),
                ..Default::default()
            })
            .unwrap();
        runtime
            .register_tool(ToolDefinition {
                name: "echo".to_string(),
                description: "Echo tool".to_string(),
                wasm_path: Some(echo_fixture().to_str().unwrap().to_string()),
                tool_type: ToolType::Wasm,
                required_capabilities: Vec::new(),
            })
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let ctx = ExecutionContext {
            agent_id: "AGENT-01".to_string(),
            agent_capabilities: vec![],
            sandbox: SandboxConfig::with_paths(vec![dir.path().to_path_buf()]),
            correlation_id: "err-test".to_string(),
            tick: 1,
            agent_snapshot: Some(make_agent_snapshot()),
            rooms: Some(make_rooms()),
        };

        // Leerer Input → Plugin gibt Err("empty input") zurueck
        // ToolRuntime konvertiert das zu anyhow::Error
        let result = runtime.execute("echo", "", &ctx);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("empty input"),
            "Plugin error must propagate: {err_msg}"
        );
    }

    // ---- AC-5: Capability-Check blockiert auch WASM-Tools ----

    #[test]
    fn e2e_capability_check_blocks_wasm_tool() {
        let mut runtime = ToolRuntime::new();
        runtime
            .plugin_host_mut()
            .load(PluginConfig {
                wasm_path: echo_fixture(),
                ..Default::default()
            })
            .unwrap();
        runtime
            .register_tool(ToolDefinition {
                name: "echo".to_string(),
                description: "Echo tool".to_string(),
                wasm_path: Some(echo_fixture().to_str().unwrap().to_string()),
                tool_type: ToolType::Wasm,
                required_capabilities: vec!["admin".to_string()],
            })
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let ctx = ExecutionContext {
            agent_id: "AGENT-01".to_string(),
            agent_capabilities: vec!["file_read".to_string()], // Kein "admin"
            sandbox: SandboxConfig::with_paths(vec![dir.path().to_path_buf()]),
            correlation_id: "cap-test".to_string(),
            tick: 1,
            agent_snapshot: Some(make_agent_snapshot()),
            rooms: Some(make_rooms()),
        };

        let result = runtime.execute("echo", "should fail", &ctx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("lacks required capabilities"));
    }
}

// Security Tests — Sandbox-Escape, Resource-Exhaustion, Isolation
#[cfg(feature = "wasm")]
mod wasm_security {
    use sentinel_wasm::{AgentSnapshot, PluginConfig};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn echo_fixture() -> PathBuf {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/echo-plugin.wasm");
        path
    }

    fn loop_fixture() -> PathBuf {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/loop-plugin.wasm");
        path
    }

    fn make_agent() -> AgentSnapshot {
        AgentSnapshot {
            agent_id: "AGENT-01".to_string(),
            name: "Test Agent".to_string(),
            ..Default::default()
        }
    }

    // ---- Isolation: Separate Agent-Homes pro Aufruf ----

    #[test]
    fn security_agent_homes_isolated() {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();
        host.load(PluginConfig {
            wasm_path: echo_fixture(),
            ..Default::default()
        })
        .unwrap();

        // Zwei verschiedene Agent-Homes
        let home1 = tempfile::tempdir().unwrap();
        let home2 = tempfile::tempdir().unwrap();

        // Schreibe in Home-1 per Host
        std::fs::write(home1.path().join("secret.txt"), "agent-01-secret").unwrap();

        // Agent-01 nutzt Home-1
        let r1 = host
            .execute(
                &echo_fixture(),
                "from home1",
                make_agent(),
                HashMap::new(),
                1,
                home1.path().to_path_buf(),
            )
            .unwrap();
        assert_eq!(r1.unwrap(), "echo: from home1");

        // Agent-02 nutzt Home-2 — kann Home-1 Dateien nicht sehen
        let r2 = host
            .execute(
                &echo_fixture(),
                "from home2",
                AgentSnapshot {
                    agent_id: "AGENT-02".to_string(),
                    name: "Other Agent".to_string(),
                    ..Default::default()
                },
                HashMap::new(),
                1,
                home2.path().to_path_buf(),
            )
            .unwrap();
        assert_eq!(r2.unwrap(), "echo: from home2");

        // Home-2 hat keine secret.txt
        assert!(!home2.path().join("secret.txt").exists());
    }

    // ---- Resource-Exhaustion: Fuel + Memory kombiniert ----

    #[test]
    fn security_fuel_exhaustion_is_deterministic() {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();
        host.load(PluginConfig {
            wasm_path: loop_fixture(),
            fuel_limit: 50_000,
            ..Default::default()
        })
        .unwrap();

        // Loop-Plugin mit 50K Fuel — muss konsistent fehlschlagen
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..5 {
            let result = host.execute(
                &loop_fixture(),
                "trigger",
                make_agent(),
                HashMap::new(),
                1,
                dir.path().to_path_buf(),
            );
            assert!(
                result.is_err(),
                "Fuel exhaustion must be deterministic and consistent"
            );
        }
    }

    #[test]
    fn security_memory_exhaustion_caught() {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();
        // 64KB — Component kann nicht instantiiert werden
        host.load(PluginConfig {
            wasm_path: echo_fixture(),
            memory_limit_bytes: 64 * 1024,
            ..Default::default()
        })
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let result = host.execute(
            &echo_fixture(),
            "test",
            make_agent(),
            HashMap::new(),
            1,
            dir.path().to_path_buf(),
        );
        assert!(result.is_err(), "64KB memory limit must prevent execution");
    }

    // ---- PluginHost State-Isolation: Kein Zustand zwischen Aufrufen ----

    #[test]
    fn security_no_state_leakage_between_calls() {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();
        host.load(PluginConfig {
            wasm_path: echo_fixture(),
            ..Default::default()
        })
        .unwrap();

        let dir = tempfile::tempdir().unwrap();

        // Erster Aufruf mit Agent-01
        let r1 = host
            .execute(
                &echo_fixture(),
                "call-1",
                AgentSnapshot {
                    agent_id: "AGENT-01".to_string(),
                    name: "First Agent".to_string(),
                    hunger: 0.9,
                    ..Default::default()
                },
                HashMap::new(),
                100,
                dir.path().to_path_buf(),
            )
            .unwrap();
        assert_eq!(r1.unwrap(), "echo: call-1");

        // Zweiter Aufruf mit Agent-02 — darf keine Daten von Agent-01 sehen
        let r2 = host
            .execute(
                &echo_fixture(),
                "call-2",
                AgentSnapshot {
                    agent_id: "AGENT-02".to_string(),
                    name: "Second Agent".to_string(),
                    hunger: 0.1,
                    ..Default::default()
                },
                HashMap::new(),
                200,
                dir.path().to_path_buf(),
            )
            .unwrap();
        assert_eq!(r2.unwrap(), "echo: call-2");
        // Wenn State leaken wuerde, haette der zweite Aufruf "First Agent" Daten.
        // Da jeder Aufruf einen neuen Store bekommt, ist das ausgeschlossen.
    }

    // ---- Regression: Nonexistent Plugin gibt klaren Fehler ----

    #[test]
    fn security_nonexistent_plugin_clear_error() {
        let host = sentinel_wasm::PluginHost::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let result = host.execute(
            &PathBuf::from("/nonexistent/evil.wasm"),
            "attack",
            make_agent(),
            HashMap::new(),
            1,
            dir.path().to_path_buf(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not loaded"));
    }

    // ---- Robustheit: Grosser Input wird korrekt verarbeitet ----

    #[test]
    fn security_large_input_handled() {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();
        host.load(PluginConfig {
            wasm_path: echo_fixture(),
            ..Default::default()
        })
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let large_input = "x".repeat(100_000); // 100KB Input

        let result = host
            .execute(
                &echo_fixture(),
                &large_input,
                make_agent(),
                HashMap::new(),
                1,
                dir.path().to_path_buf(),
            )
            .unwrap();
        let output = result.unwrap();
        assert_eq!(output.len(), "echo: ".len() + 100_000);
    }

    // ---- Concurrent Safety: Mehrere PluginHost-Instanzen unabhaengig ----

    #[test]
    fn security_multiple_plugin_hosts_independent() {
        let mut host1 = sentinel_wasm::PluginHost::new().unwrap();
        let mut host2 = sentinel_wasm::PluginHost::new().unwrap();

        host1
            .load(PluginConfig {
                wasm_path: echo_fixture(),
                ..Default::default()
            })
            .unwrap();

        // Host2 hat kein geladenes Plugin
        assert_eq!(host1.cached_count(), 1);
        assert_eq!(host2.cached_count(), 0);

        // Host1 kann ausfuehren, Host2 nicht
        let dir = tempfile::tempdir().unwrap();
        let r1 = host1.execute(
            &echo_fixture(),
            "host1",
            make_agent(),
            HashMap::new(),
            1,
            dir.path().to_path_buf(),
        );
        assert!(r1.is_ok());

        let r2 = host2.execute(
            &echo_fixture(),
            "host2",
            make_agent(),
            HashMap::new(),
            1,
            dir.path().to_path_buf(),
        );
        assert!(r2.is_err(), "Host2 has no loaded plugins");

        // Jetzt Host2 auch laden — unabhaengig von Host1
        host2
            .load(PluginConfig {
                wasm_path: echo_fixture(),
                ..Default::default()
            })
            .unwrap();
        let r3 = host2
            .execute(
                &echo_fixture(),
                "host2 now works",
                make_agent(),
                HashMap::new(),
                1,
                dir.path().to_path_buf(),
            )
            .unwrap();
        assert_eq!(r3.unwrap(), "echo: host2 now works");
    }
}

// ============================================================================
// Fake-OS / Fake-Filesystem E2E Tests
// ============================================================================
// Tests die beweisen dass ein WASM-Plugin das komplette "Fake OS" erlebt:
// - fs-read/fs-write/fs-list durch die WASM-Boundary
// - get-room-info durch die WASM-Boundary
// - Komplette Pipeline: Datei lesen → verarbeiten → Ergebnis schreiben
// - Isolation: Plugin sieht NUR sein Agent-Home
// - Fehlerbehandlung durch die WASM-Boundary
// ============================================================================

#[cfg(feature = "wasm")]
mod wasm_fs {
    use sentinel_wasm::{AgentSnapshot, PluginConfig, RoomSnapshot};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn fs_fixture() -> PathBuf {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/fs-plugin.wasm");
        path
    }

    fn make_agent_snapshot() -> AgentSnapshot {
        AgentSnapshot {
            agent_id: "AGENT-07".to_string(),
            name: "Lisa Bergmann".to_string(),
            role: "Designer".to_string(),
            hunger: 0.45,
            energy: 0.82,
            stress: 0.15,
            social_need: 0.60,
            caffeine: 0.33,
            bladder: 0.20,
            room_id: "buero-design".to_string(),
        }
    }

    fn make_rooms() -> HashMap<String, RoomSnapshot> {
        let mut rooms = HashMap::new();
        rooms.insert(
            "buero-design".to_string(),
            RoomSnapshot {
                room_id: "buero-design".to_string(),
                name: "Design Buero".to_string(),
                floor: 1,
                temperature: 21.5,
                noise_db: 40.0,
                occupant_count: 3,
            },
        );
        rooms.insert(
            "kueche-eg".to_string(),
            RoomSnapshot {
                room_id: "kueche-eg".to_string(),
                name: "Kueche EG".to_string(),
                floor: 0,
                temperature: 23.0,
                noise_db: 55.0,
                occupant_count: 2,
            },
        );
        rooms
    }

    fn load_fs_plugin() -> sentinel_wasm::PluginHost {
        let mut host = sentinel_wasm::PluginHost::new().unwrap();
        host.load(PluginConfig {
            wasm_path: fs_fixture(),
            ..Default::default()
        })
        .unwrap();
        host
    }

    // ---- fs-write durch WASM-Boundary ----

    #[test]
    fn fs_e2e_write_through_wasm() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        let result = host
            .execute(
                &fs_fixture(),
                "write hello.txt Hallo aus dem WASM Plugin!",
                make_agent_snapshot(),
                make_rooms(),
                100,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_ok(), "fs-write must succeed: {:?}", result.err());
        let output = result.unwrap();
        assert!(output.contains("written:"), "Output: {output}");
        assert!(output.contains("hello.txt"), "Output: {output}");

        // Host-Side Verifikation: Datei muss im tempdir existieren
        let content = std::fs::read_to_string(dir.path().join("hello.txt")).unwrap();
        assert_eq!(content, "Hallo aus dem WASM Plugin!");
    }

    // ---- fs-read durch WASM-Boundary ----

    #[test]
    fn fs_e2e_read_through_wasm() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        // Host-Side: Datei vorbefuellen BEVOR Plugin sie liest
        std::fs::write(dir.path().join("input.txt"), "Geheimer Inhalt 42").unwrap();

        let result = host
            .execute(
                &fs_fixture(),
                "read input.txt",
                make_agent_snapshot(),
                make_rooms(),
                200,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_ok(), "fs-read must succeed: {:?}", result.err());
        assert_eq!(result.unwrap(), "Geheimer Inhalt 42");
    }

    // ---- fs-list durch WASM-Boundary ----

    #[test]
    fn fs_e2e_list_through_wasm() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        // Mehrere Dateien vorbefuellen
        std::fs::write(dir.path().join("alpha.txt"), "a").unwrap();
        std::fs::write(dir.path().join("beta.txt"), "b").unwrap();
        std::fs::write(dir.path().join("gamma.txt"), "c").unwrap();

        let result = host
            .execute(
                &fs_fixture(),
                "list .",
                make_agent_snapshot(),
                make_rooms(),
                300,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_ok(), "fs-list must succeed: {:?}", result.err());
        let output = result.unwrap();
        let mut entries: Vec<&str> = output.lines().collect();
        entries.sort();
        assert_eq!(entries, vec!["alpha.txt", "beta.txt", "gamma.txt"]);
    }

    // ---- get-room-info durch WASM-Boundary ----

    #[test]
    fn fs_e2e_room_info_through_wasm() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        let result = host
            .execute(
                &fs_fixture(),
                "room buero-design",
                make_agent_snapshot(),
                make_rooms(),
                400,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_ok(), "get-room-info must succeed: {:?}", result.err());
        let output = result.unwrap();
        assert!(output.contains("room:buero-design"), "Output: {output}");
        assert!(output.contains("name:Design Buero"), "Output: {output}");
        assert!(output.contains("floor:1"), "Output: {output}");
        assert!(output.contains("temp:21.5"), "Output: {output}");
        assert!(output.contains("noise:40.0"), "Output: {output}");
        assert!(output.contains("occupants:3"), "Output: {output}");
    }

    // ---- get-room-info fuer nicht-existenten Raum ----

    #[test]
    fn fs_e2e_room_info_not_found() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        let result = host
            .execute(
                &fs_fixture(),
                "room fantasy-raum",
                make_agent_snapshot(),
                make_rooms(),
                400,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_err(), "nonexistent room must return Err");
        let err = result.unwrap_err();
        assert!(err.contains("not found"), "Error: {err}");
    }

    // ---- status: ALLE Host-Functions in einem Aufruf ----

    #[test]
    fn fs_e2e_status_uses_all_host_functions() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        let result = host
            .execute(
                &fs_fixture(),
                "status",
                make_agent_snapshot(),
                make_rooms(),
                500,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_ok(), "status must succeed: {:?}", result.err());
        let output = result.unwrap();

        // get-agent-info verifizieren
        assert!(output.contains("agent:Lisa Bergmann"), "Output: {output}");
        assert!(output.contains("role:Designer"), "Output: {output}");

        // get-tick verifizieren
        assert!(output.contains("tick:500"), "Output: {output}");

        // Bio-Werte verifizieren
        assert!(output.contains("hunger:0.45"), "Output: {output}");
        assert!(output.contains("energy:0.82"), "Output: {output}");
        assert!(output.contains("stress:0.15"), "Output: {output}");
        assert!(output.contains("social:0.60"), "Output: {output}");
        assert!(output.contains("caffeine:0.33"), "Output: {output}");
        assert!(output.contains("bladder:0.20"), "Output: {output}");

        // get-room-info verifizieren (room-id aus Agent-Snapshot)
        assert!(output.contains("room:Design Buero"), "Output: {output}");
        assert!(output.contains("temp:21.5"), "Output: {output}");
    }

    // ---- process: Volle Pipeline (read -> verarbeiten -> write -> list -> verify) ----

    #[test]
    fn fs_e2e_process_full_pipeline() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        // Input-Datei vorbefuellen
        let input_content = "Dies ist ein Test\nmit mehreren Zeilen\nund einigen Woertern darin";
        std::fs::write(dir.path().join("input.md"), input_content).unwrap();

        let result = host
            .execute(
                &fs_fixture(),
                "process input.md report.txt",
                make_agent_snapshot(),
                make_rooms(),
                600,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_ok(), "process must succeed: {:?}", result.err());
        let output = result.unwrap();

        // Plugin-Output: Woerter, Zeilen, Bytes
        assert!(output.contains("11 words"), "Output: {output}");
        assert!(output.contains("3 lines"), "Output: {output}");
        let expected_bytes = input_content.len();
        assert!(
            output.contains(&format!("{expected_bytes} bytes")),
            "Output: {output}"
        );

        // Host-Side Verifikation: Report-Datei muss existieren
        let report = std::fs::read_to_string(dir.path().join("report.txt")).unwrap();
        assert!(report.contains("File Analysis Report"), "Report: {report}");
        assert!(report.contains("Source: input.md"), "Report: {report}");
        assert!(report.contains("Agent: Lisa Bergmann"), "Report: {report}");
        assert!(report.contains("Tick: 600"), "Report: {report}");
        assert!(report.contains("Words: 11"), "Report: {report}");
        assert!(report.contains("Lines: 3"), "Report: {report}");
    }

    // ---- fs-write in Subdirectory durch WASM ----

    #[test]
    fn fs_e2e_write_subdirectory() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        let result = host
            .execute(
                &fs_fixture(),
                "write sub/deep/file.txt nested content",
                make_agent_snapshot(),
                make_rooms(),
                700,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_ok(), "write to subdir must succeed: {:?}", result.err());

        // Host-Side: Subdirectory + Datei existiert
        let content = std::fs::read_to_string(dir.path().join("sub/deep/file.txt")).unwrap();
        assert_eq!(content, "nested content");
    }

    // ---- fs-read auf nicht-existente Datei durch WASM ----

    #[test]
    fn fs_e2e_read_nonexistent_file() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        let result = host
            .execute(
                &fs_fixture(),
                "read does_not_exist.txt",
                make_agent_snapshot(),
                make_rooms(),
                800,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_err(), "reading nonexistent file must return Err");
        let err = result.unwrap_err();
        assert!(
            err.contains("fs-read") || err.contains("No such file"),
            "Error must be descriptive: {err}"
        );
    }

    // ---- Path-Traversal durch WASM geblockt ----

    #[test]
    fn fs_e2e_path_traversal_blocked() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        let result = host
            .execute(
                &fs_fixture(),
                "read ../../../etc/passwd",
                make_agent_snapshot(),
                make_rooms(),
                900,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_err(), "path traversal must be blocked");
        let err = result.unwrap_err();
        assert!(
            err.contains("traversal") || err.contains("not allowed"),
            "Error must mention traversal: {err}"
        );
    }

    // ---- Absolute Pfade durch WASM geblockt ----

    #[test]
    fn fs_e2e_absolute_path_blocked() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        let result = host
            .execute(
                &fs_fixture(),
                "read /etc/passwd",
                make_agent_snapshot(),
                make_rooms(),
                900,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_err(), "absolute path must be blocked");
        let err = result.unwrap_err();
        assert!(
            err.contains("absolute") || err.contains("not allowed"),
            "Error must mention absolute: {err}"
        );
    }

    // ---- fs-write + fs-read Roundtrip durch WASM ----

    #[test]
    fn fs_e2e_write_then_read_roundtrip() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        // Erst schreiben
        let write_result = host
            .execute(
                &fs_fixture(),
                "write roundtrip.dat Binary-safe content: abc-xyz-123",
                make_agent_snapshot(),
                make_rooms(),
                1000,
                dir.path().to_path_buf(),
            )
            .unwrap();
        assert!(write_result.is_ok(), "write: {:?}", write_result.err());

        // Dann lesen
        let read_result = host
            .execute(
                &fs_fixture(),
                "read roundtrip.dat",
                make_agent_snapshot(),
                make_rooms(),
                1001,
                dir.path().to_path_buf(),
            )
            .unwrap();
        assert!(read_result.is_ok(), "read: {:?}", read_result.err());
        assert_eq!(
            read_result.unwrap(),
            "Binary-safe content: abc-xyz-123"
        );
    }

    // ---- Zwei Agents sehen verschiedene Fake-Filesysteme ----

    #[test]
    fn fs_e2e_agents_have_isolated_filesystems() {
        let host = load_fs_plugin();

        let agent1_home = tempfile::tempdir().unwrap();
        let agent2_home = tempfile::tempdir().unwrap();

        let agent1 = AgentSnapshot {
            agent_id: "AGENT-01".to_string(),
            name: "Thomas Mueller".to_string(),
            role: "CEO".to_string(),
            ..make_agent_snapshot()
        };

        let agent2 = AgentSnapshot {
            agent_id: "AGENT-16".to_string(),
            name: "Michael Weber".to_string(),
            role: "CEO".to_string(),
            ..make_agent_snapshot()
        };

        // Agent 1 schreibt eine Datei
        let r1 = host
            .execute(
                &fs_fixture(),
                "write secret.txt Agent 1 geheime Daten",
                agent1.clone(),
                make_rooms(),
                1,
                agent1_home.path().to_path_buf(),
            )
            .unwrap();
        assert!(r1.is_ok());

        // Agent 2 versucht dieselbe Datei zu lesen — sieht sie NICHT
        let r2 = host
            .execute(
                &fs_fixture(),
                "read secret.txt",
                agent2,
                make_rooms(),
                1,
                agent2_home.path().to_path_buf(),
            )
            .unwrap();
        assert!(r2.is_err(), "Agent 2 must NOT see Agent 1's files");

        // Agent 1 kann seine eigene Datei lesen
        let r3 = host
            .execute(
                &fs_fixture(),
                "read secret.txt",
                agent1,
                make_rooms(),
                1,
                agent1_home.path().to_path_buf(),
            )
            .unwrap();
        assert!(r3.is_ok());
        assert_eq!(r3.unwrap(), "Agent 1 geheime Daten");
    }

    // ---- fs-list leeres Verzeichnis ----

    #[test]
    fn fs_e2e_list_empty_directory() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        let result = host
            .execute(
                &fs_fixture(),
                "list .",
                make_agent_snapshot(),
                make_rooms(),
                1100,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_ok(), "list empty dir: {:?}", result.err());
        let output = result.unwrap();
        assert!(output.is_empty(), "empty dir should return empty string, got: '{output}'");
    }

    // ---- Zweiter Room in der Map ----

    #[test]
    fn fs_e2e_room_info_second_room() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        let result = host
            .execute(
                &fs_fixture(),
                "room kueche-eg",
                make_agent_snapshot(),
                make_rooms(),
                1200,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_ok(), "kueche-eg: {:?}", result.err());
        let output = result.unwrap();
        assert!(output.contains("room:kueche-eg"), "Output: {output}");
        assert!(output.contains("name:Kueche EG"), "Output: {output}");
        assert!(output.contains("floor:0"), "Output: {output}");
        assert!(output.contains("temp:23.0"), "Output: {output}");
        assert!(output.contains("noise:55.0"), "Output: {output}");
        assert!(output.contains("occupants:2"), "Output: {output}");
    }

    // ---- Unbekanntes Kommando ----

    #[test]
    fn fs_e2e_unknown_command() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        let result = host
            .execute(
                &fs_fixture(),
                "delete everything",
                make_agent_snapshot(),
                make_rooms(),
                1300,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown command"));
    }

    // ---- query_meta fuer fs-plugin ----

    #[test]
    fn fs_e2e_query_meta() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        let meta = host.query_meta(&fs_fixture(), dir.path().to_path_buf()).unwrap();
        assert_eq!(meta.tool_name, "fs-processor");
        assert!(
            meta.tool_description.contains("File processor"),
            "Description: {}",
            meta.tool_description
        );
    }

    // ---- Grosse Datei durch WASM (10KB) ----

    #[test]
    fn fs_e2e_large_file_roundtrip() {
        let host = load_fs_plugin();
        let dir = tempfile::tempdir().unwrap();

        // 10KB Datei vorbefuellen
        let large_content: String = (0..500)
            .map(|i| format!("Zeile {i}: Lorem ipsum dolor sit amet\n"))
            .collect();
        std::fs::write(dir.path().join("large.txt"), &large_content).unwrap();

        // Plugin liest die grosse Datei
        let result = host
            .execute(
                &fs_fixture(),
                "read large.txt",
                make_agent_snapshot(),
                make_rooms(),
                1400,
                dir.path().to_path_buf(),
            )
            .unwrap();

        assert!(result.is_ok(), "large file read: {:?}", result.err());
        assert_eq!(result.unwrap(), large_content);
    }

    // ---- ToolRuntime E2E: fs-plugin als registriertes WASM-Tool ----

    #[test]
    fn fs_e2e_tool_runtime_integration() {
        use sentinel_wasm::{ExecutionContext, SandboxConfig, ToolDefinition, ToolType};

        let dir = tempfile::tempdir().unwrap();

        // Datei vorbefuellen
        std::fs::write(dir.path().join("data.txt"), "Runtime Test Daten").unwrap();

        let mut runtime = sentinel_wasm::ToolRuntime::new();

        // FS-Plugin laden
        runtime
            .plugin_host_mut()
            .load(PluginConfig {
                wasm_path: fs_fixture(),
                ..Default::default()
            })
            .unwrap();

        // Als WASM-Tool registrieren
        runtime
            .register_tool(ToolDefinition {
                name: "fs-processor".to_string(),
                description: "FS Processor".to_string(),
                wasm_path: Some(fs_fixture().to_string_lossy().to_string()),
                tool_type: ToolType::Wasm,
                required_capabilities: Vec::new(),
            })
            .unwrap();

        let ctx = ExecutionContext {
            agent_id: "AGENT-07".to_string(),
            agent_capabilities: vec![],
            sandbox: SandboxConfig::with_paths(vec![dir.path().to_path_buf()]),
            correlation_id: "test-fs-runtime".to_string(),
            tick: 700,
            agent_snapshot: Some(make_agent_snapshot()),
            rooms: Some(make_rooms()),
        };

        let result = runtime.execute("fs-processor", "read data.txt", &ctx).unwrap();
        assert!(result.success, "ToolResult.success must be true");
        assert_eq!(result.output, "Runtime Test Daten");
        assert_eq!(result.agent_id, "AGENT-07");
        assert_eq!(result.tool_name, "fs-processor");
    }
}
