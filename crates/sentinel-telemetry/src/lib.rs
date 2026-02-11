//! Cross-cutting observability layer for Project Sentinel.
//!
//! Provides structured logging, in-process metrics, health checks,
//! error severity classification, and correlation context propagation
//! for all sentinel crates.

pub mod context;
pub mod errors;
pub mod health;
pub mod logging;
pub mod metrics;

pub use context::{TraceContext, TELEMETRY_HEALTH, TELEMETRY_METRICS, TELEMETRY_TRACES};
pub use errors::{ClassifiedError, ErrorSeverity};
pub use health::{HealthRegistry, HealthStatus};
pub use logging::{init_logging, init_logging_dev};
pub use metrics::{metric_name, Counter, Histogram, MetricsRegistry, MetricsSnapshot};
