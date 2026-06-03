//! Structured logging setup for Project Sentinel.
//!
//! Two modes: JSON (production) and pretty (development).
//! Respects RUST_LOG env var for filtering.
//!
//! # Log-Level-Strategie
//!
//! | Level | Wann | Beispiel |
//! |-------|------|---------|
//! | ERROR | Fatal/Unrecoverable | redb corruption, Zenoh disconnect |
//! | WARN  | Transient/Degraded | API timeout, retry |
//! | INFO  | Business Events | Agent moved, message sent, tick completed |
//! | DEBUG | Subsystem Details | Bio-Engine Werte, Physics Berechnung |
//! | TRACE | Hot Path Instrumentation | Jeder redb get/set, jeder Zenoh publish |
//!
//! # Span-Hierarchie (geplant fuer Sprint 2+)
//!
//! ```text
//! tick{tick=t42}
//!   agent{agent_id=AGENT-01}
//!     bio{hunger=45.5, energy=72.0}
//!     physics{room_id=ROOM-3}
//!     zenoh.publish{topic=sentinel/agent/AGENT-01/action}
//!     redb.get_agent_state{agent_id=AGENT-01}
//! ```
//!
//! Die Hierarchie wird durch verschachtelte `tracing::Span`s realisiert.
//! Jeder Tick erzeugt einen Root-Span, Agent-Operationen sind Kind-Spans.

#[cfg(feature = "telemetry")]
use std::{env, error::Error, fmt as std_fmt, str::FromStr, time::Duration};

#[cfg(feature = "telemetry")]
use opentelemetry::trace::TracerProvider as _;
#[cfg(feature = "telemetry")]
use opentelemetry_otlp::{Protocol, WithExportConfig};
#[cfg(feature = "telemetry")]
use opentelemetry_sdk::{
    trace::{BatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider},
    Resource,
};
#[cfg(feature = "telemetry")]
use tracing_subscriber::{fmt, prelude::*, util::SubscriberInitExt, EnvFilter};

#[cfg(feature = "telemetry")]
pub const SENTINEL_OTLP_ENABLED: &str = "SENTINEL_OTLP_ENABLED";
#[cfg(feature = "telemetry")]
pub const SENTINEL_OTLP_PROTOCOL: &str = "SENTINEL_OTLP_PROTOCOL";
#[cfg(feature = "telemetry")]
pub const SENTINEL_OTLP_ENDPOINT: &str = "SENTINEL_OTLP_ENDPOINT";
#[cfg(feature = "telemetry")]
pub const SENTINEL_OTLP_SERVICE_NAME: &str = "SENTINEL_OTLP_SERVICE_NAME";
#[cfg(feature = "telemetry")]
pub const SENTINEL_OTLP_TIMEOUT_MS: &str = "SENTINEL_OTLP_TIMEOUT_MS";
#[cfg(feature = "telemetry")]
pub const SENTINEL_OTLP_BATCH_MS: &str = "SENTINEL_OTLP_BATCH_MS";

#[cfg(feature = "telemetry")]
pub const DEFAULT_OTLP_HTTP_ENDPOINT: &str = "http://127.0.0.1:4318/v1/traces";
#[cfg(feature = "telemetry")]
pub const DEFAULT_OTLP_GRPC_ENDPOINT: &str = "http://127.0.0.1:4317";
#[cfg(feature = "telemetry")]
pub const DEFAULT_OTLP_TIMEOUT_MS: u64 = 3_000;
#[cfg(feature = "telemetry")]
pub const DEFAULT_OTLP_BATCH_MS: u64 = 5_000;

#[cfg(feature = "telemetry")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtlpProtocol {
    Http,
    Grpc,
}

#[cfg(feature = "telemetry")]
impl OtlpProtocol {
    pub fn default_endpoint(self) -> &'static str {
        match self {
            Self::Http => DEFAULT_OTLP_HTTP_ENDPOINT,
            Self::Grpc => DEFAULT_OTLP_GRPC_ENDPOINT,
        }
    }
}

