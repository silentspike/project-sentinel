//! Benchmarks fuer EventStore Operationen.
//!
//! Misst IOPS-kritische Pfade auf SQLite (WAL mode).
//! Wichtig fuer DRAM-lose NVMe Ziel-Hardware.
//!
//! WICHTIG: Diese Benchmarks MUESSEN auf der Deployment-VM ausgefuehrt werden
//! (NICHT auf dem Build-Server/LXC). Siehe CLAUDE.md.
//!
//! Benchmark-Kategorien:
//! 1. Einzeloperationen (Latenz pro Call)
//! 2. Throughput-Szenario: 100 Ticks × 15 Agents (>100 ticks/s Schwellenwert)
//! 3. Realistisches Mixed-Workload Szenario

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use sentinel_common::DomainEvent;
use sentinel_limbo::EventStore;

/// Anzahl Agents pro Schicht (SSOT: config/rooms.toml + sprint2-domain.md)
const AGENTS_PER_SHIFT: u64 = 15;
/// Tick-Rate Schwellenwert aus CLAUDE.md (>100 ticks/s)
const MIN_TICKS_PER_SECOND: u64 = 100;

fn make_event(i: u64) -> DomainEvent {
    DomainEvent::new(
        "transit_started",
        &format!("AGENT-{:02}", (i % 15) + 1),
        &format!(r#"{{"step":{i}}}"#),
        "corr-bench",
        i * 100,
    )
}

fn bench_append_event(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench_append.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    let mut i = 0u64;
    c.bench_function("append_event", |b| {
        b.iter(|| {
            let event = make_event(i);
            black_box(store.append_event(&event).unwrap());
            i += 1;
        });
    });
}

fn bench_append_with_outbox(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench_outbox.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    let mut i = 0u64;
    c.bench_function("append_with_outbox", |b| {
        b.iter(|| {
            let event = make_event(i);
            black_box(
                store
                    .append_with_outbox(&event, "sentinel/events/bench")
                    .unwrap(),
            );
            i += 1;
        });
    });
}

fn bench_get_events_since(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench_query.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    // 1000 Events vorbereiten
    for i in 0..1000u64 {
        let event = make_event(i);
        store.append_event(&event).unwrap();
    }

    let mut group = c.benchmark_group("get_events_since");
    for limit in [10, 50, 100, 500] {
        group.bench_with_input(BenchmarkId::from_parameter(limit), &limit, |b, &limit| {
            b.iter(|| {
                black_box(store.get_events_since(0, limit).unwrap());
            });
        });
    }
    group.finish();
}

fn bench_get_events_by_aggregate(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench_aggregate.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    // 1000 Events verteilt auf 15 Agents
    for i in 0..1000u64 {
        let event = make_event(i);
        store.append_event(&event).unwrap();
    }

    c.bench_function("get_events_by_aggregate", |b| {
        b.iter(|| {
            black_box(store.get_events_by_aggregate("AGENT-01", 100).unwrap());
        });
    });
}

fn bench_save_snapshot(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench_snap.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    let mut version = 0i64;
    c.bench_function("save_snapshot", |b| {
        b.iter(|| {
            version += 1;
            black_box(
                store
                    .save_snapshot("AGENT-01", "bio_state", r#"{"hunger":42}"#, version)
                    .unwrap(),
            );
        });
    });
}

fn bench_get_latest_snapshot(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench_snap_read.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    // 10 Snapshots anlegen
    for i in 1..=10 {
        store
            .save_snapshot("AGENT-01", "bio_state", r#"{"hunger":42}"#, i)
            .unwrap();
    }

    c.bench_function("get_latest_snapshot", |b| {
        b.iter(|| {
            black_box(store.get_latest_snapshot("AGENT-01").unwrap());
        });
    });
}

fn bench_poll_outbox(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench_poll.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    // 500 Events mit Outbox
    for i in 0..500u64 {
        let event = make_event(i);
        store
            .append_with_outbox(&event, "sentinel/events/bench")
            .unwrap();
    }

    c.bench_function("poll_outbox_50", |b| {
        b.iter(|| {
            black_box(store.poll_outbox(50).unwrap());
        });
    });
}

fn bench_get_all_events(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench_rebuild.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    // 1000 Events fuer Rebuild-Szenario
    for i in 0..1000u64 {
        let event = make_event(i);
        store.append_event(&event).unwrap();
    }

    c.bench_function("get_all_events_1000", |b| {
        b.iter(|| {
            black_box(store.get_all_events().unwrap());
        });
    });
}

fn bench_event_count(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench_count.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    for i in 0..1000u64 {
        let event = make_event(i);
        store.append_event(&event).unwrap();
    }

    c.bench_function("event_count", |b| {
        b.iter(|| {
            black_box(store.event_count().unwrap());
        });
    });
}

// ──────────────────────────────────────────────
// Throughput: 100 Ticks mit 15 Agents
// ──────────────────────────────────────────────

/// Simuliert einen einzelnen Tick: Jeder der 15 Agents schreibt ein Event.
/// Misst ob ein Tick unter 10ms bleibt (= >100 ticks/s moeglich).
fn bench_single_tick_15_agents(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench_tick.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    let mut tick = 0u64;
    c.bench_function("single_tick_15_agents", |b| {
        b.iter(|| {
            for agent in 1..=AGENTS_PER_SHIFT {
                let event = DomainEvent::new(
                    "agent_action_received",
                    &format!("AGENT-{agent:02}"),
                    &format!(r#"{{"tick":{tick},"action":"work"}}"#),
                    &format!("corr-tick-{tick}"),
                    tick * 100,
                );
                black_box(store.append_event(&event).unwrap());
            }
            tick += 1;
        });
    });
}

/// Throughput-Test: 100 Ticks × 15 Agents = 1500 Events.
/// Validiert den >100 ticks/s Schwellenwert aus CLAUDE.md.
fn bench_100_ticks_15_agents_throughput(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench_throughput.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    c.bench_function("100_ticks_15_agents", |b| {
        b.iter(|| {
            for tick in 0..MIN_TICKS_PER_SECOND {
                for agent in 1..=AGENTS_PER_SHIFT {
                    let event = DomainEvent::new(
                        "agent_action_received",
                        &format!("AGENT-{agent:02}"),
                        &format!(r#"{{"tick":{tick},"action":"work"}}"#),
                        &format!("corr-tp-{tick}"),
                        tick * 100,
                    );
                    black_box(store.append_event(&event).unwrap());
                }
            }
        });
    });
}

// ──────────────────────────────────────────────
// Mixed Workload: Realistischer Tick-Zyklus
// ──────────────────────────────────────────────

/// Simuliert einen realistischen Tick-Zyklus:
/// 1. 15 Agent-Events appenden (mit Outbox fuer Zenoh)
/// 2. Outbox pollen (Publisher-Seite)
/// 3. Offset aktualisieren (Projection Bookmark)
/// 4. Alle 10 Ticks: Snapshot speichern
fn bench_mixed_workload_tick(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench_mixed.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();

    let mut tick = 0u64;
    let mut offset = 0i64;

    c.bench_function("mixed_workload_tick", |b| {
        b.iter(|| {
            // 1. Agent-Events mit Outbox
            for agent in 1..=AGENTS_PER_SHIFT {
                let event = DomainEvent::new(
                    "agent_action_received",
                    &format!("AGENT-{agent:02}"),
                    &format!(r#"{{"tick":{tick},"action":"work"}}"#),
                    &format!("corr-mix-{tick}"),
                    tick * 100,
                );
                black_box(
                    store
                        .append_with_outbox(&event, &format!("sentinel/events/AGENT-{agent:02}"))
                        .unwrap(),
                );
            }

            // 2. Outbox pollen
            black_box(store.poll_outbox(20).unwrap());

            // 3. Offset erhoehen
            offset += 1;
            store.update_offset("bench-projection", offset).unwrap();

            // 4. Snapshot alle 10 Ticks
            if tick.is_multiple_of(10) {
                black_box(
                    store
                        .save_snapshot(
                            "AGENT-01",
                            "bio_state",
                            r#"{"hunger":42,"energy":75}"#,
                            offset,
                        )
                        .unwrap(),
                );
            }

            tick += 1;
        });
    });
}

// ──────────────────────────────────────────────
// Skalierung: DB-Groesse Impact auf Reads
// ──────────────────────────────────────────────

/// Misst Read-Performance bei wachsender DB (100, 1K, 10K Events).
/// Wichtig fuer IOPS-Budget auf DRAM-loser NVMe.
fn bench_read_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_scaling");

    for event_count in [100u64, 1_000, 10_000] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("bench_scale_{event_count}.db"));
        let store = EventStore::open(path.to_str().unwrap()).unwrap();

        for i in 0..event_count {
            let event = make_event(i);
            store.append_event(&event).unwrap();
        }

        group.bench_with_input(
            BenchmarkId::new("get_events_since_50", event_count),
            &event_count,
            |b, _| {
                b.iter(|| {
                    black_box(store.get_events_since(0, 50).unwrap());
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("get_events_by_aggregate", event_count),
            &event_count,
            |b, _| {
                b.iter(|| {
                    black_box(store.get_events_by_aggregate("AGENT-01", 50).unwrap());
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("event_count", event_count),
            &event_count,
            |b, _| {
                b.iter(|| {
                    black_box(store.event_count().unwrap());
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    // Einzeloperationen
    bench_append_event,
    bench_append_with_outbox,
    bench_get_events_since,
    bench_get_events_by_aggregate,
    bench_save_snapshot,
    bench_get_latest_snapshot,
    bench_poll_outbox,
    bench_get_all_events,
    bench_event_count,
    // Throughput (>100 ticks/s Schwellenwert)
    bench_single_tick_15_agents,
    bench_100_ticks_15_agents_throughput,
    // Realistisches Szenario
    bench_mixed_workload_tick,
    // Skalierung (IOPS-Impact)
    bench_read_scaling,
);
criterion_main!(benches);
