//! Criterion-Benchmarks fuer Room Phase 2 (#289).
//!
//! Pflicht-Budgets:
//! - Route-BFS pro Move: < 100us
//! - Encounter Detection pro Tick: < 50us mit 26 Agents
//! - Tick-Duration unter Room-Phase-2-Last: < 1100ms
//!
//! Die Benchmarks messen die drei kritischen Pfade isoliert genug fuer Ursachenanalyse,
//! bleiben aber nah genug an den echten ECS-Datenstrukturen um fuer #289 belastbar zu sein.

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use sentinel_common::room::BuildingConfig;
use sentinel_common::{AgentId, Tick};
use sentinel_ecs::systems::encounter_system;
use sentinel_ecs::world::ROOM_IDS;
use sentinel_ecs::{
    create_simulation_world, spawn_agent, BioState, EventBuffer, Position, RoomChatBuffer,
    RoomDistanceMap, RoomInfoMap, SimulationTime,
};

const BENCH_AGENT_COUNT: u16 = 26;

#[derive(Clone, Copy)]
struct TransitSpec {
    agent_id: u16,
    room_id: &'static str,
    target_room: &'static str,
    route: &'static [&'static str],
    total_ms: u32,
    remaining_ms: u32,
}

const TRANSIT_SPECS: &[TransitSpec] = &[
    TransitSpec {
        agent_id: 1,
        room_id: "empfang",
        target_room: "buero-ceo",
        route: &["flur-eg", "treppenhaus", "flur-og"],
        total_ms: 80_000,
        remaining_ms: 60_000,
    },
    TransitSpec {
        agent_id: 2,
        room_id: "buero-ceo",
        target_room: "kueche",
        route: &["flur-og", "treppenhaus", "flur-eg"],
        total_ms: 80_000,
        remaining_ms: 40_000,
    },
    TransitSpec {
        agent_id: 3,
        room_id: "meetingraum-03",
        target_room: "empfang",
        route: &["flur-og", "treppenhaus", "flur-eg"],
        total_ms: 80_000,
        remaining_ms: 20_000,
    },
    TransitSpec {
        agent_id: 4,
        room_id: "buero-design-1",
        target_room: "meetingraum-02",
        route: &["flur-og"],
        total_ms: 40_000,
        remaining_ms: 30_000,
    },
    TransitSpec {
        agent_id: 5,
        room_id: "buero-dev-1",
        target_room: "meetingraum-01",
        route: &["flur-eg"],
        total_ms: 40_000,
        remaining_ms: 20_000,
    },
    TransitSpec {
        agent_id: 6,
        room_id: "buero-betriebsarzt",
        target_room: "kueche",
        route: &["flur-og", "treppenhaus", "flur-eg"],
        total_ms: 80_000,
        remaining_ms: 50_000,
    },
    TransitSpec {
        agent_id: 7,
        room_id: "buero-pm",
        target_room: "meetingraum-03",
        route: &["flur-eg", "treppenhaus", "flur-og"],
        total_ms: 80_000,
        remaining_ms: 45_000,
    },
    TransitSpec {
        agent_id: 8,
        room_id: "buero-marketing",
        target_room: "buero-sales",
        route: &["flur-eg"],
        total_ms: 40_000,
        remaining_ms: 25_000,
    },
];

fn config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/rooms.toml")
}

fn load_building_config() -> BuildingConfig {
    BuildingConfig::load(&config_path()).expect("rooms.toml must load for room phase 2 benches")
}

fn build_room_maps() -> (RoomDistanceMap, RoomInfoMap) {
    let config = load_building_config();
    (
        RoomDistanceMap::from_building_config(&config),
        RoomInfoMap::from_building_config(&config),
    )
}

fn spec_for(agent_id: u16) -> Option<TransitSpec> {
    TRANSIT_SPECS
        .iter()
        .copied()
        .find(|spec| spec.agent_id == agent_id)
}

