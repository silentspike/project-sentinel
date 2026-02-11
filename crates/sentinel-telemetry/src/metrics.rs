//! Lightweight in-process metrics for Project Sentinel.
//!
//! No Prometheus dependency - just atomic counters and histograms.
//! Designed for Dashboard/API export via MetricsSnapshot.
//!
//! # Naming Convention
//!
//! All metric names follow the pattern:
//! ```text
//! sentinel.{crate}.{operation}.{metric_type}
//! ```
//!
//! Where `metric_type` is one of:
//! - `count` - Number of occurrences (Counter)
//! - `duration_us` - Duration in microseconds (Histogram)
//! - `size_bytes` - Size in bytes (Histogram)
//!
//! Examples:
//! ```text
//! sentinel.redb.get_agent_state.count
//! sentinel.redb.get_agent_state.duration_us
//! sentinel.zenoh.publish.count
//! sentinel.limbo.insert_message.duration_us
//! ```
//!
//! Use [`metric_name`] to construct names consistently.
//!
//! # Lock-Free Guarantees
//!
//! [`Counter::increment`] and [`Histogram::observe`] are fully lock-free
//! (atomic operations only). The [`RwLock`] in [`MetricsRegistry`] is only
//! taken during metric registration (first access), never on the hot
//! increment/observe path.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "telemetry")]
use std::sync::OnceLock;
use std::sync::{Arc, RwLock};

use sentinel_common::{Tick, Timestamp};
use serde::{Deserialize, Serialize};

use crate::health::HealthStatus;

// ──────────────────────────────────────────────
// Counter
// ──────────────────────────────────────────────

/// Atomic counter metric. Thread-safe, lock-free.
pub struct Counter {
    value: AtomicU64,
}

impl Counter {
    fn new() -> Self {
        Self {
            value: AtomicU64::new(0),
        }
    }

    /// Increment by 1.
    pub fn increment(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment by a given amount.
    pub fn increment_by(&self, n: u64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    /// Get current value.
    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }
}

// ──────────────────────────────────────────────
// Histogram
// ──────────────────────────────────────────────

/// Fixed-bucket histogram. Thread-safe via atomics.
///
/// Buckets are defined at creation time and cannot be changed.
/// Each observation is placed into the appropriate bucket.
pub struct Histogram {
    /// Bucket boundaries (sorted, exclusive upper bounds)
    boundaries: Vec<f64>,
    /// Count per bucket (len = boundaries.len() + 1 for the +Inf bucket)
    buckets: Vec<AtomicU64>,
    /// Sum of all observed values (stored as f64 bits in AtomicU64)
    sum_bits: AtomicU64,
    /// Total number of observations
    count: AtomicU64,
}

impl Histogram {
    fn new(boundaries: &[f64]) -> Self {
        let mut sorted = boundaries.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        sorted.dedup();

        let bucket_count = sorted.len() + 1; // +1 fuer +Inf
        let buckets = (0..bucket_count)
            .map(|_| AtomicU64::new(0))
            .collect();

        Self {
            boundaries: sorted,
            buckets,
            sum_bits: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    /// Record an observation.
    pub fn observe(&self, value: f64) {
        // Find the bucket for this value
        let idx = self
            .boundaries
            .iter()
            .position(|&b| value <= b)
            .unwrap_or(self.boundaries.len());
        self.buckets[idx].fetch_add(1, Ordering::Relaxed);

        // Atomically add to sum using CAS loop on f64 bits
        loop {
            let current_bits = self.sum_bits.load(Ordering::Relaxed);
            let current = f64::from_bits(current_bits);
            let new = current + value;
            let new_bits = new.to_bits();
            if self
                .sum_bits
                .compare_exchange_weak(
                    current_bits,
                    new_bits,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                break;
            }
        }

        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// Get a snapshot of bucket counts.
    pub fn snapshot(&self) -> HistogramSnapshot {
        let bucket_counts: Vec<u64> = self
            .buckets
            .iter()
            .map(|b| b.load(Ordering::Relaxed))
            .collect();

        HistogramSnapshot {
            boundaries: self.boundaries.clone(),
            bucket_counts,
            sum: f64::from_bits(self.sum_bits.load(Ordering::Relaxed)),
            count: self.count.load(Ordering::Relaxed),
        }
    }
}

// ──────────────────────────────────────────────
// Snapshots (Dashboard-ready, MessagePack transport)
// ──────────────────────────────────────────────

/// Top-level metrics snapshot for Dashboard consumption.
///
/// Serialized as MessagePack over Zenoh to `sentinel/telemetry/metrics`.
/// The Dashboard subscribes directly - no intermediate layer.
///
/// **Lazy filtering:** Only subsystems with at least one non-zero metric
/// appear. Counters with value 0 and histograms with count 0 are omitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    /// Wall-clock timestamp when snapshot was taken.
    pub timestamp: Timestamp,
    /// Simulation tick at snapshot time.
    pub tick: Tick,
    /// Per-subsystem metrics (key = subsystem name, e.g. "redb", "zenoh").
    pub subsystems: HashMap<String, SubsystemMetrics>,
}

/// Metrics for a single subsystem, including health status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubsystemMetrics {
    /// Current health status of this subsystem.
    pub health: HealthStatus,
    /// Counters with value > 0 (lazy: zero-value counters omitted).
    pub counters: HashMap<String, u64>,
    /// Histograms with count > 0 (lazy: unused histograms omitted).
    pub histograms: HashMap<String, HistogramSnapshot>,
}

/// Serializable snapshot of a single histogram.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistogramSnapshot {
    pub boundaries: Vec<f64>,
    pub bucket_counts: Vec<u64>,
    pub sum: f64,
    pub count: u64,
}

// ──────────────────────────────────────────────
// MetricsRegistry (global singleton)
// ──────────────────────────────────────────────

/// Global metrics registry. Access via `MetricsRegistry::global()`.
pub struct MetricsRegistry {
    counters: RwLock<HashMap<String, Arc<Counter>>>,
    histograms: RwLock<HashMap<String, Arc<Histogram>>>,
}

#[cfg(feature = "telemetry")]
static GLOBAL_METRICS: OnceLock<MetricsRegistry> = OnceLock::new();

impl MetricsRegistry {
    /// Get the global metrics registry (created on first access).
    ///
    /// Only available with the `telemetry` feature (default: enabled).
    #[cfg(feature = "telemetry")]
    pub fn global() -> &'static Self {
        GLOBAL_METRICS.get_or_init(|| MetricsRegistry {
            counters: RwLock::new(HashMap::new()),
            histograms: RwLock::new(HashMap::new()),
        })
    }

