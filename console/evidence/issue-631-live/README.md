# Issue 631 Audit Evidence

This directory contains deterministic, public-safe derivatives of remote Cargo output.
Unmodified wrapper output is internal and untracked. No VM was accessed and no runtime,
deployment, manifest, lockfile, or dependency change was made.

## Scope and Status

The graph, source-audit, root-artifact, cargo-bloat, and clean-target workspace release
build ACs are complete. Three successful runs used separate empty issue-specific remote
project/target directories while retaining registry, source, and toolchain caches. The
included runs observed no concurrent foreign Cargo/rustc process group.

## AC Mapping

### AC-1: Complete Classification

Command:

```bash
python3 scripts/dependency-reachability-audit.py audit --check \
  --lock Cargo.lock --metadata-all <RAW_ALL> --metadata-native <RAW_NATIVE>
```

Output:

```text
coverage=717/717 unclassified=0 roots=8/8
```

Evidence: `reachability.tsv`, `reachability-summary.txt`.

### AC-2: Release Reachability and Inventory

Commands, once per each of eight roots:

```bash
cargo remote -c -- tree -p <ROOT> --target x86_64-unknown-linux-gnu -e normal --prefix depth --no-dedupe
cargo remote -c -- tree -p <ROOT> --target x86_64-unknown-linux-gnu -e build --prefix depth --no-dedupe
```

Output assertion:

```text
roots_with_normal_tree=8/8
roots_with_build_tree=8/8
```

Evidence: `trees/*.normal.txt`, `trees/*.build.txt` and the artifact inventory in the
canonical audit.

### AC-3: Feature Origin and Needed-vs-Pulled

Command, once per root:

```bash
cargo remote -c -- tree -p <ROOT> --target x86_64-unknown-linux-gnu -e normal,features --prefix depth --no-dedupe
```

Output assertion:

```text
roots_with_feature_tree=8/8
direct_feature_rows=115
source_review_rows=30
```

Evidence: `direct-release-features.tsv`, `feature-review.tsv`, and
`trees/*.features.txt`.

### AC-4: Duplicate Forcing Chains

Command:

```bash
cargo remote -c -- tree --workspace --target x86_64-unknown-linux-gnu --duplicates -e normal,build,dev --prefix depth
```

Output summary:

```text
duplicate_names=41
duplicate_version_rows=94
```

Evidence: `duplicate-versions.tsv` and the compact deduplicated reverse graph in
`duplicates/linux-reverse-trees.txt`.

### AC-5: Recommendations

Output summary:

```text
recommendation_rows=13
prune_features=10
align_version=1
investigate=2
```

Evidence: `recommendations.tsv`. Rows include stable IDs, source/tree evidence, expected
effect, and a revisit condition.

### AC-6: Binary Contributions and Structural Build Cost

Command, once per root:

```bash
cargo remote -c -- build --release --locked -p <PACKAGE> --bin <BINARY>
cargo remote -c -- bloat --release --locked -p <PACKAGE> --bin <BINARY> --crates -n 20
```

Output summary:

```text
release_root_builds=8/8
cargo_bloat_tables=8/8
aggregate_release_artifact_bytes=98740464
cargo_bloat=0.12.1
shared_target_build_timing=CONTENDED, excluded from baseline
```

Status: binary contribution **COMPLETE**. Evidence: `release-builds.tsv`,
`bloat-summary.tsv`, and `bloat/*.txt`. The clean-target builds are reported under AC-8.

### AC-7: Reproducibility

Pinned values:

```text
base_commit=f622885d7137b8cb334adf655d42749c5aa1d881
cargo_lock_sha256=9ea96b715b709d43b9b90352968c06998111476a0ebb546254db0f43e4034b22
target=x86_64-unknown-linux-gnu
remote_rustc=1.97.1
remote_cargo=1.97.1
```

Evidence: `provenance.txt` and the canonical audit.

### AC-8: Pinned Before-Baseline

Status: **COMPLETE**. Graph counts, binary contribution, three uncontended clean-target
build-cost values, and medians are pinned:

```text
run  cargo_finished  cargo_remote_e2e  overlap_markers
C05  7m 18s         466.91s           0
C06  7m 17s         459.00s           0
C07  7m 17s         458.55s           0
median 7m 17s       459.00s
```

Each included run acquired an issue-local remote lease, passed an idle preflight, used
a new empty remote project/target directory, and sampled remote Cargo/rustc process
groups every five seconds. Registry, source, and toolchain caches were retained, so
these are clean-target release builds, not cache-purged builds. Cargo's own duration and
the cargo-remote end-to-end wall time are deliberately separate.

Two attempted clean-target runs are retained in `clean-builds.tsv` as
`CONTENDED_ABORTED`. C01 observed 26 foreign-wrapper markers across 48 five-second
samples; C02 observed 12 across 34. Neither produced a Cargo `Finished in` value and
neither is eligible for a median.

C03 is retained as `UNCLASSIFIED_OVERLAP_ABORTED`: a short-lived foreign local wrapper
produced seven markers across 68 samples, but ended before its remote load could be
classified. Subsequent runs classify competing Cargo/rustc process groups directly on
the buildserver so a local wrapper without remote Rust load cannot create a false
positive.

C04 is retained as `MONITOR_RACE_ABORTED`: one process exited between `pgrep` and the
PGID/CWD readback, leaving an empty process record. Later runs ignore vanished PIDs and
accept an overlap marker only when a non-empty foreign process-group ID is still
readable.

The excluded attempts are evidence of the fail-closed load gate and do not contribute
to either median. `clean-builds.tsv` contains all seven attempts, sample counts, overlap
markers, inclusion decisions, and the median row.

### AC-9: Public Evidence Sanitization

Commands:

```bash
python3 -m unittest scripts.tests.test_dependency_reachability_audit
python3 scripts/dependency-reachability-audit.py check-public-evidence \
  docs/audits/dependency-reachability.md console/evidence/issue-631-live
python3 scripts/dependency-reachability-audit.py check-staged
```

Final output:

```text
Ran 14 tests
OK
public-evidence-scan=PASS files=45
staged-new-lines-scan=PASS
```

The test suite includes negative fixtures for IP addresses, home/workspace/remote/Cargo
paths, SSH authorities, unexpected absolute paths, and wrapper timestamp filtering.

## Verification Gates

All Rust commands ran on the remote builder. Private connection configuration is
omitted from these public command renderings.

Commands:

```bash
cargo remote -c -- check --workspace --all-targets --locked
cargo remote -c -- test --workspace --locked
cargo remote -c -- clippy --workspace --all-targets --locked -- -D warnings
```

Output excerpts:

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2m 19s
Finished `test` profile [unoptimized + debuginfo] target(s) in 8m 01s
test result: ok. 321 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
Finished `dev` profile [unoptimized + debuginfo] target(s) in 36.62s
```

## Not Tested

- No VM access or runtime behavior.
- No deploy, service restart, or production artifact.
- No startup-time, memory, throughput, or latency claim.
- No dependency or feature change from #632.
