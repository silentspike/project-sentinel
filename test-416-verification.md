# Test Verification: Issue #416 - sentinel-gaia CLI

Date: 2026-05-30
Branch: feat/issues-414-416-gaia-complete
Commit under test: pending Task 5 fix commit
Mode: package verification token-safe; Deploy-VM smoke exposed a service
autostart anomaly documented below

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
| AC-416-4 | E2E spec -> configs -> daemon loads company smoke | Deploy-VM smoke `/tmp/sentinel-gaia-smoke-20260530-121340`: 60 and 250 agents generated, validated, and loaded by real `sentinel-daemon --dry-run` | PASS |
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
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.47s
exit 0
```

### Release Build

Command:

```bash
cargo remote -- build --release -p sentinel-gaia -p sentinel-daemon
```

Relevant output:

```text
Compiling sentinel-gaia v0.1.0
Finished `release` profile [optimized] target(s) in 2.89s
```

### Workspace Gates

Command:

```bash
cargo remote -- clippy --workspace --all-targets -- -D warnings
```

Relevant output:

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2m 33s
```

Command:

```bash
cargo remote -- test --workspace
```

Result:

```text
BLOCKED by build-server resource pressure on 10.0.0.155. The run stalled
during sentinel-daemon linking while disk use reached 95% with about 2.9G
free. The cargo/rustc processes were terminated to avoid filling /tmp/builds.
Focused package tests for changed packages passed, and workspace clippy passed.
```

### Deploy-VM Smoke

Host:

```text
ubuntu@10.0.0.240
CPU: Intel(R) Core(TM) i7-3930K CPU @ 3.20GHz
Smoke root: /tmp/sentinel-gaia-smoke-20260530-121340
```

Commands:

```bash
./bin/sentinel-gaia preview --spec spec-60.toml
./bin/sentinel-gaia init --spec spec-60.toml --output-dir run60/config --yes \
  --daemon-dry-run --daemon-bin /tmp/sentinel-gaia-smoke-20260530-121340/bin/sentinel-daemon --json
python3 -m json.tool logs/init-60.json
./bin/sentinel-gaia validate --output-dir run60/config

./bin/sentinel-gaia init --spec spec-250.toml --output-dir run250/config --yes \
  --daemon-dry-run --daemon-bin /tmp/sentinel-gaia-smoke-20260530-121340/bin/sentinel-daemon --json
python3 -m json.tool logs/init-250.json
./bin/sentinel-gaia validate --output-dir run250/config
```

Summary:

```text
preview60=69 lines
init60=elapsed=0:00.01 cpu=92% maxrss_kb=10104
validate60=OK: 60 agents, 18 rooms, total room capacity 178, daemon.max_agents 60, nightrun.max_agent_id 60
init250=elapsed=0:00.03 cpu=97% maxrss_kb=10516
validate250=OK: 250 agents, 18 rooms, total room capacity 368, daemon.max_agents 250, nightrun.max_agent_id 250
ecs_runtime_count_250=250
```

Structural assertions:

```text
run60/config/gaia-spec.toml exists
run60/config/company.toml does not exist
run60/config/agents contains 60 TOMLs
run250/config/agents contains 250 TOMLs
run250/config/daemon.toml contains max_agents = 250
run250/config/nightrun.toml contains max_agent_id = 250
all 250 generated agent configs use runtime = "ecs-native"
```

Smoke metrics captured:

```text
logs/vmstat.txt
logs/mpstat.txt
logs/iostat.txt
logs/init-60-time.txt
logs/init-250-time.txt
```

### Smoke-Found Fixes

The first real VM smoke found two CLI integration defects that package tests did
not catch:

1. `--daemon-dry-run` changed cwd to the company root but passed a relative
   `run60/config/daemon.toml`, so the daemon looked for the config under the
   wrong directory. Fixed by canonicalizing the generated daemon config path
   before starting daemon dry-run/start.
2. `init --json --daemon-dry-run` inherited daemon stdout, contaminating JSON.
   Fixed by capturing daemon dry-run output and only printing it on daemon
   failure.

Regression coverage:

```text
init_can_run_daemon_dry_run_with_config_output_dir
  - asserts daemon cwd is the company root
  - asserts --config points to a readable generated daemon.toml
  - makes the fake daemon emit stdout/stderr noise
  - asserts init --json remains valid JSON
```

### Token-Safety Anomaly

The Gaia package tests did not start Gateway or make LLM calls. The Deploy-VM,
however, had production services that autostarted independently during smoke.
`sentinel-gateway` and `sentinel-judge` were observed active and produced
provider attempts in `journalctl`, including Claude access errors. They were
stopped after detection. The recurring cause was `sentinel-health-monitor.timer`,
which restarted the production services; that timer and its service were stopped
before the final inactive check:

```text
systemctl is-active cortex-gateway sentinel-gateway sentinel-cortex-gateway sentinel-judge sentinel-daemon
inactive
inactive
inactive
inactive
inactive
```

Longer-than-timer-interval check after stopping the health monitor:

```text
systemctl is-active sentinel-health-monitor.timer sentinel-health-monitor.service cortex-gateway sentinel-gateway sentinel-cortex-gateway sentinel-judge sentinel-daemon
inactive
inactive
inactive
inactive
inactive
inactive
inactive
```

This is recorded as a VM service-state anomaly, not caused by `sentinel-gaia`
itself; the actual Gaia smoke used local binaries under `/tmp` and only invoked
`sentinel-daemon --dry-run`.

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

- The daemon dry-run integration test uses a fake daemon binary and now asserts
  cwd/config readability plus clean `--json` stdout under noisy daemon output.
- Real daemon load smoke passed on the Deploy-VM for 60 and 250 generated
  agents.
- Gateway was not started during #416 package verification; Deploy-VM service
  autostart is documented above as an anomaly.
