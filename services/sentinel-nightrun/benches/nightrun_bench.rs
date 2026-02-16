//! Nightrun Benchmarks — Criterion-basiert.
//!
//! Misst die Kernoperationen des Nightrun-Service:
//! - Job-Queue Operationen (SQLite)
//! - Runner Pipeline (end-to-end Konsolidierung)
//! - Shift-Detection

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use sentinel_hippocampus::{Episode, HippocampusService};
use sentinel_limbo::EventStore;
use sentinel_nightrun::config::NightrunSettings;
use sentinel_nightrun::job_queue::JobQueue;
use sentinel_nightrun::runner::NightrunRunner;
use sentinel_nightrun::shift::{outgoing_shift_set, shift_set_for_hour};

fn make_episode(id: u64, agent: &str, summary: &str) -> Episode {
    Episode {
        id,
        agent_name: agent.to_string(),
        summary: summary.to_string(),
        relevance: 0.8,
        emotion: 0.7,
        repetitions: 1,
        hours_ago: 1.0,
        participants: vec![],
        tags: vec![],
    }
}

fn make_settings(dir: &std::path::Path) -> NightrunSettings {
    NightrunSettings {
        hippocampus_db: dir.join("hc.redb").to_str().unwrap().to_string(),
        event_store_db: dir.join("ev.db").to_str().unwrap().to_string(),
        agent_config_dir: dir.to_str().unwrap().to_string(),
        job_queue_path: dir.join("jq.db").to_str().unwrap().to_string(),
        timeout_per_agent_secs: 300,
        timeout_total_secs: 7200,
        max_episodes_per_agent: 1000,
    }
}

// === Shift Detection ===

fn bench_shift_detection(c: &mut Criterion) {
    let mut group = c.benchmark_group("nightrun.shift");

    group.bench_function("shift_set_for_hour", |b| {
        b.iter(|| {
            for hour in 0..24u8 {
                std::hint::black_box(shift_set_for_hour(hour));
            }
        });
    });

    group.bench_function("outgoing_shift_set", |b| {
        b.iter(|| {
            for shift in 1..=3u8 {
                std::hint::black_box(outgoing_shift_set(shift));
            }
        });
    });

    group.finish();
}

// === Job-Queue Operationen ===

fn bench_job_queue(c: &mut Criterion) {
    let mut group = c.benchmark_group("nightrun.job_queue");

    group.bench_function("create_run_15_agents", |b| {
        b.iter_with_setup(
            || {
                let dir = tempfile::tempdir().unwrap();
                let jq = JobQueue::open(dir.path().join("jq.db").to_str().unwrap()).unwrap();
                (jq, dir)
            },
            |(jq, _dir)| {
                let agents: Vec<String> = (0..15).map(|i| format!("Agent-{i:02}")).collect();
                jq.create_run("bench-run", &agents).unwrap();
            },
        );
    });

    group.bench_function("create_run_54_agents", |b| {
        b.iter_with_setup(
            || {
                let dir = tempfile::tempdir().unwrap();
                let jq = JobQueue::open(dir.path().join("jq.db").to_str().unwrap()).unwrap();
                (jq, dir)
            },
            |(jq, _dir)| {
                let agents: Vec<String> = (0..54).map(|i| format!("Agent-{i:02}")).collect();
                jq.create_run("bench-run", &agents).unwrap();
            },
        );
    });

    group.bench_function("mark_transitions", |b| {
        b.iter_with_setup(
            || {
                let dir = tempfile::tempdir().unwrap();
                let jq = JobQueue::open(dir.path().join("jq.db").to_str().unwrap()).unwrap();
                jq.create_run("bench-run", &["Agent-00".into()]).unwrap();
                (jq, dir)
            },
            |(jq, _dir)| {
                jq.mark_in_progress("bench-run", "Agent-00").unwrap();
                jq.mark_completed("bench-run", "Agent-00", 10, 5).unwrap();
            },
        );
    });

    group.bench_function("get_pending_15", |b| {
        let dir = tempfile::tempdir().unwrap();
        let jq = JobQueue::open(dir.path().join("jq.db").to_str().unwrap()).unwrap();
        let agents: Vec<String> = (0..15).map(|i| format!("Agent-{i:02}")).collect();
        jq.create_run("bench-run", &agents).unwrap();

        b.iter(|| {
            std::hint::black_box(jq.get_pending("bench-run").unwrap());
        });
    });

    group.finish();
}

// === Runner Pipeline ===

fn bench_runner_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("nightrun.pipeline");
    group.sample_size(10); // Pipeline ist langsam (redb writes)

    for &agent_count in &[1, 5, 15] {
        group.bench_with_input(
            BenchmarkId::new("consolidate", agent_count),
            &agent_count,
            |b, &count| {
                b.iter_with_setup(
                    || {
                        let dir = tempfile::tempdir().unwrap();
                        let settings = make_settings(dir.path());

                        let hc = HippocampusService::open(&settings.hippocampus_db).unwrap();
                        let es = EventStore::open(&settings.event_store_db).unwrap();
                        let jq = JobQueue::open(&settings.job_queue_path).unwrap();

                        // Seed episodes
                        for i in 0..count {
                            let name = format!("Agent-{i:02}");
                            let episodes: Vec<Episode> = (0..8)
                                .map(|j| {
                                    make_episode((i * 10 + j) as u64, &name, &format!("Event {j}"))
                                })
                                .collect();
                            hc.record_episodes(&name, &episodes).unwrap();
                        }

                        let run_id = format!("bench-{count}");
                        let runner = NightrunRunner::new(hc, es, jq, settings, run_id, false);
                        (runner, dir)
                    },
                    |(runner, _dir)| {
                        std::hint::black_box(runner.run(1).unwrap());
                    },
                );
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_shift_detection,
    bench_job_queue,
    bench_runner_pipeline,
);
criterion_main!(benches);
