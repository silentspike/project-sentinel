# sentinel-ecs

## Purpose

`sentinel-ecs` is the deterministic world kernel. It wires bevy ECS components and systems for agent biology, room physics, transit, perception, decisions, autonomy, event emission, and world snapshots.

## Interfaces

- `create_simulation_world()` creates the ECS `World` and schedule.
- `spawn_agent`, `despawn_agent_from_world`, `apply_personality`, and `apply_capabilities` manage agent entities.
- `snapshot_ecs_state` and `restore_ecs_state` bridge the Time Machine snapshot path.
- `SimulationPhase` defines the ordered tick pipeline.
- Runtime resources such as `LimboEventStore`, `RedbStateStore`, `ZenohFanoutSender`, `ToolRuntimeResource`, and operator buffers connect the kernel to services.

## Dependencies

- `bevy_ecs` for the deterministic world model.
- `sentinel-common`, `sentinel-bio`, `sentinel-physics`, `sentinel-redb`, `sentinel-limbo`, and `sentinel-wasm`.
- `tokio`, `tracing`, `serde`, `serde_json`, and `uuid` for integration payloads and async boundaries.

## Verify

```bash
cargo remote -c -- test -p sentinel-ecs
cargo remote -c -- test -p sentinel-ecs --test integration_event_path
cargo remote -c -- test -p sentinel-ecs --test wasm_ecs_integration
```

Benchmarks under `benches/` are issue-specific performance tools and should be run on the runtime/benchmark host, not the cargo build server.
