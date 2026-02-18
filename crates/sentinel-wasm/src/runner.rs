//! Tool runtime for agent capabilities.
//!
//! Provides a registry for tool definitions and sandboxed execution.
//! Native handlers for FileRead/FileWrite with filesystem restrictions.
//! WASM module execution via wasmtime (behind `wasm` feature flag).

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::time::Instant;

use crate::sandbox::SandboxConfig;

/// Kategorien verfuegbarer Tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolType {
    FileRead,
    FileWrite,
    Chat,
    Calendar,
    Search,
    /// WASM-Modul, geladen aus `ToolDefinition::wasm_path`.
    Wasm,
}

/// Definition eines einzelnen Tools.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// Pfad zum WASM-Modul (nur fuer `ToolType::Wasm`).
    pub wasm_path: Option<String>,
    pub tool_type: ToolType,
    /// Capabilities die ein Agent haben muss um dieses Tool zu nutzen.
    pub required_capabilities: Vec<String>,
}

/// Strukturiertes Tool-Ergebnis.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolResult {
    pub tool_name: String,
    pub agent_id: String,
    pub output: String,
    pub success: bool,
    pub duration_ms: u64,
}

impl ToolResult {
    /// Konvertiert das Ergebnis in ein DomainEvent.
    pub fn to_domain_event(&self, correlation_id: &str, tick: u64) -> sentinel_common::DomainEvent {
        sentinel_common::DomainEvent::new(
            "tool_result",
            &self.agent_id,
            &serde_json::to_string(self).unwrap_or_default(),
            correlation_id,
            tick,
        )
    }
}

/// Ausfuehrungskontext fuer einen Tool-Call.
pub struct ExecutionContext {
    pub agent_id: String,
    pub agent_capabilities: Vec<String>,
    pub sandbox: SandboxConfig,
    pub correlation_id: String,
    pub tick: u64,
}

/// Registry und Executor fuer Agent-Tools.
pub struct ToolRuntime {
    tools: HashMap<String, ToolDefinition>,
    #[cfg(feature = "wasm")]
    wasm_engine: wasmtime::Engine,
}

