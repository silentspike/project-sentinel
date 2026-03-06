//! TOML-basierte Daemon-Konfiguration.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::adaptive_tick::AdaptiveConfig;

/// Top-level Config-Wrapper (TOML hat `[daemon]` Section).
#[derive(Debug, Deserialize)]
pub struct DaemonConfigFile {
    pub daemon: DaemonConfig,
}

/// Daemon-Konfiguration.
#[derive(Debug, Deserialize)]
pub struct DaemonConfig {
    /// Verzeichnis mit Agent-TOMLs und rooms.toml.
    pub config_dir: PathBuf,

    /// Verzeichnis fuer redb + limbo Datenbanken.
    pub data_dir: PathBuf,

    /// ECS Tick-Intervall in Millisekunden (default: 1000).
    #[serde(default = "default_tick_rate")]
    pub tick_rate_ms: u64,

    /// Maximale Anzahl gleichzeitiger Agents (default: 30, mind. 24 fuer 15 Schicht + 9 Sonder).
    #[serde(default = "default_max_agents")]
    pub max_agents: usize,

    /// Zenoh Key-Space Prefix.
    #[serde(default = "default_zenoh_prefix")]
    pub zenoh_prefix: String,

    /// Simulations-Zeitskala (default: 1.0 = Echtzeit).
    /// 60.0 = 1 Sim-Minute pro Echtzeit-Sekunde, 0.5 = halbe Geschwindigkeit.
    #[serde(default = "default_time_scale")]
    pub time_scale: f32,

    /// Command das im bwrap-Sandbox pro Agent ausgefuehrt wird.
    /// Default: agent-runtime (TOGAF: leichtgewichtiger Sandbox-Prozess).
    #[serde(default = "default_agent_command")]
    pub agent_command: Vec<String>,

    /// NATS-Konfiguration fuer Judge-Alert-Consumption.
    #[serde(default)]
    pub nats: NatsConfig,

    /// PSI-basierte adaptive Tick-Rate (TOGAF Adaptive Scheduling).
    #[serde(default)]
    pub adaptive: AdaptiveConfig,
}

/// NATS JetStream Konfiguration fuer den Daemon.
#[derive(Debug, Deserialize)]
pub struct NatsConfig {
    /// NATS server URL (default: nats://127.0.0.1:4222).
    #[serde(default = "default_nats_url")]
    pub url: String,
}

impl Default for NatsConfig {
    fn default() -> Self {
        Self {
            url: default_nats_url(),
        }
    }
}

fn default_nats_url() -> String {
    "nats://127.0.0.1:4222".to_string()
}

fn default_tick_rate() -> u64 {
    1000
}

fn default_max_agents() -> usize {
    30
}

fn default_time_scale() -> f32 {
    1.0
}

fn default_agent_command() -> Vec<String> {
    // TOGAF: /usr/bin/agent-runtime (leichtgewichtiger Sandbox-Prozess)
    // LLM-Calls gehen NICHT ueber diesen Prozess, sondern via Cortex Gateway.
    vec!["/usr/bin/agent-runtime".to_string()]
}

fn default_zenoh_prefix() -> String {
    "sentinel".to_string()
}

impl DaemonConfig {
    /// Laedt Config aus einer TOML-Datei.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Config lesen: {}", path.display()))?;
        let file: DaemonConfigFile = toml::from_str(&content)
            .with_context(|| format!("Config parsen: {}", path.display()))?;
        Ok(file.daemon)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_config() {
        let toml_str = r#"
[daemon]
config_dir = "/opt/sentinel/config"
data_dir = "/opt/sentinel/data"
tick_rate_ms = 500
max_agents = 10
zenoh_prefix = "test"
"#;
        let file: DaemonConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.daemon.tick_rate_ms, 500);
        assert_eq!(file.daemon.max_agents, 10);
        assert_eq!(file.daemon.zenoh_prefix, "test");
    }

    #[test]
    fn test_defaults() {
        let toml_str = r#"
[daemon]
config_dir = "/tmp/cfg"
data_dir = "/tmp/data"
"#;
        let file: DaemonConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.daemon.tick_rate_ms, 1000);
        assert_eq!(file.daemon.max_agents, 30);
        assert_eq!(file.daemon.zenoh_prefix, "sentinel");
        assert_eq!(file.daemon.time_scale, 1.0);
        assert_eq!(file.daemon.agent_command, vec!["/usr/bin/agent-runtime"]);
    }

    #[test]
    fn test_time_scale_custom() {
        let toml_str = r#"
[daemon]
config_dir = "/tmp/cfg"
data_dir = "/tmp/data"
time_scale = 60.0
"#;
        let file: DaemonConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.daemon.time_scale, 60.0);
    }

    #[test]
    fn test_agent_command_custom() {
        let toml_str = r#"
[daemon]
config_dir = "/tmp/cfg"
data_dir = "/tmp/data"
agent_command = ["sleep", "infinity"]
"#;
        let file: DaemonConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.daemon.agent_command, vec!["sleep", "infinity"]);
    }

    #[test]
    fn test_adaptive_defaults() {
        let toml_str = r#"
[daemon]
config_dir = "/tmp/cfg"
data_dir = "/tmp/data"
"#;
        let file: DaemonConfigFile = toml::from_str(toml_str).unwrap();
        assert!(file.daemon.adaptive.enabled);
        assert_eq!(file.daemon.adaptive.cpu_threshold, 85.0);
        assert_eq!(file.daemon.adaptive.mem_threshold, 80.0);
        assert_eq!(file.daemon.adaptive.io_threshold, 70.0);
        assert_eq!(file.daemon.adaptive.min_tick_rate_ms, 2000);
        assert_eq!(file.daemon.adaptive.psi_sample_interval, 10);
    }

    #[test]
    fn test_adaptive_custom() {
        let toml_str = r#"
[daemon]
config_dir = "/tmp/cfg"
data_dir = "/tmp/data"

[daemon.adaptive]
enabled = false
cpu_threshold = 50.0
mem_threshold = 60.0
io_threshold = 40.0
min_tick_rate_ms = 3000
psi_sample_interval = 5
"#;
        let file: DaemonConfigFile = toml::from_str(toml_str).unwrap();
        assert!(!file.daemon.adaptive.enabled);
        assert_eq!(file.daemon.adaptive.cpu_threshold, 50.0);
        assert_eq!(file.daemon.adaptive.mem_threshold, 60.0);
        assert_eq!(file.daemon.adaptive.io_threshold, 40.0);
        assert_eq!(file.daemon.adaptive.min_tick_rate_ms, 3000);
        assert_eq!(file.daemon.adaptive.psi_sample_interval, 5);
    }
}
