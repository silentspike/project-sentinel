# sentinel-redb

## Purpose

`sentinel-redb` is the hot-state ACID key-value store for agent state, room state, relationships, personalities, evolution data, fact caches, simulation metadata, and API control-plane pattern snapshots.

## Interfaces

- `StateStore::open` creates all redb tables.
- Agent, room, relationship, personality, and evolution accessors store serialized payloads.
- `ApiCpSnapshot` and `ApiCpPatternSnapshot` persist synthesis/control-plane pattern state.
- Simulation metadata APIs preserve time virtualization state across restarts.

## Dependencies

- `redb`, `serde`, `serde_json`, `tracing`, and `anyhow`.
- `sentinel-common` for ID contracts.
- `sentinel-telemetry` for optional operation latency metrics.

## Verify

```bash
cargo remote -c -- test -p sentinel-redb
```

When changing schema/table names, also verify daemon snapshot/restore and nightrun consolidation paths that read or write these tables.