impl ToolRuntime {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            #[cfg(feature = "wasm")]
            wasm_engine: {
                let mut config = wasmtime::Config::new();
                config.consume_fuel(true);
                wasmtime::Engine::new(&config).expect("Failed to create Wasm engine")
            },
        }
    }

    /// Registriert ein neues Tool. Fehler bei Duplikat.
    pub fn register_tool(&mut self, definition: ToolDefinition) -> Result<()> {
        if self.tools.contains_key(&definition.name) {
            return Err(anyhow!("Tool '{}' already registered", definition.name));
        }
        self.tools.insert(definition.name.clone(), definition);
        Ok(())
    }

    /// Gibt alle registrierten Tools zurueck.
    pub fn list_tools(&self) -> Vec<&ToolDefinition> {
        self.tools.values().collect()
    }

    /// Sucht ein Tool nach Name.
    pub fn get_tool(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools.get(name)
    }

    /// Anzahl registrierter Tools.
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// Fuehrt ein Tool mit Sandbox-Isolation und Capability-Check aus.
    ///
    /// Prueft Capabilities, wendet Filesystem-Restrictions an,
    /// und gibt ein strukturiertes `ToolResult` zurueck.
    pub fn execute(&self, name: &str, input: &str, ctx: &ExecutionContext) -> Result<ToolResult> {
        let start = Instant::now();

        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| anyhow!("Tool '{}' not found", name))?;

        // Capability Check
        if !crate::registry::can_execute(&ctx.agent_capabilities, tool) {
            return Err(anyhow!(
                "Agent '{}' lacks required capabilities for tool '{}'",
                ctx.agent_id,
                name
            ));
        }

        let output = match tool.tool_type {
            ToolType::FileRead => self.execute_file_read(input, &ctx.sandbox)?,
            ToolType::FileWrite => self.execute_file_write(input, &ctx.sandbox)?,
            ToolType::Wasm => {
                #[cfg(feature = "wasm")]
                {
                    self.execute_wasm(tool, input, &ctx.sandbox)?
                }
                #[cfg(not(feature = "wasm"))]
                {
                    return Err(anyhow!(
                        "Wasm support not enabled. Compile with --features wasm"
                    ));
                }
            }
            other => {
                return Err(anyhow!("Tool type {:?} not yet implemented", other));
            }
        };

        let duration = start.elapsed();

        // Post-hoc timeout check fuer native Tools.
        // WASM-Module werden via fuel-Mechanismus begrenzt.
        if duration > ctx.sandbox.max_execution_time {
            return Err(anyhow!(
                "Tool '{}' exceeded timeout ({:?} > {:?})",
                name,
                duration,
                ctx.sandbox.max_execution_time
            ));
        }

        Ok(ToolResult {
            tool_name: name.to_string(),
            agent_id: ctx.agent_id.clone(),
            output,
            success: true,
            duration_ms: duration.as_millis() as u64,
        })
    }

    /// Liest eine Datei, geprueft gegen Sandbox-Pfade.
    fn execute_file_read(&self, input: &str, sandbox: &SandboxConfig) -> Result<String> {
        let path = std::path::Path::new(input.trim());
        if !sandbox.is_path_allowed(path) {
            return Err(anyhow!(
                "Sandbox violation: path '{}' not in allowed paths",
                input.trim()
            ));
        }
        let content = std::fs::read_to_string(path)?;
        Ok(content)
    }

    /// Schreibt eine Datei (Format: "pfad\ninhalt"), geprueft gegen Sandbox-Pfade.
    fn execute_file_write(&self, input: &str, sandbox: &SandboxConfig) -> Result<String> {
        let (path_str, content) = input
            .split_once('\n')
            .ok_or_else(|| anyhow!("FileWrite input must be 'path\\ncontent'"))?;
        let path = std::path::Path::new(path_str.trim());
        if !sandbox.is_path_allowed(path) {
            return Err(anyhow!(
                "Sandbox violation: path '{}' not in allowed paths",
                path_str.trim()
            ));
        }
        std::fs::write(path, content)?;
        Ok(format!(
            "Written {} bytes to {}",
            content.len(),
            path_str.trim()
        ))
    }

    /// Fuehrt ein WASM-Modul via wasmtime aus.
    ///
    /// Nutzt fuel-Mechanismus fuer CPU-Begrenzung.
    /// Modul muss `execute() -> i32` exportieren (0 = Erfolg).
    #[cfg(feature = "wasm")]
    fn execute_wasm(
        &self,
        tool: &ToolDefinition,
        _input: &str,
        sandbox: &SandboxConfig,
    ) -> Result<String> {
        let wasm_path = tool
            .wasm_path
            .as_ref()
            .ok_or_else(|| anyhow!("Wasm tool '{}' has no wasm_path", tool.name))?;

        // Lade Modul (unterstuetzt .wasm und .wat)
        let module = wasmtime::Module::from_file(&self.wasm_engine, wasm_path)?;
        let mut store = wasmtime::Store::new(&self.wasm_engine, ());

        // Fuel-basiertes CPU-Limit (1ms ~ 1M fuel units)
        let fuel = sandbox.max_cpu_ms.saturating_mul(1_000_000);
        store.set_fuel(fuel)?;

        let linker = wasmtime::Linker::new(&self.wasm_engine);
        let instance = linker.instantiate(&mut store, &module)?;

        // Modul muss `execute() -> i32` exportieren
        let execute_fn = instance
            .get_typed_func::<(), i32>(&mut store, "execute")
            .map_err(|_| anyhow!("Wasm module must export 'execute() -> i32'"))?;

        let result_code = execute_fn.call(&mut store, ())?;

        if result_code == 0 {
            Ok(format!("Wasm module '{}' executed successfully", tool.name))
        } else {
            Err(anyhow!(
                "Wasm module '{}' returned error code {}",
                tool.name,
                result_code
            ))
        }
    }
}

