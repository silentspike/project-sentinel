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

// Wasm-spezifische Tests — nur mit wasm-Feature kompilierbar
#[cfg(feature = "wasm")]
mod wasm_tests {
    use super::*;

    // AC-1: Wasm Tools starten reproduzierbar via wasmtime
    #[test]
    fn ac_19_01_wasm_execution() {
        let mut runtime = ToolRuntime::new();

        // Minimales WAT-Modul: execute() -> 0 (Erfolg)
        let wat = r#"(module
            (func (export "execute") (result i32)
                i32.const 0
            )
        )"#;

        let dir = tempfile::tempdir().unwrap();
        let wasm_path = dir.path().join("success.wat");
        std::fs::write(&wasm_path, wat).unwrap();

        let tool = ToolDefinition {
            name: "wasm_ok".to_string(),
            description: "Successful wasm tool".to_string(),
            wasm_path: Some(wasm_path.to_str().unwrap().to_string()),
            tool_type: ToolType::Wasm,
            required_capabilities: Vec::new(),
        };
        runtime.register_tool(tool).unwrap();

        let sandbox = SandboxConfig::with_paths(vec![dir.path().to_path_buf()]);
        let ctx = ctx_with_sandbox(sandbox);
        let result = runtime.execute("wasm_ok", "", &ctx).unwrap();

        assert!(result.success);
        assert!(result.output.contains("executed successfully"));
    }

    // AC-1: Wasm-Modul mit Fehler-Rueckgabe
    #[test]
    fn ac_19_01_wasm_error_code() {
        let mut runtime = ToolRuntime::new();

        // WAT-Modul: execute() -> 1 (Fehler)
        let wat = r#"(module
            (func (export "execute") (result i32)
                i32.const 1
            )
        )"#;

        let dir = tempfile::tempdir().unwrap();
        let wasm_path = dir.path().join("error.wat");
        std::fs::write(&wasm_path, wat).unwrap();

        let tool = ToolDefinition {
            name: "wasm_err".to_string(),
            description: "Failing wasm tool".to_string(),
            wasm_path: Some(wasm_path.to_str().unwrap().to_string()),
            tool_type: ToolType::Wasm,
            required_capabilities: Vec::new(),
        };
        runtime.register_tool(tool).unwrap();

        let sandbox = SandboxConfig::with_paths(vec![dir.path().to_path_buf()]);
        let ctx = ctx_with_sandbox(sandbox);
        let result = runtime.execute("wasm_err", "", &ctx);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("error code 1"));
    }

    // AC-4: Fuel-Exhaustion verhindert Endlosschleifen
    #[test]
    fn ac_19_04_wasm_fuel_exhaustion() {
        let mut runtime = ToolRuntime::new();

        // WAT-Modul: Endlosschleife
        let wat = r#"(module
            (func (export "execute") (result i32)
                (local $i i32)
                (loop $loop
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br $loop)
                )
                i32.const 0
            )
        )"#;

        let dir = tempfile::tempdir().unwrap();
        let wasm_path = dir.path().join("infinite.wat");
        std::fs::write(&wasm_path, wat).unwrap();

        let tool = ToolDefinition {
            name: "wasm_hang".to_string(),
            description: "Hanging wasm tool".to_string(),
            wasm_path: Some(wasm_path.to_str().unwrap().to_string()),
            tool_type: ToolType::Wasm,
            required_capabilities: Vec::new(),
        };
        runtime.register_tool(tool).unwrap();

        // Minimale Sandbox: nur 1ms CPU -> sehr wenig Fuel
        let mut sandbox = SandboxConfig::restrictive();
        sandbox.allowed_paths = vec![dir.path().to_path_buf()];
        sandbox.max_cpu_ms = 1; // 1M fuel — Endlosschleife verbraucht das schnell
        let ctx = ctx_with_sandbox(sandbox);

        let result = runtime.execute("wasm_hang", "", &ctx);
        assert!(
            result.is_err(),
            "Infinite loop must be stopped by fuel exhaustion"
        );
        // wasmtime stoppt die Ausfuehrung — Fehlermeldung variiert je nach Version
        // (z.B. "all fuel consumed", "wasm trap", "error while executing")
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("fuel")
                || err_msg.contains("Fuel")
                || err_msg.contains("wasm")
                || err_msg.contains("executing"),
            "Error should indicate WASM execution was stopped: {err_msg}"
        );
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
        assert!(result.is_err());
    }
}
