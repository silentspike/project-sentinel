//! Cross-cutting observability layer for Project Sentinel.
//!
//! Provides structured logging, in-process metrics, health checks,
//! and correlation context propagation for all sentinel crates.

pub mod context;
pub mod health;
pub mod logging;
pub mod metrics;

pub use context::TraceContext;
pub use health::{HealthRegistry, HealthStatus};
pub use logging::{init_logging, init_logging_dev};
pub use metrics::{Counter, Histogram, MetricsRegistry, MetricsSnapshot};
