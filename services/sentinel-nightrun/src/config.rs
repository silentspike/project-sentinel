//! Nightrun-Konfiguration (TOML-basiert).

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct NightrunConfig {
    pub nightrun: NightrunSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NightrunSettings {
    /// Pfad zur Hippocampus redb-Datenbank.
    pub hippocampus_db: String,
    /// Pfad zur Limbo Event-Store SQLite-Datenbank.
    pub event_store_db: String,
    /// Verzeichnis mit Agent-TOML-Definitionen.
    pub agent_config_dir: String,
    /// Pfad zur Job-Queue SQLite-Datenbank.
    pub job_queue_path: String,
    /// Timeout pro Agent in Sekunden (default: 300).
    #[serde(default = "default_timeout_per_agent")]
    pub timeout_per_agent_secs: u64,
    /// Gesamt-Timeout in Sekunden (default: 7200).
    #[serde(default = "default_timeout_total")]
    pub timeout_total_secs: u64,
    /// Max Episodes pro Agent bevor Skip (default: 1000).
    #[serde(default = "default_max_episodes")]
    pub max_episodes_per_agent: usize,
}

fn default_timeout_per_agent() -> u64 {
    300
}
fn default_timeout_total() -> u64 {
    7200
}
fn default_max_episodes() -> usize {
    1000
}

impl NightrunConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config: {}", path.display()))?;
        let config: NightrunConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config: {}", path.display()))?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_config() {
        let toml_str = r#"
[nightrun]
hippocampus_db = "data/hippocampus.redb"
event_store_db = "data/events.db"
agent_config_dir = "config/agents"
job_queue_path = "data/nightrun-jobs.db"
timeout_per_agent_secs = 120
timeout_total_secs = 3600
max_episodes_per_agent = 500
"#;
        let config: NightrunConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.nightrun.hippocampus_db, "data/hippocampus.redb");
        assert_eq!(config.nightrun.timeout_per_agent_secs, 120);
        assert_eq!(config.nightrun.max_episodes_per_agent, 500);
    }

    #[test]
    fn parse_config_with_defaults() {
        let toml_str = r#"
[nightrun]
hippocampus_db = "data/hippocampus.redb"
event_store_db = "data/events.db"
agent_config_dir = "config/agents"
job_queue_path = "data/nightrun-jobs.db"
"#;
        let config: NightrunConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.nightrun.timeout_per_agent_secs, 300);
        assert_eq!(config.nightrun.timeout_total_secs, 7200);
        assert_eq!(config.nightrun.max_episodes_per_agent, 1000);
    }
}
