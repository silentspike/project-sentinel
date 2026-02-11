//! Acceptance Tests fuer Issue #19: sentinel-wasm
//!
//! Testet ToolRuntime: register/lookup, file_read, file_write,
//! unknown tool error.

use sentinel_wasm::{ToolDefinition, ToolRuntime, ToolType};
use std::io::Write;

// AC #19.02: Register 3 Tools, list_tools().len()==3, get_tool("X") found
#[test]
fn ac_19_02_register_lookup() {
    let mut runtime = ToolRuntime::new();

    let tool1 = ToolDefinition {
        name: "file_read".to_string(),
        description: "Read a file".to_string(),
        wasm_path: None,
        tool_type: ToolType::FileRead,
    };
    let tool2 = ToolDefinition {
        name: "file_write".to_string(),
        description: "Write a file".to_string(),
        wasm_path: None,
        tool_type: ToolType::FileWrite,
    };
    let tool3 = ToolDefinition {
        name: "chat".to_string(),
        description: "Chat tool".to_string(),
        wasm_path: None,
        tool_type: ToolType::Chat,
    };

    runtime.register_tool(tool1).unwrap();
    runtime.register_tool(tool2).unwrap();
    runtime.register_tool(tool3).unwrap();

    assert_eq!(
        runtime.list_tools().len(),
        3,
        "After registering 3 tools, list_tools should return 3"
    );

    let found = runtime.get_tool("file_read");
    assert!(
        found.is_some(),
        "get_tool('file_read') should find the registered tool"
    );
    assert_eq!(found.unwrap().name, "file_read");

    let found2 = runtime.get_tool("chat");
    assert!(
        found2.is_some(),
        "get_tool('chat') should find the registered tool"
    );
}

// AC #19.03: Tempfile erstellen, execute("file_read", path) -> Inhalt korrekt
#[test]
fn ac_19_03_file_read() {
    let mut runtime = ToolRuntime::new();
    runtime
        .register_tool(ToolDefinition {
            name: "file_read".to_string(),
            description: "Read a file".to_string(),
            wasm_path: None,
            tool_type: ToolType::FileRead,
        })
        .unwrap();

    // Erstelle Temp-Datei mit bekanntem Inhalt
    let mut temp_file = tempfile::NamedTempFile::new().unwrap();
    let expected = "Hello from acceptance test!";
    temp_file.write_all(expected.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let path = temp_file.path().to_str().unwrap();
    let result = runtime.execute("file_read", path).unwrap();

    assert_eq!(
        result, expected,
        "file_read should return exact file content"
    );
}

// AC #19.04: execute("file_write", "path\ninhalt"), Datei pruefen
#[test]
fn ac_19_04_file_write() {
    let mut runtime = ToolRuntime::new();
    runtime
        .register_tool(ToolDefinition {
            name: "file_write".to_string(),
            description: "Write a file".to_string(),
            wasm_path: None,
            tool_type: ToolType::FileWrite,
        })
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test_output.txt");
    let content = "Written by acceptance test";
    let input = format!("{}\n{}", file_path.display(), content);

    let result = runtime.execute("file_write", &input).unwrap();

    // Verifiziere die Datei existiert und den richtigen Inhalt hat
    let read_back = std::fs::read_to_string(&file_path).unwrap();
    assert_eq!(
        read_back, content,
        "File content should match what was written"
    );
    assert!(
        result.contains("Written"),
        "Result should confirm write, got: '{}'",
        result
    );
}

// AC #19.05: execute("unknown", "") -> Err
#[test]
fn ac_19_05_unknown_tool_error() {
    let runtime = ToolRuntime::new();

    let result = runtime.execute("nonexistent_tool", "");

    assert!(
        result.is_err(),
        "Executing an unknown tool should return an error"
    );
}
