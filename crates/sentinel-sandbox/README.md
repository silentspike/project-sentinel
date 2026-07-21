# sentinel-sandbox

## Purpose

`sentinel-sandbox` enforces agent process isolation with bwrap, Landlock, cgroups v2, network namespaces, and PSI reporting.

## Interfaces

- `SandboxEnforcer` starts and tracks isolated agent processes.
- `BwrapConfig`, `LandlockRuleset`, and `CgroupLimits` describe isolation policy (network: full cage via `bwrap --unshare-all`, post-spawn isolation verified against the child PID).
- `AgentProcess` and `SandboxHandle` represent managed runtime state.
- `psi_publisher` exposes pressure metrics for bio stress and monitoring.
- `BwrapNanoRuntime` implements the shared `NanoRuntime` contract for the
  `bwrap-landlock` key. Its `snapshot` captures workload config, isolation
  metadata, and agent-home filesystem state. It does not checkpoint process RAM
  and does not claim CRIU/live-process migration. Its idempotent `stop` releases
  the addressed process, cgroup, retained home-snapshot pins, and adapter state.
  Its workbench exec channel uses schema-versioned, nonblocking start/poll/cancel
  JSONL exchanges, binds every response to one invocation and byte-exact start
  digest, and retains bounded terminal results for retry-safe polling. Frames are
  limited to 1 MiB, retained output to 256 KiB, and the reader queue to 64 frames;
  each control input must be exactly one record without embedded CR/LF boundaries.
  Protocol/order/correlation failures, overflow, EOF, and an unacknowledged cancel
  or deadline terminate and reap the selected process group; production cgroup
  cleanup is the stronger process-tree backstop. Child frames and command
  arguments are not logged, child stderr is discarded, and failures expose
  typed public-safe `NanoExecErrorCode` values instead of payload text. Terminal
  transport failures are retained for stable in-process retry responses while
  their process resources are released immediately; durable restart recovery
  remains a caller responsibility.

## Dependencies

- `sentinel-common` and `sentinel-zenoh`.
- `landlock`, `tokio`, `serde`, `serde_json`, `tracing`, and `anyhow`.
- Host support for bwrap, cgroups v2, and optional network namespace setup.

## Verify

```bash
cargo remote -c -- test -p sentinel-sandbox
cargo remote -c -- test -p sentinel-sandbox --test breakout
cargo remote -c -- test -p sentinel-sandbox --test nano_runtime_conformance -- --ignored
```

Sandbox policy changes require runtime verification on the deploy VM because kernel features and permissions are host-dependent.
