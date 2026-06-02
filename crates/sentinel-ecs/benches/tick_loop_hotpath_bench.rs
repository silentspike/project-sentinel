//! Issue #276 hot-path benchmarks for ECS tick-loop allocation work.
//!
//! These benchmarks isolate the two ECS-side allocation hotspots:
//! - `physics_system` room aggregation for 26 active agents
//! - `generate_perception` text generation for 26 active agents
//!
//! Build with `cargo remote -c -- build -p sentinel-ecs --benches`.
//! Run on the deployment VM or one consistent local machine, never on the
//! cargo-remote build server.

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, Criterion};
use sentinel_common::room::BuildingConfig;
use sentinel_common::{AgentId, Tick};
use sentinel_ecs::systems::physics_system;
use sentinel_ecs::world::ROOM_IDS;
use sentinel_ecs::{
    create_simulation_world, generate_perception, spawn_agent, BioState, EventBuffer, Personality,
    Position, RoomDistanceMap, RoomInfoMap, SimulationTime, SmellEvent,
};

const BENCH_AGENT_COUNT: u16 = 26;

#[derive(Debug, Clone)]
struct PerceptionInput {
    bio: BioState,
    position: Position,
    personality: Personality,
    room_noise_db: f32,
    room_temp_c: f32,
    room_co2_ppm: f32,
    focus_hours: f32,
}

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

fn load_building_config() -> BuildingConfig {
    BuildingConfig::load(&config_path()).expect("rooms.toml must load for #276 benches")
}

fn build_room_maps() -> (RoomDistanceMap, RoomInfoMap) {
    let config = load_building_config();
    (
        RoomDistanceMap::from_building_config(&config),
        RoomInfoMap::from_building_config(&config),
    )
}

fn build_physics_world() -> (bevy_ecs::prelude::World, bevy_ecs::prelude::Schedule) {
    let (mut world, _) = create_simulation_world();
    let (room_distances, room_info) = build_room_maps();
    world.insert_resource(room_distances);
    world.insert_resource(room_info);

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

    prepare_physics_tick(&mut world, 1);

    let mut schedule = bevy_ecs::prelude::Schedule::default();
    schedule.add_systems(physics_system);
    (world, schedule)
}

fn prepare_physics_tick(world: &mut bevy_ecs::prelude::World, tick: u64) {
    world.resource_mut::<EventBuffer>().events.clear();

    let mut time = world.resource_mut::<SimulationTime>();
    time.tick = Tick(tick);
    time.tick_count = tick;
    time.delta_seconds = 1.0;
    time.sim_hour = 8.0 + (tick as f32 / 3600.0);
}

fn build_perception_inputs() -> Vec<PerceptionInput> {
    (1..=BENCH_AGENT_COUNT)
        .map(|id| {
            let room_index = (id as usize).saturating_sub(1) % ROOM_IDS.len();
            PerceptionInput {
                bio: BioState {
                    hunger: 20.0 + f32::from(id % 7) * 10.0,
                    energy: 85.0 - f32::from(id % 6) * 12.0,
                    caffeine_mg: f32::from(id % 5) * 8.0,
                    bladder: 15.0 + f32::from(id % 8) * 10.0,
                    stress: 10.0 + f32::from(id % 9) * 9.0,
                    social_need: 35.0 + f32::from(id % 6) * 11.0,
                    comfort: 70.0,
                },
                position: Position {
                    room_id: ROOM_IDS[room_index].to_string(),
                    in_transit: false,
                    transit_target: None,
                    transit_remaining_ms: 0,
                    transit_correlation_id: None,
                    transit_route: Vec::new(),
                    transit_total_ms: 0,
                    transit_paused: false,
                    transit_pause_tick: 0,
                    transit_source: None,
                },
                personality: Personality {
                    openness: 0.5,
                    conscientiousness: 0.6,
                    extraversion: if id % 2 == 0 { 0.7 } else { 0.3 },
                    agreeableness: 0.5,
                    neuroticism: 0.3,
                    caffeine_tolerance: if id % 3 == 0 { 0.8 } else { 0.2 },
                    is_morning_person: id % 2 == 0,
                },
                room_noise_db: 35.0 + f32::from(id % 5) * 8.0,
                room_temp_c: 18.0 + f32::from(id % 9),
                room_co2_ppm: 700.0 + f32::from(id % 8) * 120.0,
                focus_hours: 1.0 + f32::from(id % 6),
            }
        })
        .collect()
}

fn bench_physics_system_26_agents(c: &mut Criterion) {
    let (mut world, mut schedule) = build_physics_world();
    let mut tick = 1u64;

    let mut group = c.benchmark_group("issue276.physics_system");
    group.sample_size(50);
    group.bench_function("tick_26_agents", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                prepare_physics_tick(&mut world, tick);
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

fn bench_generate_perception_26_agents(c: &mut Criterion) {
    let inputs = build_perception_inputs();
    let smells = vec![
        SmellEvent {
            source_room: "kueche".to_string(),
            smell_type: "coffee".to_string(),
            intensity: 0.8,
            radius_rooms: 2,
            decay_per_room: 0.2,
            created_tick: 1,
            duration_ticks: 120,
        },
        SmellEvent {
            source_room: "buero-it".to_string(),
            smell_type: "printer_toner".to_string(),
            intensity: 0.5,
            radius_rooms: 1,
            decay_per_room: 0.2,
            created_tick: 1,
            duration_ticks: 60,
        },
    ];
    let present_agents = vec![
        ("Mia".to_string(), "arbeitet konzentriert".to_string()),
        ("Jonas".to_string(), "telefoniert".to_string()),
        ("Lea".to_string(), "liest Tickets".to_string()),
    ];

    let mut group = c.benchmark_group("issue276.generate_perception");
    group.sample_size(50);
    group.bench_function("texts_26_agents", |b| {
        b.iter(|| {
            for input in &inputs {
                black_box(generate_perception(
                    black_box(&input.bio),
                    black_box(&input.position),
                    black_box(&input.personality),
                    black_box(input.room_noise_db),
                    black_box(input.room_temp_c),
                    black_box(input.room_co2_ppm),
                    black_box(&smells),
                    black_box(&present_agents),
                    black_box("08:30"),
                    black_box(input.focus_hours),
                ));
            }
        });
    });
    group.finish();
}

criterion_group!(
    issue276_hotpath_benches,
    bench_physics_system_26_agents,
    bench_generate_perception_26_agents
);
criterion_main!(issue276_hotpath_benches);
