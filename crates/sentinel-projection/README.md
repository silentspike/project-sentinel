# sentinel-projection

## Purpose

`sentinel-projection` maintains CQRS read models for dashboard and operator views. It consumes append-only events from `sentinel-limbo` and updates optimized SQLite projection tables.

## Interfaces

- `ProjectionWorker` reads events from the event store and advances offsets.
- `ReadModelStore` owns projection schema and materialized views.
- `ProjectionConfig` loads worker/store configuration.
- Handlers update `agent_live_view`, `room_live_view`, and `kpi_1m`.

## Dependencies

- `sentinel-common`, `sentinel-physics`, `sentinel-limbo`, and `sentinel-telemetry`.
- `rusqlite`, `serde`, `serde_json`, `uuid`, `tracing`, and `anyhow`.

## Verify

```bash
cargo remote -c -- test -p sentinel-projection
cargo remote -c -- test -p sentinel-projection --test acceptance
```

Runtime dashboard changes should be verified on the deploy VM because projection reads compete with the live writer.
