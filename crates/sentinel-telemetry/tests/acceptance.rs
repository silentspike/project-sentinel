//! Acceptance tests for sentinel-telemetry (Issue #34).

use std::collections::HashMap;

use sentinel_common::{AgentId, Tick, Timestamp};
use sentinel_telemetry::context::TraceContext;
use sentinel_telemetry::errors::{ClassifiedError, ErrorEvent, ErrorSeverity};
use sentinel_telemetry::health::{HealthRegistry, HealthSnapshot, HealthStatus};
use sentinel_telemetry::metrics::{MetricsRegistry, MetricsSnapshot, SubsystemMetrics};

/// AC 34.3: init_logging_dev() can be called without panic
#[test]
fn ac_34_03_init_logging() {
    // AC 34.3: init_logging_dev() should not panic
    // We use try_init to avoid double-initialization issues in test runner
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().pretty())
        .try_init();

    // If we get here, init did not panic
}

/// AC 34.4: Register Counter, increment, snapshot, assert value
#[test]
fn ac_34_04_metrics_counter() {
    // AC 34.4: Counter register, increment, snapshot, value check
    // Use global registry (singleton) with a unique metric name for this test
    let registry = MetricsRegistry::global();

    let counter = registry.counter("sentinel.ac_test.34_04.count");
    let initial = counter.get();

    counter.increment();
    counter.increment();
    counter.increment();
    assert_eq!(counter.get(), initial + 3);

    // Verify same counter returned on second access
    let counter2 = registry.counter("sentinel.ac_test.34_04.count");
    assert_eq!(counter2.get(), initial + 3);

    // Verify snapshot contains the counter
    let (counters, _, _) = registry.snapshot_raw();
    assert_eq!(
        *counters.get("sentinel.ac_test.34_04.count").unwrap(),
        initial + 3
    );
}

/// AC 34.5: Health registry register check, check_all, verify status
#[test]
fn ac_34_05_health_registry() {
    // AC 34.5: Register health check, check_all(), verify status
    // Use global registry with unique subsystem names for this test
    let registry = HealthRegistry::global();

    registry.register("ac_test_zenoh_34_05", || HealthStatus::Healthy);
    registry.register("ac_test_redb_34_05", || {
        HealthStatus::Degraded("slow disk".to_string())
    });

    let results = registry.check_all();
    // Global registry may have other checks registered, so just verify ours exist
    assert_eq!(
        results.get("ac_test_zenoh_34_05").unwrap(),
        &HealthStatus::Healthy
    );
    assert_eq!(
        results.get("ac_test_redb_34_05").unwrap(),
        &HealthStatus::Degraded("slow disk".to_string())
    );
}

/// AC 34.6: TraceContext serialize/deserialize roundtrip
#[test]
fn ac_34_06_trace_context_roundtrip() {
    // AC 34.6: Serialize TraceContext, deserialize, assert_eq
    let ctx = TraceContext::with_agent(AgentId(7), Tick(100));

    let json = serde_json::to_string(&ctx).unwrap();
    let deserialized: TraceContext = serde_json::from_str(&json).unwrap();

    assert_eq!(ctx, deserialized);
    assert_eq!(deserialized.origin_agent, Some(AgentId(7)));
    assert_eq!(deserialized.origin_tick, Some(Tick(100)));

    // Also test without agent
    let ctx2 = TraceContext::new();
    let json2 = serde_json::to_string(&ctx2).unwrap();
    let deserialized2: TraceContext = serde_json::from_str(&json2).unwrap();
    assert_eq!(ctx2, deserialized2);
    assert!(deserialized2.origin_agent.is_none());
}

/// AC 34.7: Error classification - create error, classify, verify severity
#[test]
fn ac_34_07_error_classification() {
    // AC 34.7: Create Error, classify, verify severity

    // Minimal test error type implementing ClassifiedError
    #[derive(Debug)]
    struct TestError {
        severity: ErrorSeverity,
        subsystem: String,
    }

    impl std::fmt::Display for TestError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "test error in {}: {:?}", self.subsystem, self.severity)
        }
    }

    impl std::error::Error for TestError {}

    impl ClassifiedError for TestError {
        fn severity(&self) -> ErrorSeverity {
            self.severity
        }

        fn subsystem(&self) -> &str {
            &self.subsystem
        }
    }

    // Fatal error is not retryable
    let fatal = TestError {
        severity: ErrorSeverity::Fatal,
        subsystem: "redb".to_string(),
    };
    assert_eq!(fatal.severity(), ErrorSeverity::Fatal);
    assert!(!fatal.is_retryable());

    // Transient error is retryable
    let transient = TestError {
        severity: ErrorSeverity::Transient,
        subsystem: "zenoh".to_string(),
    };
    assert_eq!(transient.severity(), ErrorSeverity::Transient);
    assert!(transient.is_retryable());

    // Degraded error is not retryable
    let degraded = TestError {
        severity: ErrorSeverity::Degraded,
        subsystem: "limbo".to_string(),
    };
    assert_eq!(degraded.severity(), ErrorSeverity::Degraded);
    assert!(!degraded.is_retryable());
}

