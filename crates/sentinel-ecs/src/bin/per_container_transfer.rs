//! #497 AC-5 / benchmark harness — per-container snapshot/restore transfer.
//!
//! Manual 2-VM test (Issue Out-of-Scope: "local + manual file copy for the 2-VM test"):
//!   node-0:  `per_container_transfer snapshot <agent> <out.json>`
//!            -> writes the serialized NanoContainerSnapshot + prints the container state-hash.
//!   `scp <out.json>` node-0 -> node-1, then
//!   node-1:  `per_container_transfer restore <agent> <in.json>`
//!            -> restores into a FRESH world, re-snapshots, prints the container state-hash.
//!   The two hashes must be equal (B == A) — that is AC-5 across two machines. The scp step
//!   replaces the #569 control stream; it proves the snapshot/restore primitives transfer a
//!   container faithfully, NOT the live daemon move (which is #501).
//!
//!   `per_container_transfer bench` -> snapshot/restore latency p50/p95/p99/max + bytes/agent,
//!   small/medium/large state sweep. Run on the test VM (.241), never via cargo remote.

use std::time::Instant;

use bevy_ecs::world::World;
use sentinel_common::{AgentId, NanoContainerEcsSnapshot, NanoContainerSnapshot, Tick};
use sentinel_ecs::{
    create_simulation_world, restore_agent_ecs_state, snapshot_agent_ecs_state, spawn_agent,
    SimulationTime,
};

/// Wrap a per-agent ECS snapshot into the transfer envelope (ECS-native: no redb/fs/cut here —
/// `state_hash` ignores the metadata anyway, and a fresh harness world has no persisted redb rows).
fn envelope(ecs: &NanoContainerEcsSnapshot) -> NanoContainerSnapshot {
    NanoContainerSnapshot {
        agent_id: ecs.agent_id,
        captured_at_tick: 0,
        ecs: ecs.clone(),
        redb_rows: Default::default(),
        fs_subtree: None,
        cut: Default::default(),
    }
}

/// Deterministic world: `n` agents, ticked `ticks` times to drift their state.
fn build_world(n: u16, ticks: u64) -> World {
    let (mut world, mut sched) = create_simulation_world();
    for i in 1..=n {
        spawn_agent(
            &mut world,
            AgentId(i),
            &format!("Agent {i}"),
            "Dev",
            1,
            "buero-dev-1",
        );
    }
    for t in 1..=ticks {
        world.resource_mut::<SimulationTime>().tick = Tick(t);
        sched.run(&mut world);
    }
    world
}

fn summarize(mut us: Vec<u64>) -> String {
    us.sort_unstable();
    let p = |q: f64| us[(((us.len() - 1) as f64) * q) as usize];
    format!(
        "p50={} p95={} p99={} max={} us",
        p(0.50),
        p(0.95),
        p(0.99),
        us[us.len() - 1]
    )
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str).unwrap_or("bench") {
        "snapshot" => {
            let agent: u16 = args[2].parse().expect("agent id");
            let out = &args[3];
            // Drift enough agents so the target one has non-trivial state.
            let mut world = build_world(agent.max(2), 30);
            let snap = envelope(
                &snapshot_agent_ecs_state(&mut world, AgentId(agent)).expect("agent exists"),
            );
            let bytes = serde_json::to_vec(&snap).expect("serialize");
            std::fs::write(out, &bytes).expect("write file");
            println!(
                "snapshot agent={agent} bytes={} state_hash={:016x}",
                bytes.len(),
                snap.state_hash()
            );
        }
        "restore" => {
            let agent: u16 = args[2].parse().expect("agent id");
            let inp = &args[3];
            let bytes = std::fs::read(inp).expect("read file");
            let snap: NanoContainerSnapshot = serde_json::from_slice(&bytes).expect("deserialize");
            // Restore into a FRESH world (the "other node"), then re-snapshot + re-hash.
            let (mut world, _) = create_simulation_world();
            restore_agent_ecs_state(&mut world, &snap.ecs);
            let rehash = envelope(
                &snapshot_agent_ecs_state(&mut world, AgentId(agent)).expect("restored agent"),
            )
            .state_hash();
            println!("restore  agent={agent} state_hash={rehash:016x}");
        }
        "bench" => {
            let iters = 200usize;
            for (label, ticks) in [("small", 1u64), ("medium", 60), ("large", 600)] {
                let mut world = build_world(26, ticks);
                let mut snap_us = Vec::with_capacity(iters);
                let mut rest_us = Vec::with_capacity(iters);
                let mut env = envelope(
                    &snapshot_agent_ecs_state(&mut world, AgentId(1)).expect("agent 1 exists"),
                );
                let mut bytes = 0usize;
                for _ in 0..iters {
                    let t0 = Instant::now();
                    let s = snapshot_agent_ecs_state(&mut world, AgentId(1)).expect("agent 1");
                    snap_us.push(t0.elapsed().as_micros() as u64);
                    env = envelope(&s);
                    bytes = serde_json::to_vec(&env).expect("serialize").len();
                }
                for _ in 0..iters {
                    let (mut w2, _) = create_simulation_world();
                    let t0 = Instant::now();
                    restore_agent_ecs_state(&mut w2, &env.ecs);
                    rest_us.push(t0.elapsed().as_micros() as u64);
                }
                println!(
                    "[{label}] bytes/agent={bytes} | snapshot {} | restore {}",
                    summarize(snap_us),
                    summarize(rest_us)
                );
            }
        }
        other => eprintln!(
            "unknown mode {other:?}; usage: per_container_transfer [snapshot <agent> <file> | restore <agent> <file> | bench]"
        ),
    }
}
