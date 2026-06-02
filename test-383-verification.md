# Issue #383 Verification - Component READMEs

Status: verified for PR evidence; issue remains open until merge-to-main verification.

## Scope

- Current Rust product components with `Cargo.toml` under `crates/sentinel-*` and `services/*`.
- Current Go modules/services under `cmd/`, `pkg/`, and `services/`.
- Excluded: WASM fixture crates under `crates/sentinel-wasm/tests/fixtures/`.

## AC Matrix

| AC | Requirement | Evidence |
| --- | --- | --- |
| AC-1 | All current Rust component dirs have README with purpose, interfaces, dependencies, verify | 21 Rust component dirs checked by `scripts/check-component-readmes.sh` |
| AC-2 | All Go modules/services have README with purpose, interfaces, dependencies, verify | 4 Go dirs checked by `scripts/check-component-readmes.sh` |
| AC-3 | Component READMEs are linked from `llms.txt` or a central index | `llms.txt` links to `docs/component-readmes.md`; index links all 25 README files |
| AC-4 | TOGAF Cluster 10 updated | `docs/togaf-gap-v22.md` marks Component-level READMEs as done with #383 evidence |
| AC-5 | Verification documents actual current component count | This file and script output record 25 total, 21 Rust, 4 Go |

## Current Component Count

```bash
find crates services cmd pkg -name Cargo.toml -o -name go.mod | sort
```

Product scope after excluding WASM test fixtures:

```text
Rust components: 21
Go components:   4
Total:           25
```

## Coverage Check

```bash
scripts/check-component-readmes.sh
```

Output:

```text
component READMEs: 25 total (21 Rust, 4 Go)
```

The script checks:

- README exists for each discovered component.
- `## Purpose`, `## Interfaces`, `## Dependencies`, and `## Verify` headings exist.
- `docs/component-readmes.md` references each component README.

## Final Gates

```bash
cargo remote -H root@10.0.0.155 -t /tmp/cargo-remote -d stable -- clippy --workspace --all-targets -- -D warnings
# PASS
```

```bash
cargo remote -H root@10.0.0.155 -t /tmp/cargo-remote -d stable -- test --workspace
# PASS
```

## Linked Indexes

- `docs/component-readmes.md`
- `llms.txt`
- `README.md`
- `docs/togaf-gap-v22.md`

## Not Tested

- Runtime deploy. #383 is documentation-only.
- WASM fixture crates. They are test fixtures, not product components.
