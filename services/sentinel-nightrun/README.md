# sentinel-nightrun

## Purpose

`sentinel-nightrun` performs shift-change and nightly consolidation work outside the hot ECS tick loop. It handles deterministic replay, hash-chain integrity, guardrails, job queuing, and hippocampus/evolution persistence.

## Interfaces

- `main.rs` and `runner.rs` run the service loop and CLI/runtime entrypoints.
- `replay.rs` reconstructs deterministic state from events.
- `hash_chain.rs` records integrity markers for replay/consolidation runs.
- `job_queue.rs` manages queued consolidation work.
- `shift.rs` detects current shift and maps simulated hour to shift set.
- `guardrails.rs` protects service execution boundaries.

## Dependencies

- `sentinel-common`, `sentinel-hippocampus`, `sentinel-limbo`, and `sentinel-telemetry`.
- `tokio`, `tracing`, `serde`, `serde_json`, `toml`, `uuid`, `clap`, `rusqlite`, `sha2`, and `libc`.

## Verify

```bash
cargo remote -c -- test -p sentinel-nightrun
cargo remote -c -- test -p sentinel-nightrun --lib shift
cargo remote -c -- build -p sentinel-nightrun --release
```

Shift-detection changes need fixed-time regression tests because wrong shift mapping changes agent spawn/consolidation behavior.
