# Unsafe Audit

TOGAF cluster: 03 Infrastructure

Status: baseline for issue #392.

## Scope

This audit covers first-party Rust sources under `crates/` and `services/`.
Generated FlatBuffers bindings under `crates/sentinel-common/src/generated/`
are excluded from the first-party baseline because they are regenerated from
schemas and contain upstream-generated accessor code.

Dependency unsafe remains out of scope for this document and is handled through
dependency audit tooling.

## Baseline

The enforced baseline is stored in
`docs/security/unsafe-baseline.json` and checked by
`scripts/check-unsafe-baseline.py`.

Current first-party count:

| Component | Count | Notes |
| --- | ---: | --- |
| `crates/sentinel-ebpf-probes` | 10 | eBPF helper calls, verifier-checked map pointers, ring-buffer writes |
| `crates/sentinel-ebpf` | 6 | eBPF userspace ring-buffer reads, `aya::Pod`, monotonic clock FFI |
| `crates/sentinel-fs` | 1 | `io_uring` submission queue push |
| `crates/sentinel-sandbox` | 6 | bwrap child-FD plumbing and PID handoff |
| `services/sentinel-nightrun` | 2 | `localtime_r` FFI and `MaybeUninit::assume_init` |
| `services/sentinel-gaia-loop` | 1 | Process-group termination for Claude and spawned tools |
| Other first-party sources in scope | 0 | No counted unsafe constructs |

Total baseline: 26 counted unsafe constructs, all with nearby `SAFETY:`
justification.

## cargo-geiger Evidence

Tooling was installed and run on the build server `10.0.0.155`:

- `cargo-geiger 0.13.0`
- Workspace root run is not usable because `cargo-geiger` rejects the virtual
  manifest; package runs were used where the package can be scanned with the
  normal host toolchain.
- Dependency parse warnings make `cargo-geiger` exit non-zero for some packages;
  those warnings are dependency scan noise, not first-party source failures.

Representative package root lines:

```text
sentinel-fs 0.1.0:
50/50=100.00% functions, 3839/3846=99.82% expressions, 20/20=100.00% impls, 0/0 traits, 137/137 methods

sentinel-nightrun 0.1.0:
26/26=100.00% functions, 1266/1270=99.69% expressions, 11/11=100.00% impls, 0/0 traits, 44/44 methods
```

`sentinel-ebpf` and `sentinel-ebpf-probes` are covered by the first-party
baseline script because the normal host `cargo-geiger` package scan does not
cover the eBPF feature/target shape cleanly.

## Changes From Audit

- Replaced `std::mem::zeroed()` in `services/sentinel-nightrun/src/shift.rs`
  with `MaybeUninit<libc::tm>` and explicit `localtime_r` null checking.
- Added regression coverage for fixed UTC epoch boundary seconds so the
  `localtime_r` wrapper preserves local-hour and shift mapping behavior.
- Added local `SAFETY:` comments to first-party unsafe blocks and unsafe impls.
- Added `scripts/check-unsafe-baseline.py` to fail when unsafe grows beyond the
  documented baseline or when a counted unsafe construct lacks a nearby
  `SAFETY:` comment.
- Added the unsafe baseline check to the always-running CI lint job.

## Verification

```bash
python3 scripts/check-unsafe-baseline.py
```

Expected summary:

```text
unsafe constructs: 26 / baseline 26
```
