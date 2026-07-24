# Artificial Duplicate Negative Gate

The temporary fixture added this exact dev dependency to
`crates/sentinel-telemetry/Cargo.toml` and let `cargo remote` resolve the
corresponding lockfile row:

```toml
itertools = "=0.12.1" # Issue #633 artificial duplicate negative-gate fixture.
```

Remote negative check:

```bash
cargo remote -c -- deny check bans
```

```text
error[duplicate]: found 2 duplicate entries for crate 'itertools'
itertools 0.12.1
itertools 0.13.0
itertools 0.14.0
bans FAILED
negative_remote_exit=2
```

The fixture commit `f20a885c2059f233ec3eb1ab47a7a5ea4b1a30f2` produced
GitHub Actions run
[`30082119419`](https://github.com/silentspike/project-sentinel/actions/runs/30082119419):

- [Bans job `89445908431`](https://github.com/silentspike/project-sentinel/actions/runs/30082119419/job/89445908431):
  `failure`, with the same three `itertools` versions.
- [`ci-pass` job `89448131861`](https://github.com/silentspike/project-sentinel/actions/runs/30082119419/job/89448131861):
  `failure`, because its evaluated Bans dependency result was `failure`.

Cleanup commit `1f70361bf56b37edff9181143f9514bee4c9890d` removed all 11
fixture lines. Verification against the pre-fixture implementation commit:

```bash
git diff --exit-code f380cd89ac0094cb941f71260c562df2485ef5a3 \
  -- Cargo.lock crates/sentinel-telemetry/Cargo.toml
```

```text
fixture_cleanup_tree=PASS
Cargo.lock_sha256=5cededd5a1595815229265805c36d1f50cbf8492cec64b4d2679891e98cca1e6
```
