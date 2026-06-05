//! Issue-spezifische Benchmarks fuer Platform-Controlplane (#263).
//!
//! Diese Benchmarks werden fuer die Deploy-VM gebaut und dort ausgefuehrt.

use std::collections::HashMap;
use std::hint::black_box;
use std::path::{Path, PathBuf};

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use sentinel_common::{AgentId, DomainEvent};
use sentinel_daemon::config::PlatformControlplaneConfig;
use sentinel_daemon::platform_controlplane::metrics::{
    collect_with_cgroup_root, PlatformMetrics, PlatformMetricsCollector,
};
use sentinel_daemon::platform_controlplane::rules::evaluate_rules;
use sentinel_daemon::platform_controlplane::PlatformControlplane;
use sentinel_ebpf::collector::{IoSnapshot, MetricsSnapshot, StalledAgent};
use sentinel_ebpf::loader::MonitoringMode;
use sentinel_limbo::EventStore;

const AGENT_COUNT: usize = 15;

struct MetricsFixture {
    _dir: tempfile::TempDir,
    cgroup_root: PathBuf,
    events_db_path: String,
    event_store: EventStore,
    agent_names: Vec<String>,
    ebpf_snapshot: Option<MetricsSnapshot>,
    collector: PlatformMetricsCollector,
}

fn agent_names() -> Vec<String> {
    (0..AGENT_COUNT)
        .map(|idx| format!("Bench Agent {:02}", idx + 1))
        .collect()
}

fn seed_event_store(store: &EventStore, count: usize, projection_lag: i64) {
    for idx in 0..count {
        let event = DomainEvent::new(
            "bench_event",
            &format!("AGENT-{idx:02}"),
            r#"{"bench":true}"#,
            "bench-corr",
            idx as u64,
        );
        store.append_event(&event).expect("bench event appended");
    }
    let latest_id = store.get_latest_event_id().expect("latest event id");
    let projection_offset = (latest_id - projection_lag).max(0);
    store
        .update_offset("sentinel-projection", projection_offset)
        .expect("projection offset updated");
}

fn write_cgroup_fixture(root: &Path, names: &[String]) {
    for (idx, name) in names.iter().enumerate() {
        let agent_dir = root.join(name);
        std::fs::create_dir_all(&agent_dir).expect("agent cgroup dir");
        let current = 64_u64 * 1024 * 1024 + (idx as u64 * 1024);
        let max = 128_u64 * 1024 * 1024;
        std::fs::write(agent_dir.join("memory.current"), current.to_string())
            .expect("memory.current");
        std::fs::write(agent_dir.join("memory.max"), max.to_string()).expect("memory.max");
    }
}

fn synthetic_ebpf_snapshot(names: &[String]) -> MetricsSnapshot {
    let stalled_agents = names
        .iter()
        .take(3)
        .enumerate()
        .map(|(idx, name)| StalledAgent {
            cgroup_id: (idx + 1) as u64,
            agent_name: name.clone(),
            seconds_since_write: 42,
        })
        .collect();

    let io_metrics = names
        .iter()
        .enumerate()
        .map(|(idx, name)| {
            (
                (idx + 1) as u64,
                IoSnapshot {
                    cgroup_name: name.clone(),
                    read_ops: 100,
                    write_ops: 120,
                    read_bytes: 8_000 + idx as u64,
                    write_bytes: 12_000 + idx as u64 * 100,
                },
            )
        })
        .collect();

    MetricsSnapshot {
        stalled_agents,
        io_metrics,
        network_metrics: HashMap::new(),
        psi_metrics: HashMap::new(),
        cycle_duration: std::time::Duration::from_micros(250),
        mode: MonitoringMode::Userspace,
        ring_buffer_drops: 0,
    }
}

fn setup_metrics_fixture() -> MetricsFixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let cgroup_root = dir.path().join("sentinel");
    std::fs::create_dir_all(&cgroup_root).expect("cgroup root");

    let agent_names = agent_names();
    write_cgroup_fixture(&cgroup_root, &agent_names);

    let events_db_path = dir.path().join("events.db");
    let event_store =
        EventStore::open(events_db_path.to_str().expect("db path")).expect("event store");
    seed_event_store(&event_store, 200, 75);

    let ebpf_snapshot = Some(synthetic_ebpf_snapshot(&agent_names));
    let mut collector = PlatformMetricsCollector::default();
    let _ = collect_with_cgroup_root(
        &mut collector,
        &ebpf_snapshot,
        &event_store,
        events_db_path.to_str().expect("events path"),
        &agent_names,
        1,
        Vec::new(),
        false,
        &cgroup_root,
    );

    MetricsFixture {
        _dir: dir,
        cgroup_root,
        events_db_path: events_db_path.to_string_lossy().into_owned(),
        event_store,
        agent_names,
        ebpf_snapshot,
        collector,
    }
}

