use anyhow::{anyhow, Result};
use std::collections::HashMap;

/// Kategorien verfuegbarer Tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolType {
    FileRead,
    FileWrite,
    Chat,
    Calendar,
    Search,
}

/// Definition eines einzelnen Tools.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub wasm_path: Option<String>,
    pub tool_type: ToolType,
}

/// Registry und Executor fuer Agent-Tools.
pub struct ToolRuntime {
    tools: HashMap<String, ToolDefinition>,
}

impl ToolRuntime {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
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

    /// Fuehrt ein Tool aus. Native Handler fuer FileRead/FileWrite, Placeholder fuer Rest.
    pub fn execute(&self, name: &str, input: &str) -> Result<String> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| anyhow!("Tool '{}' not found", name))?;

        match tool.tool_type {
            ToolType::FileRead => {
                // Liest die Datei am Pfad input.trim()
                let content = std::fs::read_to_string(input.trim())?;
                Ok(content)
            }
            ToolType::FileWrite => {
                // Input = "pfad\ninhalt" - split bei erstem \n
                let (path, content) = input
                    .split_once('\n')
                    .ok_or_else(|| anyhow!("FileWrite input must be 'path\\ncontent'"))?;
                std::fs::write(path.trim(), content)?;
                Ok(format!(
                    "Written {} bytes to {}",
                    content.len(),
                    path.trim()
                ))
            }
            _ => {
                // Chat, Calendar, Search = Placeholder
                Ok(format!("Tool not yet implemented: {}", tool.name))
            }
        }
    }

    /// Anzahl registrierter Tools.
    pub fn tool_count(&self) -> usize {
        self.tools.len()
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

    #[test]
    fn register_and_list() {
        let mut runtime = ToolRuntime::new();

        let tool1 = ToolDefinition {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            wasm_path: None,
            tool_type: ToolType::FileRead,
        };

        let tool2 = ToolDefinition {
            name: "write_file".to_string(),
            description: "Write a file".to_string(),
            wasm_path: None,
            tool_type: ToolType::FileWrite,
        };

        runtime.register_tool(tool1).unwrap();
        runtime.register_tool(tool2).unwrap();

        assert_eq!(runtime.list_tools().len(), 2);
        assert_eq!(runtime.tool_count(), 2);
    }

    #[test]
    fn get_by_name() {
        let mut runtime = ToolRuntime::new();

        let tool = ToolDefinition {
            name: "my_tool".to_string(),
            description: "Test tool".to_string(),
            wasm_path: None,
            tool_type: ToolType::Chat,
        };

        runtime.register_tool(tool).unwrap();

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
    fn file_read() {
        let mut runtime = ToolRuntime::new();

        let tool = ToolDefinition {
            name: "file_read".to_string(),
            description: "Read files".to_string(),
            wasm_path: None,
            tool_type: ToolType::FileRead,
        };

        runtime.register_tool(tool).unwrap();

        // Erstelle eine Temp-Datei mit Inhalt
        let mut temp_file = NamedTempFile::new().unwrap();
        let expected_content = "Hello from temp file!";
        temp_file.write_all(expected_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let path = temp_file.path().to_str().unwrap();
        let result = runtime.execute("file_read", path).unwrap();

        assert_eq!(result, expected_content);
    }

    #[test]
    fn file_write() {
        let mut runtime = ToolRuntime::new();

        let tool = ToolDefinition {
            name: "file_write".to_string(),
            description: "Write files".to_string(),
            wasm_path: None,
            tool_type: ToolType::FileWrite,
        };

        runtime.register_tool(tool).unwrap();

        // Erstelle einen temp file path (aber nicht die Datei)
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_str().unwrap().to_string();

        let content = "Test content written by execute!";
        let input = format!("{}\n{}", path, content);

        let result = runtime.execute("file_write", &input).unwrap();

        // Verifiziere dass die Datei geschrieben wurde
        let read_content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read_content, content);

        // Verifiziere Response-String
        assert!(result.contains(&format!("Written {} bytes", content.len())));
        assert!(result.contains(&path));
    }
}