#[cfg(feature = "telemetry")]
impl FromStr for OtlpProtocol {
    type Err = ObservabilityInitError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "http" | "http/protobuf" | "http-protobuf" => Ok(Self::Http),
            "grpc" | "tonic" => Ok(Self::Grpc),
            other => Err(ObservabilityInitError::InvalidConfig(format!(
                "{SENTINEL_OTLP_PROTOCOL} must be http or grpc, got {other:?}"
            ))),
        }
    }
}

#[cfg(feature = "telemetry")]
impl std_fmt::Display for OtlpProtocol {
    fn fmt(&self, f: &mut std_fmt::Formatter<'_>) -> std_fmt::Result {
        match self {
            Self::Http => f.write_str("http"),
            Self::Grpc => f.write_str("grpc"),
        }
    }
}

#[cfg(feature = "telemetry")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtlpConfig {
    pub enabled: bool,
    pub protocol: OtlpProtocol,
    pub endpoint: String,
    pub service_name: String,
    pub timeout: Duration,
    pub batch_interval: Duration,
}

#[cfg(feature = "telemetry")]
impl OtlpConfig {
    pub fn from_env(
        default_service_name: impl Into<String>,
    ) -> Result<Self, ObservabilityInitError> {
        Self::from_lookup(default_service_name, |key| env::var(key).ok())
    }

    pub fn from_lookup(
        default_service_name: impl Into<String>,
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, ObservabilityInitError> {
        let default_service_name = default_service_name.into();
        let enabled = match lookup(SENTINEL_OTLP_ENABLED) {
            Some(value) => parse_bool(SENTINEL_OTLP_ENABLED, &value)?,
            None => false,
        };
        let protocol = match lookup(SENTINEL_OTLP_PROTOCOL) {
            Some(value) => OtlpProtocol::from_str(&value)?,
            None => OtlpProtocol::Http,
        };
        let endpoint = lookup(SENTINEL_OTLP_ENDPOINT)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| protocol.default_endpoint().to_string());
        let service_name = lookup(SENTINEL_OTLP_SERVICE_NAME)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(default_service_name);
        let timeout = parse_duration_ms(
            SENTINEL_OTLP_TIMEOUT_MS,
            lookup(SENTINEL_OTLP_TIMEOUT_MS),
            DEFAULT_OTLP_TIMEOUT_MS,
        )?;
        let batch_interval = parse_duration_ms(
            SENTINEL_OTLP_BATCH_MS,
            lookup(SENTINEL_OTLP_BATCH_MS),
            DEFAULT_OTLP_BATCH_MS,
        )?;

        Ok(Self {
            enabled,
            protocol,
            endpoint,
            service_name,
            timeout,
            batch_interval,
        })
    }
}

#[cfg(feature = "telemetry")]
#[derive(Debug)]
pub enum ObservabilityInitError {
    InvalidConfig(String),
    Exporter(String),
    Subscriber(String),
}

#[cfg(feature = "telemetry")]
impl std_fmt::Display for ObservabilityInitError {
    fn fmt(&self, f: &mut std_fmt::Formatter<'_>) -> std_fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(f, "invalid observability config: {message}"),
            Self::Exporter(message) => write!(f, "OTLP exporter setup failed: {message}"),
            Self::Subscriber(message) => write!(f, "tracing subscriber setup failed: {message}"),
        }
    }
}

#[cfg(feature = "telemetry")]
impl Error for ObservabilityInitError {}

#[cfg(feature = "telemetry")]
#[must_use]
#[derive(Debug)]
pub struct ObservabilityGuard {
    tracer_provider: Option<SdkTracerProvider>,
    shutdown_timeout: Duration,
}

#[cfg(feature = "telemetry")]
impl ObservabilityGuard {
    pub fn otlp_enabled(&self) -> bool {
        self.tracer_provider.is_some()
    }

