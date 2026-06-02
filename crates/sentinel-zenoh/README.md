# sentinel-zenoh

## Purpose

`sentinel-zenoh` is the Rust communication bus wrapper. It provides publish/subscribe, scoped queries, stale-response filtering, in-flight limits, and shared-memory fallback behavior for the daemon-side runtime.

## Interfaces

- `SentinelBus` wraps a Zenoh session and exposes publish/subscribe/query methods.
- `BusConfig` loads transport and in-flight limits.
- `ScopedQuery`, `QueryScope`, and `QueryResponse` define request/response contracts.
- `topics` is the topic namespace SSOT.
- `flatbuf` contains FlatBuffer-compatible payload helpers.

## Dependencies

- `zenoh`, `tokio`, `tracing`, `anyhow`, `uuid`, `serde`, `serde_json`, and `flatbuffers`.
- `sentinel-common` for IDs and ticks.
- `sentinel-telemetry` for optional transport and in-flight metrics.

## Verify

```bash
cargo remote -c -- test -p sentinel-zenoh
cargo remote -c -- test -p sentinel-zenoh --test acceptance
```

Transport-latency benchmarks belong on the runtime/benchmark host, not the cargo remote build server.