fn reset_positions(world: &mut bevy_ecs::prelude::World) {
    let mut query = world.query::<(&sentinel_ecs::AgentIdentity, &mut Position)>();
    for (identity, mut pos) in query.iter_mut(world) {
        let room_index = (identity.agent_id.0 as usize).saturating_sub(1) % ROOM_IDS.len();
        pos.room_id = ROOM_IDS[room_index].to_string();
        pos.in_transit = false;
        pos.transit_target = None;
        pos.transit_remaining_ms = 0;
        pos.transit_correlation_id = None;
        pos.transit_route.clear();
        pos.transit_total_ms = 0;
        pos.transit_paused = false;
        pos.transit_pause_tick = 0;
        pos.transit_source = None;

        if let Some(spec) = spec_for(identity.agent_id.0) {
            pos.room_id = spec.room_id.to_string();
            pos.in_transit = true;
            pos.transit_target = Some(spec.target_room.to_string());
            pos.transit_remaining_ms = spec.remaining_ms;
            pos.transit_route = spec.route.iter().map(|room| (*room).to_string()).collect();
            pos.transit_total_ms = spec.total_ms;
            pos.transit_source = Some(spec.room_id.to_string());
        }
    }
}

fn reset_bio_state(world: &mut bevy_ecs::prelude::World) {
    let mut query = world.query::<(&sentinel_ecs::AgentIdentity, &mut BioState)>();
    for (identity, mut bio) in query.iter_mut(world) {
        bio.hunger = 20.0;
        bio.energy = 80.0;
        bio.caffeine_mg = 50.0;
        bio.bladder = 10.0;
        bio.stress = 15.0;
        bio.social_need = 40.0;
        bio.comfort = 75.0;

        match identity.agent_id.0 % 6 {
            0 => bio.bladder = 92.0,
            1 => bio.energy = 12.0,
            2 => bio.hunger = 88.0,
            3 => bio.stress = 78.0,
            4 => bio.caffeine_mg = 8.0,
            _ => bio.social_need = 85.0,
        }
    }
}

fn seed_chat_activity(world: &mut bevy_ecs::prelude::World, tick: u64) {
    let names: Vec<String> = {
        let mut query = world.query::<&sentinel_ecs::AgentIdentity>();
        query
            .iter(world)
            .map(|identity| identity.name.clone())
            .collect()
    };

    *world.resource_mut::<RoomChatBuffer>() = RoomChatBuffer::default();

    let mut room_chat_buffer = world.resource_mut::<RoomChatBuffer>();
    let _ = room_chat_buffer.add(
        "empfang",
        "Besucher".to_string(),
        "Hallo Agent-01, bitte einmal melden.".to_string(),
        tick,
        &names,
    );
    let _ = room_chat_buffer.add(
        "buero-ceo",
        "Besucher".to_string(),
        "Kurzer Status fuer das Management.".to_string(),
        tick.saturating_sub(1),
        &names,
    );
    let _ = room_chat_buffer.add(
        "buero-betriebsarzt",
        "Besucher".to_string(),
        "Es geht um die Auslastung im Raum.".to_string(),
        tick.saturating_sub(2),
        &names,
    );
}

fn prepare_room_phase2_tick(world: &mut bevy_ecs::prelude::World, tick: u64) {
    reset_positions(world);
    reset_bio_state(world);
    seed_chat_activity(world, tick);
    world.resource_mut::<EventBuffer>().events.clear();

    let mut time = world.resource_mut::<SimulationTime>();
    time.tick = Tick(tick);
    time.tick_count = tick;
    time.delta_seconds = 1.0;
    time.sim_hour = 8.0 + (tick as f32 / 3600.0);
}

fn build_room_phase2_tick_world() -> (bevy_ecs::prelude::World, bevy_ecs::prelude::Schedule) {
    let (mut world, schedule) = create_simulation_world();
    let (room_distances, room_info) = build_room_maps();
    world.insert_resource(room_distances);
    world.insert_resource(room_info);

    for id in 1..=BENCH_AGENT_COUNT {
        let room_index = (id as usize).saturating_sub(1) % ROOM_IDS.len();
        spawn_agent(
            &mut world,
            AgentId(id),
            &format!("Agent-{id:02}"),
            "Benchmark",
            1,
            ROOM_IDS[room_index],
        );
    }

    prepare_room_phase2_tick(&mut world, 3);
    (world, schedule)
}

fn build_encounter_world() -> (bevy_ecs::prelude::World, bevy_ecs::prelude::Schedule) {
    let (mut world, _) = create_simulation_world();
    for id in 1..=BENCH_AGENT_COUNT {
        spawn_agent(
            &mut world,
            AgentId(id),
            &format!("Encounter-{id:02}"),
            "Benchmark",
            1,
            "empfang",
        );
    }

    let mut schedule = bevy_ecs::prelude::Schedule::default();
    schedule.add_systems(encounter_system);
    (world, schedule)
}

