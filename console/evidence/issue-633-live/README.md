# Issue 633 Cargo-Deny Bans Evidence

This directory contains public-safe structural and CI evidence for the Cargo
duplicate-version gate. Raw cargo-remote wrapper output is not committed.

## AC Mapping

| AC | Result | Evidence |
| --- | --- | --- |
| AC-1 | `multiple-versions = "deny"` is active. The 39 duplicate groups and 50 exact lower-version skips match the finished Issue #632 handoff, and each skip has a forcing chain, reason, and concrete revisit condition. | `structural-gate.md`, `deny.toml` |
| AC-2 | An artificial unskipped duplicate is pushed to the PR branch, the Bans job and `ci-pass` fail, and the fixture is then removed completely. | `negative-gate.md` |
| AC-3 | The maintainer-selected design keeps `ci-pass` as the only direct required context while a path-filtered Bans job participates in its DAG for Cargo policy inputs. | `ci-contract.md`, Issue #633 decision comment |
| AC-4 | The dependency policy requires same-PR alignment or a narrowly reviewed temporary exact skip with removal conditions. | `docs/dependency-policy.md` |

## Boundaries

- Runtime target class: `NONE`.
- No VM, deploy target, runtime service, or benchmark target is accessed.
- No performance measurement or build-server timing is recorded.
- Rust and Cargo commands run only through `cargo remote -c --`.
- The artificial negative-test manifest and lockfile changes do not remain in the
  final tree.

## Evidence Digest

After final CI readback:

```bash
(cd console/evidence/issue-633-live && sha256sum * | sha256sum)
```
