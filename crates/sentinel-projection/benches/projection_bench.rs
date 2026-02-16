//! Benchmarks fuer den Projection Worker.
//!
//! Misst Event-Processing-Rate, Rebuild-Throughput, Offset-Writes
//! und Idempotenz-Check-Overhead.
//!
//! WICHTIG: Diese Benchmarks MUESSEN auf der Deployment-VM ausgefuehrt werden
//! (NICHT auf dem Build-Server/LXC). Siehe CLAUDE.md.
//!
//! Performance-Budgets:
//! - Event Processing Rate: >1000 events/s
//! - Rebuild Duration: <5 min / 1M events (extrapoliert)
//! - Offset Write: <1ms
//! - Idempotency Check: <500us

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use sentinel_common::{AgentId, DomainEvent, DomainEventPayload};
use sentinel_limbo::EventStore;
use sentinel_projection::{ProjectionConfig, ProjectionWorker};

// ── Helpers ────────────────────────────────────

fn make_payload(i: u64) -> DomainEventPayload {
    match i % 8 {
        0 => DomainEventPayload::AgentSpawned {
            agent_id: AgentId((i % 15 + 1) as u16),
            name: format!("Agent-{}", i % 15 + 1),
            role: "Developer".to_string(),
            shift_set: 1,
        },
        1 => DomainEventPayload::TransitStarted {
            agent_id: AgentId((i % 15 + 1) as u16),
            from_room: "buero-dev-1".to_string(),
            to_room: "kueche".to_string(),
            duration_ms: 5000,
        },
        2 => DomainEventPayload::TransitCompleted {
            agent_id: AgentId((i % 15 + 1) as u16),
            room_id: "kueche".to_string(),
        },
        3 => DomainEventPayload::AgentActionReceived {
            agent_id: AgentId((i % 15 + 1) as u16),
            action_type: "Chat".to_string(),
            target_room: None,
            content: Some("Benchmark event".to_string()),
        },
        4 => DomainEventPayload::TickSnapshot {
            tick: i,
            agent_count: 15,
        },
        5 => DomainEventPayload::AgentStatusChanged {
            agent_id: AgentId((i % 15 + 1) as u16),
            old_status: "active".to_string(),
            new_status: "paused".to_string(),
        },
        6 => DomainEventPayload::BioActionPerformed {
            agent_id: AgentId((i % 15 + 1) as u16),
            action: "coffee".to_string(),
        },
        _ => DomainEventPayload::ChaosTriggered {
            event_type: sentinel_common::EventType::PhoneRing,
            target_room: Some("buero-dev-1".to_string()),
            description: "Benchmark chaos".to_string(),
        },
    }
}

fn seed_events(store: &EventStore, count: u64) {
    for i in 0..count {
        let payload = make_payload(i);
        let mut event = DomainEvent::new(
            payload.event_type_str(),
            &format!("AGENT-{:02}", (i % 15) + 1),
            &payload.to_json(),
            "corr-bench",
            i * 100,
        );
        event.timestamp_ms = i * 1000;
        store.append_event(&event).unwrap();
    }
}

fn make_worker(event_store: Arc<EventStore>, db_path: &str) -> ProjectionWorker {
    let config = ProjectionConfig {
        poll_interval: Duration::from_millis(1),
        batch_size: 100,
        db_path: db_path.to_string(),
    };
    ProjectionWorker::new(event_store, config).unwrap()
}

// ── Benchmarks ─────────────────────────────────

/// Event Processing Rate: Verarbeite 10K Events, messe Throughput.
/// Budget: >1000 events/s.
fn bench_event_processing_rate(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let es_path = dir.path().join("bench_rate_es.db");
    let store = Arc::new(EventStore::open(es_path.to_str().unwrap()).unwrap());
    seed_events(&store, 10_000);

    c.bench_function("event_processing_10k", |b| {
        b.iter(|| {
            // Frischen Worker pro Iteration (cleane DB)
            let rm_iter_path = dir
                .path()
                .join(format!("bench_rate_rm_{}.db", rand_suffix()));
            let worker = make_worker(Arc::clone(&store), rm_iter_path.to_str().unwrap());
            black_box(worker.rebuild().unwrap());
        });
    });
}

/// Rebuild Duration: Vollstaendiger Rebuild aus 10K Events.
/// Extrapoliert auf 1M: Budget <5 min.
fn bench_rebuild_duration(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let es_path = dir.path().join("bench_rebuild_es.db");

    let store = Arc::new(EventStore::open(es_path.to_str().unwrap()).unwrap());
    seed_events(&store, 10_000);

    c.bench_function("rebuild_10k_events", |b| {
        b.iter(|| {
            let rm_path = dir
                .path()
                .join(format!("bench_rebuild_rm_{}.db", rand_suffix()));
            let worker = make_worker(Arc::clone(&store), rm_path.to_str().unwrap());
            let count = black_box(worker.rebuild().unwrap());
            assert_eq!(count, 10_000);
        });
    });
}

/// Offset Write Latenz: Einzelner update_offset() Call.
/// Budget: <1ms.
fn bench_offset_write(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let es_path = dir.path().join("bench_offset_es.db");

    let store = EventStore::open(es_path.to_str().unwrap()).unwrap();

    let mut offset = 0i64;
    c.bench_function("offset_write", |b| {
        b.iter(|| {
            offset += 1;
            store.update_offset("sentinel-projection", offset).unwrap();
            black_box(());
        });
    });
}

/// Idempotenz-Check Overhead: Handler mit row_id <= last_event_id.
/// Budget: <500us.
fn bench_idempotency_check(c: &mut Criterion) {
    use sentinel_projection::ReadModelStore;

    let dir = tempfile::tempdir().unwrap();
    let rm_path = dir.path().join("bench_idem_rm.db");

    let store = ReadModelStore::open(rm_path.to_str().unwrap()).unwrap();

    // Agent einfuegen mit event_id=1000
    {
        let txn = store.begin_transaction().unwrap();
        txn.begin().unwrap();
        txn.upsert_agent(1, "Klaus", "Developer", 1, "active", 1000)
            .unwrap();
        txn.commit().unwrap();
    }

    // Jetzt mit niedrigerem row_id updaten (idempotent skip)
    c.bench_function("idempotency_skip", |b| {
        b.iter(|| {
            let txn = store.begin_transaction().unwrap();
            txn.begin().unwrap();
            txn.upsert_agent(1, "KlausNeu", "Designer", 2, "paused", 500)
                .unwrap();
            black_box(());
            txn.commit().unwrap();
        });
    });
}

fn rand_suffix() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

criterion_group!(
    benches,
    bench_event_processing_rate,
    bench_rebuild_duration,
    bench_offset_write,
    bench_idempotency_check,
);
criterion_main!(benches);