fn prepare_encounter_tick_dense(world: &mut bevy_ecs::prelude::World, tick: u64) {
    world.resource_mut::<EventBuffer>().events.clear();

    let mut query = world.query::<&mut Position>();
    for mut pos in query.iter_mut(world) {
        pos.room_id = "empfang".to_string();
        pos.in_transit = true;
        pos.transit_target = Some("buero-ceo".to_string());
        pos.transit_remaining_ms = 40_000;
        pos.transit_correlation_id = None;
        pos.transit_route = vec![
            "flur-eg".to_string(),
            "treppenhaus".to_string(),
            "flur-og".to_string(),
        ];
        pos.transit_total_ms = 80_000;
        pos.transit_paused = false;
        pos.transit_pause_tick = 0;
        pos.transit_source = Some("empfang".to_string());
    }

    let mut time = world.resource_mut::<SimulationTime>();
    time.tick = Tick(tick);
    time.tick_count = tick;
    time.delta_seconds = 1.0;
    time.sim_hour = 8.0;
}

fn prepare_encounter_tick_realistic(world: &mut bevy_ecs::prelude::World, tick: u64) {
    reset_positions(world);
    world.resource_mut::<EventBuffer>().events.clear();

    let mut time = world.resource_mut::<SimulationTime>();
    time.tick = Tick(tick);
    time.tick_count = tick;
    time.delta_seconds = 1.0;
    time.sim_hour = 8.0;
}

fn bench_route_bfs(c: &mut Criterion) {
    let (room_distances, _) = build_room_maps();

    let mut group = c.benchmark_group("room_phase2.route_bfs");
    group.sample_size(50);

    for (label, from, to) in [
        ("same_floor_2_hops", "empfang", "kueche"),
        ("cross_floor_4_hops", "empfang", "buero-ceo"),
        ("upper_to_lower_wing", "meetingraum-03", "buero-dev-1"),
        ("full_office_span", "buero-betriebsarzt", "buero-admin"),
    ] {
        group.bench_with_input(
            BenchmarkId::new("per_move", label),
            &(from, to),
            |b, pair| {
                b.iter(|| black_box(room_distances.route(black_box(pair.0), black_box(pair.1))))
            },
        );
    }

    group.finish();
}

fn bench_encounter_detection(c: &mut Criterion) {
    let (mut realistic_world, mut schedule) = build_encounter_world();
    let (mut dense_world, mut dense_schedule) = build_encounter_world();
    let mut realistic_tick = 3u64;
    let mut dense_tick = 3u64;

    c.bench_function("room_phase2.encounter_detection_26_agents", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                prepare_encounter_tick_realistic(&mut realistic_world, realistic_tick);
                let start = Instant::now();
                schedule.run(&mut realistic_world);
                total += start.elapsed();
                black_box(realistic_world.resource::<EventBuffer>().events.len());
                realistic_tick = if realistic_tick >= 30 {
                    3
                } else {
                    realistic_tick + 3
                };
            }
            total
        })
    });

    c.bench_function("room_phase2.encounter_detection_dense_26_agents", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                prepare_encounter_tick_dense(&mut dense_world, dense_tick);
                let start = Instant::now();
                dense_schedule.run(&mut dense_world);
                total += start.elapsed();
                black_box(dense_world.resource::<EventBuffer>().events.len());
                dense_tick = dense_tick.saturating_add(3);
            }
            total
        })
    });
}

fn bench_room_phase2_tick(c: &mut Criterion) {
    let (mut world, mut schedule) = build_room_phase2_tick_world();
    let mut tick = 3u64;

    c.bench_function("room_phase2.bio_tick_26_agents", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                prepare_room_phase2_tick(&mut world, tick);
                let start = Instant::now();
                schedule.run(&mut world);
                total += start.elapsed();
                black_box(world.resource::<EventBuffer>().events.len());
                tick = if tick >= 30 { 3 } else { tick + 3 };
            }
            total
        })
    });
}

criterion_group!(
    room_phase2_benches,
    bench_route_bfs,
    bench_encounter_detection,
    bench_room_phase2_tick
);
criterion_main!(room_phase2_benches);
