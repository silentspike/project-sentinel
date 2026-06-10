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
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
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
// Gauge
// ──────────────────────────────────────────────

/// Atomic gauge metric. Thread-safe, lock-free.
///
/// Unlike Counter, a Gauge can go up and down. Useful for tracking
/// current values like in-flight queries, active connections, etc.
pub struct Gauge {
    value: AtomicI64,
}

impl Gauge {
    fn new() -> Self {
        Self {
            value: AtomicI64::new(0),
        }
    }

    /// Set to an absolute value.
    pub fn set(&self, val: i64) {
        self.value.store(val, Ordering::Relaxed);
    }

    /// Increment by 1.
    pub fn increment(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement by 1.
    pub fn decrement(&self) {
        self.value.fetch_sub(1, Ordering::Relaxed);
    }

    /// Get current value.
    pub fn get(&self) -> i64 {
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
        let buckets = (0..bucket_count).map(|_| AtomicU64::new(0)).collect();

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
                .compare_exchange_weak(current_bits, new_bits, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }

        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// Get a snapshot of bucket counts, including estimated p50 and p99.
    pub fn snapshot(&self) -> HistogramSnapshot {
        let bucket_counts: Vec<u64> = self
            .buckets
            .iter()
            .map(|b| b.load(Ordering::Relaxed))
            .collect();
        let count = self.count.load(Ordering::Relaxed);

        let p50 = percentile_from_buckets(&self.boundaries, &bucket_counts, count, 0.50);
        let p95 = percentile_from_buckets(&self.boundaries, &bucket_counts, count, 0.95);
        let p99 = percentile_from_buckets(&self.boundaries, &bucket_counts, count, 0.99);

        HistogramSnapshot {
            boundaries: self.boundaries.clone(),
            bucket_counts,
            sum: f64::from_bits(self.sum_bits.load(Ordering::Relaxed)),
            count,
            p50,
            p95,
            p99,
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
    /// Gauges with value != 0 (lazy: zero-value gauges omitted).
    pub gauges: HashMap<String, i64>,
}

/// Serializable snapshot of a single histogram.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistogramSnapshot {
    pub boundaries: Vec<f64>,
    pub bucket_counts: Vec<u64>,
    pub sum: f64,
    pub count: u64,
    /// Estimated 50th percentile (median): upper boundary of the median bucket.
    pub p50: f64,
    /// Estimated 95th percentile: upper boundary of the p95 bucket.
    pub p95: f64,
    /// Estimated 99th percentile: upper boundary of the p99 bucket.
    pub p99: f64,
}

// ──────────────────────────────────────────────
// MetricsRegistry (global singleton)
// ──────────────────────────────────────────────

/// Global metrics registry. Access via `MetricsRegistry::global()`.
pub struct MetricsRegistry {
    counters: RwLock<HashMap<String, Arc<Counter>>>,
    histograms: RwLock<HashMap<String, Arc<Histogram>>>,
    gauges: RwLock<HashMap<String, Arc<Gauge>>>,
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
            gauges: RwLock::new(HashMap::new()),
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
        Arc::clone(
            counters
                .entry(name.to_string())
                .or_insert_with(|| Arc::new(Counter::new())),
        )
    }

    /// Get or create a gauge by name.
    pub fn gauge(&self, name: &str) -> Arc<Gauge> {
        // Fast path: read lock
        {
            let gauges = self.gauges.read().unwrap();
            if let Some(g) = gauges.get(name) {
                return Arc::clone(g);
            }
        }
        // Slow path: write lock
        let mut gauges = self.gauges.write().unwrap();
        Arc::clone(
            gauges
                .entry(name.to_string())
                .or_insert_with(|| Arc::new(Gauge::new())),
        )
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
    pub fn snapshot_raw(
        &self,
    ) -> (
        HashMap<String, u64>,
        HashMap<String, HistogramSnapshot>,
        HashMap<String, i64>,
    ) {
        let counters = self.counters.read().unwrap();
        let histograms = self.histograms.read().unwrap();
        let gauges = self.gauges.read().unwrap();

        let filtered_counters: HashMap<String, u64> = counters
            .iter()
            .filter_map(|(k, v)| {
                let val = v.get();
                if val > 0 {
                    Some((k.clone(), val))
                } else {
                    None
                }
            })
            .collect();

        let filtered_histograms: HashMap<String, HistogramSnapshot> = histograms
            .iter()
            .filter_map(|(k, v)| {
                let snap = v.snapshot();
                if snap.count > 0 {
                    Some((k.clone(), snap))
                } else {
                    None
                }
            })
            .collect();

        let filtered_gauges: HashMap<String, i64> = gauges
            .iter()
            .filter_map(|(k, v)| {
                let val = v.get();
                if val != 0 {
                    Some((k.clone(), val))
                } else {
                    None
                }
            })
            .collect();

        (filtered_counters, filtered_histograms, filtered_gauges)
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

/// Estimate a percentile from histogram buckets.
///
/// Returns the **upper boundary of the bucket** containing the target rank
/// (no interpolation within the bucket) — quantiles are therefore quantized
/// to the configured boundaries. Returns 0.0 if count is 0.
fn percentile_from_buckets(
    boundaries: &[f64],
    bucket_counts: &[u64],
    total: u64,
    quantile: f64,
) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let target = (total as f64 * quantile).ceil() as u64;
    let mut cumulative: u64 = 0;

    for (i, &count) in bucket_counts.iter().enumerate() {
        cumulative += count;
        if cumulative >= target {
            // Return upper boundary of this bucket (or last boundary for +Inf bucket)
            return if i < boundaries.len() {
                boundaries[i]
            } else {
                // +Inf bucket: best estimate is last boundary (or sum/total)
                boundaries.last().copied().unwrap_or(0.0)
            };
        }
    }
    boundaries.last().copied().unwrap_or(0.0)
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
    fn test_gauge_basic() {
        let gauge = Gauge::new();
        assert_eq!(gauge.get(), 0);
        gauge.increment();
        assert_eq!(gauge.get(), 1);
        gauge.increment();
        assert_eq!(gauge.get(), 2);
        gauge.decrement();
        assert_eq!(gauge.get(), 1);
        gauge.set(42);
        assert_eq!(gauge.get(), 42);
        gauge.set(-5);
        assert_eq!(gauge.get(), -5);
    }

    #[test]
    fn test_registry_gauge() {
        let registry = MetricsRegistry {
            counters: RwLock::new(HashMap::new()),
            histograms: RwLock::new(HashMap::new()),
            gauges: RwLock::new(HashMap::new()),
        };

        let g1 = registry.gauge("test.inflight");
        g1.increment();
        let g2 = registry.gauge("test.inflight");
        g2.increment();

        // Gleiche Gauge-Instanz
        assert_eq!(g1.get(), 2);
        assert_eq!(g2.get(), 2);
    }

    #[test]
    fn test_registry_counter() {
        let registry = MetricsRegistry {
            counters: RwLock::new(HashMap::new()),
            histograms: RwLock::new(HashMap::new()),
            gauges: RwLock::new(HashMap::new()),
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
            gauges: RwLock::new(HashMap::new()),
        };

        registry.counter("ops").increment_by(42);
        registry
            .histogram("latency", &[1.0, 5.0, 10.0])
            .observe(3.0);
        registry.gauge("inflight").set(5);

        let (counters, histograms, gauges) = registry.snapshot_raw();
        assert_eq!(*counters.get("ops").unwrap(), 42);
        assert!(histograms.contains_key("latency"));
        assert_eq!(histograms.get("latency").unwrap().count, 1);
        assert_eq!(*gauges.get("inflight").unwrap(), 5);
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
            gauges: RwLock::new(HashMap::new()),
        };

        // Create metrics but don't increment some
        registry.counter("active").increment();
        registry.counter("inactive"); // value = 0
        registry.histogram("used", &[1.0]).observe(0.5);
        registry.histogram("unused", &[1.0]); // count = 0
        registry.gauge("nonzero").set(3);
        registry.gauge("zero"); // value = 0

        let (counters, histograms, gauges) = registry.snapshot_raw();

        // Only non-zero metrics appear
        assert!(counters.contains_key("active"));
        assert!(!counters.contains_key("inactive"));
        assert!(histograms.contains_key("used"));
        assert!(!histograms.contains_key("unused"));
        assert!(gauges.contains_key("nonzero"));
        assert!(!gauges.contains_key("zero"));
    }

    #[test]
    fn test_metrics_snapshot_serializable() {
        use crate::health::HealthStatus;
        use sentinel_common::{Tick, Timestamp};

        let mut subsystems = HashMap::new();
        subsystems.insert(
            "redb".to_string(),
            SubsystemMetrics {
                health: HealthStatus::Healthy,
                counters: HashMap::from([("ops".to_string(), 42)]),
                histograms: HashMap::new(),
                gauges: HashMap::new(),
            },
        );

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

    #[test]
    fn test_histogram_percentiles() {
        let hist = Histogram::new(&[10.0, 50.0, 100.0, 500.0]);
        // 50 fast observations, 49 medium, 1 slow
        for _ in 0..50 {
            hist.observe(5.0); // bucket <=10
        }
        for _ in 0..49 {
            hist.observe(30.0); // bucket <=50
        }
        hist.observe(200.0); // bucket <=500

        let snap = hist.snapshot();
        assert_eq!(snap.count, 100);
        // p50 should be in the <=10 bucket (50% of data is <=10)
        assert_eq!(snap.p50, 10.0);
        // p95 is at observation 95, which lies in the <=50 bucket
        assert_eq!(snap.p95, 50.0);
        // p99 should be in the <=50 bucket (99% is at observation 99, which is <=50)
        assert_eq!(snap.p99, 50.0);
    }

    #[test]
    fn test_histogram_p95_known_distribution() {
        let hist = Histogram::new(&[1.0, 10.0, 100.0]);
        // 95 observations <=1, 5 observations in the <=100 bucket:
        // rank 95 falls exactly into the first bucket, rank 99 into <=100.
        for _ in 0..95 {
            hist.observe(0.5);
        }
        for _ in 0..5 {
            hist.observe(50.0);
        }
        let snap = hist.snapshot();
        assert_eq!(snap.p50, 1.0);
        assert_eq!(snap.p95, 1.0);
        assert_eq!(snap.p99, 100.0);
        assert!(snap.p50 <= snap.p95 && snap.p95 <= snap.p99);
    }

    #[test]
    fn test_histogram_empty_percentiles() {
        let hist = Histogram::new(&[10.0, 100.0]);
        let snap = hist.snapshot();
        assert_eq!(snap.p50, 0.0);
        assert_eq!(snap.p95, 0.0);
        assert_eq!(snap.p99, 0.0);
    }
}
