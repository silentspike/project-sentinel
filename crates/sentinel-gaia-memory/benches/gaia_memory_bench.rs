//! Benchmarks for Gaia Console Memory (#443).
//!
//! Run these on a benchmark/runtime VM, not through cargo-remote. The
//! rehydration benchmark intentionally proves the no-replay path:
//! `events_replayed=0` and `event_rows_loaded=0`.

use std::hint::black_box;
use std::path::Path;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use sentinel_gaia_memory::graph::{FactQuery, FactWrite, GaiaConsoleMemoryStore};
use sentinel_gaia_memory::memory_file::{GaiaConsoleMemoryFile, MemorySection};
use sentinel_gaia_memory::rehydrate::{
    rehydrate_from_data_dir, RehydrateRequest, EVENTS_DB_FILE_NAME, HIPPOCAMPUS_DB_FILE_NAME,
    PROJECTION_DB_FILE_NAME,
};
use sentinel_gaia_memory::GRAPH_FILE_NAME;
use sentinel_hippocampus::{Episode, HippocampusStore, NarrativeState};
use sentinel_limbo::EventStore;
use sentinel_projection::ReadModelStore;

fn fact(subject: &str, relation: &str, value: &str, ts: u64) -> FactWrite {
    FactWrite::literal(subject, relation, value, ts, ts)
}

fn seed_graph(path: &Path, count: usize) -> GaiaConsoleMemoryStore {
    let store = GaiaConsoleMemoryStore::open(path.join(GRAPH_FILE_NAME)).unwrap();
    for i in 0..count {
        store
            .insert_fact(fact(
                &format!("company:sentinel:{i}"),
                "issue_443_fact",
                &format!("fact-value-{i}"),
                i as u64 + 1,
            ))
            .unwrap();
    }
    store
}

fn episode(id: u64, summary: &str) -> Episode {
    Episode {
        id,
        agent_name: "Thomas".to_string(),
        summary: summary.to_string(),
        relevance: 1.0,
        emotion: 0.5,
        repetitions: 1,
        hours_ago: 0.0,
        participants: Vec::new(),
        tags: Vec::new(),
    }
}

fn seed_rehydrate_dir(path: &Path) {
    let event_store = EventStore::open(&path.join(EVENTS_DB_FILE_NAME).to_string_lossy()).unwrap();
    black_box(event_store.event_count().unwrap());
    drop(event_store);

    let projection =
        ReadModelStore::open(&path.join(PROJECTION_DB_FILE_NAME).to_string_lossy()).unwrap();
    {
        let txn = projection.begin_transaction().unwrap();
        txn.begin().unwrap();
        txn.upsert_agent(7, "Thomas", "Engineer", 1, "active", 10)
            .unwrap();
        txn.update_agent_room(7, "buero-dev-1", 11).unwrap();
        txn.commit().unwrap();
    }
    drop(projection);

    let memory_file = GaiaConsoleMemoryFile::open_or_create(path).unwrap();
    memory_file
        .append_entry(MemorySection::Notes, 12, "bench wake-up context")
        .unwrap();

    let hippocampus_path = path.join(HIPPOCAMPUS_DB_FILE_NAME);
    let hippocampus = HippocampusStore::open(&hippocampus_path.to_string_lossy()).unwrap();
    hippocampus
        .store_narrative(
            "Thomas",
            &NarrativeState {
                agent_name: "Thomas".to_string(),
                summary: "Benchmark narrative".to_string(),
                episode_count: 1,
            },
        )
        .unwrap();
    hippocampus
        .store_episodes("Thomas", &[episode(1, "Benchmark live memory")])
        .unwrap();
    hippocampus
        .store_fact("facts/projects/aurora", "Aurora is active")
        .unwrap();
}

fn bench_graph_insert(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let store = GaiaConsoleMemoryStore::open(dir.path().join(GRAPH_FILE_NAME)).unwrap();
    let mut i = 0u64;

    c.bench_function("gaia_console_memory.graph_insert_fact", |b| {
        b.iter(|| {
            let write = fact(
                &format!("company:sentinel:insert:{i}"),
                "bench_insert",
                &format!("value-{i}"),
                i + 1,
            );
            i += 1;
            black_box(store.insert_fact(write).unwrap());
        })
    });
}

fn bench_graph_query(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let store = seed_graph(dir.path(), 1_000);
    let query = FactQuery::current("company:sentinel:777", "issue_443_fact");

    c.bench_function("gaia_console_memory.graph_query_current_1k", |b| {
        b.iter(|| {
            let results = store.query_facts(black_box(query.clone())).unwrap();
            black_box(results);
        })
    });
}

fn bench_graph_supersede(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let store = GaiaConsoleMemoryStore::open(dir.path().join(GRAPH_FILE_NAME)).unwrap();
    store
        .insert_fact(fact(
            "company:sentinel:supersede",
            "bench_status",
            "initial",
            1,
        ))
        .unwrap();
    let mut i = 2u64;

    c.bench_function("gaia_console_memory.graph_supersede_fact", |b| {
        b.iter(|| {
            let write = fact(
                "company:sentinel:supersede",
                "bench_status",
                &format!("value-{i}"),
                i,
            );
            i += 1;
            black_box(store.supersede_fact(write).unwrap());
        })
    });
}

fn bench_rehydrate_readonly(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    seed_rehydrate_dir(dir.path());

    let mut request = RehydrateRequest::new(dir.path());
    request.agent_name = Some("Thomas".to_string());
    request.fact_keys = vec!["facts/projects/aurora".to_string()];
    request.max_memory_bytes = 2_048;
    request.max_agents = 8;
    request.max_episodes = 4;

    c.bench_function("gaia_console_memory.rehydrate_readonly_zero_replay", |b| {
        b.iter(|| {
            let context = rehydrate_from_data_dir(black_box(&request)).unwrap();
            assert_eq!(context.events_replayed, 0);
            assert_eq!(context.event_rows_loaded, 0);
            assert_eq!(context.event_copy_count, 0);
            black_box(context);
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(20)
        .measurement_time(Duration::from_secs(2));
    targets = bench_graph_insert, bench_graph_query, bench_graph_supersede, bench_rehydrate_readonly
}
criterion_main!(benches);
