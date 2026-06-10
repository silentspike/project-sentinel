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
pub mod phase;

pub use context::{
    TraceContext, TELEMETRY_ERRORS, TELEMETRY_HEALTH, TELEMETRY_METRICS, TELEMETRY_TRACES,
};
pub use errors::{ClassifiedError, ErrorEvent, ErrorSeverity};
pub use export::{ExporterConfig, TelemetryExporter, TelemetryTransport};
pub use health::{HealthRegistry, HealthSnapshot, HealthStatus, SubsystemHealth};
#[cfg(feature = "telemetry")]
pub use logging::{init_logging, init_logging_dev};
pub use metrics::{
    metric_name, Counter, Gauge, Histogram, MetricsRegistry, MetricsSnapshot, SubsystemMetrics,
};
pub use phase::{
    phase_label, phase_metric_name, PHASE_DURATION_BOUNDARIES_MS, PHASE_DURATION_PROM_NAME,
};