    pub fn shutdown(&self) -> Result<(), ObservabilityInitError> {
        if let Some(provider) = &self.tracer_provider {
            provider
                .shutdown_with_timeout(self.shutdown_timeout)
                .map_err(|err| ObservabilityInitError::Exporter(err.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(feature = "telemetry")]
impl Drop for ObservabilityGuard {
    fn drop(&mut self) {
        if let Some(provider) = &self.tracer_provider {
            let _ = provider.shutdown_with_timeout(self.shutdown_timeout);
        }
    }
}

/// Initialize structured logging with JSON output (production mode).
///
/// Respects RUST_LOG env var. Default filter: `info`.
/// Call once at startup.
///
/// Only available with the `telemetry` feature (default: enabled).
#[cfg(feature = "telemetry")]
pub fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json())
        .init();
}

/// Initialize structured logging with pretty console output (development mode).
///
/// Respects RUST_LOG env var. Default filter: `debug`.
/// Call once at startup.
///
/// Only available with the `telemetry` feature (default: enabled).
#[cfg(feature = "telemetry")]
pub fn init_logging_dev() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().pretty())
        .init();
}

/// Initialize production observability for a service.
///
/// `RUST_LOG` controls log filtering. OTLP trace export is disabled by default
/// and can be enabled with `SENTINEL_OTLP_ENABLED=true`.
#[cfg(feature = "telemetry")]
pub fn init_observability(
    default_service_name: impl Into<String>,
) -> Result<ObservabilityGuard, ObservabilityInitError> {
    let config = OtlpConfig::from_env(default_service_name)?;
    init_observability_with_config(config)
}

#[cfg(feature = "telemetry")]
pub fn init_observability_with_config(
    config: OtlpConfig,
) -> Result<ObservabilityGuard, ObservabilityInitError> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = fmt::layer().json();

    if config.enabled {
        let provider = build_tracer_provider(&config)?;
        let tracer = provider.tracer(config.service_name.clone());
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .with(otel_layer)
            .try_init()
            .map_err(|err| ObservabilityInitError::Subscriber(err.to_string()))?;

        Ok(ObservabilityGuard {
            tracer_provider: Some(provider),
            shutdown_timeout: config.timeout,
        })
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .try_init()
            .map_err(|err| ObservabilityInitError::Subscriber(err.to_string()))?;

        Ok(ObservabilityGuard {
            tracer_provider: None,
            shutdown_timeout: config.timeout,
        })
    }
}

#[cfg(feature = "telemetry")]
pub fn build_tracer_provider(
    config: &OtlpConfig,
) -> Result<SdkTracerProvider, ObservabilityInitError> {
    let exporter = match config.protocol {
        OtlpProtocol::Http => opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(config.endpoint.clone())
            .with_timeout(config.timeout)
            .with_protocol(Protocol::HttpBinary)
            .build(),
        OtlpProtocol::Grpc => opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(config.endpoint.clone())
            .with_timeout(config.timeout)
            .build(),
    }
    .map_err(|err| ObservabilityInitError::Exporter(err.to_string()))?;

    let batch_config = BatchConfigBuilder::default()
        .with_scheduled_delay(config.batch_interval)
        .build();
    let batch_processor = BatchSpanProcessor::builder(exporter)
        .with_batch_config(batch_config)
        .build();
    let resource = Resource::builder_empty()
        .with_service_name(config.service_name.clone())
        .build();

    Ok(SdkTracerProvider::builder()
        .with_span_processor(batch_processor)
        .with_resource(resource)
        .build())
}

#[cfg(feature = "telemetry")]
fn parse_bool(name: &str, value: &str) -> Result<bool, ObservabilityInitError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(ObservabilityInitError::InvalidConfig(format!(
            "{name} must be boolean, got {other:?}"
        ))),
    }
}

#[cfg(feature = "telemetry")]
fn parse_duration_ms(
    name: &str,
    value: Option<String>,
    default_ms: u64,
) -> Result<Duration, ObservabilityInitError> {
    let Some(value) = value else {
        return Ok(Duration::from_millis(default_ms));
    };
    let millis = value.trim().parse::<u64>().map_err(|err| {
        ObservabilityInitError::InvalidConfig(format!("{name} must be milliseconds: {err}"))
    })?;
    Ok(Duration::from_millis(millis))
}