impl Default for ToolRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_tool(name: &str, tool_type: ToolType) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: format!("Test tool {name}"),
            wasm_path: None,
            tool_type,
            required_capabilities: Vec::new(),
        }
    }

    fn test_ctx(sandbox: SandboxConfig) -> ExecutionContext {
        ExecutionContext {
            agent_id: "AGENT-01".to_string(),
            agent_capabilities: vec!["file_read".to_string(), "file_write".to_string()],
            sandbox,
            correlation_id: "test-correlation".to_string(),
            tick: 1,
        }
    }

    #[test]
    fn register_and_list() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("read_file", ToolType::FileRead))
            .unwrap();
        runtime
            .register_tool(make_tool("write_file", ToolType::FileWrite))
            .unwrap();
        assert_eq!(runtime.list_tools().len(), 2);
        assert_eq!(runtime.tool_count(), 2);
    }

    #[test]
    fn duplicate_registration_fails() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("dupe", ToolType::FileRead))
            .unwrap();
        let result = runtime.register_tool(make_tool("dupe", ToolType::FileRead));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("already registered"));
    }

    #[test]
    fn get_by_name() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("my_tool", ToolType::Chat))
            .unwrap();
        let found = runtime.get_tool("my_tool");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "my_tool");
        assert_eq!(found.unwrap().tool_type, ToolType::Chat);
    }

    #[test]
    fn get_nonexistent() {
        let runtime = ToolRuntime::new();
        assert!(runtime.get_tool("xyz").is_none());
    }

    #[test]
    fn file_read_with_sandbox() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("file_read", ToolType::FileRead))
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut temp_file = NamedTempFile::new_in(dir.path()).unwrap();
        let expected_content = "Hello from temp file!";
        temp_file.write_all(expected_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let sandbox = SandboxConfig::with_paths(vec![dir.path().to_path_buf()]);
        let ctx = test_ctx(sandbox);
        let path = temp_file.path().to_str().unwrap();
        let result = runtime.execute("file_read", path, &ctx).unwrap();

        assert_eq!(result.output, expected_content);
        assert!(result.success);
        assert_eq!(result.tool_name, "file_read");
    }

    #[test]
    fn file_read_sandbox_violation() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("file_read", ToolType::FileRead))
            .unwrap();

        // Sandbox erlaubt nur /tmp/allowed, Datei liegt in /tmp/other
        let allowed_dir = tempfile::tempdir().unwrap();
        let other_dir = tempfile::tempdir().unwrap();
        let other_file = other_dir.path().join("secret.txt");
        std::fs::write(&other_file, "secret").unwrap();

        let sandbox = SandboxConfig::with_paths(vec![allowed_dir.path().to_path_buf()]);
        let ctx = test_ctx(sandbox);

        let result = runtime.execute("file_read", other_file.to_str().unwrap(), &ctx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Sandbox violation"));
    }

    #[test]
    fn file_write_with_sandbox() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("file_write", ToolType::FileWrite))
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test_output.txt");
        let content = "Written by test!";
        let input = format!("{}\n{}", file_path.display(), content);

        let sandbox = SandboxConfig::with_paths(vec![dir.path().to_path_buf()]);
        let ctx = test_ctx(sandbox);
        let result = runtime.execute("file_write", &input, &ctx).unwrap();

        let read_back = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(read_back, content);
        assert!(result.output.contains("Written"));
    }

    #[test]
    fn file_write_sandbox_violation() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("file_write", ToolType::FileWrite))
            .unwrap();

        let allowed_dir = tempfile::tempdir().unwrap();
        let other_dir = tempfile::tempdir().unwrap();
        let other_file = other_dir.path().join("nope.txt");
        let input = format!("{}\ndata", other_file.display());

        let sandbox = SandboxConfig::with_paths(vec![allowed_dir.path().to_path_buf()]);
        let ctx = test_ctx(sandbox);

        let result = runtime.execute("file_write", &input, &ctx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Sandbox violation"));
    }

    #[test]
    fn unknown_tool_error() {
        let runtime = ToolRuntime::new();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let result = runtime.execute("nonexistent", "", &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn capability_check_denies_execution() {
        let mut runtime = ToolRuntime::new();
        let tool = ToolDefinition {
            name: "admin_tool".to_string(),
            description: "Needs admin cap".to_string(),
            wasm_path: None,
            tool_type: ToolType::FileRead,
            required_capabilities: vec!["admin".to_string()],
        };
        runtime.register_tool(tool).unwrap();

        let sandbox = SandboxConfig::restrictive();
        // Agent hat nur file_read/file_write, nicht admin
        let ctx = test_ctx(sandbox);
        let result = runtime.execute("admin_tool", "/dev/null", &ctx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("lacks required capabilities"));
    }

    #[test]
    fn tool_result_to_domain_event() {
        let result = ToolResult {
            tool_name: "file_read".to_string(),
            agent_id: "AGENT-01".to_string(),
            output: "file content".to_string(),
            success: true,
            duration_ms: 5,
        };
        let event = result.to_domain_event("corr-123", 42);
        assert_eq!(event.event_type, "tool_result");
        assert_eq!(event.aggregate_id, "AGENT-01");
        assert_eq!(event.correlation_id, "corr-123");
        assert_eq!(event.tick, 42);
        // Payload enthaelt den serialisierten ToolResult
        assert!(event.payload.contains("file_read"));
        assert!(event.payload.contains("AGENT-01"));
    }

    #[test]
    fn unimplemented_tool_type_error() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("chat", ToolType::Chat))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let result = runtime.execute("chat", "", &ctx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not yet implemented"));
    }

    #[test]
    fn wasm_without_feature_returns_error() {
        let mut runtime = ToolRuntime::new();
        let tool = ToolDefinition {
            name: "wasm_tool".to_string(),
            description: "A wasm tool".to_string(),
            wasm_path: Some("/fake/path.wasm".to_string()),
            tool_type: ToolType::Wasm,
            required_capabilities: Vec::new(),
        };
        runtime.register_tool(tool).unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let result = runtime.execute("wasm_tool", "", &ctx);

        // Ohne wasm-Feature: Fehler. Mit wasm-Feature: anderer Fehler (File not found).
        assert!(result.is_err());
    }
}
