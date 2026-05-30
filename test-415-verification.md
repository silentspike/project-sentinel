# Test Verification: Issue #415 - Gaia Generator-Core

Date: 2026-05-30
Branch: feat/issues-414-416-gaia-complete
Commit under test: pending Task 2 commit
Mode: token-safe; no Gateway start, no LLM calls

## Scope

Issue #415 adds the deterministic Gaia generator library that turns a company spec into valid Sentinel runtime config files:

- `gaia-spec.toml` as the persisted Gaia input/SSOT
- `agents/AGENT-*.toml`
- `rooms.toml`
- `daemon.toml`
- `nightrun.toml`

Gaia deliberately does not write `config/company.toml`, because that file already uses the Gateway/runtime `[company] name/city` schema.

## Acceptance Criteria

| AC | Requirement | Evidence | Result |
| --- | --- | --- | --- |
| AC-415-1 | Company spec derives structure, departments, roles, hierarchy, and shifts | `custom_spec_derives_structure_hierarchy_roles_and_shifts` | PASS |
| AC-415-2 | N valid Agent TOMLs, validation green, reproducible same seed | `same_seed_is_reproducible`, `generated_agents_are_valid_and_use_ecs_runtime`, `write_to_dir_outputs_loadable_agent_tomls` | PASS |
| AC-415-3 | `rooms.toml` bidirectional adjacency and validation green | `generated_rooms_are_bidirectional_and_have_capacity` using `BuildingConfig::validate(75)` | PASS |
| AC-415-4 | `daemon.toml` consistent with `max_agents == N` | `daemon_and_nightrun_configs_track_agent_count` validates 120-agent output | PASS |
| AC-415-5 | Remote clippy/test gates green | `cargo remote -- test -p sentinel-gaia`; `cargo remote -- clippy -p sentinel-gaia --all-targets -- -D warnings` | PASS |

## Command Evidence

### Format

Command:

```bash
cargo fmt --check
```

Output summary:

```text
exit 0
```

### Unit Tests

Command:

```bash
cargo remote -- test -p sentinel-gaia
```

Relevant output:

```text
running 7 tests
test tests::custom_spec_derives_structure_hierarchy_roles_and_shifts ... ok
test tests::generated_rooms_are_bidirectional_and_have_capacity ... ok
test tests::same_seed_is_reproducible ... ok
test tests::refuses_overwrite_unless_explicit ... ok
test tests::generated_agents_are_valid_and_use_ecs_runtime ... ok
test tests::write_to_dir_outputs_loadable_agent_tomls ... ok
test tests::daemon_and_nightrun_configs_track_agent_count ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### Clippy

Command:

```bash
cargo remote -- clippy -p sentinel-gaia --all-targets -- -D warnings
```

Relevant output:

```text
Checking sentinel-gaia v0.1.0
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.33s
exit 0
```

### Common Regression After #414 Clippy Fix

Command:

```bash
cargo remote -- test -p sentinel-common
```

Relevant output:

```text
running 56 tests
test result: ok. 56 passed; 0 failed

running 2 tests
test result: ok. 2 passed; 0 failed

running 5 tests
test result: ok. 5 passed; 0 failed

running 5 tests
test result: ok. 5 passed; 0 failed

running 4 tests
test result: ok. 4 passed; 0 failed
```

## Structural Evidence

Command:

```bash
rg "GAIA_SPEC_FILENAME|company\\.toml|RUNTIME_ECS_NATIVE|nano_runtime" services/sentinel-gaia/src/lib.rs -n
```

Relevant output:

```text
use sentinel_common::{AgentId, RUNTIME_ECS_NATIVE};
pub const GAIA_SPEC_FILENAME: &str = "gaia-spec.toml";
relative_path: PathBuf::from(GAIA_SPEC_FILENAME),
if agent.runtime.nano_runtime.as_deref() != Some(RUNTIME_ECS_NATIVE) {
bail!("Gaia must not emit runtime company.toml; use gaia-spec.toml");
nano_runtime: RUNTIME_ECS_NATIVE.to_string(),
assert!(generated.file(GAIA_SPEC_FILENAME).is_some());
assert!(generated.file("company.toml").is_none());
```

## Notes

- Runtime key is emitted from `sentinel_common::RUNTIME_ECS_NATIVE`, not a string literal.
- Gaia input is persisted as `gaia-spec.toml`; generated runtime config intentionally excludes `company.toml`.
- Generation uses deterministic `blake3` hashing from `(seed, agent_id, field)` and no LLM or OS randomness.