    /// Get or create a counter by name.
    pub fn counter(&self, name: &str) -> Arc<Counter> {
        // Fast path: read lock
        {
            let counters = self.counters.read().unwrap();
            if let Some(c) = counters.get(name) {
                return Arc::clone(c);
            }
        }
        // Slow path: write lock
        let mut counters = self.counters.write().unwrap();
        Arc::clone(counters.entry(name.to_string()).or_insert_with(|| Arc::new(Counter::new())))
    }

    /// Get or create a histogram by name with given bucket boundaries.
    pub fn histogram(&self, name: &str, boundaries: &[f64]) -> Arc<Histogram> {
        // Fast path: read lock
        {
            let histograms = self.histograms.read().unwrap();
            if let Some(h) = histograms.get(name) {
                return Arc::clone(h);
            }
        }
        // Slow path: write lock
        let mut histograms = self.histograms.write().unwrap();
        Arc::clone(
            histograms
                .entry(name.to_string())
                .or_insert_with(|| Arc::new(Histogram::new(boundaries))),
        )
    }

    /// Take a raw snapshot of all counters and histograms.
    ///
    /// **Lazy filtering:** Only returns counters with value > 0 and
    /// histograms with count > 0. Zero-value metrics are omitted.
    ///
    /// Returns `(counters, histograms)` maps. Use these to build a
    /// [`MetricsSnapshot`] with timestamp, tick, and health data.
    pub fn snapshot_raw(&self) -> (HashMap<String, u64>, HashMap<String, HistogramSnapshot>) {
        let counters = self.counters.read().unwrap();
        let histograms = self.histograms.read().unwrap();

        let filtered_counters: HashMap<String, u64> = counters
            .iter()
            .filter_map(|(k, v)| {
                let val = v.get();
                if val > 0 { Some((k.clone(), val)) } else { None }
            })
            .collect();

        let filtered_histograms: HashMap<String, HistogramSnapshot> = histograms
            .iter()
            .filter_map(|(k, v)| {
                let snap = v.snapshot();
                if snap.count > 0 { Some((k.clone(), snap)) } else { None }
            })
            .collect();

        (filtered_counters, filtered_histograms)
    }
}

