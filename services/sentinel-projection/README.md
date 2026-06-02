# sentinel-projection

## Purpose

`sentinel-projection` is the service wrapper around the `sentinel-projection` crate. It runs the projection worker process that keeps dashboard read models current from the append-only event store.

## Interfaces

- `main.rs` parses CLI/config, initializes logging, opens the event store and read model store, and starts the worker loop.
- The service exposes no LLM path; it is a projection-read/write process for dashboard state.
- Read model contracts live in `crates/sentinel-projection`.

## Dependencies

- `sentinel-common`, `sentinel-limbo`, `sentinel-projection`, and `sentinel-telemetry`.
- `tokio`, `tracing`, `tracing-subscriber`, `clap`, and `anyhow`.

## Verify

```bash
cargo remote -c -- test -p sentinel-projection-service
cargo remote -c -- build -p sentinel-projection-service --release
```

Dashboard/projection changes require deploy-VM smoke checks against port `8000` and the projection database, with the gateway left inactive unless the issue explicitly needs it.
