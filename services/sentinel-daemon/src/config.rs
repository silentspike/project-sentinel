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

    /// Zenoh SHM Core-Bus Konfiguration.
    #[serde(default)]
    pub zenoh: ZenohConfig,

    /// NATS-Konfiguration fuer Judge-Alert-Consumption.
    #[serde(default)]
    pub nats: NatsConfig,

    /// Lokale Operator-API fuer manuelle Chaos-Trigger aus dem Dashboard.
    #[serde(default)]
    pub operator_api: OperatorApiConfig,

    /// PSI-basierte adaptive Tick-Rate (TOGAF Adaptive Scheduling).
    #[serde(default)]
    pub adaptive: AdaptiveConfig,

    /// sentinel-fs FUSE Mountpoint (default: None = kein FUSE, nutzt /ram/agents/).
    /// Wenn gesetzt: FUSE-Mount wird beim Start initialisiert, bwrap bindet
    /// `{fs_mount}/{agent_name}` → `/home/{agent_name}`.
    #[serde(default)]
    pub fs_mount: Option<String>,

    /// Time Machine: Tiered Snapshot + Event Retention.
    #[serde(default)]
    pub retention: RetentionConfig,

    /// Smart Resource Management: Dynamische cgroup-Limits pro Agent.
    #[serde(default)]
    pub resource_manager: ResourceManagerConfig,

    /// Platform-Controlplane: Self-Healing Background Service.
    #[serde(default)]
    pub platform_controlplane: PlatformControlplaneConfig,
}

/// Konfiguration fuer den Resource Manager (dynamische cgroup-Limits).
#[derive(Debug, Deserialize, Clone)]
pub struct ResourceManagerConfig {
    /// Feature-Gate: false = statische Limits wie bisher.
    #[serde(default = "default_rm_enabled")]
    pub enabled: bool,

    /// Profil-Check Intervall in Ticks (default: 30).
    #[serde(default = "default_rm_check_interval")]
    pub check_interval_ticks: u64,

    /// Ticks ohne Aktivitaet bis ein Agent als Idle gilt (default: 300 = 5 Min).
    #[serde(default = "default_rm_idle_threshold")]
    pub idle_threshold_ticks: u64,

    /// Max Agents gleichzeitig im Heavy-Profil (default: 3).
    #[serde(default = "default_rm_max_heavy")]
    pub max_heavy: usize,

    /// Mindest-Zyklen im neuen Profil bevor Transition (Hysterese, default: 3).
    #[serde(default = "default_rm_min_transition")]
    pub min_transition_cycles: u32,
}

/// Konfiguration fuer World Snapshots und Event Retention (Time Machine).
#[derive(Debug, Deserialize, Clone)]
pub struct RetentionConfig {
    /// Intervall in Ticks fuer Hourly Snapshots (default: 3600 = 1h bei 1s Tick).
    #[serde(default = "default_hourly_interval")]
    pub hourly_interval_ticks: u64,

    /// Wie viele Stunden Hourly Snapshots behalten (default: 24).
    #[serde(default = "default_daily_keep")]
    pub daily_keep_hours: u32,

    /// Wie viele Tage Daily Snapshots behalten (default: 7).
    #[serde(default = "default_weekly_keep")]
    pub weekly_keep_days: u32,

    /// Wie viele Wochen Weekly Snapshots behalten (default: 4).
    #[serde(default = "default_monthly_keep")]
    pub monthly_keep_weeks: u32,

    /// Wie viele Stunden Live-Events behalten bevor Pruning (default: 2, 1h Puffer).
    #[serde(default = "default_event_retention")]
    pub event_retention_hours: u32,

    /// Automatisches Pruning nach Snapshot-Erstellung.
    #[serde(default = "default_true")]
    pub auto_prune: bool,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            hourly_interval_ticks: 3600,
            daily_keep_hours: 24,
            weekly_keep_days: 7,
            monthly_keep_weeks: 4,
            event_retention_hours: 2,
            auto_prune: true,
        }
    }
}

