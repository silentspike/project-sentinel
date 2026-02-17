//! Capability-basierte Tool-Zugriffskontrolle.
//!
//! Agents haben Capabilities (z.B. `["file_read", "file_write"]`).
//! Tools deklarieren `required_capabilities`.
//! Ein Agent darf ein Tool nur nutzen wenn er ALLE required Capabilities hat.

use crate::ToolDefinition;

/// Prueft ob ein Agent die benoetigten Capabilities fuer ein Tool hat.
///
/// Gibt `true` zurueck wenn der Agent ALLE required Capabilities des Tools besitzt.
/// Ein Tool ohne required Capabilities ist fuer jeden Agent zugaenglich.
pub fn can_execute(agent_capabilities: &[String], tool: &ToolDefinition) -> bool {
    tool.required_capabilities
        .iter()
        .all(|required| agent_capabilities.iter().any(|cap| cap == required))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolType;

    fn make_tool(name: &str, capabilities: Vec<&str>) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: format!("Test tool {name}"),
            wasm_path: None,
            tool_type: ToolType::FileRead,
            required_capabilities: capabilities.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn no_capabilities_required_allows_all() {
        let tool = make_tool("public_tool", vec![]);
        assert!(can_execute(&[], &tool));
        assert!(can_execute(&["anything".into()], &tool));
    }

    #[test]
    fn matching_capability_allows() {
        let tool = make_tool("reader", vec!["file_read"]);
        let caps = vec!["file_read".to_string(), "file_write".to_string()];
        assert!(can_execute(&caps, &tool));
    }

    #[test]
    fn missing_capability_denies() {
        let tool = make_tool("writer", vec!["file_write"]);
        let caps = vec!["file_read".to_string()];
        assert!(!can_execute(&caps, &tool));
    }

    #[test]
    fn all_capabilities_must_match() {
        let tool = make_tool("admin_tool", vec!["file_read", "file_write", "admin"]);
        let partial_caps = vec!["file_read".to_string(), "file_write".to_string()];
        assert!(!can_execute(&partial_caps, &tool));

        let full_caps = vec![
            "file_read".to_string(),
            "file_write".to_string(),
            "admin".to_string(),
        ];
        assert!(can_execute(&full_caps, &tool));
    }

    #[test]
    fn empty_agent_capabilities_denied_if_tool_requires() {
        let tool = make_tool("restricted", vec!["special"]);
        assert!(!can_execute(&[], &tool));
    }
}
