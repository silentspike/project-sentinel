# Issue #442 Remote Verification

Date: 2026-06-27

Rust build, test, and clippy verification was executed on the remote build host `root@10.0.0.155` through `cargo remote`.

## sentinel-gaia-loop Tests

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- test -p sentinel-gaia-loop
```

Output excerpt:

```text
Finished `test` profile [unoptimized + debuginfo] target(s) in 5.74s
running 17 tests
test session::tests::setup_args_allow_deterministic_sentinel_gaia_binary ... ok
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.01s
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

Exit status: `0`

## Dashboard Gaia Routes

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- test -p sentinel-dashboard-backend gaia
```

Output excerpt:

```text
Finished `test` profile [unoptimized + debuginfo] target(s) in 44.51s
test gaia::tests::rejects_unsafe_session_ids ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 42 filtered out; finished in 0.00s
running 3 tests
test gaia_deep_route_runs_fake_claude_with_budget_cap ... ok
test gaia_read_routes_return_console_jsonl ... ok
test gaia_routes_require_auth ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
```

Exit status: `0`

## Release Build

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- build -p sentinel-gaia-loop -p sentinel-dashboard-backend -p sentinel-ctl -p sentinel-gaia --release
```

Output excerpt:

```text
Finished `release` profile [optimized] target(s) in 36.84s
```

Exit status: `0`

## Workspace Clippy

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- clippy --workspace --all-targets -- -D warnings
```

Output excerpt:

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 56.55s
```

Exit status: `0`

## Local Format And Patch Hygiene

Command:

```bash
cargo fmt --check
```

Output:

```text
```

Exit status: `0`

## NATS Fallback Rerun

After live target inspection showed that `.241/.242` do not have `nats-server` installed, `sentinel-gaia-loop serve` was updated to keep running scheduled EventStore scans when the NATS acceleration path is unavailable. The affected Rust gates were rerun on the remote build host.

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- test -p sentinel-gaia-loop
```

Output excerpt:

```text
Finished `test` profile [unoptimized + debuginfo] target(s) in 1m 46s
running 17 tests
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.52s
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

Exit status: `0`

## EventStore Catch-up Rerun

Live AC-1 on `.241` exposed that a fresh readiness service could lag behind a large live EventStore when it only scanned one fixed-size page per interval. The service path was changed to page until caught up during recovery and scheduled scans.

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- test -p sentinel-gaia-loop
```

Output excerpt:

```text
Finished `test` profile [unoptimized + debuginfo] target(s) in 2.90s
running 18 tests
test readiness::tests::catch_up_scans_multiple_pages_until_current_cursor ... ok
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.31s
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

Exit status: `0`

## Setup Dry-run Path Rerun

Live AC-3 exposed that `sentinel-gaia init --output-dir config --daemon-dry-run` resolved the daemon working directory to an empty parent path. The generated files were valid and a manual daemon dry-run succeeded; the integrated dry-run path was fixed and regression-tested.

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- test -p sentinel-gaia -p sentinel-gaia-loop
```

Output excerpt:

```text
Finished `test` profile [unoptimized + debuginfo] target(s) in 2.57s
running 16 tests
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
running 2 tests
test tests::daemon_working_dir_uses_current_dir_for_bare_config_output ... ok
test tests::daemon_working_dir_uses_parent_for_nested_config_output ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 18 tests
test readiness::tests::catch_up_scans_multiple_pages_until_current_cursor ... ok
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.01s
```

Exit status: `0`

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- build -p sentinel-gaia -p sentinel-ctl -p sentinel-gaia-loop --release
```

Output excerpt:

```text
Finished `release` profile [optimized] target(s) in 59.31s
```

Exit status: `0`

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- clippy --workspace --all-targets -- -D warnings
```

Output excerpt:

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.29s
```

Exit status: `0`

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- build -p sentinel-gaia-loop --release
```

Output excerpt:

```text
Finished `release` profile [optimized] target(s) in 13.31s
```

Exit status: `0`

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- clippy --workspace --all-targets -- -D warnings
```

Output excerpt:

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.02s
```

Exit status: `0`

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- build -p sentinel-gaia-loop --release
```

Output excerpt:

```text
Finished `release` profile [optimized] target(s) in 4m 02s
```

Exit status: `0`

Command:

```bash
cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- clippy --workspace --all-targets -- -D warnings
```

Output excerpt:

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2m 42s
```

Exit status: `0`

Command:

```bash
git diff --check
```

Output:

```text
```

Exit status: `0`
