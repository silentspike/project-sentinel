//! Tool runtime for agent capabilities.
//!
//! Provides a registry for tool definitions and sandboxed execution.
//! Native handlers for FileRead/FileWrite with filesystem restrictions.
//! WASM module execution via wasmtime (behind `wasm` feature flag).

use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};

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

fn tool_exceeds_wall_clock_limit(_: ToolType, duration: Duration, limit: Duration) -> bool {
    duration > limit
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
    /// ECS agent snapshot for WASM Component Model plugins.
    #[cfg(feature = "wasm")]
    pub agent_snapshot: Option<crate::host::AgentSnapshot>,
    /// Room data for WASM Component Model plugins.
    #[cfg(feature = "wasm")]
    pub rooms: Option<std::collections::HashMap<String, crate::host::RoomSnapshot>>,
}

/// Registry und Executor fuer Agent-Tools.
pub struct ToolRuntime {
    tools: HashMap<String, ToolDefinition>,
    #[cfg(feature = "wasm")]
    plugin_host: crate::plugin::PluginHost,
}

impl ToolRuntime {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            #[cfg(feature = "wasm")]
            plugin_host: crate::plugin::PluginHost::new().expect("Failed to create PluginHost"),
        }
    }

    /// Returns a mutable reference to the PluginHost (for loading plugins).
    #[cfg(feature = "wasm")]
    pub fn plugin_host_mut(&mut self) -> &mut crate::plugin::PluginHost {
        &mut self.plugin_host
    }

    /// Returns a reference to the PluginHost.
    #[cfg(feature = "wasm")]
    pub fn plugin_host(&self) -> &crate::plugin::PluginHost {
        &self.plugin_host
    }

    /// Registriert ein neues Tool. Fehler bei Duplikat.
    pub fn register_tool(&mut self, definition: ToolDefinition) -> Result<()> {
        if self.tools.contains_key(&definition.name) {
            return Err(anyhow!("Tool '{}' already registered", definition.name));
        }
        self.tools.insert(definition.name.clone(), definition);
        Ok(())
    }

    /// Removes a tool definition after its last owning workload stops.
    pub fn unregister_tool(&mut self, name: &str) -> bool {
        self.tools.remove(name).is_some()
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
                    self.execute_component(tool, input, ctx)?
                }
                #[cfg(not(feature = "wasm"))]
                {
                    return Err(anyhow!(
                        "Wasm support not enabled. Compile with --features wasm"
                    ));
                }
            }
            ToolType::Chat => self.execute_chat(input, &ctx.agent_id)?,
            ToolType::Calendar => self.execute_calendar(input, &ctx.agent_id)?,
            ToolType::Search => self.execute_search(input)?,
        };

        let duration = start.elapsed();

        // Fuel bounds executed WASM instructions, but synchronous WASI/host
        // calls may block without consuming fuel. Keep the wall-clock fence
        // mode-independent so every tool result is rejected after its deadline.
        if tool_exceeds_wall_clock_limit(tool.tool_type, duration, ctx.sandbox.max_execution_time) {
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

    /// Sendet eine Nachricht an einen anderen Agenten.
    ///
    /// Input: JSON `{"target":"AGENT-XX","message":"text"}`
    /// Output: Bestaetigungs-String mit Absender, Empfaenger und Nachricht.
    /// Die tatsaechliche Zustellung erfolgt durch den Orchestrator.
    fn execute_chat(&self, input: &str, sender_id: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct ChatInput {
            target: String,
            message: String,
        }

        let parsed: ChatInput = serde_json::from_str(input.trim()).map_err(|e| {
            anyhow!("Chat input must be JSON {{\"target\":\"AGENT-XX\",\"message\":\"text\"}}: {e}")
        })?;

        if parsed.target.is_empty() {
            return Err(anyhow!("Chat target must not be empty"));
        }
        if parsed.message.is_empty() {
            return Err(anyhow!("Chat message must not be empty"));
        }
        // Agent-ID Format validieren (AGENT-XX)
        if !parsed.target.starts_with("AGENT-") {
            return Err(anyhow!(
                "Chat target must be a valid agent ID (AGENT-XX), got '{}'",
                parsed.target
            ));
        }

        Ok(format!(
            "Message from {} to {}: {}",
            sender_id, parsed.target, parsed.message
        ))
    }

    /// Verwaltet Kalendereintraege fuer einen Agenten.
    ///
    /// Input: JSON `{"action":"query"|"create"|"cancel","date":"YYYY-MM-DD","time":"HH:MM","subject":"...","attendees":["AGENT-XX"]}`
    /// Output: Bestaetigungs-String oder Abfrageergebnis.
    /// Die tatsaechliche Kalender-Persistenz erfolgt durch den Orchestrator.
    fn execute_calendar(&self, input: &str, agent_id: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct CalendarInput {
            action: String,
            #[serde(default)]
            date: String,
            #[serde(default)]
            time: String,
            #[serde(default)]
            subject: String,
            #[serde(default)]
            attendees: Vec<String>,
        }

        let parsed: CalendarInput = serde_json::from_str(input.trim())
            .map_err(|e| anyhow!("Calendar input must be JSON {{\"action\":\"create\",\"date\":\"...\",\"subject\":\"...\"}}: {e}"))?;

        match parsed.action.as_str() {
            "create" => {
                if parsed.date.is_empty() || parsed.subject.is_empty() {
                    return Err(anyhow!("Calendar create requires 'date' and 'subject'"));
                }
                let time_str = if parsed.time.is_empty() {
                    String::new()
                } else {
                    format!(" {}", parsed.time)
                };
                let attendees_str = if parsed.attendees.is_empty() {
                    String::new()
                } else {
                    format!(" with {}", parsed.attendees.join(", "))
                };
                Ok(format!(
                    "Calendar entry created for {}: {}{} - {}{}",
                    agent_id, parsed.date, time_str, parsed.subject, attendees_str
                ))
            }
            "query" => {
                let scope = if parsed.date.is_empty() {
                    "today".to_string()
                } else {
                    parsed.date.clone()
                };
                Ok(format!(
                    "Calendar query for {}: showing entries for {}",
                    agent_id, scope
                ))
            }
            "cancel" => {
                if parsed.subject.is_empty() && parsed.date.is_empty() {
                    return Err(anyhow!("Calendar cancel requires 'subject' or 'date'"));
                }
                let identifier = if !parsed.subject.is_empty() {
                    &parsed.subject
                } else {
                    &parsed.date
                };
                Ok(format!(
                    "Calendar entry cancelled for {}: {}",
                    agent_id, identifier
                ))
            }
            other => Err(anyhow!(
                "Unknown calendar action '{}'. Expected 'create', 'query', or 'cancel'",
                other
            )),
        }
    }

    /// Durchsucht Dokumente und Wissen innerhalb der Simulation.
    ///
    /// Input: JSON `{"query":"search terms","scope":"documents"|"agents"|"rooms"}`
    /// Output: Suchergebnis-String. Die tatsaechliche Suche wird vom Orchestrator
    /// mit Zugriff auf den ECS-World-State durchgefuehrt.
    fn execute_search(&self, input: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct SearchInput {
            query: String,
            #[serde(default = "default_search_scope")]
            scope: String,
        }

        fn default_search_scope() -> String {
            "documents".to_string()
        }

        let parsed: SearchInput = serde_json::from_str(input.trim()).map_err(|e| {
            anyhow!("Search input must be JSON {{\"query\":\"...\",\"scope\":\"documents\"}}: {e}")
        })?;

        if parsed.query.is_empty() {
            return Err(anyhow!("Search query must not be empty"));
        }

        let valid_scopes = ["documents", "agents", "rooms"];
        if !valid_scopes.contains(&parsed.scope.as_str()) {
            return Err(anyhow!(
                "Search scope must be one of {:?}, got '{}'",
                valid_scopes,
                parsed.scope
            ));
        }

        Ok(format!(
            "Search results for '{}' in {}: query dispatched",
            parsed.query, parsed.scope
        ))
    }

    /// Fuehrt ein WASM Component Model Plugin aus.
    ///
    /// Nutzt fuel-Mechanismus fuer deterministische CPU-Begrenzung.
    /// Plugin muss `execute(input: string) -> result<string, string>` exportieren.
    /// Input wird an das Plugin weitergegeben, Output kommt aus Plugin-Return.
    #[cfg(feature = "wasm")]
    fn execute_component(
        &self,
        tool: &ToolDefinition,
        input: &str,
        ctx: &ExecutionContext,
    ) -> Result<String> {
        let wasm_path_str = tool
            .wasm_path
            .as_ref()
            .ok_or_else(|| anyhow!("Wasm tool '{}' has no wasm_path", tool.name))?;
        let wasm_path = std::path::PathBuf::from(wasm_path_str);

        if !self.plugin_host.is_loaded(&wasm_path) {
            return Err(anyhow!(
                "Plugin '{}' not loaded. Call plugin_host.load() first.",
                tool.name
            ));
        }

        // Build agent snapshot from context.
        let agent_snapshot = ctx.agent_snapshot.clone().unwrap_or_default();

        let rooms = ctx.rooms.clone().unwrap_or_default();

        // Agent home: use first allowed path or a temp directory.
        let agent_home = ctx
            .sandbox
            .allowed_paths
            .first()
            .cloned()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));

        match self.plugin_host.execute(
            &wasm_path,
            input,
            agent_snapshot,
            rooms,
            ctx.tick,
            agent_home,
        )? {
            Ok(output) => Ok(output),
            Err(plugin_err) => Err(anyhow!(
                "Plugin '{}' returned error: {}",
                tool.name,
                plugin_err
            )),
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
            #[cfg(feature = "wasm")]
            agent_snapshot: None,
            #[cfg(feature = "wasm")]
            rooms: None,
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
    fn native_duration_above_wall_limit_is_rejected() {
        assert!(tool_exceeds_wall_clock_limit(
            ToolType::FileRead,
            Duration::from_millis(501),
            Duration::from_millis(500),
        ));
    }

    #[test]
    fn wasm_duration_above_wall_limit_is_rejected() {
        assert!(tool_exceeds_wall_clock_limit(
            ToolType::Wasm,
            Duration::from_secs(2),
            Duration::from_millis(500),
        ));
    }

    // ---- Chat Tool Tests ----

    #[test]
    fn chat_sends_message() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("chat", ToolType::Chat))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let input = r#"{"target":"AGENT-05","message":"Hallo Lisa!"}"#;
        let result = runtime.execute("chat", input, &ctx).unwrap();
        assert!(result.success);
        assert!(result.output.contains("AGENT-01"));
        assert!(result.output.contains("AGENT-05"));
        assert!(result.output.contains("Hallo Lisa!"));
    }

    #[test]
    fn chat_invalid_json_error() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("chat", ToolType::Chat))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let result = runtime.execute("chat", "not json", &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Chat input"));
    }

    #[test]
    fn chat_empty_target_error() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("chat", ToolType::Chat))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let input = r#"{"target":"","message":"hi"}"#;
        let result = runtime.execute("chat", input, &ctx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must not be empty"));
    }

    #[test]
    fn chat_invalid_agent_id_error() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("chat", ToolType::Chat))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let input = r#"{"target":"Lisa","message":"hi"}"#;
        let result = runtime.execute("chat", input, &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("AGENT-XX"));
    }

    // ---- Calendar Tool Tests ----

    #[test]
    fn calendar_create_entry() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("calendar", ToolType::Calendar))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let input = r#"{"action":"create","date":"2026-02-22","time":"14:00","subject":"Sprint Review","attendees":["AGENT-05","AGENT-10"]}"#;
        let result = runtime.execute("calendar", input, &ctx).unwrap();
        assert!(result.success);
        assert!(result.output.contains("Calendar entry created"));
        assert!(result.output.contains("2026-02-22"));
        assert!(result.output.contains("14:00"));
        assert!(result.output.contains("Sprint Review"));
        assert!(result.output.contains("AGENT-05"));
    }

    #[test]
    fn calendar_query() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("calendar", ToolType::Calendar))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let input = r#"{"action":"query","date":"2026-02-22"}"#;
        let result = runtime.execute("calendar", input, &ctx).unwrap();
        assert!(result.success);
        assert!(result.output.contains("Calendar query"));
        assert!(result.output.contains("2026-02-22"));
    }

    #[test]
    fn calendar_query_default_today() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("calendar", ToolType::Calendar))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let input = r#"{"action":"query"}"#;
        let result = runtime.execute("calendar", input, &ctx).unwrap();
        assert!(result.output.contains("today"));
    }

    #[test]
    fn calendar_cancel() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("calendar", ToolType::Calendar))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let input = r#"{"action":"cancel","subject":"Sprint Review"}"#;
        let result = runtime.execute("calendar", input, &ctx).unwrap();
        assert!(result.success);
        assert!(result.output.contains("cancelled"));
        assert!(result.output.contains("Sprint Review"));
    }

    #[test]
    fn calendar_create_missing_fields_error() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("calendar", ToolType::Calendar))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let input = r#"{"action":"create"}"#;
        let result = runtime.execute("calendar", input, &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("requires"));
    }

    #[test]
    fn calendar_unknown_action_error() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("calendar", ToolType::Calendar))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let input = r#"{"action":"delete"}"#;
        let result = runtime.execute("calendar", input, &ctx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown calendar action"));
    }

    // ---- Search Tool Tests ----

    #[test]
    fn search_documents() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("search", ToolType::Search))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let input = r#"{"query":"Projektplan Q1","scope":"documents"}"#;
        let result = runtime.execute("search", input, &ctx).unwrap();
        assert!(result.success);
        assert!(result.output.contains("Projektplan Q1"));
        assert!(result.output.contains("documents"));
    }

    #[test]
    fn search_default_scope() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("search", ToolType::Search))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let input = r#"{"query":"meeting notes"}"#;
        let result = runtime.execute("search", input, &ctx).unwrap();
        assert!(result.output.contains("documents"));
    }

    #[test]
    fn search_agents_scope() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("search", ToolType::Search))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let input = r#"{"query":"Lisa","scope":"agents"}"#;
        let result = runtime.execute("search", input, &ctx).unwrap();
        assert!(result.output.contains("agents"));
    }

    #[test]
    fn search_invalid_scope_error() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("search", ToolType::Search))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let input = r#"{"query":"test","scope":"internet"}"#;
        let result = runtime.execute("search", input, &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be one of"));
    }

    #[test]
    fn search_empty_query_error() {
        let mut runtime = ToolRuntime::new();
        runtime
            .register_tool(make_tool("search", ToolType::Search))
            .unwrap();
        let sandbox = SandboxConfig::restrictive();
        let ctx = test_ctx(sandbox);
        let input = r#"{"query":""}"#;
        let result = runtime.execute("search", input, &ctx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must not be empty"));
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
