//! TOML-basierte Daemon-Konfiguration.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use sentinel_common::agent_config::AgentConfigValidation;
use serde::Deserialize;

use crate::adaptive_tick::AdaptiveConfig;

pub const OPERATOR_CREDENTIAL_FILE_ENV: &str = "SENTINEL_OPERATOR_CREDENTIAL_FILE";
pub const CREDENTIALS_DIRECTORY_ENV: &str = "CREDENTIALS_DIRECTORY";
const CREDENTIAL_MIN_BYTES: usize = 32;
const CREDENTIAL_MAX_BYTES: usize = 4096;
const O_DIRECTORY: i32 = 0o200000;
const O_NONBLOCK: i32 = 0o4000;
const O_NOFOLLOW: i32 = 0o400000;
const OPERATOR_CREDENTIAL_NAME: &str = "operator-api";

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

    /// Per-Phase-Timing (#381): Dauer-Histogramme pro `SimulationPhase`
    /// (sentinel_phase_duration_ms auf :9090). Telemetrie-only, Budget
    /// < 0,1% der Tick-Zeit (default: true).
    #[serde(default = "default_phase_timing_enabled")]
    pub phase_timing_enabled: bool,

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

    /// Optional one-time, operator-authenticated EpisodeProducer legacy cutover.
    #[serde(default)]
    pub episode_projection_cutover: Option<EpisodeProjectionCutoverConfig>,

    /// Prometheus-eBPF Metrics-HTTP-Server (#525: loopback secure default).
    #[serde(default)]
    pub metrics: MetricsConfig,

    /// PSI-basierte adaptive Tick-Rate (TOGAF Adaptive Scheduling).
    #[serde(default)]
    pub adaptive: AdaptiveConfig,

    /// sentinel-fs FUSE Mountpoint (default: None = kein FUSE, nutzt /ram/agents/).
    /// Wenn gesetzt: FUSE-Mount wird beim Start initialisiert, bwrap bindet
    /// `{fs_mount}/{agent_name}` → `/home/{agent_name}`.
    #[serde(default)]
    pub fs_mount: Option<String>,

    /// TOGAF Cluster 12: optionale Cluster-Identität (`[daemon.cluster]`).
    /// **Absent = Single-Node** (aktuelles Verhalten unverändert). Present = dieser
    /// Knoten hat eine Cluster-Identität (Genesis-Seed oder provisioniertes Mitglied);
    /// siehe `sentinel_common::cluster` + `docs/adr/ADR-0495-G-GENESIS-…`.
    #[serde(default)]
    pub cluster: Option<sentinel_common::ClusterConfig>,

    /// redb-Durability fuer sentinel-fs Metadata-Commits.
    ///
    /// `immediate` fsyncs every commit. `eventual` skips fsync for the FUSE
    /// hot path and is only appropriate when lower write latency is preferred
    /// over crash durability for the most recent metadata commits.
    #[serde(default)]
    pub fs_metadata_durability: FsMetadataDurability,

    /// Time Machine: Tiered Snapshot + Event Retention.
    #[serde(default)]
    pub retention: RetentionConfig,

    /// Smart Resource Management: Dynamische cgroup-Limits pro Agent.
    #[serde(default)]
    pub resource_manager: ResourceManagerConfig,

    /// Platform-Controlplane: Self-Healing Background Service.
    #[serde(default)]
    pub platform_controlplane: PlatformControlplaneConfig,

    /// Traffic Control Defaults fuer den Gateway-Start.
    #[serde(default)]
    pub traffic_control: TrafficControlConfig,
}

#[derive(Debug, Deserialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FsMetadataDurability {
    #[default]
    Immediate,
    Eventual,
}

/// Traffic-Control Defaults fuer den Gateway Bootstrap.
#[derive(Debug, Deserialize, Clone)]
pub struct TrafficControlConfig {
    #[serde(default)]
    pub synthesis_enabled: bool,
    #[serde(default)]
    pub sequencing_enabled: bool,
    #[serde(default)]
    pub tick_sync_enabled: bool,
    #[serde(default)]
    pub apicp_enabled: bool,
    #[serde(default = "default_tc_tick_sync_timeout_ms")]
    pub tick_sync_timeout_ms: u64,
    #[serde(default = "default_tc_p3_timeout_ms")]
    pub p3_timeout_ms: u64,
    #[serde(default = "default_tc_gateway_request_timeout_ms")]
    pub gateway_request_timeout_ms: u64,
    #[serde(default = "default_tc_max_forward_concurrency")]
    pub max_forward_concurrency: usize,
    #[serde(default = "default_tc_intercept_mode")]
    pub intercept_mode: String,
}

impl Default for TrafficControlConfig {
    fn default() -> Self {
        Self {
            synthesis_enabled: false,
            sequencing_enabled: false,
            tick_sync_enabled: false,
            apicp_enabled: false,
            tick_sync_timeout_ms: default_tc_tick_sync_timeout_ms(),
            p3_timeout_ms: default_tc_p3_timeout_ms(),
            gateway_request_timeout_ms: default_tc_gateway_request_timeout_ms(),
            max_forward_concurrency: default_tc_max_forward_concurrency(),
            intercept_mode: default_tc_intercept_mode(),
        }
    }
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

