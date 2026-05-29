# sentinel-fs

## Purpose

`sentinel-fs` implements the artifact plane: a content-addressed, deduplicated, compressed, agent-scoped filesystem layer with redb metadata and optional FUSE exposure.

## Interfaces

- `ArtifactPlane` and `LayerManager` manage base/agent copy-on-write layers.
- `cas`, `chunker`, `ingest`, `read_planner`, and `segment` handle content-defined chunks, dedup, and streaming reads.
- `metadata` stores inode, dirent, refcount, and trash queue state in redb.
- `gc` and `commit_scheduler` coordinate cleanup and durability.
- `cli` provides operator entrypoints for local diagnostics.

## Dependencies

- `redb`, `sha2`, `blake3`, `zstd`, `rayon`, `serde`, `serde_json`, `bincode`, and `tracing`.
- `sentinel-common` and `sentinel-telemetry`.
- Optional `fuser`, `libc`, and `io-uring` features for host integration.

## Verify

```bash
cargo remote -c -- test -p sentinel-fs
python3 scripts/check-unsafe-baseline.py
```

Filesystem and dedup performance benchmarks must run on the target benchmark host with system metrics, not on the cargo remote build server.