#[cfg(all(test, feature = "telemetry"))]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // Logging kann nur einmal pro Prozess initialisiert werden.
    // Wir testen dass die Funktionen nicht paniken.
    // Separate Tests wuerden sich gegenseitig stoeren.

    #[test]
    fn test_init_logging_does_not_panic() {
        // try_init statt init um Doppel-Initialisierung zu vermeiden
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().json())
            .try_init();
    }

    #[test]
    fn test_init_logging_dev_does_not_panic() {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));

        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().pretty())
            .try_init();
    }

    #[test]
    fn otlp_config_defaults_to_disabled_http() {
        let config = OtlpConfig::from_lookup("sentinel-test", |_| None).unwrap();

        assert!(!config.enabled);
        assert_eq!(config.protocol, OtlpProtocol::Http);
        assert_eq!(config.endpoint, DEFAULT_OTLP_HTTP_ENDPOINT);
        assert_eq!(config.service_name, "sentinel-test");
        assert_eq!(
            config.timeout,
            Duration::from_millis(DEFAULT_OTLP_TIMEOUT_MS)
        );
        assert_eq!(
            config.batch_interval,
            Duration::from_millis(DEFAULT_OTLP_BATCH_MS)
        );
    }

    #[test]
    fn otlp_config_supports_grpc_defaults() {
        let vars = HashMap::from([
            (SENTINEL_OTLP_ENABLED, "true".to_string()),
            (SENTINEL_OTLP_PROTOCOL, "grpc".to_string()),
            (SENTINEL_OTLP_SERVICE_NAME, "sentinel-daemon".to_string()),
            (SENTINEL_OTLP_TIMEOUT_MS, "750".to_string()),
            (SENTINEL_OTLP_BATCH_MS, "250".to_string()),
        ]);
        let config = OtlpConfig::from_lookup("fallback", |key| vars.get(key).cloned()).unwrap();

        assert!(config.enabled);
        assert_eq!(config.protocol, OtlpProtocol::Grpc);
        assert_eq!(config.endpoint, DEFAULT_OTLP_GRPC_ENDPOINT);
        assert_eq!(config.service_name, "sentinel-daemon");
        assert_eq!(config.timeout, Duration::from_millis(750));
        assert_eq!(config.batch_interval, Duration::from_millis(250));
    }

    #[test]
    fn otlp_config_rejects_invalid_protocol() {
        let vars = HashMap::from([(SENTINEL_OTLP_PROTOCOL, "smtp".to_string())]);
        let err = OtlpConfig::from_lookup("sentinel-test", |key| vars.get(key).cloned())
            .expect_err("invalid protocol must be rejected");

        assert!(err.to_string().contains(SENTINEL_OTLP_PROTOCOL));
    }

    #[test]
    fn otlp_config_rejects_invalid_enabled_flag() {
        let vars = HashMap::from([(SENTINEL_OTLP_ENABLED, "maybe".to_string())]);
        let err = OtlpConfig::from_lookup("sentinel-test", |key| vars.get(key).cloned())
            .expect_err("invalid enabled flag must be rejected");

        assert!(err.to_string().contains(SENTINEL_OTLP_ENABLED));
    }

    #[test]
    fn build_http_exporter_for_offline_collector_does_not_panic() {
        let config = OtlpConfig {
            enabled: true,
            protocol: OtlpProtocol::Http,
            endpoint: "http://127.0.0.1:9/v1/traces".to_string(),
            service_name: "sentinel-test".to_string(),
            timeout: Duration::from_millis(10),
            batch_interval: Duration::from_millis(10),
        };

        let provider = build_tracer_provider(&config).expect("http exporter should build offline");
        let _ = provider.shutdown_with_timeout(Duration::from_millis(10));
    }
}
