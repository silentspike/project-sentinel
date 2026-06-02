# sentinel-common

## Purpose

`sentinel-common` is the shared contract crate for Rust components. It defines domain IDs, agent and room components, domain events, feature flags, PSI data, room metadata, and world snapshot types.

## Interfaces

- `types.rs` defines newtypes, action models, room descriptors, and snapshot structs.
- `components.rs` defines ECS component payloads used by `sentinel-ecs`.
- `events.rs` defines append-only `DomainEvent` and payload variants.
- `snapshot_codec.rs` provides bincode-compatible world snapshot encode/decode plus the Kani-friendly `SnapshotCursor` codec.
- `agent_config.rs`, `feature_flags.rs`, `psi.rs`, and `room.rs` are shared loaders and utility contracts.

## Dependencies

- Serialization: `serde`, `serde_json`, `bincode`, `toml`, `flatbuffers`.
- Runtime contracts: `bevy_ecs`, `uuid`, `thiserror`, `anyhow`, `tracing`.

## Verify

```bash
cargo remote -c -- test -p sentinel-common
cargo remote -c -- test -p sentinel-common snapshot_codec
scripts/verify-kani.sh
```

The complete `WorldSnapshot` payload roundtrip is unit-tested; Kani proves the heap-free snapshot cursor roundtrip because the full `String`/`Vec` graph is not solver-friendly.