fn default_hourly_interval() -> u64 {
    3600
}
fn default_daily_keep() -> u32 {
    24
}
fn default_weekly_keep() -> u32 {
    7
}
fn default_monthly_keep() -> u32 {
    4
}
fn default_event_retention() -> u32 {
    2
}

/// Lokale Loopback-API fuer manuelle Operator-Eingriffe.
#[derive(Debug, Deserialize, Clone)]
pub struct OperatorApiConfig {
    /// API aktivieren.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Bind-Adresse fuer den lokalen HTTP-Listener.
    #[serde(default = "default_operator_bind_addr")]
    pub bind_addr: String,
    /// Optionales Shared Secret fuer Dashboard-Proxy -> Daemon.
    #[serde(default)]
    pub shared_secret: Option<String>,
}

impl Default for OperatorApiConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind_addr: default_operator_bind_addr(),
            shared_secret: None,
        }
    }
}

/// Zenoh SHM Core-Bus Konfiguration.
///
/// TOML-Werte dienen als Defaults. ENV-Variablen ueberschreiben TOML-Werte
/// (via `BusConfig::from_env()` Fallback). Reihenfolge: ENV > TOML > Hardcoded Default.
#[derive(Debug, Deserialize)]
pub struct ZenohConfig {
    /// SHM Transport aktivieren (default: false).
    #[serde(default)]
    pub shm_enabled: bool,
    /// SHM Buffer-Groesse in Bytes (default: 1MB).
    #[serde(default = "default_shm_buffer_size")]
    pub shm_buffer_size_bytes: usize,
    /// Fan-Out Channel Kapazitaet (default: 256).
    #[serde(default = "default_fanout_capacity")]
    pub fanout_channel_capacity: usize,
    /// Query Responder aktivieren (default: true).
    #[serde(default = "default_true")]
    pub query_responder_enabled: bool,
    /// Query Deadline in Millisekunden (default: 100).
    #[serde(default = "default_query_deadline")]
    pub query_deadline_ms: u64,
    /// Max gleichzeitige Queries global (default: 128).
    #[serde(default = "default_max_inflight_global")]
    pub max_inflight_global: usize,
    /// Max gleichzeitige Queries pro Agent (default: 8).
    #[serde(default = "default_max_inflight_per_agent")]
    pub max_inflight_per_agent: usize,
}

impl Default for ZenohConfig {
    fn default() -> Self {
        Self {
            shm_enabled: false,
            shm_buffer_size_bytes: default_shm_buffer_size(),
            fanout_channel_capacity: default_fanout_capacity(),
            query_responder_enabled: true,
            query_deadline_ms: default_query_deadline(),
            max_inflight_global: default_max_inflight_global(),
            max_inflight_per_agent: default_max_inflight_per_agent(),
        }
    }
}

impl ZenohConfig {
    /// Konvertiert in `BusConfig` fuer sentinel-zenoh.
    /// ENV-Variablen ueberschreiben TOML-Werte.
    pub fn to_bus_config(&self) -> sentinel_zenoh::config::BusConfig {
        use sentinel_zenoh::config::BusConfig;
        let env = BusConfig::from_env();
        BusConfig {
            shm_enabled: std::env::var("SENTINEL_ZENOH_SHM")
                .ok()
                .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                .unwrap_or(self.shm_enabled),
            shm_p99_target_us: env.shm_p99_target_us,
            query_deadline_ms: env_or("SENTINEL_ZENOH_QUERY_DEADLINE_MS", self.query_deadline_ms),
            query_cancel_enabled: env.query_cancel_enabled,
            max_inflight_global: env_or_usize(
                "SENTINEL_ZENOH_MAX_INFLIGHT_GLOBAL",
                self.max_inflight_global,
            ),
            max_inflight_per_agent: env_or_usize(
                "SENTINEL_ZENOH_MAX_INFLIGHT_PER_AGENT",
                self.max_inflight_per_agent,
            ),
            shm_buffer_size_bytes: env_or_usize(
                "SENTINEL_ZENOH_SHM_BUFFER_SIZE",
                self.shm_buffer_size_bytes,
            ),
            fanout_channel_capacity: env_or_usize(
                "SENTINEL_ZENOH_FANOUT_CAPACITY",
                self.fanout_channel_capacity,
            ),
            query_responder_enabled: std::env::var("SENTINEL_ZENOH_QUERY_RESPONDER")
                .ok()
                .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                .unwrap_or(self.query_responder_enabled),
        }
    }
}

