# Issue #414 Verification - Configurable AgentId Bounds

Issue: https://github.com/silentspike/project-sentinel/issues/414

## Scope

Replace the historical hard `1..=60` AgentId upper bound with explicit
validation bounds while preserving the shipped 60-agent config as the default.
The configured bound is now used by Agent TOML loading, daemon dry-run,
daemon orchestration, Judge-Alert AgentId parsing, and nightrun Agent TOML
selection.

## AC Matrix

| AC | Evidence | Status |
| --- | --- | --- |
| AC-414-1 | `sentinel_common::DEFAULT_MAX_AGENT_ID` and `AgentIdBounds` preserve default 60 behavior and allow explicit larger bounds. | PASS |
| AC-414-2 | `load_agent_config_with_validation` and `load_all_agents_with_validation` validate Agent TOMLs against the caller-provided bound. | PASS |
| AC-414-3 | `sentinel-daemon` derives Agent TOML validation from `daemon.max_agents`; dry-run, orchestration, and Judge-Alert parsing use it. | PASS |
| AC-414-4 | `sentinel-nightrun` has an explicit `max_agent_id`, uses it for Agent TOML loading, and logs loader errors before conservative fallback. | PASS |

## Focused Tests

Command:

```bash
cargo fmt --check
cargo remote -- test -p sentinel-common
cargo remote -- test -p sentinel-daemon agent_config_validation
cargo remote -- test -p sentinel-daemon config::tests::test_defaults
cargo remote -- test -p sentinel-daemon judge_alert_agent_id_uses_configured_bounds
cargo remote -- test -p sentinel-nightrun
```

Observed:

```text
cargo fmt --check: exit 0
sentinel-common: 56 unit tests passed; acceptance tests 2 + 5 + 5 passed; snapshot_roundtrip 4 passed
sentinel-daemon agent_config_validation: 1 passed; 204 filtered out
sentinel-daemon config::tests::test_defaults: 2 passed; 203 filtered out
sentinel-daemon judge_alert_agent_id_uses_configured_bounds: 1 passed; 205 filtered out
sentinel-nightrun: 39 unit tests passed; 14 integration tests passed; doc-tests passed
```

## Structural Checks

Command:

```bash
rg -n "AgentId::new\(|load_agent_config\(|load_all_agents\(" crates services -g '*.rs'
rg -n "out of range \(1-60\)|1\.\.=60|InvalidAgentId\(" crates/sentinel-common services/sentinel-daemon services/sentinel-nightrun config/nightrun.toml -g '*.rs' -g '*.toml'
```

Observed:

```text
Production Agent TOML load paths now use load_all_agents_with_validation.
The remaining load_all_agents calls are the default 60-agent common acceptance tests.
The remaining 1..=60 assertion is the shipped config gap/duplicate test for the existing 60 Agent TOMLs.
No stale "out of range (1-60)" validation message remains.
```

## Token Safety

Gateway was not started. Verification used Rust unit/integration tests only on
the build server (`10.0.0.155`) and local formatting/grep checks.
