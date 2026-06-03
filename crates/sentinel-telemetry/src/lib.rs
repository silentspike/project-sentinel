//! Cross-cutting observability layer for Project Sentinel.
//!
//! Provides structured logging, in-process metrics, health checks,
//! error severity classification, and correlation context propagation
//! for all sentinel crates.

pub mod context;
pub mod errors;
pub mod export;
pub mod health;
pub mod logging;
pub mod metrics;

pub use context::{
    TraceContext, TELEMETRY_ERRORS, TELEMETRY_HEALTH, TELEMETRY_METRICS, TELEMETRY_TRACES,
};
pub use errors::{ClassifiedError, ErrorEvent, ErrorSeverity};
pub use export::{ExporterConfig, TelemetryExporter, TelemetryTransport};
pub use health::{HealthRegistry, HealthSnapshot, HealthStatus, SubsystemHealth};
#[cfg(feature = "telemetry")]
pub use logging::{
    build_tracer_provider, init_logging, init_logging_dev, init_observability,
    init_observability_with_config, ObservabilityGuard, ObservabilityInitError, OtlpConfig,
    OtlpProtocol, DEFAULT_OTLP_GRPC_ENDPOINT, DEFAULT_OTLP_HTTP_ENDPOINT, SENTINEL_OTLP_BATCH_MS,
    SENTINEL_OTLP_ENABLED, SENTINEL_OTLP_ENDPOINT, SENTINEL_OTLP_PROTOCOL,
    SENTINEL_OTLP_SERVICE_NAME, SENTINEL_OTLP_TIMEOUT_MS,
};
pub use metrics::{
    metric_name, Counter, Gauge, Histogram, MetricsRegistry, MetricsSnapshot, SubsystemMetrics,
};
