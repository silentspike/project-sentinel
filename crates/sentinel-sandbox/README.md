# sentinel-sandbox

## Purpose

`sentinel-sandbox` enforces agent process isolation with bwrap, Landlock, cgroups v2, network namespaces, and PSI reporting.

## Interfaces

- `SandboxEnforcer` starts and tracks isolated agent processes.
- `BwrapConfig`, `LandlockRuleset`, `NetworkNsConfig`, and `CgroupLimits` describe isolation policy.
- `AgentProcess` and `SandboxHandle` represent managed runtime state.
- `psi_publisher` exposes pressure metrics for bio stress and monitoring.

## Dependencies

- `sentinel-common` and `sentinel-zenoh`.
- `landlock`, `tokio`, `serde`, `serde_json`, `tracing`, and `anyhow`.
- Host support for bwrap, cgroups v2, and optional network namespace setup.

## Verify

```bash
cargo remote -c -- test -p sentinel-sandbox
cargo remote -c -- test -p sentinel-sandbox --test breakout
```

Sandbox policy changes require runtime verification on the deploy VM because kernel features and permissions are host-dependent.
