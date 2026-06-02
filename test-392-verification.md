# Issue #392 Verification - Unsafe Audit

Status: verified for PR evidence; issue remains open until merge-to-main verification.

## Scope

- First-party Rust unsafe inventory and documentation.
- Unsafe growth guard in CI.
- Safe replacement for the nightrun `localtime_r` initialization path.
- Regression tests for fixed epoch/shift-boundary behavior.

Dependency unsafe is out of scope for #392 and remains covered by audit/supply-chain tooling.

## AC Matrix

| AC | Requirement | Evidence |
| --- | --- | --- |
| AC-1 | `cargo-geiger` report/evidence documented | `docs/security/unsafe-audit.md`; package scans on 10.0.0.155 for `sentinel-fs` and `sentinel-nightrun`; workspace virtual manifest limitation documented |
| AC-2 | Every remaining first-party unsafe block has local `SAFETY:` justification | `python3 scripts/check-unsafe-baseline.py` passes with 19/19 baseline |
| AC-3 | Repo check fails when unsafe count grows beyond baseline | `scripts/check-unsafe-baseline.py`; wired into `.github/workflows/ci.yml` lint job |
| AC-4 | Trivial unsafe replaced or justified | `services/sentinel-nightrun/src/shift.rs` uses `MaybeUninit<libc::tm>` and checks `localtime_r` result |
| AC-5 | shift.rs/time FFI protected by fixed-time regression tests | Remote `cargo test -p sentinel-nightrun --lib shift` passed, 7 tests |

## Commands And Output

```bash
cargo fmt --all -- --check
# PASS
```

```bash
python3 scripts/check-unsafe-baseline.py
```

Output:

```text
unsafe constructs: 19 / baseline 19
  crates/sentinel-ebpf-probes/src/agent_health.rs: 2
  crates/sentinel-ebpf-probes/src/io_profile.rs: 4
  crates/sentinel-ebpf-probes/src/network.rs: 4
  crates/sentinel-ebpf/src/bin/ebpf-verify.rs: 3
  crates/sentinel-ebpf/src/collector.rs: 3
  crates/sentinel-fs/src/segment.rs: 1
  services/sentinel-nightrun/src/shift.rs: 2
```

```bash
cargo remote -H root@10.0.0.155 -t /tmp/cargo-remote -d stable -- test -p sentinel-nightrun --lib shift
```

Relevant output:

```text
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured
```

`cargo-geiger` note:

```text
cargo-geiger 0.13.0 does not accept the workspace virtual manifest here.
Package scans on 10.0.0.155 produced usable first-party lines for sentinel-fs and sentinel-nightrun.
sentinel-ebpf is covered by the first-party baseline script because its eBPF feature/target shape does not produce a single comparable geiger root line.
```

Recorded root lines:

```text
sentinel-fs:        50/50=100.00% 3839/3846=99.82% 20/20=100.00% 0/0=100.00% 137/137=100.00% ! sentinel-fs 0.1.0
sentinel-nightrun:  26/26=100.00% 1266/1270=99.69% 11/11=100.00% 0/0=100.00% 44/44=100.00% ! sentinel-nightrun 0.1.0
```

## Final Gates

```bash
cargo remote -H root@10.0.0.155 -t /tmp/cargo-remote -d stable -- clippy --workspace --all-targets -- -D warnings
# PASS
```

```bash
cargo remote -H root@10.0.0.155 -t /tmp/cargo-remote -d stable -- test --workspace
# PASS
```

## Not Tested

- Dependency unsafe reduction. That is intentionally outside #392.
- Runtime deploy. #392 changes static audit tooling and one nightrun time FFI implementation; service deploy is not required before PR review, but final issue closure still happens only after merge verification.
