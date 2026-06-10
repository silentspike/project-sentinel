//! Criterion benchmarks for sentinel-telemetry metrics primitives.
//!
//! Verifies the performance budget from Issue #34:
//! - Counter increment: < 1 ns
//! - Histogram observe:  < 5 ns
//! - Gauge operations:   < 1 ns
//! - Metrics snapshot:   < 100 µs

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use sentinel_telemetry::metrics::MetricsRegistry;

// ──────────────────────────────────────────────
// 1. Counter — Budget: < 1 ns per increment
// ──────────────────────────────────────────────

fn bench_counter_increment(c: &mut Criterion) {
    let mut group = c.benchmark_group("counter");

    let registry = MetricsRegistry::global();
    let counter = registry.counter("bench.counter.inc");

    group.bench_function("increment", |b| {
        b.iter(|| {
            counter.increment();
        });
    });

    group.bench_function("increment_by", |b| {
        b.iter(|| {
            counter.increment_by(black_box(7));
        });
    });

    group.bench_function("get", |b| {
        b.iter(|| {
            black_box(counter.get());
        });
    });

    group.finish();
}

// ──────────────────────────────────────────────
// 2. Histogram — Budget: < 5 ns per observe
// ──────────────────────────────────────────────

fn bench_histogram_observe(c: &mut Criterion) {
    let mut group = c.benchmark_group("histogram");

    let registry = MetricsRegistry::global();

    // 4-bucket histogram (typical use case)
    let hist_4 = registry.histogram("bench.hist.4bucket", &[10.0, 50.0, 100.0, 500.0]);
    group.bench_function("observe_4_buckets", |b| {
        let mut i = 0u64;
        b.iter(|| {
            i = i.wrapping_add(1);
            hist_4.observe(black_box((i % 600) as f64));
        });
    });

    // 8-bucket histogram (#381 default candidate)
    let boundaries_8: Vec<f64> = (1..=8).map(|i| i as f64 * 25.0).collect();
    let hist_8 = registry.histogram("bench.hist.8bucket", &boundaries_8);
    group.bench_function("observe_8_buckets", |b| {
        let mut i = 0u64;
        b.iter(|| {
            i = i.wrapping_add(1);
            hist_8.observe(black_box((i % 250) as f64));
        });
    });

    // 16-bucket histogram (stress test — more buckets = more linear scan)
    let boundaries_16: Vec<f64> = (1..=16).map(|i| i as f64 * 10.0).collect();
    let hist_16 = registry.histogram("bench.hist.16bucket", &boundaries_16);
    group.bench_function("observe_16_buckets", |b| {
        let mut i = 0u64;
        b.iter(|| {
            i = i.wrapping_add(1);
            hist_16.observe(black_box((i % 200) as f64));
        });
    });

    // Snapshot (cold path, not hot-path critical)
    group.bench_function("snapshot_4_buckets", |b| {
        b.iter(|| {
            black_box(hist_4.snapshot());
        });
    });

    group.finish();
}

// ──────────────────────────────────────────────
// 2b. Phase-Histogramme (#381) — Budget: 10x observe < 1 µs/Tick
//
// Bucket-Sweep 4/8/16 mit identischer, phasen-typischer Werteverteilung
// (µs-Bereich dominiert, einzelne ms- und >100-ms-Ausreisser) — findet das
// beste Setting fuer PHASE_DURATION_BOUNDARIES_MS auf der Deploy-VM.
// ──────────────────────────────────────────────

/// Phasen-typische Dauer in ms: ~80% µs-Bereich, ~18% einstellige ms,
/// ~2% Persist-artige Ausreisser. Deterministisch (kein RNG im Hot-Loop).
fn phase_like_value_ms(i: u64) -> f64 {
    match i % 50 {
        0 => 120.0 + (i % 7) as f64 * 30.0, // seltener Ausreisser (persist unter Last)
        n if n < 10 => 1.0 + (i % 9) as f64 * 0.5, // einstellige ms
        _ => 0.002 + (i % 40) as f64 * 0.004, // 2-160 µs
    }
}

