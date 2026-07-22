# Remote Rust Gates

All Rust commands run through cargo-remote. No local Cargo, rustc, rust-analyzer,
benchmark, VM, or runtime command is used.

## Formatting

```bash
cargo remote -c -- fmt --all -- --check
```

```text
exit=0
format=PASS
```

## Workspace Check

```bash
cargo remote -c -- check --workspace --all-targets --locked -j1
```

```text
Checking criterion v0.8.2
Checking sentinel-dashboard-backend v0.1.0 (<REMOTE_WORKSPACE>/services/sentinel-dashboard-backend)
Checking sentinel-nightrun v0.1.0 (<REMOTE_WORKSPACE>/services/sentinel-nightrun)
Checking sentinel-projection-service v0.1.0 (<REMOTE_WORKSPACE>/services/sentinel-projection)
Finished dev profile
exit=0
```

## Workspace Test

The first final-state attempt was rejected as evidence because a concurrent-load
infrastructure kill terminated the Wasmtime compiler:

```bash
cargo remote -c -- test --workspace --locked -j1
```

```text
error: could not compile wasmtime (lib)
process did not exit successfully: rustc ... (signal: 9, SIGKILL)
accepted_as_gate=false
```

One subsequent run reached the test phase but was rejected because the existing
one-millisecond circuit-breaker timing test raced under shared load. A targeted retry
was also rejected when the linker was killed before the test started. Neither attempt
is counted as a gate.

The required successful rerun used serial test execution and reduced test-profile
debug data:

```bash
cargo remote -b 'RUST_BACKTRACE=1 CARGO_PROFILE_TEST_DEBUG=0' -c -- \
  test --workspace --locked -j1 -- --test-threads=1
```

```text
Finished test profile
workspace test binaries and documentation tests completed
workspace_result=PASS
exit=0
```

## Clippy

```bash
cargo remote -c -- clippy --workspace --all-targets --locked -j1 -- -D warnings
```

```text
Finished dev profile
warnings_denied=true
exit=0
```

## Workspace Build

```bash
cargo remote -b 'RUST_BACKTRACE=1 CARGO_PROFILE_DEV_DEBUG=0' -c -- \
  build --workspace --locked -j1
```

```text
Finished dev profile
workspace_build=PASS
exit=0
```

## Release Build

The complete eight-root command verifies that the selected production roots compile
together:

```bash
cargo remote -c -- build --release --locked -j1 \
  -p sentinel-daemon -p sentinel-projection-service \
  -p sentinel-dashboard-backend -p sentinel-gaia-loop \
  -p agent-runtime -p sentinel-ctl -p sentinel-gaia -p sentinel-nightrun
```

```text
Compiling sentinel-daemon v0.1.0 (<REMOTE_WORKSPACE>/services/sentinel-daemon)
Compiling sentinel-dashboard-backend v0.1.0 (<REMOTE_WORKSPACE>/services/sentinel-dashboard-backend)
Compiling sentinel-nightrun v0.1.0 (<REMOTE_WORKSPACE>/services/sentinel-nightrun)
Finished release profile
exit=0
```

Artifact bytes must be compared with the root-isolated Issue #631 baseline. Each root
was therefore rebuilt separately with this command shape:

```bash
cargo remote -c -- build --release --locked -j1 -p <PACKAGE> --bin <BINARY>
```

```text
root_builds=8/8
artifact_bytes_and_sha256=recorded
exit=0
```

The first root-isolated attempt omitted `-j1` and was rejected after `bevy_ecs` received
SIGKILL. The serial rerun above completed all eight roots. Artifact construction is a
correctness and structural-size gate only; no remote duration is retained or
interpreted as performance evidence.
