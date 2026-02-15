//! Benchmarks fuer RuntimeOrchestrator Operationen (Issue #15).
//!
//! Misst Overhead der Runtime-Schicht ueber dem rohen EventStore:
//! - Spawn/Despawn mit Event-Emission (AC-2)
//! - Shift-Transition mit Bulk-Remove (AC-2)
//! - save_state/restore Snapshot-Roundtrip (AC-4)
//! - Realistischer Schichtwechsel-Zyklus
//!
//! WICHTIG: Diese Benchmarks MUESSEN auf der Deployment-VM ausgefuehrt werden
//! (NICHT auf dem Build-Server/LXC). Siehe CLAUDE.md.

use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use sentinel_common::components::{AgentIdentity, ShiftInfo};
use sentinel_common::AgentId;
use sentinel_limbo::EventStore;
use sentinel_runtime::RuntimeOrchestrator;

/// Anzahl Agents pro Schicht (SSOT: config/rooms.toml)
const AGENTS_PER_SHIFT: u16 = 15;

fn create_identity(id: u16, name: &str, role: &str) -> AgentIdentity {
    AgentIdentity {
        agent_id: AgentId(id),
        name: name.to_string(),
        role: role.to_string(),
    }
}

fn create_shift(shift_set: u8, start: u8, end: u8) -> ShiftInfo {
    ShiftInfo {
        shift_set,
        shift_start_hour: start,
        shift_end_hour: end,
        is_on_duty: true,
    }
}

fn temp_store() -> (tempfile::TempDir, Arc<EventStore>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench_runtime.db");
    let store = EventStore::open(path.to_str().unwrap()).unwrap();
    (dir, Arc::new(store))
}

// ──────────────────────────────────────────────
// Einzeloperationen: Spawn / Despawn Latenz
// ──────────────────────────────────────────────

/// Spawn eines einzelnen Agenten MIT Event-Emission.
/// Misst: HashMap-Insert + JSON-Serialize + append_with_outbox.
fn bench_spawn_with_events(c: &mut Criterion) {
    let (_dir, store) = temp_store();
    let mut id_counter = 0u16;

    c.bench_function("spawn_agent_with_event", |b| {
        b.iter(|| {
            id_counter += 1;
            let mut orch = RuntimeOrchestrator::new(1000).with_event_store(store.clone());
            orch.set_tick(id_counter as u64);
            black_box(
                orch.spawn_agent(
                    create_identity(id_counter, &format!("Agent-{id_counter}"), "Worker"),
                    create_shift(1, 6, 14),
                )
                .unwrap(),
            );
        });
    });
}

/// Spawn OHNE Event-Store (Baseline fuer Overhead-Vergleich).
fn bench_spawn_without_events(c: &mut Criterion) {
    let mut id_counter = 0u16;

    c.bench_function("spawn_agent_no_event", |b| {
        b.iter(|| {
            id_counter += 1;
            let mut orch = RuntimeOrchestrator::new(1000);
            black_box(
                orch.spawn_agent(
                    create_identity(id_counter, &format!("Agent-{id_counter}"), "Worker"),
                    create_shift(1, 6, 14),
                )
                .unwrap(),
            );
        });
    });
}

/// Despawn eines Agenten MIT Event-Emission.
fn bench_despawn_with_events(c: &mut Criterion) {
    let (_dir, store) = temp_store();

    c.bench_function("despawn_agent_with_event", |b| {
        b.iter_custom(|iters| {
            let mut total = std::time::Duration::ZERO;
            for i in 0..iters {
                let id = (i as u16) + 1;
                let mut orch = RuntimeOrchestrator::new(1000).with_event_store(store.clone());
                orch.set_tick(i);
                orch.spawn_agent(
                    create_identity(id, &format!("Agent-{id}"), "Worker"),
                    create_shift(1, 6, 14),
                )
                .unwrap();

                let start = std::time::Instant::now();
                black_box(orch.despawn_agent(AgentId(id)).unwrap());
                total += start.elapsed();
            }
            total
        });
    });
}

// ──────────────────────────────────────────────
// Shift-Transition: Bulk-Remove mit Event
// ──────────────────────────────────────────────

/// Shift-Transition mit 15 Agents (entfernt Set 1, behaelt Set 2 + Sonder).
fn bench_shift_transition_15_agents(c: &mut Criterion) {
    let (_dir, store) = temp_store();

    c.bench_function("shift_transition_15_agents", |b| {
        b.iter_custom(|iters| {
            let mut total = std::time::Duration::ZERO;
            for i in 0..iters {
                let mut orch = RuntimeOrchestrator::new(50).with_event_store(store.clone());
                orch.set_tick(i);

                // Spawn 15 Set-1 + 1 Sonder
                for id in 1..=AGENTS_PER_SHIFT {
                    orch.spawn_agent(
                        create_identity(id, &format!("S1-{id}"), "Worker"),
                        create_shift(1, 6, 14),
                    )
                    .unwrap();
                }
                orch.spawn_agent(
                    create_identity(46, "Betriebsrat", "Sonder"),
                    create_shift(0, 0, 23),
                )
                .unwrap();

                let start = std::time::Instant::now();
                let removed = black_box(orch.shift_transition(2));
                total += start.elapsed();
                assert_eq!(removed.len(), AGENTS_PER_SHIFT as usize);
            }
            total
        });
    });
}

