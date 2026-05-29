# sentinel-limbo

## Purpose

`sentinel-limbo` is the append-only SQLite event store and transactional outbox for Rust services. It is the source for event replay, projection offsets, snapshots, and Zenoh/NATS publish handoff.

## Interfaces

- `EventStore` opens the database and owns schema creation.
- `append_with_outbox` atomically persists a domain event and pending publish row.
- `read_from_offset`, `get_latest_event_id`, and projection offset APIs support CQRS readers.
- Snapshot APIs persist runtime and world snapshots.
- `OutboxPublisher` drains pending publishes.
- `classify_offset_update` is the pure monotonic-offset contract used by Kani.

## Dependencies

- `rusqlite` with bundled SQLite, `tokio`, `serde`, `serde_json`, `uuid`, `tracing`, and `anyhow`.
- `sentinel-common` for domain events.
- `sentinel-telemetry` when the `telemetry` feature is active.

## Verify

```bash
cargo remote -c -- test -p sentinel-limbo
cargo remote -c -- test -p sentinel-limbo offset
scripts/verify-kani.sh
```

The Kani model proves deterministic operation-id dedup and offset monotonicity; real SQLite I/O remains covered by integration tests.
