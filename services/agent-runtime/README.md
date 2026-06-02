# agent-runtime

## Purpose

`agent-runtime` is the lightweight process launched inside an agent sandbox. It has zero external Rust dependencies and exists to provide a minimal controllable process with observable heartbeat I/O.

## Interfaces

- `main.rs` reads stdin for future command dispatch and exits on EOF or `shutdown`.
- It writes `/tmp/heartbeat` every five seconds so eBPF/userspace health collectors can observe activity.
- stderr logs process start, shutdown, and heartbeat write failures.

## Dependencies

- Rust standard library only.
- Runtime host dependencies come from the parent sandbox: bwrap, cgroups, and daemon process supervision.

## Verify

```bash
cargo remote -c -- test -p agent-runtime
cargo remote -c -- build -p agent-runtime --release
```

Live behavior is verified through `sentinel-daemon` sandbox/runtime-health tests because the daemon owns process launch and supervision.
