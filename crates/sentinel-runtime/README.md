# sentinel-runtime

## Purpose

`sentinel-runtime` orchestrates agent lifecycle state outside the ECS tick loop. It tracks active/sleeping/suspended/errored agents, emits lifecycle events, and supports snapshot-based restore.

## Interfaces

- `RuntimeOrchestrator` manages spawn, despawn, pause, resume, error, recovery, and shift transitions.
- `RuntimeEventSink` lets ECS and gateway-facing integrations observe lifecycle transitions.
- `AgentStatus`, `AgentHandle`, and runtime snapshots are the core state model.
- Event emission uses `sentinel-limbo` so lifecycle changes remain replayable.
- `EcsNativeRuntime` implements the shared `NanoRuntime` contract for the
  `ecs-native` runtime key. It maps `snapshot`/`restore` to the ECS world
  snapshot codec and uses the shared conformance harness as its contract test.

## Dependencies

- `sentinel-common` for agent identity, shift info, ticks, and events.
- `sentinel-ecs` for ECS world snapshot and restore in the `ecs-native`
  NanoRuntime adapter.
- `sentinel-limbo` for persistence.
- `anyhow`, `tracing`, `serde`, `serde_json`, and `uuid`.

## Verify

```bash
cargo remote -c -- test -p sentinel-runtime
cargo remote -c -- test -p sentinel-runtime --test acceptance
cargo remote -c -- test -p sentinel-runtime --test nano_runtime_conformance
```

Runtime behavior changes usually require daemon-level smoke evidence because the daemon owns the live runtime orchestration.
