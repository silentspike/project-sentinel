# sentinel-ebpf-probes

## Purpose

`sentinel-ebpf-probes` contains the no-std eBPF programs embedded or loaded by `sentinel-ebpf`. It is intentionally separate from the main workspace because it targets `bpfel-unknown-none`.

## Interfaces

- `agent-health` tracks write activity per cgroup via `fentry/vfs_write`.
- `io-profile` tracks block I/O completion counters per cgroup.
- `network` tracks TCP connect/close events for LLM API network visibility.
- Per-CPU hash maps and ring buffers are the public contract to the userspace loader.

## Dependencies

- `aya-ebpf` and `aya-log-ebpf` from the aya git source.
- Nightly Rust, `build-std=core`, and the `bpfel-unknown-none` target.

## Verify

```bash
cd crates/sentinel-ebpf-probes
cargo +nightly build -Z build-std=core --target bpfel-unknown-none --release
```

This crate is not part of the normal workspace build. Pair successful probe builds with `sentinel-ebpf` userspace loader tests.
