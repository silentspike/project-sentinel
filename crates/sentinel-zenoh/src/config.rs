//! Zenoh bus configuration, parsed from environment variables.

/// Default p99 latency target for SHM transport (microseconds).
const DEFAULT_SHM_P99_TARGET_US: u64 = 200;
/// Default query deadline (milliseconds).
const DEFAULT_QUERY_DEADLINE_MS: u64 = 100;
/// Default maximum in-flight queries globally.
const DEFAULT_MAX_INFLIGHT_GLOBAL: usize = 128;
/// Default maximum in-flight queries per agent.
const DEFAULT_MAX_INFLIGHT_PER_AGENT: usize = 8;
/// Default SHM buffer size in bytes (1 MB).
const DEFAULT_SHM_BUFFER_SIZE_BYTES: usize = 1_048_576;
/// Default fan-out channel capacity.
const DEFAULT_FANOUT_CHANNEL_CAPACITY: usize = 256;

/// Zenoh bus configuration.
///
/// All values are parsed from environment variables with sensible defaults.
/// See Issue #6 for the full ENV specification.
#[derive(Debug, Clone)]
pub struct BusConfig {
    /// Enable SHM transport (`SENTINEL_ZENOH_SHM`, default: false)
    pub shm_enabled: bool,
    /// p99 latency target in microseconds (`SENTINEL_ZENOH_SHM_P99_TARGET_US`, default: 200)
    pub shm_p99_target_us: u64,
    /// Default query deadline in milliseconds (`SENTINEL_ZENOH_QUERY_DEADLINE_MS`, default: 100)
    pub query_deadline_ms: u64,
    /// Enable query cancellation (`SENTINEL_ZENOH_QUERY_CANCEL`, default: true)
    pub query_cancel_enabled: bool,
    /// Maximum in-flight queries globally (`SENTINEL_ZENOH_MAX_INFLIGHT_GLOBAL`, default: 128)
    pub max_inflight_global: usize,
    /// Maximum in-flight queries per agent (`SENTINEL_ZENOH_MAX_INFLIGHT_PER_AGENT`, default: 8)
    pub max_inflight_per_agent: usize,
    /// SHM buffer size in bytes (`SENTINEL_ZENOH_SHM_BUFFER_SIZE`, default: 1MB)
    pub shm_buffer_size_bytes: usize,
    /// Fan-out channel capacity (`SENTINEL_ZENOH_FANOUT_CAPACITY`, default: 256)
    pub fanout_channel_capacity: usize,
    /// Enable query responder (`SENTINEL_ZENOH_QUERY_RESPONDER`, default: true)
    pub query_responder_enabled: bool,
}

impl Default for BusConfig {
    fn default() -> Self {
        Self {
            shm_enabled: false,
            shm_p99_target_us: DEFAULT_SHM_P99_TARGET_US,
            query_deadline_ms: DEFAULT_QUERY_DEADLINE_MS,
            query_cancel_enabled: true,
            max_inflight_global: DEFAULT_MAX_INFLIGHT_GLOBAL,
            max_inflight_per_agent: DEFAULT_MAX_INFLIGHT_PER_AGENT,
            shm_buffer_size_bytes: DEFAULT_SHM_BUFFER_SIZE_BYTES,
            fanout_channel_capacity: DEFAULT_FANOUT_CHANNEL_CAPACITY,
            query_responder_enabled: true,
        }
    }
}

impl BusConfig {
    /// Parse configuration from environment variables with defaults.
    pub fn from_env() -> Self {
        Self {
            shm_enabled: parse_bool_env("SENTINEL_ZENOH_SHM", false),
            shm_p99_target_us: parse_u64_env(
                "SENTINEL_ZENOH_SHM_P99_TARGET_US",
                DEFAULT_SHM_P99_TARGET_US,
            ),
            query_deadline_ms: parse_u64_env(
                "SENTINEL_ZENOH_QUERY_DEADLINE_MS",
                DEFAULT_QUERY_DEADLINE_MS,
            ),
            query_cancel_enabled: parse_bool_env("SENTINEL_ZENOH_QUERY_CANCEL", true),
            max_inflight_global: parse_usize_env(
                "SENTINEL_ZENOH_MAX_INFLIGHT_GLOBAL",
                DEFAULT_MAX_INFLIGHT_GLOBAL,
            ),
            max_inflight_per_agent: parse_usize_env(
                "SENTINEL_ZENOH_MAX_INFLIGHT_PER_AGENT",
                DEFAULT_MAX_INFLIGHT_PER_AGENT,
            ),
            shm_buffer_size_bytes: parse_usize_env(
                "SENTINEL_ZENOH_SHM_BUFFER_SIZE",
                DEFAULT_SHM_BUFFER_SIZE_BYTES,
            ),
            fanout_channel_capacity: parse_usize_env(
                "SENTINEL_ZENOH_FANOUT_CAPACITY",
                DEFAULT_FANOUT_CHANNEL_CAPACITY,
            ),
            query_responder_enabled: parse_bool_env("SENTINEL_ZENOH_QUERY_RESPONDER", true),
        }
    }
}

fn parse_bool_env(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|v| {
            let s = v.to_ascii_lowercase();
            matches!(s.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(default)
}

fn parse_u64_env(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn parse_usize_env(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = BusConfig::default();
        assert!(!config.shm_enabled);
        assert_eq!(config.shm_p99_target_us, 200);
        assert_eq!(config.query_deadline_ms, 100);
        assert!(config.query_cancel_enabled);
        assert_eq!(config.max_inflight_global, 128);
        assert_eq!(config.max_inflight_per_agent, 8);
    }

    #[test]
    fn test_config_from_env_without_vars() {
        // Ohne gesetzte Env-Vars sollte from_env() Defaults liefern
        let config = BusConfig::from_env();
        // shm_enabled koennte von externen Tests gesetzt sein, pruefen wir nur die numerischen
        assert_eq!(config.shm_p99_target_us, 200);
        assert_eq!(config.query_deadline_ms, 100);
        assert_eq!(config.max_inflight_global, 128);
        assert_eq!(config.max_inflight_per_agent, 8);
    }

    #[test]
    fn test_parse_bool_env() {
        assert!(parse_bool_env("__NONEXISTENT_VAR__", true));
        assert!(!parse_bool_env("__NONEXISTENT_VAR__", false));
    }
}
