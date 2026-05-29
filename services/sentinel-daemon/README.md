# sentinel-daemon

## Purpose

`sentinel-daemon` is the main Rust service. It owns the ECS tick loop, runtime orchestration, operator API, platform control plane, snapshot/restore, sandbox supervision, telemetry, and bridge points to storage, Zenoh, NATS, eBPF, WASM, and nightrun/evolution work.

## Interfaces

- `main.rs` loads config, initializes logging, and starts the orchestrator.
- `orchestrator.rs` wires the tick loop and runtime resources.
- `operator_api.rs` exposes operator commands such as chat, Gaia, broadcast, snapshot, and restore.
- `snapshot.rs`, `runtime_control.rs`, and `runtime_health.rs` own Time Machine and process-health paths.
- `llm_bridge.rs`, `nats_consumer.rs`, `query_responder.rs`, and `fanout.rs` connect to external services and buses.

## Dependencies

- Internal crates: `sentinel-common`, `sentinel-ecs`, `sentinel-projection`, `sentinel-runtime`, `sentinel-zenoh`, `sentinel-limbo`, `sentinel-redb`, `sentinel-sandbox`, `sentinel-telemetry`, `sentinel-ebpf`, `sentinel-hippocampus`, `sentinel-wasm`, and optional `sentinel-fs`.
- Runtime libraries: `tokio`, `tracing`, `serde`, `serde_json`, `toml`, `redb`, `bincode`, `sha2`, `clap`, `chrono`, `uuid`, optional `reqwest`, and optional `async-nats`.

## Verify

```bash
cargo remote -c -- test -p sentinel-daemon
cargo remote -c -- clippy -p sentinel-daemon --all-targets -- -D warnings
cargo remote -c -- build -p sentinel-daemon --release
```

Service changes require deploy-VM verification on `10.0.0.240` with the actual systemd `ExecStart` path checked before replacing binaries.