    /// #491 (TM-3): feineres Anchor-Intervall in der ERSTEN Stunde (Ticks), damit der Bounded
    /// Replay `(anchor, target]` kurz bleibt und jeder Tick guenstig erreichbar ist. Default 300
    /// (5 min bei 1s Tick) — finaler Wert aus dem Benchmark-Sweep (#491 Teil C). Gilt nur solange
    /// `tick < hourly_interval_ticks`; danach greift `hourly_interval_ticks` (Tiered Retention).
    #[serde(default = "default_first_hour_interval")]
    pub first_hour_interval_ticks: u64,

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
            first_hour_interval_ticks: 300,
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
fn default_first_hour_interval() -> u64 {
    300
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
#[derive(Deserialize, Clone)]
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

impl fmt::Debug for OperatorApiConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperatorApiConfig")
            .field("enabled", &self.enabled)
            .field("bind_addr", &self.bind_addr)
            .field("shared_secret_configured", &self.shared_secret.is_some())
            .finish()
    }
}

/// Sealed values generated from the exact legacy Hippocampus state and the
/// selected immutable EventStore prefix. The operator secret is never stored
/// here; it is required only for the first validation, after which the durable
/// cutover receipt can reopen without this one-time configuration.
#[derive(Debug, Deserialize, Clone)]
pub struct EpisodeProjectionCutoverConfig {
    pub source_row_id: i64,
    pub legacy_state_digest: String,
    pub source_cut_digest: String,
    pub authorization_digest: String,
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

/// Prometheus-eBPF Metrics-HTTP-Server Konfiguration (#525).
///
/// Loopback-only secure default (wie operator_api): der einzige Consumer ist der
/// dashboard-backend proxy auf 127.0.0.1. Deliberate exposure = override via TOML.
#[derive(Debug, Deserialize, Clone)]
pub struct MetricsConfig {
    /// Bind-Adresse fuer den Prometheus text HTTP-Listener (default: 127.0.0.1:9090).
    #[serde(default = "default_metrics_bind_addr")]
    pub bind_addr: String,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            bind_addr: default_metrics_bind_addr(),
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

fn default_metrics_bind_addr() -> String {
    "127.0.0.1:9090".to_string()
}

fn default_tc_tick_sync_timeout_ms() -> u64 {
    2000
}

fn default_tc_p3_timeout_ms() -> u64 {
    5000
}

fn default_tc_gateway_request_timeout_ms() -> u64 {
    150_000
}

fn default_tc_max_forward_concurrency() -> usize {
    3
}

fn default_tc_intercept_mode() -> String {
    "auto".to_string()
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

fn default_phase_timing_enabled() -> bool {
    true
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
    #[serde(default = "default_pcp_ebpf_collect_interval")]
    pub ebpf_collect_interval_ticks: u64,
    #[serde(default = "default_pcp_stall_detection_threshold_secs")]
    pub stall_detection_threshold_secs: u64,
    #[serde(default = "default_pcp_stall_recent_activity_grace_ticks")]
    pub stall_recent_activity_grace_ticks: u64,
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
    #[serde(default = "default_pcp_write_anomaly_baseline_multiplier")]
    pub write_anomaly_baseline_multiplier: f64,
    #[serde(default = "default_pcp_write_anomaly_cooldown")]
    pub write_anomaly_cooldown_ticks: u64,
    #[serde(default = "default_pcp_runtime_reconcile_enabled")]
    pub runtime_reconcile_enabled: bool,
    #[serde(default = "default_pcp_runtime_reconcile_interval")]
    pub runtime_reconcile_interval_ticks: u64,
    #[serde(default = "default_pcp_runtime_reconcile_respawn_missing")]
    pub runtime_reconcile_respawn_missing: bool,
    #[serde(default = "default_pcp_runtime_reconcile_rebuild")]
    pub runtime_reconcile_projection_rebuild: bool,
    #[serde(default = "default_pcp_monitored_services")]
    pub monitored_services: Vec<String>,
    #[serde(default = "default_pcp_service_check_interval")]
    pub service_check_interval_secs: u64,
    #[serde(default = "default_pcp_llm_enabled")]
    pub llm_enabled: bool,
    #[serde(default = "default_pcp_llm_analysis_interval")]
    pub llm_analysis_interval_secs: u64,
    #[serde(default = "default_pcp_llm_retry_delay")]
    pub llm_retry_delay_secs: u64,
    #[serde(default = "default_pcp_llm_gateway_timeout_ms")]
    pub llm_gateway_timeout_ms: u64,
    #[serde(default = "default_pcp_llm_prompt_template")]
    pub llm_prompt_template: String,
    #[serde(default = "default_pcp_llm_max_context_events")]
    pub llm_max_context_events: usize,
    #[serde(default = "default_pcp_llm_max_failed_interventions")]
    pub llm_max_failed_interventions: usize,
    #[serde(default = "default_pcp_llm_trigger_queue_capacity")]
    pub llm_trigger_queue_capacity: usize,
    #[serde(default = "default_pcp_llm_analysis_channel_capacity")]
    pub llm_analysis_channel_capacity: usize,
}

fn default_pcp_enabled() -> bool {
    true
}
fn default_pcp_cycle_interval() -> u64 {
    60
}
fn default_pcp_ebpf_collect_interval() -> u64 {
    10
}
fn default_pcp_stall_detection_threshold_secs() -> u64 {
    30
}
fn default_pcp_stall_recent_activity_grace_ticks() -> u64 {
    120
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
fn default_pcp_write_anomaly_baseline_multiplier() -> f64 {
    10.0
}
fn default_pcp_write_anomaly_cooldown() -> u64 {
    60
}
fn default_pcp_runtime_reconcile_enabled() -> bool {
    true
}
fn default_pcp_runtime_reconcile_interval() -> u64 {
    60
}
fn default_pcp_runtime_reconcile_respawn_missing() -> bool {
    true
}
fn default_pcp_runtime_reconcile_rebuild() -> bool {
    true
}
fn default_pcp_monitored_services() -> Vec<String> {
    vec!["sentinel-judge".into(), "sentinel-projection".into()]
}
fn default_pcp_service_check_interval() -> u64 {
    60
}
fn default_pcp_llm_enabled() -> bool {
    true
}
fn default_pcp_llm_analysis_interval() -> u64 {
    300
}
fn default_pcp_llm_retry_delay() -> u64 {
    60
}
fn default_pcp_llm_gateway_timeout_ms() -> u64 {
    30_000
}
fn default_pcp_llm_prompt_template() -> String {
    "platform-controlplane-default".to_string()
}
fn default_pcp_llm_max_context_events() -> usize {
    10
}
fn default_pcp_llm_max_failed_interventions() -> usize {
    3
}
fn default_pcp_llm_trigger_queue_capacity() -> usize {
    16
}
fn default_pcp_llm_analysis_channel_capacity() -> usize {
    16
}

impl Default for PlatformControlplaneConfig {
    fn default() -> Self {
        Self {
            enabled: default_pcp_enabled(),
            cycle_interval_ticks: default_pcp_cycle_interval(),
            ebpf_collect_interval_ticks: default_pcp_ebpf_collect_interval(),
            stall_detection_threshold_secs: default_pcp_stall_detection_threshold_secs(),
            stall_recent_activity_grace_ticks: default_pcp_stall_recent_activity_grace_ticks(),
            stall_cooldown_ticks: default_pcp_stall_cooldown(),
            prune_cooldown_ticks: default_pcp_prune_cooldown(),
            max_event_store_bytes: default_pcp_max_event_store(),
            max_projection_lag: default_pcp_max_projection_lag(),
            memory_pressure_threshold: default_pcp_memory_pressure(),
            max_escalation: default_pcp_max_escalation(),
            write_anomaly_threshold_bytes_per_sec: default_pcp_write_anomaly_threshold(),
            write_anomaly_baseline_multiplier: default_pcp_write_anomaly_baseline_multiplier(),
            write_anomaly_cooldown_ticks: default_pcp_write_anomaly_cooldown(),
            runtime_reconcile_enabled: default_pcp_runtime_reconcile_enabled(),
            runtime_reconcile_interval_ticks: default_pcp_runtime_reconcile_interval(),
            runtime_reconcile_respawn_missing: default_pcp_runtime_reconcile_respawn_missing(),
            runtime_reconcile_projection_rebuild: default_pcp_runtime_reconcile_rebuild(),
            monitored_services: default_pcp_monitored_services(),
            service_check_interval_secs: default_pcp_service_check_interval(),
            llm_enabled: default_pcp_llm_enabled(),
            llm_analysis_interval_secs: default_pcp_llm_analysis_interval(),
            llm_retry_delay_secs: default_pcp_llm_retry_delay(),
            llm_gateway_timeout_ms: default_pcp_llm_gateway_timeout_ms(),
            llm_prompt_template: default_pcp_llm_prompt_template(),
            llm_max_context_events: default_pcp_llm_max_context_events(),
            llm_max_failed_interventions: default_pcp_llm_max_failed_interventions(),
            llm_trigger_queue_capacity: default_pcp_llm_trigger_queue_capacity(),
            llm_analysis_channel_capacity: default_pcp_llm_analysis_channel_capacity(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CredentialFileIdentity {
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    links: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl CredentialFileIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            group: metadata.gid(),
            mode: metadata.mode() & 0o7777,
            links: metadata.nlink(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CredentialDirectoryIdentity {
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    is_directory: bool,
}

impl CredentialDirectoryIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            group: metadata.gid(),
            mode: metadata.mode() & 0o7777,
            is_directory: metadata.is_dir(),
        }
    }
}

#[derive(Debug)]
struct OpenCredential {
    file: File,
    file_identity: CredentialFileIdentity,
    parent_identities: Vec<CredentialDirectoryIdentity>,
}

fn effective_uid() -> Result<u32> {
    std::fs::metadata("/proc/self")
        .map(|metadata| metadata.uid())
        .context("operator credential owner authority unavailable")
}

fn validate_credential_metadata(
    metadata: &std::fs::Metadata,
    expected_owner: u32,
) -> Result<CredentialFileIdentity> {
    let identity = CredentialFileIdentity::from_metadata(metadata);
    if !credential_file_identity_is_allowed(&identity, metadata.is_file(), expected_owner) {
        return Err(anyhow!("operator credential file metadata is invalid"));
    }
    if identity.size < CREDENTIAL_MIN_BYTES as u64 || identity.size > CREDENTIAL_MAX_BYTES as u64 {
        return Err(anyhow!("operator credential length is invalid"));
    }
    Ok(identity)
}

fn credential_file_identity_is_allowed(
    identity: &CredentialFileIdentity,
    is_regular: bool,
    expected_owner: u32,
) -> bool {
    let systemd_owned =
        identity.owner == 0 && identity.group == 0 && matches!(identity.mode, 0o400 | 0o440);
    let service_owned = identity.owner == expected_owner && matches!(identity.mode, 0o400 | 0o600);
    is_regular && identity.links == 1 && (systemd_owned || service_owned)
}

fn validate_credential_directory(
    metadata: &std::fs::Metadata,
    expected_owner: u32,
) -> Result<CredentialDirectoryIdentity> {
    let file_identity = CredentialFileIdentity::from_metadata(metadata);
    if !credential_directory_identity_is_allowed(&file_identity, metadata.is_dir(), expected_owner)
    {
        return Err(anyhow!("operator credential directory metadata is invalid"));
    }
    Ok(CredentialDirectoryIdentity::from_metadata(metadata))
}

fn credential_directory_identity_is_allowed(
    identity: &CredentialFileIdentity,
    is_directory: bool,
    expected_owner: u32,
) -> bool {
    is_directory
        && (identity.owner == 0 || identity.owner == expected_owner)
        && identity.mode & 0o7022 == 0
}

fn open_credential_file(path: &Path) -> Result<OpenCredential> {
    if !path.is_absolute() {
        return Err(anyhow!("operator credential path must be absolute"));
    }
    let components = path
        .components()
        .map(|component| match component {
            Component::RootDir => Ok(None),
            Component::Normal(value) => Ok(Some(value.to_os_string())),
            _ => Err(anyhow!("operator credential path is invalid")),
        })
        .collect::<Result<Vec<_>>>()?;
    let names = components.into_iter().flatten().collect::<Vec<_>>();
    let (file_name, parents) = names
        .split_last()
        .ok_or_else(|| anyhow!("operator credential path is invalid"))?;

    let expected_owner = effective_uid()?;
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(O_DIRECTORY | O_NOFOLLOW)
        .open("/")
        .context("operator credential root unavailable")?;
    let mut parent_identities = vec![validate_credential_directory(
        &directory.metadata()?,
        expected_owner,
    )?];
    for component in parents {
        let candidate =
            PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(component);
        let next = OpenOptions::new()
            .read(true)
            .custom_flags(O_DIRECTORY | O_NOFOLLOW)
            .open(candidate)
            .context("operator credential directory is invalid")?;
        let identity = validate_credential_directory(
            &next
                .metadata()
                .context("operator credential directory metadata unavailable")?,
            expected_owner,
        )?;
        parent_identities.push(identity);
        directory = next;
    }

    let candidate =
        PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(file_name);
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .open(candidate)
        .context("operator credential file unavailable")?;
    let file_identity = validate_credential_metadata(&file.metadata()?, expected_owner)?;
    Ok(OpenCredential {
        file,
        file_identity,
        parent_identities,
    })
}

fn read_operator_credential_with_hook(
    path: &Path,
    after_open: impl FnOnce() -> Result<()>,
) -> Result<String> {
    let OpenCredential {
        mut file,
        file_identity: initial_identity,
        parent_identities: initial_parents,
    } = open_credential_file(path)?;
    after_open()?;

    let mut bytes = vec![0_u8; initial_identity.size as usize];
    let mut offset = 0;
    while offset < bytes.len() {
        let count = file
            .read(&mut bytes[offset..])
            .context("operator credential read failed")?;
        if count == 0 {
            return Err(anyhow!(
                "operator credential read was shorter than declared"
            ));
        }
        offset += count;
    }
    let mut trailing = [0_u8; 1];
    if file
        .read(&mut trailing)
        .context("operator credential read failed")?
        != 0
    {
        return Err(anyhow!("operator credential has trailing data"));
    }
    let after_read = validate_credential_metadata(&file.metadata()?, effective_uid()?)?;
    let reopened = open_credential_file(path)?;
    if initial_identity != after_read
        || initial_identity != reopened.file_identity
        || initial_parents != reopened.parent_identities
        || bytes.len() as u64 != initial_identity.size
    {
        return Err(anyhow!("operator credential identity changed"));
    }

    let value =
        String::from_utf8(bytes).map_err(|_| anyhow!("operator credential encoding is invalid"))?;
    if value.trim() != value || value.chars().any(char::is_control) {
        return Err(anyhow!("operator credential content is invalid"));
    }
    Ok(value)
}

pub(crate) fn read_operator_credential(path: &Path) -> Result<String> {
    read_operator_credential_with_hook(path, || Ok(()))
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

    /// Bind the production Operator API credential before dry-run or orchestration.
    pub fn bind_operator_credential(
        &mut self,
        credentials_directory: Option<&Path>,
        credential_path: Option<&Path>,
    ) -> Result<()> {
        if self.operator_api.shared_secret.is_some() {
            if credential_path.is_some() {
                return Err(anyhow!("operator credential authorities are ambiguous"));
            }
            return Err(anyhow!(
                "embedded operator credentials are not allowed in production"
            ));
        }
        if !self.operator_api.enabled {
            if credential_path.is_some() {
                return Err(anyhow!(
                    "operator credential configured while API is disabled"
                ));
            }
            return Ok(());
        }
        let directory = credentials_directory.ok_or_else(|| {
            anyhow!("systemd credentials directory is required when API is enabled")
        })?;
        let path = credential_path
            .ok_or_else(|| anyhow!("operator credential file is required when API is enabled"))?;
        if !directory.is_absolute() || path != directory.join(OPERATOR_CREDENTIAL_NAME) {
            return Err(anyhow!(
                "operator credential path must be the systemd credential leaf"
            ));
        }
        self.operator_api.shared_secret = Some(read_operator_credential(path)?);
        Ok(())
    }

    /// Liefert die Agent-TOML Validierung passend zu `max_agents`.
    pub fn agent_config_validation(&self) -> Result<AgentConfigValidation> {
        let max_agent_id = u16::try_from(self.max_agents).map_err(|_| {
            anyhow!(
                "daemon.max_agents {} exceeds AgentId u16 range",
                self.max_agents
            )
        })?;
        if max_agent_id == 0 {
            return Err(anyhow!("daemon.max_agents must be >= 1"));
        }
        Ok(AgentConfigValidation::with_max_agent_id(max_agent_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    use std::process::Command;

    const TEST_CREDENTIAL: &str = "0123456789abcdef0123456789abcdef";

    fn safe_tempdir() -> tempfile::TempDir {
        let root = std::env::var_os("RUNNER_TEMP")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/work/tmp/project-sentinel"));
        fs::create_dir_all(&root).unwrap();
        assert!(root.is_absolute());
        for ancestor in root.ancestors() {
            let metadata = fs::metadata(ancestor).unwrap();
            assert_ne!(metadata.mode() & 0o1777, 0o1777);
        }
        tempfile::Builder::new()
            .prefix("operator-credential-daemon-")
            .tempdir_in(root)
            .unwrap()
    }

    fn operator_config(enabled: bool, embedded: Option<&str>) -> DaemonConfig {
        let embedded = embedded
            .map(|value| format!("shared_secret = \"{value}\""))
            .unwrap_or_default();
        let source = format!(
            r#"
[daemon]
config_dir = "/opt/sentinel/config"
data_dir = "/opt/sentinel/data"

[daemon.operator_api]
enabled = {enabled}
{embedded}
"#
        );
        toml::from_str::<DaemonConfigFile>(&source).unwrap().daemon
    }

    fn write_credential(path: &Path, value: &[u8], mode: u32) {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                assert!(
                    metadata.file_type().is_file(),
                    "test credential reset requires an existing regular file"
                );
                fs::remove_file(path).expect("remove existing test credential");
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("inspect existing test credential: {error}"),
        }
        fs::write(path, value).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn production_operator_api_requires_and_binds_secure_credential() {
        let directory = safe_tempdir();
        let path = directory.path().join("operator-api");
        write_credential(&path, TEST_CREDENTIAL.as_bytes(), 0o400);

        let mut missing = operator_config(true, None);
        assert!(missing.bind_operator_credential(None, None).is_err());
        assert!(missing.operator_api.shared_secret.is_none());

        let mut config = operator_config(true, None);
        config
            .bind_operator_credential(Some(directory.path()), Some(&path))
            .unwrap();
        assert_eq!(
            config.operator_api.shared_secret.as_deref(),
            Some(TEST_CREDENTIAL)
        );

        let wrong_path = directory.path().join("alternate");
        write_credential(&wrong_path, TEST_CREDENTIAL.as_bytes(), 0o400);
        let mut wrong = operator_config(true, None);
        assert!(wrong
            .bind_operator_credential(Some(directory.path()), Some(&wrong_path))
            .is_err());

        if effective_uid().unwrap() == 0 {
            write_credential(&path, TEST_CREDENTIAL.as_bytes(), 0o440);
            assert!(read_operator_credential(&path).is_ok());
        }
    }

    #[test]
    fn production_operator_api_rejects_embedded_and_ambiguous_authorities() {
        let directory = safe_tempdir();
        let path = directory.path().join("operator-api");
        write_credential(&path, TEST_CREDENTIAL.as_bytes(), 0o600);
        let embedded = "embedded-secret-that-must-never-appear";

        let mut embedded_only = operator_config(true, Some(embedded));
        let error = embedded_only
            .bind_operator_credential(None, None)
            .unwrap_err();
        assert!(!format!("{error:?}").contains(embedded));

        let mut both = operator_config(true, Some(embedded));
        let error = both
            .bind_operator_credential(Some(directory.path()), Some(&path))
            .unwrap_err();
        assert!(error.to_string().contains("ambiguous"));
        assert!(!format!("{error:?}").contains(embedded));
        assert!(!format!("{both:?}").contains(embedded));
    }

    #[test]
    fn disabled_operator_api_rejects_unused_credential_authority() {
        let directory = safe_tempdir();
        let path = directory.path().join("operator-api");
        write_credential(&path, TEST_CREDENTIAL.as_bytes(), 0o400);

        let mut disabled = operator_config(false, None);
        disabled
            .bind_operator_credential(Some(directory.path()), None)
            .unwrap();
        assert!(disabled.operator_api.shared_secret.is_none());
        assert!(disabled
            .bind_operator_credential(Some(directory.path()), Some(&path))
            .is_err());
    }

    #[test]
    fn operator_credential_rejects_invalid_content_and_metadata() {
        let directory = safe_tempdir();
        let cases: &[(&str, &[u8], u32)] = &[
            ("empty", b"", 0o400),
            ("short", b"0123456789abcdef", 0o400),
            ("leading-space", b" 123456789abcdef0123456789abcdef", 0o400),
            ("control", b"0123456789abcdef0123456789abcde\n", 0o400),
            ("unsafe-mode", TEST_CREDENTIAL.as_bytes(), 0o640),
        ];
        for (name, value, mode) in cases {
            let path = directory.path().join(name);
            write_credential(&path, value, *mode);
            assert!(
                read_operator_credential(&path).is_err(),
                "case {name} must fail closed"
            );
        }

        let oversized = directory.path().join("oversized");
        write_credential(&oversized, &vec![b'x'; CREDENTIAL_MAX_BYTES + 1], 0o400);
        assert!(read_operator_credential(&oversized).is_err());

        let target = directory.path().join("target");
        let symlink_path = directory.path().join("symlink");
        write_credential(&target, TEST_CREDENTIAL.as_bytes(), 0o400);
        symlink(&target, &symlink_path).unwrap();
        assert!(read_operator_credential(&symlink_path).is_err());

        let hardlink = directory.path().join("hardlink");
        fs::hard_link(&target, &hardlink).unwrap();
        assert!(read_operator_credential(&target).is_err());
        assert!(read_operator_credential(&hardlink).is_err());

        let owner_only = directory.path().join("owner-only");
        write_credential(&owner_only, TEST_CREDENTIAL.as_bytes(), 0o400);
        let metadata = fs::metadata(&owner_only).unwrap();
        let mut foreign_file = CredentialFileIdentity::from_metadata(&metadata);
        foreign_file.owner = metadata.uid().saturating_add(1).max(1);
        foreign_file.group = metadata.gid().saturating_add(1).max(1);
        assert!(!credential_file_identity_is_allowed(
            &foreign_file,
            true,
            metadata.uid()
        ));
        let mut group_readable_service_file = CredentialFileIdentity::from_metadata(&metadata);
        group_readable_service_file.owner = 1_000;
        group_readable_service_file.group = 1_000;
        group_readable_service_file.mode = 0o440;
        assert!(!credential_file_identity_is_allowed(
            &group_readable_service_file,
            true,
            1_000
        ));
        group_readable_service_file.owner = 0;
        group_readable_service_file.group = 0;
        assert!(credential_file_identity_is_allowed(
            &group_readable_service_file,
            true,
            1_000
        ));

        let fifo = directory.path().join("fifo");
        assert!(Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        assert!(read_operator_credential(&fifo).is_err());

        let unsafe_parent = directory.path().join("unsafe-parent");
        fs::create_dir(&unsafe_parent).unwrap();
        fs::set_permissions(&unsafe_parent, fs::Permissions::from_mode(0o770)).unwrap();
        let unsafe_leaf = unsafe_parent.join("operator-api");
        write_credential(&unsafe_leaf, TEST_CREDENTIAL.as_bytes(), 0o400);
        assert!(read_operator_credential(&unsafe_leaf).is_err());

        let real_parent = directory.path().join("real-parent");
        let linked_parent = directory.path().join("linked-parent");
        fs::create_dir(&real_parent).unwrap();
        write_credential(
            &real_parent.join("operator-api"),
            TEST_CREDENTIAL.as_bytes(),
            0o400,
        );
        symlink(&real_parent, &linked_parent).unwrap();
        assert!(read_operator_credential(&linked_parent.join("operator-api")).is_err());

        let parent_metadata = fs::metadata(directory.path()).unwrap();
        let mut foreign_parent = CredentialFileIdentity::from_metadata(&parent_metadata);
        foreign_parent.owner = parent_metadata.uid().saturating_add(1).max(1);
        assert!(!credential_directory_identity_is_allowed(
            &foreign_parent,
            true,
            parent_metadata.uid()
        ));
        for unsafe_mode in [0o1700, 0o2700, 0o4700, 0o770] {
            let mut unsafe_identity = CredentialFileIdentity::from_metadata(&parent_metadata);
            unsafe_identity.owner = 1_000;
            unsafe_identity.mode = unsafe_mode;
            assert!(!credential_directory_identity_is_allowed(
                &unsafe_identity,
                true,
                1_000
            ));
        }
    }

    #[test]
    fn operator_credential_rejects_path_replacement_after_open() {
        let directory = safe_tempdir();
        let path = directory.path().join("operator-api");
        let original = directory.path().join("operator-api.original");
        write_credential(&path, TEST_CREDENTIAL.as_bytes(), 0o400);

        let result = read_operator_credential_with_hook(&path, || {
            fs::rename(&path, &original)?;
            write_credential(&path, b"fedcba9876543210fedcba9876543210", 0o400);
            Ok(())
        });
        assert!(result.unwrap_err().to_string().contains("identity changed"));
    }

    #[test]
    fn operator_credential_allows_unrelated_sibling_create_and_remove() {
        let directory = safe_tempdir();
        let path = directory.path().join("operator-api");
        let sibling = directory.path().join("unrelated-sibling");
        write_credential(&path, TEST_CREDENTIAL.as_bytes(), 0o400);

        let credential = read_operator_credential_with_hook(&path, || {
            fs::write(&sibling, b"unrelated")?;
            fs::remove_file(&sibling)?;
            Ok(())
        })
        .unwrap();

        assert_eq!(credential, TEST_CREDENTIAL);
    }

    #[test]
    fn operator_credential_rejects_in_place_and_parent_identity_changes() {
        let directory = safe_tempdir();
        let path = directory.path().join("operator-api");

        write_credential(&path, TEST_CREDENTIAL.as_bytes(), 0o400);
        assert!(read_operator_credential_with_hook(&path, || {
            std::thread::sleep(std::time::Duration::from_millis(2));
            fs::write(&path, b"fedcba9876543210fedcba9876543210")?;
            Ok(())
        })
        .is_err());

        write_credential(&path, TEST_CREDENTIAL.as_bytes(), 0o400);
        assert!(read_operator_credential_with_hook(&path, || {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            Ok(())
        })
        .is_err());

        write_credential(&path, TEST_CREDENTIAL.as_bytes(), 0o400);
        let link = directory.path().join("linked");
        assert!(read_operator_credential_with_hook(&path, || {
            fs::hard_link(&path, &link)?;
            Ok(())
        })
        .is_err());
        fs::remove_file(&link).unwrap();

        write_credential(&path, TEST_CREDENTIAL.as_bytes(), 0o400);
        assert!(read_operator_credential_with_hook(&path, || {
            fs::OpenOptions::new()
                .write(true)
                .open(&path)?
                .set_len(16)?;
            Ok(())
        })
        .is_err());

        write_credential(&path, TEST_CREDENTIAL.as_bytes(), 0o400);
        assert!(read_operator_credential_with_hook(&path, || {
            use std::io::Write;
            let mut append = fs::OpenOptions::new().append(true).open(&path)?;
            append.write_all(b"x")?;
            Ok(())
        })
        .is_err());

        let parent = directory.path().join("credentials");
        fs::create_dir(&parent).unwrap();
        let parent_path = parent.join("operator-api");
        write_credential(&parent_path, TEST_CREDENTIAL.as_bytes(), 0o400);
        let old_parent = directory.path().join("credentials.old");
        assert!(read_operator_credential_with_hook(&parent_path, || {
            fs::rename(&parent, &old_parent)?;
            fs::create_dir(&parent)?;
            write_credential(
                &parent.join("operator-api"),
                TEST_CREDENTIAL.as_bytes(),
                0o400,
            );
            Ok(())
        })
        .is_err());
    }

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

[daemon.metrics]
bind_addr = "127.0.0.1:7777"
"#;
        let file: DaemonConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.daemon.tick_rate_ms, 500);
        assert_eq!(file.daemon.max_agents, 10);
        assert_eq!(file.daemon.zenoh_prefix, "test");
        assert_eq!(
            file.daemon.fs_metadata_durability,
            FsMetadataDurability::Immediate
        );
        assert!(file.daemon.operator_api.enabled);
        assert_eq!(file.daemon.operator_api.bind_addr, "127.0.0.1:9999");
        assert_eq!(
            file.daemon.operator_api.shared_secret.as_deref(),
            Some("secret")
        );
        assert_eq!(file.daemon.metrics.bind_addr, "127.0.0.1:7777");
    }

    #[test]
    fn test_cluster_section_optional() {
        // Single-Node-Default: ohne [daemon.cluster] = None (backward compat).
        let single = r#"
[daemon]
config_dir = "/tmp/cfg"
data_dir = "/tmp/data"
"#;
        let f: DaemonConfigFile = toml::from_str(single).unwrap();
        assert!(
            f.daemon.cluster.is_none(),
            "ohne [daemon.cluster] bleibt der Daemon Single-Node"
        );

        // Cluster-Seed-Node: [daemon.cluster] vorhanden (Genesis, #495).
        let seed = r#"
[daemon]
config_dir = "/opt/sentinel/config"
data_dir = "/opt/sentinel/data"

[daemon.cluster]
node_id = "550e8400-e29b-41d4-a716-446655440000"
cluster_id = "550e8400-e29b-41d4-a716-446655440001"
seed = true
alias = "test-node-0"
"#;
        let f: DaemonConfigFile = toml::from_str(seed).unwrap();
        let cluster = f.daemon.cluster.expect("[daemon.cluster] vorhanden");
        assert!(cluster.seed);
        assert_eq!(cluster.alias.as_deref(), Some("test-node-0"));
        assert_eq!(cluster.role(), sentinel_common::ClusterRole::Seed);
        assert_eq!(
            cluster.initial_lifecycle(),
            sentinel_common::NodeLifecycleState::GenesisSeed
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
        assert_eq!(file.daemon.metrics.bind_addr, "127.0.0.1:9090");
        assert_eq!(file.daemon.tick_rate_ms, 1000);
        assert_eq!(file.daemon.max_agents, 30);
        assert_eq!(
            file.daemon
                .agent_config_validation()
                .unwrap()
                .agent_id_bounds
                .max,
            30
        );
        assert_eq!(file.daemon.zenoh_prefix, "sentinel");
        assert_eq!(file.daemon.time_scale, 1.0);
        assert_eq!(
            file.daemon.fs_metadata_durability,
            FsMetadataDurability::Immediate
        );
        assert_eq!(file.daemon.agent_command, vec!["/usr/bin/agent-runtime"]);
        assert!(!file.daemon.traffic_control.synthesis_enabled);
        assert!(!file.daemon.traffic_control.sequencing_enabled);
        assert!(!file.daemon.traffic_control.tick_sync_enabled);
        assert!(!file.daemon.traffic_control.apicp_enabled);
        assert_eq!(file.daemon.traffic_control.tick_sync_timeout_ms, 2000);
        assert_eq!(file.daemon.traffic_control.p3_timeout_ms, 5000);
        assert_eq!(
            file.daemon.traffic_control.gateway_request_timeout_ms,
            150_000
        );
        assert_eq!(file.daemon.traffic_control.max_forward_concurrency, 3);
        assert_eq!(file.daemon.traffic_control.intercept_mode, "auto");
        assert!(file.daemon.platform_controlplane.enabled);
        assert_eq!(file.daemon.platform_controlplane.cycle_interval_ticks, 60);
        assert_eq!(
            file.daemon
                .platform_controlplane
                .ebpf_collect_interval_ticks,
            10
        );
        assert_eq!(
            file.daemon
                .platform_controlplane
                .stall_detection_threshold_secs,
            30
        );
        assert_eq!(
            file.daemon
                .platform_controlplane
                .stall_recent_activity_grace_ticks,
            120
        );
        assert_eq!(
            file.daemon
                .platform_controlplane
                .service_check_interval_secs,
            60
        );
        assert_eq!(
            file.daemon.platform_controlplane.monitored_services,
            vec![
                "sentinel-judge".to_string(),
                "sentinel-projection".to_string()
            ]
        );
        assert!(!file
            .daemon
            .platform_controlplane
            .monitored_services
            .iter()
            .any(|service| service == "sentinel-gateway"));
        assert!(file.daemon.platform_controlplane.runtime_reconcile_enabled);
        assert_eq!(
            file.daemon
                .platform_controlplane
                .runtime_reconcile_interval_ticks,
            60
        );
        assert!(
            file.daemon
                .platform_controlplane
                .runtime_reconcile_respawn_missing
        );
        assert!(
            file.daemon
                .platform_controlplane
                .runtime_reconcile_projection_rebuild
        );
        assert!(file.daemon.platform_controlplane.llm_enabled);
        assert_eq!(
            file.daemon.platform_controlplane.llm_analysis_interval_secs,
            300
        );
        assert_eq!(file.daemon.platform_controlplane.llm_retry_delay_secs, 60);
        assert_eq!(
            file.daemon.platform_controlplane.llm_gateway_timeout_ms,
            30_000
        );
        assert_eq!(
            file.daemon.platform_controlplane.llm_prompt_template,
            "platform-controlplane-default"
        );
        assert_eq!(file.daemon.platform_controlplane.llm_max_context_events, 10);
        assert_eq!(
            file.daemon
                .platform_controlplane
                .llm_max_failed_interventions,
            3
        );
    }

    #[test]
    fn agent_config_validation_rejects_unrepresentable_max_agents() {
        let toml_str = r#"
[daemon]
config_dir = "/tmp/cfg"
data_dir = "/tmp/data"
max_agents = 70000
"#;
        let file: DaemonConfigFile = toml::from_str(toml_str).unwrap();
        assert!(file.daemon.agent_config_validation().is_err());
    }

    #[test]
    fn test_parse_fs_metadata_eventual_durability() {
        let toml_str = r#"
[daemon]
config_dir = "/tmp/cfg"
data_dir = "/tmp/data"
fs_metadata_durability = "eventual"
"#;
        let file: DaemonConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(
            file.daemon.fs_metadata_durability,
            FsMetadataDurability::Eventual
        );
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

    #[test]
    fn test_traffic_control_custom() {
        let toml_str = r#"
[daemon]
config_dir = "/tmp/cfg"
data_dir = "/tmp/data"

[daemon.traffic_control]
synthesis_enabled = true
sequencing_enabled = true
tick_sync_enabled = true
apicp_enabled = true
tick_sync_timeout_ms = 1500
p3_timeout_ms = 7000
gateway_request_timeout_ms = 180000
max_forward_concurrency = 5
intercept_mode = "manual"
"#;
        let file: DaemonConfigFile = toml::from_str(toml_str).unwrap();
        assert!(file.daemon.traffic_control.synthesis_enabled);
        assert!(file.daemon.traffic_control.sequencing_enabled);
        assert!(file.daemon.traffic_control.tick_sync_enabled);
        assert!(file.daemon.traffic_control.apicp_enabled);
        assert_eq!(file.daemon.traffic_control.tick_sync_timeout_ms, 1500);
        assert_eq!(file.daemon.traffic_control.p3_timeout_ms, 7000);
        assert_eq!(
            file.daemon.traffic_control.gateway_request_timeout_ms,
            180_000
        );
        assert_eq!(file.daemon.traffic_control.max_forward_concurrency, 5);
        assert_eq!(file.daemon.traffic_control.intercept_mode, "manual");
    }

    #[test]
    fn test_platform_controlplane_custom() {
        let toml_str = r#"
[daemon]
config_dir = "/tmp/cfg"
data_dir = "/tmp/data"

[daemon.platform_controlplane]
enabled = true
cycle_interval_ticks = 30
ebpf_collect_interval_ticks = 2
stall_detection_threshold_secs = 12
stall_recent_activity_grace_ticks = 90
stall_cooldown_ticks = 45
prune_cooldown_ticks = 1200
max_event_store_bytes = 123456
max_projection_lag = 42
memory_pressure_threshold = 0.75
max_escalation = 4
write_anomaly_threshold_bytes_per_sec = 111
write_anomaly_baseline_multiplier = 12.5
write_anomaly_cooldown_ticks = 22
runtime_reconcile_enabled = false
runtime_reconcile_interval_ticks = 7
runtime_reconcile_respawn_missing = false
runtime_reconcile_projection_rebuild = false
service_check_interval_secs = 15
llm_enabled = false
llm_analysis_interval_secs = 600
llm_retry_delay_secs = 17
llm_gateway_timeout_ms = 45000
llm_prompt_template = "custom-template"
llm_max_context_events = 23
llm_max_failed_interventions = 5
llm_trigger_queue_capacity = 7
llm_analysis_channel_capacity = 11
"#;
        let file: DaemonConfigFile = toml::from_str(toml_str).unwrap();
        let cfg = file.daemon.platform_controlplane;
        assert!(cfg.enabled);
        assert_eq!(cfg.cycle_interval_ticks, 30);
        assert_eq!(cfg.ebpf_collect_interval_ticks, 2);
        assert_eq!(cfg.stall_detection_threshold_secs, 12);
        assert_eq!(cfg.stall_recent_activity_grace_ticks, 90);
        assert_eq!(cfg.stall_cooldown_ticks, 45);
        assert_eq!(cfg.prune_cooldown_ticks, 1200);
        assert_eq!(cfg.max_event_store_bytes, 123456);
        assert_eq!(cfg.max_projection_lag, 42);
        assert_eq!(cfg.memory_pressure_threshold, 0.75);
        assert_eq!(cfg.max_escalation, 4);
        assert_eq!(cfg.write_anomaly_threshold_bytes_per_sec, 111);
        assert_eq!(cfg.write_anomaly_baseline_multiplier, 12.5);
        assert_eq!(cfg.write_anomaly_cooldown_ticks, 22);
        assert!(!cfg.runtime_reconcile_enabled);
        assert_eq!(cfg.runtime_reconcile_interval_ticks, 7);
        assert!(!cfg.runtime_reconcile_respawn_missing);
        assert!(!cfg.runtime_reconcile_projection_rebuild);
        assert_eq!(cfg.service_check_interval_secs, 15);
        assert!(!cfg.llm_enabled);
        assert_eq!(cfg.llm_analysis_interval_secs, 600);
        assert_eq!(cfg.llm_retry_delay_secs, 17);
        assert_eq!(cfg.llm_gateway_timeout_ms, 45_000);
        assert_eq!(cfg.llm_prompt_template, "custom-template");
        assert_eq!(cfg.llm_max_context_events, 23);
        assert_eq!(cfg.llm_max_failed_interventions, 5);
        assert_eq!(cfg.llm_trigger_queue_capacity, 7);
        assert_eq!(cfg.llm_analysis_channel_capacity, 11);
    }
}