fn bench_phase_histogram(c: &mut Criterion) {
    use sentinel_telemetry::{phase_metric_name, PHASE_DURATION_BOUNDARIES_MS};

    let mut group = c.benchmark_group("phase_histogram");
    let registry = MetricsRegistry::global();

    let sweep_4: Vec<f64> = vec![0.05, 1.0, 25.0, 500.0];
    let sweep_8: Vec<f64> = PHASE_DURATION_BOUNDARIES_MS.to_vec();
    let sweep_16: Vec<f64> = vec![
        0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0,
        500.0,
    ];

    for (label, boundaries) in [
        ("observe_phase_4_buckets", &sweep_4),
        ("observe_phase_8_buckets", &sweep_8),
        ("observe_phase_16_buckets", &sweep_16),
    ] {
        let hist = registry.histogram(&format!("bench.phase.{label}"), boundaries);
        group.bench_function(label, |b| {
            let mut i = 0u64;
            b.iter(|| {
                i = i.wrapping_add(1);
                hist.observe(black_box(phase_like_value_ms(i)));
            });
        });
    }

    // Realer Per-Tick-Pfad des Daemons: 10 Phasen-Histogramme nacheinander
    // recorden (orchestrator.rs nach schedule.run). Budget: < 1 µs gesamt.
    let phase_names = [
        "input",
        "biology",
        "physics",
        "transit",
        "chaos",
        "mood",
        "perception",
        "decision",
        "output",
        "persist",
    ];
    let phase_hists: Vec<_> = phase_names
        .iter()
        .map(|p| registry.histogram(&phase_metric_name(p), &PHASE_DURATION_BOUNDARIES_MS))
        .collect();
    group.bench_function("record_all_10_phases_per_tick", |b| {
        let mut i = 0u64;
        b.iter(|| {
            i = i.wrapping_add(1);
            for (k, hist) in phase_hists.iter().enumerate() {
                hist.observe(black_box(phase_like_value_ms(i.wrapping_add(k as u64))));
            }
        });
    });

    group.finish();
}

// ──────────────────────────────────────────────
// 3. Gauge — Budget: < 1 ns per operation
// ──────────────────────────────────────────────

fn bench_gauge_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("gauge");

    let registry = MetricsRegistry::global();
    let gauge = registry.gauge("bench.gauge.ops");

    group.bench_function("set", |b| {
        let mut i = 0i64;
        b.iter(|| {
            i = i.wrapping_add(1);
            gauge.set(black_box(i));
        });
    });

    group.bench_function("increment", |b| {
        b.iter(|| {
            gauge.increment();
        });
    });

    group.bench_function("decrement", |b| {
        b.iter(|| {
            gauge.decrement();
        });
    });

    group.bench_function("get", |b| {
        b.iter(|| {
            black_box(gauge.get());
        });
    });

    group.finish();
}

// ──────────────────────────────────────────────
// 4. Registry Lookup — Hot-Path (read lock)
// ──────────────────────────────────────────────

fn bench_registry_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("registry_lookup");

    let registry = MetricsRegistry::global();

    // Pre-register metrics so we hit the fast read-lock path
    let _ = registry.counter("bench.lookup.counter");
    let _ = registry.histogram("bench.lookup.histogram", &[1.0, 5.0, 10.0]);
    let _ = registry.gauge("bench.lookup.gauge");

    group.bench_function("counter_existing", |b| {
        b.iter(|| {
            black_box(registry.counter("bench.lookup.counter"));
        });
    });

    group.bench_function("histogram_existing", |b| {
        b.iter(|| {
            black_box(registry.histogram("bench.lookup.histogram", &[1.0, 5.0, 10.0]));
        });
    });

    group.bench_function("gauge_existing", |b| {
        b.iter(|| {
            black_box(registry.gauge("bench.lookup.gauge"));
        });
    });

    group.finish();
}

// ──────────────────────────────────────────────
// 5. Snapshot — Budget: < 100 µs
// ──────────────────────────────────────────────

fn bench_snapshot_raw(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_raw");

    let registry = MetricsRegistry::global();

    // Populate with a realistic number of metrics.
    // Note: global registry accumulates metrics across benchmark groups,
    // so actual count is higher. This is conservative — if snapshot_raw
    // stays under budget with extra metrics, it will also pass with fewer.
    for i in 0..100 {
        registry
            .counter(&format!("bench.snap.counter.{i}"))
            .increment_by(i as u64 + 1);
    }
    for i in 0..50 {
        registry
            .histogram(&format!("bench.snap.hist.{i}"), &[10.0, 50.0, 100.0, 500.0])
            .observe((i as f64 + 1.0) * 5.0);
    }
    for i in 0..50 {
        registry
            .gauge(&format!("bench.snap.gauge.{i}"))
            .set(i as i64 + 1);
    }

    group.bench_function("200_metrics", |b| {
        b.iter(|| {
            black_box(registry.snapshot_raw());
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_counter_increment,
    bench_histogram_observe,
    bench_phase_histogram,
    bench_gauge_operations,
    bench_registry_lookup,
    bench_snapshot_raw,
);
criterion_main!(benches);
