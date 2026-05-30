# Test Verification: Issue #416 - sentinel-gaia CLI

Date: 2026-05-30
Branch: feat/issues-414-416-gaia-complete
Commit under test: pending Task 3 commit
Mode: token-safe; no Gateway start, no LLM calls

## Scope

Issue #416 exposes Gaia as an operator-facing CLI path:

- `sentinel-gaia init` for interactive and spec-file driven config generation
- `sentinel-gaia preview` for non-mutating generation review
- `sentinel-gaia validate` for disk-output validation
- `sentinel-gaia print-example-spec` for reproducible input scaffolding

The CLI persists Gaia input as `gaia-spec.toml`. It does not write runtime
`company.toml`, because that file already has the Gateway/company-context schema.

## Acceptance Criteria

| AC | Requirement | Evidence | Result |
| --- | --- | --- | --- |
| AC-416-1 | CLI collects spec interactively and by scripted input | `init_interactive_scripted_input_collects_spec`; `init_from_spec_writes_valid_configs_and_protects_existing_output` | PASS |
| AC-416-2 | CLI writes valid configs through generator core with preview/confirmation support | `init_from_spec_writes_valid_configs_and_protects_existing_output`; `validate_output_dir_checks_written_files` | PASS |
| AC-416-3 | Optional daemon dry-run/start exists; existing configs are protected without force/backup | `init_can_run_daemon_dry_run_with_config_output_dir`; rerun refusal and `--force` backup assertions | PASS |
| AC-416-4 | E2E spec -> configs -> daemon loads company smoke | Package-level fake-daemon dry-run passed; Deploy-VM real daemon dry-run smoke remains in Task 5 before PR completion | PARTIAL |
| AC-416-5 | Remote clippy/test gates green | `cargo remote -- test -p sentinel-gaia`; `cargo remote -- clippy -p sentinel-gaia --all-targets -- -D warnings` | PASS |

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

### Package Tests

Command:

```bash
cargo remote -- test -p sentinel-gaia
```

Relevant output:

```text
running 8 tests
test tests::custom_spec_derives_structure_hierarchy_roles_and_shifts ... ok
test tests::generated_rooms_are_bidirectional_and_have_capacity ... ok
test tests::same_seed_is_reproducible ... ok
test tests::refuses_overwrite_unless_explicit ... ok
test tests::generated_agents_are_valid_and_use_ecs_runtime ... ok
test tests::validate_output_dir_checks_written_files ... ok
test tests::write_to_dir_outputs_loadable_agent_tomls ... ok
test tests::daemon_and_nightrun_configs_track_agent_count ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

running 3 tests
test init_interactive_scripted_input_collects_spec ... ok
test init_can_run_daemon_dry_run_with_config_output_dir ... ok
test init_from_spec_writes_valid_configs_and_protects_existing_output ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### Clippy

Command:

```bash
cargo remote -- clippy -p sentinel-gaia --all-targets -- -D warnings
```

Relevant output:

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.75s
exit 0
```

## Structural Evidence

Commands:

```bash
rg -n "Init|Preview|Validate|PrintExampleSpec|daemon_dry_run|start_daemon|force" services/sentinel-gaia/src/main.rs
rg -n "validate_output_dir|read_spec|GAIA_SPEC_FILENAME|RUNTIME_ECS_NATIVE" services/sentinel-gaia/src/lib.rs
```

Relevant checks:

```text
sentinel-gaia has subcommands: init, preview, validate, print-example-spec.
init supports --spec, --output-dir, --yes, --force, --daemon-dry-run, --start-daemon, --daemon-bin, and --json.
validate_output_dir loads gaia-spec.toml, generated agents, rooms.toml, daemon.toml, and nightrun.toml from disk.
Generated agent runtime validation compares against sentinel_common::RUNTIME_ECS_NATIVE.
```

## Notes

- The daemon dry-run integration test uses a fake daemon binary and asserts
  `--config <output>/daemon.toml --dry-run` invocation.
- Real daemon load smoke on the Deploy-VM is deliberately deferred to the final
  Task 5 smoke, where built binaries run on `ubuntu@10.0.0.240` under `/tmp`.
- Gateway was not started during #416 package verification.