fn synthetic_metrics() -> (PlatformMetrics, HashMap<String, AgentId>) {
    let agent_names = agent_names();
    let stalled_agents = agent_names.iter().take(4).cloned().collect();
    let agent_memory_pressure = agent_names
        .iter()
        .take(5)
        .enumerate()
        .map(|(idx, name)| (name.clone(), 0.91 + idx as f64 * 0.01))
        .collect();
    let agent_write_rates = agent_names
        .iter()
        .take(2)
        .map(|name| (name.clone(), 6.0 * 1024.0 * 1024.0))
        .collect();
    let last_action_ticks = HashMap::from([
        (agent_names[0].clone(), 10_u64),
        (agent_names[1].clone(), 11_u64),
    ]);
    let agent_ids = agent_names
        .iter()
        .enumerate()
        .map(|(idx, name)| (name.clone(), AgentId((idx + 1) as u16)))
        .collect();

    (
        PlatformMetrics {
            stalled_agents,
            event_store_size_bytes: 600 * 1024 * 1024,
            projection_lag: 15_000,
            agent_memory_pressure,
            agent_write_rates,
            tick: 123,
            failed_services: vec!["sentinel-judge".to_string()],
            llm_circuit_open: false,
            last_action_ticks,
        },
        agent_ids,
    )
}

fn bench_rule_evaluation(c: &mut Criterion) {
    let (metrics, agent_ids) = synthetic_metrics();
    let write_rate_baselines = metrics
        .agent_write_rates
        .iter()
        .map(|(name, rate)| (name.clone(), rate / 12.0))
        .collect::<HashMap<_, _>>();
    let config = PlatformControlplaneConfig {
        cycle_interval_ticks: 1,
        stall_recent_activity_grace_ticks: 3,
        ..PlatformControlplaneConfig::default()
    };
    let cooldowns = HashMap::new();

    c.bench_function("platform_cp.rule_evaluation", |b| {
        b.iter(|| {
            let actions = evaluate_rules(
                black_box(&metrics),
                black_box(&cooldowns),
                black_box(123),
                black_box(&config),
                black_box(&write_rate_baselines),
                black_box(&agent_ids),
            );
            black_box(actions);
        });
    });
}

fn bench_metrics_collection(c: &mut Criterion) {
    c.bench_function("platform_cp.metrics_collection_15_agents", |b| {
        b.iter_batched(
            setup_metrics_fixture,
            |mut fixture| {
                let metrics = collect_with_cgroup_root(
                    &mut fixture.collector,
                    &fixture.ebpf_snapshot,
                    &fixture.event_store,
                    &fixture.events_db_path,
                    &fixture.agent_names,
                    2,
                    Vec::new(),
                    false,
                    &fixture.cgroup_root,
                );
                black_box(metrics);
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_tick_overhead_without_cp(c: &mut Criterion) {
    c.bench_function("platform_cp.tick_overhead_without_cp", |b| {
        b.iter_batched(
            || (),
            |_| {
                let start = std::time::Instant::now();
                let mut tick = 0_u64;
                for _ in 0..1_000 {
                    tick = tick.wrapping_add(1);
                    black_box(tick);
                }
                black_box(start.elapsed());
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_tick_overhead_with_cp(c: &mut Criterion) {
    c.bench_function("platform_cp.tick_overhead_with_cp", |b| {
        b.iter_batched(
            || {
                let fixture = setup_metrics_fixture();
                let config = PlatformControlplaneConfig {
                    enabled: true,
                    cycle_interval_ticks: 60,
                    llm_enabled: false,
                    stall_recent_activity_grace_ticks: 3,
                    ..PlatformControlplaneConfig::default()
                };
                let (metrics, agent_ids) = synthetic_metrics();
                (
                    PlatformControlplane::new(config),
                    fixture.event_store,
                    metrics,
                    agent_ids,
                )
            },
            |(mut platform_cp, event_store, metrics, agent_ids)| {
                let start = std::time::Instant::now();
                let mut tick = 0_u64;
                for _ in 0..1_000 {
                    tick += 1;
                    if platform_cp.should_run(tick) {
                        let output = platform_cp.cycle(&metrics, &event_store, tick, &agent_ids);
                        black_box(output);
                    }
                }
                black_box(start.elapsed());
            },
            BatchSize::SmallInput,
        );
    });
}

criterion_group!(
    platform_controlplane_benches,
    bench_rule_evaluation,
    bench_metrics_collection,
    bench_tick_overhead_without_cp,
    bench_tick_overhead_with_cp
);
criterion_main!(platform_controlplane_benches);
