# Structural Gate

Canonical input:

```text
base_commit=85b8bef1f1f4bba758bee7eb04248e2622612392
Cargo.lock_sha256=5cededd5a1595815229265805c36d1f50cbf8492cec64b4d2679891e98cca1e6
remaining-duplicates.tsv_sha256=b569bb455d4bd78f2de4429b4dc47dbfc6d5a0373885c50d8d91042d4e82845c
```

The activation check parses `deny.toml`, selects the highest SemVer in each row
of `console/evidence/issue-632-live/remaining-duplicates.tsv` as the unskipped
baseline, compares all remaining exact versions with `[bans].skip`, and checks
the three adjacent comment fields:

```text
handoff_duplicate_names=39
handoff_version_rows=89
expected_exact_skips=50
configured_exact_skips=50
missing_skips=[]
unexpected_skips=[]
skip_comment_contract_failures=[]
multiple_versions=deny
multiple_versions_include_dev=true
structural_contract=PASS
```

Positive Bans gate:

```bash
cargo remote -c -- deny check bans
```

```text
bans ok
exit_code=0
```

`cargo-deny` also reports unmatched-skip warnings for seven inactive lockfile
versions and one unnecessary-skip warning. They are retained at activation
because Issue #633 consumes the exact finished #632 handoff rather than silently
changing its decisions. The gate result is green, and future graph changes make
these warnings visible for reviewed skip removal.