fn env_or(key: &str, toml_val: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(toml_val)
}

fn env_or_usize(key: &str, toml_val: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(toml_val)
}

fn default_shm_buffer_size() -> usize {
    1_048_576
}
fn default_fanout_capacity() -> usize {
    256
}
fn default_true() -> bool {
    true
}
fn default_query_deadline() -> u64 {
    100
}
fn default_max_inflight_global() -> usize {
    128
}
fn default_max_inflight_per_agent() -> usize {
    8
}

fn default_operator_bind_addr() -> String {
    "127.0.0.1:8084".to_string()
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

fn default_rm_enabled() -> bool {
    true
}
fn default_rm_check_interval() -> u64 {
    30
}
fn default_rm_idle_threshold() -> u64 {
    300
}
fn default_rm_max_heavy() -> usize {
    3
}
fn default_rm_min_transition() -> u32 {
    3
}

impl Default for ResourceManagerConfig {
    fn default() -> Self {
        Self {
            enabled: default_rm_enabled(),
            check_interval_ticks: default_rm_check_interval(),
            idle_threshold_ticks: default_rm_idle_threshold(),
            max_heavy: default_rm_max_heavy(),
            min_transition_cycles: default_rm_min_transition(),
        }
    }
}

/// Konfiguration fuer den Platform-Controlplane (Self-Healing).
#[derive(Debug, Deserialize, Clone)]
pub struct PlatformControlplaneConfig {
    #[serde(default = "default_pcp_enabled")]
    pub enabled: bool,
    #[serde(default = "default_pcp_cycle_interval")]
    pub cycle_interval_ticks: u64,
    #[serde(default = "default_pcp_stall_cooldown")]
    pub stall_cooldown_ticks: u64,
    #[serde(default = "default_pcp_prune_cooldown")]
    pub prune_cooldown_ticks: u64,
    #[serde(default = "default_pcp_max_event_store")]
    pub max_event_store_bytes: u64,
    #[serde(default = "default_pcp_max_projection_lag")]
    pub max_projection_lag: i64,
    #[serde(default = "default_pcp_memory_pressure")]
    pub memory_pressure_threshold: f64,
    #[serde(default = "default_pcp_max_escalation")]
    pub max_escalation: u32,
    #[serde(default = "default_pcp_write_anomaly_threshold")]
    pub write_anomaly_threshold_bytes_per_sec: u64,
    #[serde(default = "default_pcp_write_anomaly_cooldown")]
    pub write_anomaly_cooldown_ticks: u64,
    #[serde(default = "default_pcp_monitored_services")]
    pub monitored_services: Vec<String>,
    #[serde(default = "default_pcp_service_check_interval")]
    pub service_check_interval_secs: u64,
}

fn default_pcp_enabled() -> bool {
    true
}
fn default_pcp_cycle_interval() -> u64 {
    60
}
fn default_pcp_stall_cooldown() -> u64 {
    60
}
fn default_pcp_prune_cooldown() -> u64 {
    3600
}
fn default_pcp_max_event_store() -> u64 {
    500 * 1024 * 1024
}
fn default_pcp_max_projection_lag() -> i64 {
    10_000
}
fn default_pcp_memory_pressure() -> f64 {
    0.9
}
fn default_pcp_max_escalation() -> u32 {
    3
}
fn default_pcp_write_anomaly_threshold() -> u64 {
    5_000_000 // 5 MB/s
}
fn default_pcp_write_anomaly_cooldown() -> u64 {
    60
}
fn default_pcp_monitored_services() -> Vec<String> {
    vec![
        "sentinel-judge".into(),
        "sentinel-projection".into(),
        "sentinel-cortex".into(),
    ]
}
fn default_pcp_service_check_interval() -> u64 {
    60
}

impl Default for PlatformControlplaneConfig {
    fn default() -> Self {
        Self {
            enabled: default_pcp_enabled(),
            cycle_interval_ticks: default_pcp_cycle_interval(),
            stall_cooldown_ticks: default_pcp_stall_cooldown(),
            prune_cooldown_ticks: default_pcp_prune_cooldown(),
            max_event_store_bytes: default_pcp_max_event_store(),
            max_projection_lag: default_pcp_max_projection_lag(),
            memory_pressure_threshold: default_pcp_memory_pressure(),
            max_escalation: default_pcp_max_escalation(),
            write_anomaly_threshold_bytes_per_sec: default_pcp_write_anomaly_threshold(),
            write_anomaly_cooldown_ticks: default_pcp_write_anomaly_cooldown(),
            monitored_services: default_pcp_monitored_services(),
            service_check_interval_secs: default_pcp_service_check_interval(),
        }
    }
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

[daemon.operator_api]
enabled = true
bind_addr = "127.0.0.1:9999"
shared_secret = "secret"
"#;
        let file: DaemonConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.daemon.tick_rate_ms, 500);
        assert_eq!(file.daemon.max_agents, 10);
        assert_eq!(file.daemon.zenoh_prefix, "test");
        assert!(file.daemon.operator_api.enabled);
        assert_eq!(file.daemon.operator_api.bind_addr, "127.0.0.1:9999");
        assert_eq!(
            file.daemon.operator_api.shared_secret.as_deref(),
            Some("secret")
        );
    }

    #[test]
    fn test_defaults() {
        let toml_str = r#"
[daemon]
config_dir = "/tmp/cfg"
data_dir = "/tmp/data"
"#;
        let file: DaemonConfigFile = toml::from_str(toml_str).unwrap();
        assert!(file.daemon.operator_api.enabled);
        assert_eq!(file.daemon.operator_api.bind_addr, "127.0.0.1:8084");
        assert!(file.daemon.operator_api.shared_secret.is_none());
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

    #[test]
    fn test_zenoh_defaults() {
        let toml_str = r#"
[daemon]
config_dir = "/tmp/cfg"
data_dir = "/tmp/data"
"#;
        let file: DaemonConfigFile = toml::from_str(toml_str).unwrap();
        assert!(!file.daemon.zenoh.shm_enabled);
        assert_eq!(file.daemon.zenoh.shm_buffer_size_bytes, 1_048_576);
        assert_eq!(file.daemon.zenoh.fanout_channel_capacity, 256);
        assert!(file.daemon.zenoh.query_responder_enabled);
        assert_eq!(file.daemon.zenoh.query_deadline_ms, 100);
        assert_eq!(file.daemon.zenoh.max_inflight_global, 128);
        assert_eq!(file.daemon.zenoh.max_inflight_per_agent, 8);
    }

    #[test]
    fn test_zenoh_custom() {
        let toml_str = r#"
[daemon]
config_dir = "/tmp/cfg"
data_dir = "/tmp/data"

[daemon.zenoh]
shm_enabled = true
shm_buffer_size_bytes = 2097152
fanout_channel_capacity = 512
query_responder_enabled = false
query_deadline_ms = 200
max_inflight_global = 64
max_inflight_per_agent = 4
"#;
        let file: DaemonConfigFile = toml::from_str(toml_str).unwrap();
        assert!(file.daemon.zenoh.shm_enabled);
        assert_eq!(file.daemon.zenoh.shm_buffer_size_bytes, 2_097_152);
        assert_eq!(file.daemon.zenoh.fanout_channel_capacity, 512);
        assert!(!file.daemon.zenoh.query_responder_enabled);
        assert_eq!(file.daemon.zenoh.query_deadline_ms, 200);
        assert_eq!(file.daemon.zenoh.max_inflight_global, 64);
        assert_eq!(file.daemon.zenoh.max_inflight_per_agent, 4);
    }
}