/// Build a metric name following the sentinel naming convention.
///
/// Format: `sentinel.{crate_name}.{operation}.{metric_type}`
///
/// # Examples
/// ```
/// use sentinel_telemetry::metrics::metric_name;
/// assert_eq!(
///     metric_name("redb", "get_agent_state", "duration_us"),
///     "sentinel.redb.get_agent_state.duration_us"
/// );
/// ```
pub fn metric_name(crate_name: &str, operation: &str, metric_type: &str) -> String {
    format!("sentinel.{crate_name}.{operation}.{metric_type}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counter_increment() {
        let counter = Counter::new();
        assert_eq!(counter.get(), 0);
        counter.increment();
        assert_eq!(counter.get(), 1);
        counter.increment_by(5);
        assert_eq!(counter.get(), 6);
    }

    #[test]
    fn test_histogram_observe() {
        let hist = Histogram::new(&[10.0, 50.0, 100.0]);
        hist.observe(5.0); // bucket 0: <=10
        hist.observe(25.0); // bucket 1: <=50
        hist.observe(75.0); // bucket 2: <=100
        hist.observe(200.0); // bucket 3: +Inf

        let snap = hist.snapshot();
        assert_eq!(snap.bucket_counts, vec![1, 1, 1, 1]);
        assert_eq!(snap.count, 4);
        assert!((snap.sum - 305.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_histogram_boundary_values() {
        let hist = Histogram::new(&[10.0, 50.0]);
        hist.observe(10.0); // exactly at boundary -> bucket 0 (<=10)
        hist.observe(50.0); // exactly at boundary -> bucket 1 (<=50)

        let snap = hist.snapshot();
        assert_eq!(snap.bucket_counts, vec![1, 1, 0]);
    }

    #[test]
    fn test_registry_counter() {
        let registry = MetricsRegistry {
            counters: RwLock::new(HashMap::new()),
            histograms: RwLock::new(HashMap::new()),
        };

        let c1 = registry.counter("test.requests");
        c1.increment();
        let c2 = registry.counter("test.requests");
        c2.increment();

        // Same counter instance
        assert_eq!(c1.get(), 2);
        assert_eq!(c2.get(), 2);
    }

    #[test]
    fn test_registry_snapshot_raw() {
        let registry = MetricsRegistry {
            counters: RwLock::new(HashMap::new()),
            histograms: RwLock::new(HashMap::new()),
        };

        registry.counter("ops").increment_by(42);
        registry
            .histogram("latency", &[1.0, 5.0, 10.0])
            .observe(3.0);

        let (counters, histograms) = registry.snapshot_raw();
        assert_eq!(*counters.get("ops").unwrap(), 42);
        assert!(histograms.contains_key("latency"));
        assert_eq!(histograms.get("latency").unwrap().count, 1);
    }

    #[test]
    fn test_metric_name() {
        assert_eq!(
            metric_name("redb", "get_agent_state", "duration_us"),
            "sentinel.redb.get_agent_state.duration_us"
        );
        assert_eq!(
            metric_name("zenoh", "publish", "count"),
            "sentinel.zenoh.publish.count"
        );
        assert_eq!(
            metric_name("limbo", "insert_message", "duration_us"),
            "sentinel.limbo.insert_message.duration_us"
        );
    }

    #[test]
    fn test_snapshot_lazy_filtering() {
        let registry = MetricsRegistry {
            counters: RwLock::new(HashMap::new()),
            histograms: RwLock::new(HashMap::new()),
        };

        // Create metrics but don't increment some
        registry.counter("active").increment();
        registry.counter("inactive"); // value = 0
        registry.histogram("used", &[1.0]).observe(0.5);
        registry.histogram("unused", &[1.0]); // count = 0

        let (counters, histograms) = registry.snapshot_raw();

        // Only non-zero metrics appear
        assert!(counters.contains_key("active"));
        assert!(!counters.contains_key("inactive"));
        assert!(histograms.contains_key("used"));
        assert!(!histograms.contains_key("unused"));
    }

    #[test]
    fn test_metrics_snapshot_serializable() {
        use sentinel_common::{Tick, Timestamp};
        use crate::health::HealthStatus;

        let mut subsystems = HashMap::new();
        subsystems.insert("redb".to_string(), SubsystemMetrics {
            health: HealthStatus::Healthy,
            counters: HashMap::from([("ops".to_string(), 42)]),
            histograms: HashMap::new(),
        });

        let snap = MetricsSnapshot {
            timestamp: Timestamp(1000),
            tick: Tick(5),
            subsystems,
        };

        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("\"redb\""));
        assert!(json.contains("42"));

        // Deserialize roundtrip
        let deserialized: MetricsSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.tick, Tick(5));
        assert!(deserialized.subsystems.contains_key("redb"));
    }
}
