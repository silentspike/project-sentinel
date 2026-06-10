//! Issue #381 AC-4 benchmark: full-tick overhead of per-phase timing.
//!
//! Compares the complete simulation schedule (26 agents) without vs. with
//! `install_phase_timing` + the daemon-side record path (read resource,
//! observe 10 histograms). Both variants run in the same binary — the opt-in
//! design needs no feature flag or second build.
//!
//! Budget (Cluster 05b): delta < 0.1% of the tick budget (1 s ⇒ < 1 ms).
//!
//! Build with `cargo remote -c -- build -p sentinel-ecs --benches --release`.
//! Run on the deployment VM (i7-3930K) or one consistent local machine,
//! never on the cargo-remote build server. Capture `vmstat 1` / `mpstat 1`
//! alongside every run.

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, Criterion};
use sentinel_common::room::BuildingConfig;
use sentinel_common::{AgentId, Tick};
use sentinel_ecs::world::ROOM_IDS;
use sentinel_ecs::{
    create_simulation_world, install_phase_timing, spawn_agent, EventBuffer, PhaseTimings,
    RoomDistanceMap, RoomInfoMap, SimulationTime, PHASE_NAMES,
};

const BENCH_AGENT_COUNT: u16 = 26;

fn config_path() -> PathBuf {
    if let Ok(repo_root) = std::env::var("SENTINEL_REPO_ROOT") {
        return Path::new(&repo_root).join("config/rooms.toml");
    }

    if let Ok(current_dir) = std::env::current_dir() {
        let candidate = current_dir.join("config/rooms.toml");
        if candidate.exists() {
            return candidate;
        }
    }

    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/rooms.toml")
}

fn build_world(with_timing: bool) -> (bevy_ecs::prelude::World, bevy_ecs::prelude::Schedule) {
    let (mut world, mut schedule) = create_simulation_world();
    if with_timing {
        install_phase_timing(&mut world, &mut schedule);
    }

    let config = BuildingConfig::load(&config_path()).expect("rooms.toml must load for #381 bench");
    world.insert_resource(RoomDistanceMap::from_building_config(&config));
    world.insert_resource(RoomInfoMap::from_building_config(&config));

    for id in 1..=BENCH_AGENT_COUNT {
        let room_index = (id as usize).saturating_sub(1) % ROOM_IDS.len();
        spawn_agent(
            &mut world,
            AgentId(id),
            &format!("Bench-Agent-{id:02}"),
            "Benchmark",
            1,
            ROOM_IDS[room_index],
        );
    }

    (world, schedule)
}

fn prepare_tick(world: &mut bevy_ecs::prelude::World, tick: u64) {
    world.resource_mut::<EventBuffer>().events.clear();
    let mut time = world.resource_mut::<SimulationTime>();
    time.tick = Tick(tick);
    time.tick_count = tick;
    time.delta_seconds = 1.0;
    time.sim_hour = 8.0 + (tick as f32 / 3600.0);
}

fn bench_full_tick_baseline(c: &mut Criterion) {
    let (mut world, mut schedule) = build_world(false);
    let mut tick = 1u64;

    let mut group = c.benchmark_group("issue381.phase_timing");
    group.sample_size(50);
    group.bench_function("full_tick_baseline_26_agents", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                prepare_tick(&mut world, tick);
                let start = Instant::now();
                schedule.run(&mut world);
                total += start.elapsed();
                black_box(world.resource::<EventBuffer>().events.len());
                tick = if tick >= 59 { 1 } else { tick + 1 };
            }
            total
        });
    });
    group.finish();
}

fn bench_full_tick_with_phase_timing(c: &mut Criterion) {
    let (mut world, mut schedule) = build_world(true);
    let mut tick = 1u64;

    // Daemon-side record path, identical to orchestrator.rs after schedule.run.
    let registry = sentinel_telemetry::MetricsRegistry::global();
    let phase_hists: Vec<_> = PHASE_NAMES
        .iter()
        .map(|p| {
            registry.histogram(
                &sentinel_telemetry::phase_metric_name(p),
                &sentinel_telemetry::PHASE_DURATION_BOUNDARIES_MS,
            )
        })
        .collect();

    let mut group = c.benchmark_group("issue381.phase_timing");
    group.sample_size(50);
    group.bench_function("full_tick_with_phase_timing_26_agents", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                prepare_tick(&mut world, tick);
                let start = Instant::now();
                schedule.run(&mut world);
                if let Some(timings) = world.get_resource::<PhaseTimings>() {
                    for (i, hist) in phase_hists.iter().enumerate() {
                        if let Some(ms) = timings.duration_ms(i) {
                            hist.observe(ms);
                        }
                    }
                }
                total += start.elapsed();
                black_box(world.resource::<EventBuffer>().events.len());
                tick = if tick >= 59 { 1 } else { tick + 1 };
            }
            total
        });
    });
    group.finish();
}

criterion_group!(
    issue381_phase_timing_benches,
    bench_full_tick_baseline,
    bench_full_tick_with_phase_timing
);
criterion_main!(issue381_phase_timing_benches);