// ──────────────────────────────────────────────
// Snapshot: save_state / restore (AC-4)
// ──────────────────────────────────────────────

/// save_state() mit N Agents (parametrisiert: 5, 15, 50).
fn bench_save_state(c: &mut Criterion) {
    let mut group = c.benchmark_group("save_state");

    for agent_count in [5u16, 15, 50] {
        let (_dir, store) = temp_store();
        let mut orch = RuntimeOrchestrator::new(100).with_event_store(store.clone());
        orch.set_tick(1);

        for id in 1..=agent_count {
            orch.spawn_agent(
                create_identity(id, &format!("Agent-{id}"), "Worker"),
                create_shift(1, 6, 14),
            )
            .unwrap();
        }

        group.bench_with_input(
            BenchmarkId::from_parameter(agent_count),
            &agent_count,
            |b, _| {
                b.iter(|| {
                    black_box(orch.save_state().unwrap());
                });
            },
        );
    }
    group.finish();
}

/// restore() mit N Agents (parametrisiert: 5, 15, 50).
fn bench_restore(c: &mut Criterion) {
    let mut group = c.benchmark_group("restore");

    for agent_count in [5u16, 15, 50] {
        let (_dir, store) = temp_store();
        let mut orch = RuntimeOrchestrator::new(100).with_event_store(store.clone());
        orch.set_tick(1);

        for id in 1..=agent_count {
            orch.spawn_agent(
                create_identity(id, &format!("Agent-{id}"), "Worker"),
                create_shift(1, 6, 14),
            )
            .unwrap();
        }
        orch.save_state().unwrap();

        group.bench_with_input(
            BenchmarkId::from_parameter(agent_count),
            &agent_count,
            |b, _| {
                b.iter(|| {
                    black_box(RuntimeOrchestrator::restore(store.clone(), 100).unwrap());
                });
            },
        );
    }
    group.finish();
}

// ──────────────────────────────────────────────
// Realistischer Zyklus: Schichtwechsel + Restart
// ──────────────────────────────────────────────

/// Vollstaendiger Schichtwechsel-Zyklus:
/// 1. Spawn 15 Agents (Set 1) + 1 Sonder
/// 2. Shift-Transition zu Set 2 (entfernt Set 1, behaelt Sonder)
/// 3. Spawn 15 Agents (Set 2)
/// 4. save_state()
///
/// Misst den gesamten Zyklus als "Schichtwechsel + Persist".
fn bench_full_shift_cycle(c: &mut Criterion) {
    let (_dir, store) = temp_store();

    c.bench_function("full_shift_cycle_15_agents", |b| {
        b.iter_custom(|iters| {
            let mut total = std::time::Duration::ZERO;
            for i in 0..iters {
                let mut orch = RuntimeOrchestrator::new(50).with_event_store(store.clone());
                orch.set_tick(i * 100);

                // 1. Spawn Set 1 + Sonder
                for id in 1..=AGENTS_PER_SHIFT {
                    orch.spawn_agent(
                        create_identity(id, &format!("S1-{id}"), "Worker"),
                        create_shift(1, 6, 14),
                    )
                    .unwrap();
                }
                orch.spawn_agent(
                    create_identity(46, "Betriebsrat", "Sonder"),
                    create_shift(0, 0, 23),
                )
                .unwrap();

                let start = std::time::Instant::now();

                // 2. Shift to Set 2
                let _ = orch.shift_transition(2);

                // 3. Spawn Set 2
                for id in 16..=(15 + AGENTS_PER_SHIFT) {
                    orch.spawn_agent(
                        create_identity(id, &format!("S2-{id}"), "Worker"),
                        create_shift(2, 14, 22),
                    )
                    .unwrap();
                }

                // 4. Persist
                orch.save_state().unwrap();

                total += start.elapsed();
            }
            total
        });
    });
}

/// Restart-Zyklus: save_state + drop + restore.
/// Misst die Recovery-Zeit nach simuliertem Neustart.
fn bench_restart_cycle(c: &mut Criterion) {
    let (_dir, store) = temp_store();

    // Pre-fill: 15 agents + save
    {
        let mut orch = RuntimeOrchestrator::new(50).with_event_store(store.clone());
        orch.set_tick(1);
        for id in 1..=AGENTS_PER_SHIFT {
            orch.spawn_agent(
                create_identity(id, &format!("Agent-{id}"), "Worker"),
                create_shift(1, 6, 14),
            )
            .unwrap();
        }
        orch.save_state().unwrap();
    }

    c.bench_function("restart_cycle_15_agents", |b| {
        b.iter(|| {
            // Restore (simulates restart)
            let restored = black_box(RuntimeOrchestrator::restore(store.clone(), 50).unwrap());
            assert_eq!(restored.agent_count(), AGENTS_PER_SHIFT as usize);
        });
    });
}

criterion_group!(
    benches,
    // Einzeloperationen
    bench_spawn_with_events,
    bench_spawn_without_events,
    bench_despawn_with_events,
    // Shift-Transition
    bench_shift_transition_15_agents,
    // Snapshot (AC-4)
    bench_save_state,
    bench_restore,
    // Realistische Zyklen
    bench_full_shift_cycle,
    bench_restart_cycle,
);
criterion_main!(benches);
