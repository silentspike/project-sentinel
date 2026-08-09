# Detached Dependency Group Gates

The original implementation commits have no message bodies, so this follow-up
evidence binds each immutable commit set to a freshly replayed final state. Every
replay used a detached worktree based on
`origin/main@9d8bb2fc9cca1140867aff20280df7fb54b0a6f2`. The listed diff digest is
the SHA-256 of the staged binary diff for that group's relevant manifests,
lockfile, and benchmark sources.

All Rust commands ran through `cargo remote -c --`. Raw wrapper output remains
untracked; the normalized excerpts below omit paths, hosts, users, timestamps,
transfer progress, and durations. No VM, runtime, benchmark execution, or
performance measurement was involved.

## Final Group Matrix

| Group | Effective rows | Immutable source commits | Diff SHA-256 | Result |
| --- | --- | --- | --- | --- |
| G1 | DEP-001 prune; DEP-002 leave | `776b108489c16f8f039de7ce8c8fd9a07857c699`, `d0fa37982f1a6ff34536e29a907e9557b5519a0b`, `27e4d6b389bfa5a091ff6c48e672b14e70d19c49` | `5853ebfe6867cc99095817d75de45937c084d60ee54dbbb7ff54f1f480a43eb8` | PASS |
| G2 | DEP-003 prune | `b6552e449893b94e9ccab28f1e6b5b8b28b9b3c8` | `61143430ef7f9e4f8d300f3a30af7fa249cf2daa9f77e19b52bb664f74ebb6a3` | PASS |
| G3 | DEP-004 leave; DEP-005, DEP-006, DEP-007, DEP-009 prune | `ee639af327684a3eaf5eb67ff15b277df603c91f`, `ef0ad9d5926cfeaa0eb1d0335a75c8fa3e0a1c83` | `c5e81a293c3739cc12bcf63cccbadaf6c9031766b22503f51397a235fb5254a3` | PASS |
| G4 | DEP-008, DEP-010 prune | `d2316707c6eeedf0be8cadcf0a73ed107bc2429e` | `ea7ccbcf13925ebf3d564a4cd5d1b0c18023e76cf07f144afbe9738e742823d4` | PASS |
| G5 | DEP-011 align | `8e4daa82dbf4381e3d4db634ea2917c65cb7304d` | `b8e426e6dfb3cfe1f6b2d76efed85cac797a8f69dc8ec10374c667be1fb48450` | PASS |

DEP-012 and DEP-013 remain `investigate` and have no dependency mutation group.

## G1 Workspace Runtime Features

```bash
cargo remote -c -- check --workspace --all-targets --locked -j1
```

```text
Checking sentinel-telemetry v0.1.0 (<REMOTE_WORKSPACE>/crates/sentinel-telemetry)
Checking sentinel-daemon v0.1.0 (<REMOTE_WORKSPACE>/services/sentinel-daemon)
Checking sentinel-dashboard-backend v0.1.0 (<REMOTE_WORKSPACE>/services/sentinel-dashboard-backend)
Checking sentinel-nightrun v0.1.0 (<REMOTE_WORKSPACE>/services/sentinel-nightrun)
Finished dev profile
exit=0
```

## G2 Futures Features

```bash
cargo remote -c -- check --locked -j1 --all-targets \
  -p sentinel-daemon -p sentinel-dashboard-backend -p sentinel-gaia-loop
```

```text
Checking sentinel-daemon v0.1.0 (<REMOTE_WORKSPACE>/services/sentinel-daemon)
Checking sentinel-gaia-loop v0.1.0 (<REMOTE_WORKSPACE>/services/sentinel-gaia-loop)
Checking sentinel-dashboard-backend v0.1.0 (<REMOTE_WORKSPACE>/services/sentinel-dashboard-backend)
Finished dev profile
exit=0
```

## G3 Dashboard Features And Edges

```bash
cargo remote -c -- check -p sentinel-dashboard-backend --all-targets --locked -j1
cargo remote -c -- test -p sentinel-dashboard-backend --locked -j1
```

```text
Checking sentinel-dashboard-backend v0.1.0 (<REMOTE_WORKSPACE>/services/sentinel-dashboard-backend)
check_exit=0
tests_passed=86
tests_failed=0
tests_ignored=2
test_exit=0
```

## G4 Projection And Nightrun Edges

```bash
cargo remote -c -- check --locked -j1 --all-targets \
  -p sentinel-projection-service -p sentinel-nightrun
cargo remote -c -- test --locked -j1 \
  -p sentinel-projection-service -p sentinel-nightrun
```

```text
Checking sentinel-nightrun v0.1.0 (<REMOTE_WORKSPACE>/services/sentinel-nightrun)
Checking sentinel-projection-service v0.1.0 (<REMOTE_WORKSPACE>/services/sentinel-projection)
check_exit=0
tests_passed=53
tests_failed=0
tests_ignored=0
test_exit=0
```

## G5 Criterion Alignment

This is a compilation gate for benchmark targets, not benchmark execution.

```bash
cargo remote -c -- check --locked -j1 --benches \
  -p sentinel-telemetry -p sentinel-zenoh
```

```text
Checking criterion v0.8.2
Checking sentinel-telemetry v0.1.0 (<REMOTE_WORKSPACE>/crates/sentinel-telemetry)
Checking sentinel-zenoh v0.1.0 (<REMOTE_WORKSPACE>/crates/sentinel-zenoh)
Finished dev profile
exit=0
```

## Rejected DEP-002 Experiment

The first G1 implementation state removed tracing-subscriber JSON support. It was
not accepted as a gate or as a valid prune.

```bash
cargo remote -c -- check -p sentinel-telemetry --locked -j1
```

```text
Checking sentinel-telemetry v0.1.0 (<REMOTE_WORKSPACE>/crates/sentinel-telemetry)
error[E0599]: no method named `json` found for struct `tracing_subscriber::fmt::Layer`
  --> crates/sentinel-telemetry/src/logging.rs:45:28
error: could not compile `sentinel-telemetry` (lib) due to 1 previous error
exit=101
accepted_as_gate=false
effective_decision=leave
correcting_commit=27e4d6b389bfa5a091ff6c48e672b14e70d19c49
```

## Rejected DEP-004 Experiment

Pruning only the dashboard's direct zstd defaults left the same defaults
release-reachable through `sentinel-console-plane -> sentinel-fs`. The experiment
therefore had no release-graph pruning effect and was restored before the G3 gates.

```bash
cargo remote -c -- tree -p sentinel-dashboard-backend -e features,no-dev
```

```text
direct_zstd_defaults_disabled=true
transitive_zstd_defaults_release_reachable=true
release_path=sentinel-dashboard-backend -> sentinel-console-plane -> sentinel-fs -> zstd/default
prune_effect=false
accepted_as_gate=false
effective_decision=leave
correcting_commit=ef0ad9d5926cfeaa0eb1d0335a75c8fa3e0a1c83
```