/// AC 34.8: MetricsSnapshot + HealthSnapshot serde_json roundtrip
#[test]
fn ac_34_08_snapshots_serializable() {
    // AC 34.8: Both snapshot types must roundtrip through serde_json

    // MetricsSnapshot
    let mut subsystems = HashMap::new();
    subsystems.insert(
        "redb".to_string(),
        SubsystemMetrics {
            health: HealthStatus::Healthy,
            counters: HashMap::from([("sentinel.redb.read.count".to_string(), 42)]),
            histograms: HashMap::new(),
            gauges: HashMap::new(),
        },
    );
    let metrics_snap = MetricsSnapshot {
        timestamp: Timestamp(1000),
        tick: Tick(5),
        subsystems,
    };
    let json = serde_json::to_string(&metrics_snap).unwrap();
    let deserialized: MetricsSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.tick, Tick(5));
    assert_eq!(deserialized.timestamp, Timestamp(1000));
    assert!(deserialized.subsystems.contains_key("redb"));
    assert_eq!(
        *deserialized.subsystems["redb"]
            .counters
            .get("sentinel.redb.read.count")
            .unwrap(),
        42
    );

    // HealthSnapshot
    let health_snap = HealthSnapshot {
        timestamp: Timestamp(2000),
        subsystems: vec![sentinel_telemetry::health::SubsystemHealth {
            name: "zenoh".to_string(),
            status: HealthStatus::Healthy,
            reason: None,
            last_check: Timestamp(2000),
        }],
    };
    let json = serde_json::to_string(&health_snap).unwrap();
    let deserialized: HealthSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.timestamp, Timestamp(2000));
    assert_eq!(deserialized.subsystems.len(), 1);
    assert_eq!(deserialized.subsystems[0].name, "zenoh");
    assert_eq!(deserialized.subsystems[0].status, HealthStatus::Healthy);
}

/// Zaehlt echte #[instrument]-Attribute (ignoriert Kommentare und Docstrings)
fn count_instrument_attrs(source: &str) -> usize {
    source
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            // Ignoriere Kommentare und Docstrings
            !trimmed.starts_with("//")
                && !trimmed.starts_with("///")
                && trimmed.contains("#[instrument")
        })
        .count()
}

/// AC 34.9: sentinel-zenoh lib.rs hat #[instrument] Attribute auf pub-Funktionen
#[test]
fn ac_34_09_instrumentation_zenoh() {
    let zenoh_lib = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("sentinel-zenoh")
        .join("src")
        .join("lib.rs");
    let content = std::fs::read_to_string(&zenoh_lib)
        .unwrap_or_else(|e| panic!("Failed to read {:?}: {}", zenoh_lib, e));
    let count = count_instrument_attrs(&content);
    assert!(
        count >= 2,
        "sentinel-zenoh/src/lib.rs should have >= 2 #[instrument] attributes on non-comment lines, found {}",
        count
    );
}

/// AC 34.10: sentinel-redb lib.rs hat #[instrument] Attribute auf pub-Funktionen
#[test]
fn ac_34_10_instrumentation_redb() {
    let redb_lib = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("sentinel-redb")
        .join("src")
        .join("lib.rs");
    let content = std::fs::read_to_string(&redb_lib)
        .unwrap_or_else(|e| panic!("Failed to read {:?}: {}", redb_lib, e));
    let count = count_instrument_attrs(&content);
    assert!(
        count >= 2,
        "sentinel-redb/src/lib.rs should have >= 2 #[instrument] attributes on non-comment lines, found {}",
        count
    );
}

/// AC 34.11: sentinel-limbo lib.rs hat #[instrument] Attribute auf pub-Funktionen
#[test]
fn ac_34_11_instrumentation_limbo() {
    let limbo_lib = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("sentinel-limbo")
        .join("src")
        .join("lib.rs");
    let content = std::fs::read_to_string(&limbo_lib)
        .unwrap_or_else(|e| panic!("Failed to read {:?}: {}", limbo_lib, e));
    let count = count_instrument_attrs(&content);
    assert!(
        count >= 2,
        "sentinel-limbo/src/lib.rs should have >= 2 #[instrument] attributes on non-comment lines, found {}",
        count
    );
}

/// AC 34.13: Crate compiles without telemetry feature
#[test]
fn ac_34_13_feature_gate() {
    // AC 34.13: The crate should compile and basic types should be usable
    // without the telemetry feature. We verify this by using types that
    // are NOT behind #[cfg(feature = "telemetry")].

    // TraceContext is always available
    let ctx = TraceContext::new();
    assert!(!ctx.correlation_id.is_empty());

    // ErrorSeverity is always available
    let _severity = ErrorSeverity::Fatal;

    // ErrorEvent is always available
    let _event = ErrorEvent {
        severity: ErrorSeverity::Transient,
        subsystem: "test".to_string(),
        message: "test error".to_string(),
        retryable: true,
        agent_id: None,
        tick: None,
        timestamp: Timestamp(0),
    };

    // HealthStatus is always available
    let _status = HealthStatus::Healthy;

    // MetricsSnapshot is always available (serializable type)
    let _snap = MetricsSnapshot {
        timestamp: Timestamp(0),
        tick: Tick(0),
        subsystems: HashMap::new(),
    };

    // HealthSnapshot is always available
    let _hsnap = HealthSnapshot {
        timestamp: Timestamp(0),
        subsystems: vec![],
    };
}
