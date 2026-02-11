//! Lightweight in-process metrics for Project Sentinel.
//!
//! No Prometheus dependency - just atomic counters and histograms.
//! Designed for Dashboard/API export via MetricsSnapshot.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use serde::Serialize;

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
// Snapshots (serializable for Dashboard/Export)
// ──────────────────────────────────────────────

/// Serializable snapshot of all metrics.
#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    pub counters: HashMap<String, u64>,
    pub histograms: HashMap<String, HistogramSnapshot>,
}

/// Serializable snapshot of a single histogram.
#[derive(Debug, Clone, Serialize)]
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

static GLOBAL_METRICS: OnceLock<MetricsRegistry> = OnceLock::new();

impl MetricsRegistry {
    /// Get the global metrics registry (created on first access).
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

    /// Take a snapshot of all metrics for export.
    pub fn snapshot(&self) -> MetricsSnapshot {
        let counters = self.counters.read().unwrap();
        let histograms = self.histograms.read().unwrap();

        MetricsSnapshot {
            counters: counters
                .iter()
                .map(|(k, v)| (k.clone(), v.get()))
                .collect(),
            histograms: histograms
                .iter()
                .map(|(k, v)| (k.clone(), v.snapshot()))
                .collect(),
        }
    }
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
    fn test_registry_snapshot() {
        let registry = MetricsRegistry {
            counters: RwLock::new(HashMap::new()),
            histograms: RwLock::new(HashMap::new()),
        };

        registry.counter("ops").increment_by(42);
        registry
            .histogram("latency", &[1.0, 5.0, 10.0])
            .observe(3.0);

        let snap = registry.snapshot();
        assert_eq!(*snap.counters.get("ops").unwrap(), 42);
        assert!(snap.histograms.contains_key("latency"));
        assert_eq!(snap.histograms.get("latency").unwrap().count, 1);
    }

    #[test]
    fn test_snapshot_serializable() {
        let registry = MetricsRegistry {
            counters: RwLock::new(HashMap::new()),
            histograms: RwLock::new(HashMap::new()),
        };
        registry.counter("test").increment();

        let snap = registry.snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("\"test\""));
        assert!(json.contains("1"));
    }
}
