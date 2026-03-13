//! Telemetry exporter for publishing metrics and health over Zenoh.
//!
//! The [`TelemetryExporter`] runs as a background task, periodically
//! collecting metrics and health snapshots and publishing them to
//! the configured Zenoh topics.
//!
//! # Design
//!
//! The exporter is decoupled from Zenoh via the [`TelemetryTransport`]
//! trait. This allows:
//! - Testing without Zenoh runtime
//! - Swapping transport (Zenoh, stdout, file) without changing exporter logic
//! - Feature-gating the Zenoh dependency

use std::time::Duration;

#[cfg(feature = "telemetry")]
use sentinel_common::Timestamp;

use crate::context::TELEMETRY_ERRORS;
#[cfg(feature = "telemetry")]
use crate::context::{TELEMETRY_HEALTH, TELEMETRY_METRICS};
use crate::errors::ErrorEvent;
#[cfg(feature = "telemetry")]
use crate::health::HealthRegistry;
#[cfg(feature = "telemetry")]
use crate::metrics::{MetricsRegistry, MetricsSnapshot};

// ──────────────────────────────────────────────
// Transport Trait
// ──────────────────────────────────────────────

/// Transport abstraction for telemetry publishing.
///
/// Implemented by `SentinelBus` (Zenoh) in sentinel-zenoh crate.
/// Also useful for testing (mock transport) and alternative outputs.
pub trait TelemetryTransport: Send + Sync {
    /// Publish serialized data to a topic.
    fn publish(
        &self,
        topic: &str,
        payload: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

// ──────────────────────────────────────────────
// Exporter Configuration
// ──────────────────────────────────────────────

/// Configuration for the telemetry exporter.
pub struct ExporterConfig {
    /// Interval between metrics snapshots (default: 1s).
    pub metrics_interval: Duration,
    /// Interval between health checks (default: 5s).
    pub health_interval: Duration,
}

impl Default for ExporterConfig {
    fn default() -> Self {
        Self {
            metrics_interval: Duration::from_secs(1),
            health_interval: Duration::from_secs(5),
        }
    }
}

// ──────────────────────────────────────────────
// TelemetryExporter
// ──────────────────────────────────────────────

/// Background exporter that publishes MetricsSnapshot and HealthSnapshot
/// over a [`TelemetryTransport`] at configured intervals.
///
/// # Usage
///
/// ```ignore
/// let bus = SentinelBus::new().await?;
/// let exporter = TelemetryExporter::new(bus, ExporterConfig::default());
/// // Run until cancellation:
/// exporter.export_once(Tick(0), Timestamp(now_ms));
/// ```
pub struct TelemetryExporter<T: TelemetryTransport> {
    transport: T,
    config: ExporterConfig,
}

impl<T: TelemetryTransport> TelemetryExporter<T> {
    /// Create a new exporter with the given transport and config.
    pub fn new(transport: T, config: ExporterConfig) -> Self {
        Self { transport, config }
    }

    /// Returns the configured metrics interval.
    pub fn metrics_interval(&self) -> Duration {
        self.config.metrics_interval
    }

    /// Returns the configured health interval.
    pub fn health_interval(&self) -> Duration {
        self.config.health_interval
    }

    /// Export a metrics snapshot once.
    ///
    /// Collects current metrics from the global registry and publishes
    /// to `sentinel/telemetry/metrics`.
    #[cfg(feature = "telemetry")]
    pub fn export_metrics(
        &self,
        tick: sentinel_common::Tick,
        timestamp: Timestamp,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let registry = MetricsRegistry::global();
        let (counters, histograms, gauges) = registry.snapshot_raw();

        // Build per-subsystem metrics from flat counter/histogram/gauge maps
        let mut subsystems = std::collections::HashMap::new();
        for (name, value) in &counters {
            // Extract subsystem from metric name: sentinel.{subsystem}.{op}.{type}
            if let Some(subsystem) = extract_subsystem(name) {
                let entry = subsystems.entry(subsystem.to_string()).or_insert_with(|| {
                    crate::metrics::SubsystemMetrics {
                        health: crate::health::HealthStatus::Healthy,
                        counters: std::collections::HashMap::new(),
                        histograms: std::collections::HashMap::new(),
                        gauges: std::collections::HashMap::new(),
                    }
                });
                entry.counters.insert(name.clone(), *value);
            }
        }
        for (name, snap) in histograms {
            if let Some(subsystem) = extract_subsystem(&name) {
                let entry = subsystems.entry(subsystem.to_string()).or_insert_with(|| {
                    crate::metrics::SubsystemMetrics {
                        health: crate::health::HealthStatus::Healthy,
                        counters: std::collections::HashMap::new(),
                        histograms: std::collections::HashMap::new(),
                        gauges: std::collections::HashMap::new(),
                    }
                });
                entry.histograms.insert(name, snap);
            }
        }
        for (name, value) in &gauges {
            if let Some(subsystem) = extract_subsystem(name) {
                let entry = subsystems.entry(subsystem.to_string()).or_insert_with(|| {
                    crate::metrics::SubsystemMetrics {
                        health: crate::health::HealthStatus::Healthy,
                        counters: std::collections::HashMap::new(),
                        histograms: std::collections::HashMap::new(),
                        gauges: std::collections::HashMap::new(),
                    }
                });
                entry.gauges.insert(name.clone(), *value);
            }
        }

        let snapshot = MetricsSnapshot {
            timestamp,
            tick,
            subsystems,
        };

        let payload = serde_json::to_vec(&snapshot)?;
        self.transport.publish(TELEMETRY_METRICS, &payload)
    }

    /// Export a health snapshot once.
    ///
    /// Runs all registered health checks and publishes
    /// to `sentinel/telemetry/health`.
    #[cfg(feature = "telemetry")]
    pub fn export_health(
        &self,
        timestamp: Timestamp,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let registry = HealthRegistry::global();
        let snapshot = registry.snapshot(timestamp);
        let payload = serde_json::to_vec(&snapshot)?;
        self.transport.publish(TELEMETRY_HEALTH, &payload)
    }

    /// Publish a single error event.
    ///
    /// Called immediately when an error occurs (event-driven, not polled).
    pub fn export_error(
        &self,
        event: &ErrorEvent,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let payload = serde_json::to_vec(event)?;
        self.transport.publish(TELEMETRY_ERRORS, &payload)
    }
}

/// Extract subsystem name from a metric following the naming convention.
/// Format: `sentinel.{subsystem}.{operation}.{type}`
#[cfg(feature = "telemetry")]
fn extract_subsystem(metric_name: &str) -> Option<&str> {
    let parts: Vec<&str> = metric_name.splitn(3, '.').collect();
    if parts.len() >= 2 && parts[0] == "sentinel" {
        Some(parts[1])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type MessageLog = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

    /// Mock transport that records all published messages.
    struct MockTransport {
        messages: MessageLog,
    }

    impl MockTransport {
        fn new() -> (Self, MessageLog) {
            let messages = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    messages: Arc::clone(&messages),
                },
                messages,
            )
        }
    }

    impl TelemetryTransport for MockTransport {
        fn publish(
            &self,
            topic: &str,
            payload: &[u8],
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.messages
                .lock()
                .unwrap()
                .push((topic.to_string(), payload.to_vec()));
            Ok(())
        }
    }

    #[test]
    fn test_export_error_event() {
        use crate::errors::{ErrorEvent, ErrorSeverity};
        use sentinel_common::{AgentId, Tick, Timestamp};

        let (transport, messages) = MockTransport::new();
        let exporter = TelemetryExporter::new(transport, ExporterConfig::default());

        let event = ErrorEvent {
            severity: ErrorSeverity::Transient,
            subsystem: "zenoh".to_string(),
            message: "Connection timeout".to_string(),
            retryable: true,
            agent_id: Some(AgentId(3)),
            tick: Some(Tick(42)),
            timestamp: Timestamp(1000),
        };

        exporter.export_error(&event).unwrap();

        let msgs = messages.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].0, "sentinel/telemetry/errors");

        // Verify payload is valid JSON
        let deserialized: ErrorEvent = serde_json::from_slice(&msgs[0].1).unwrap();
        assert_eq!(deserialized.subsystem, "zenoh");
    }

    #[test]
    fn test_extract_subsystem() {
        assert_eq!(extract_subsystem("sentinel.redb.read.count"), Some("redb"));
        assert_eq!(
            extract_subsystem("sentinel.zenoh.publish.latency_us"),
            Some("zenoh")
        );
        assert_eq!(extract_subsystem("invalid"), None);
        assert_eq!(extract_subsystem("other.prefix.op"), None);
    }

    #[test]
    fn test_exporter_config_defaults() {
        let config = ExporterConfig::default();
        assert_eq!(config.metrics_interval, Duration::from_secs(1));
        assert_eq!(config.health_interval, Duration::from_secs(5));
    }
}
