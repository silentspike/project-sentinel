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
