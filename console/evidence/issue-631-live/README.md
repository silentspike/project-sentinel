# Issue 631 Audit Evidence

This directory contains deterministic, public-safe derivatives of remote Cargo output.
Unmodified wrapper output is internal and untracked. No VM was accessed and no runtime,
deployment, manifest, lockfile, or dependency change was made.

## Scope and Status

The graph, source-audit, root-artifact, cargo-bloat, and structural baseline ACs are
complete. Build-server timing and performance data are excluded.

## AC Mapping

### AC-1: Complete Classification

Command:

```bash
python3 scripts/dependency-reachability-audit.py audit --check \
  --lock Cargo.lock --metadata-all <RAW_ALL> --metadata-native <RAW_NATIVE> \
  --trees-dir console/evidence/issue-631-live/trees \
  --output-dir console/evidence/issue-631-live
```

Output:

```text
coverage=717/717 unclassified=0 roots=8/8 duplicate_versions=94 closure_rows=8485 evidence_match=PASS
```

Evidence: `reachability.tsv`, `reachability-summary.txt`.

### AC-2: Release Reachability and Inventory

Commands, once per each of eight roots:

```bash
cargo remote --no-copy-lock -- tree -p <ROOT> --target x86_64-unknown-linux-gnu -e normal --prefix depth --no-dedupe
cargo remote --no-copy-lock -- tree -p <ROOT> --target x86_64-unknown-linux-gnu -e normal,build --prefix depth
```

Output assertion:

```text
roots_with_normal_tree=8/8
roots_with_normal_build_tree=8/8
```

Evidence: `trees/*.normal.graph.tsv`, `trees/*.normal-build.graph.tsv`, the four
`trees/workspace-*.graph.tsv` source graphs, `workspace-reachability-sets.tsv`, and the
artifact inventory in the canonical audit. The compact graph files contain each package
and active parent/child edge once, with context and root flags; repeated path expansions
from Cargo's `--no-dedupe` rendering remain internal. The classifier derives root
membership only from these resolved Cargo graph sources; metadata does not activate
optional dependencies.

Workspace context commands:

```bash
cargo remote --no-copy-lock -- tree --workspace --target x86_64-unknown-linux-gnu -e normal,build --prefix depth --no-dedupe
cargo remote --no-copy-lock -- tree --workspace --target x86_64-unknown-linux-gnu -e normal,build,dev --prefix depth --no-dedupe
cargo remote --no-copy-lock -- tree --workspace --target x86_64-unknown-linux-gnu -e dev --prefix depth --no-dedupe
cargo remote --no-copy-lock -- tree --workspace --target all -e normal,build,dev --prefix depth --no-dedupe
```

The dev-only tree seeds dev context, which is expanded only across Cargo-active native
edges. Foreign-target context is propagated only across all-target Cargo-active edges
whose target specification is absent from the native metadata resolution.

### AC-3: Feature Origin and Needed-vs-Pulled

Command, once per root:

```bash
cargo remote --no-copy-lock -- tree -p <ROOT> --target x86_64-unknown-linux-gnu -e normal,features --prefix depth
```

Output assertion:

```text
roots_with_feature_tree=8/8
direct_feature_rows=115
source_review_rows=30
```

Evidence: `direct-release-features.tsv`, `feature-review.tsv`, and
`trees/*.features.txt`.
The feature trees use Cargo's deduplicated rendering so activated features remain visible
without repeating identical transitive subtrees. The compact normal/build graph sources,
derived from complete non-deduplicated raw trees, remain the classifier input.

### AC-4: Duplicate Forcing Chains

Command:

```bash
cargo remote --no-copy-lock -- tree --workspace --target all -e normal,build,dev --prefix depth --no-dedupe
```

Output summary:

```text
duplicate_names=41
duplicate_version_rows=94
active_tree_closures=85
disabled_metadata_closures=9
reverse_closure_rows=8485
```

Evidence: `duplicate-versions.tsv`, the independently derived
`trees/workspace-all-targets.graph.tsv`, `workspace-all-target-edges.tsv`, and the
complete per-version reverse closures in `duplicates/reverse-closure.tsv`. Every edge
carries its dependency kind/target constraint and an explicit Cargo-active boolean.
Disabled optional versions use a separately labeled metadata-constraint closure and are
never reported as active reachability.

### AC-5: Binary and Crate Contributions

Command, once per root:

```bash
cargo remote --no-copy-lock -- build --release --locked -p <PACKAGE> --bin <BINARY>
cargo remote --no-copy-lock -- bloat --release --locked -p <PACKAGE> --bin <BINARY> --crates -n 20
```

Output summary:

```text
release_root_builds=8/8
cargo_bloat_tables=8/8
aggregate_release_artifact_bytes=99275536
cargo_bloat=0.12.1
```

Status: binary contribution **COMPLETE**. Evidence: `release-builds.tsv`,
`bloat-summary.tsv`, and `bloat/*.txt`.

### AC-6: Actionable Recommendations

Output summary:

```text
recommendation_rows=13
prune_features=10
align_version=1
investigate=2
```

Evidence: `recommendations.tsv`. Rows include stable IDs, source/tree evidence, expected
effect, and a revisit condition.

### AC-7: Reproducibility

Pinned values:

```text
base_commit=94134b14c380e0cdc55c34222cd74698f97cf555
cargo_lock_sha256=29b97c217ff9694e116e0e6ce856e5ab761b808d5b2289bd56cb255373e14b93
target=x86_64-unknown-linux-gnu
remote_rustc=1.97.1
remote_cargo=1.97.1
```

Evidence: `provenance.txt` and the canonical audit.
The graph regeneration base and the verified zero manifest/lockfile delta through the
pinned base are recorded separately; source-dependent daemon and dashboard contribution
rows were refreshed on the pinned base.

Compact source regeneration from the complete internal Cargo trees:

```bash
python3 scripts/dependency-reachability-audit.py audit --compact-sources-only \
  --lock Cargo.lock --metadata-all <RAW_ALL> --metadata-native <RAW_NATIVE> \
  --raw-trees-dir <RAW_ROOT_TREES> --trees-dir <FRESH_COMPACT_DIR> \
  --workspace-native-build-tree <RAW_WORKSPACE_NATIVE_BUILD> \
  --workspace-native-all-tree <RAW_WORKSPACE_NATIVE_ALL> \
  --workspace-native-dev-tree <RAW_WORKSPACE_NATIVE_DEV> \
  --workspace-all-targets-tree <RAW_WORKSPACE_ALL_TARGETS>
diff -qr <FRESH_COMPACT_DIR> console/evidence/issue-631-live/trees \
  --exclude='*.features.txt'
```

Output:

```text
compact_graph_files=20 compact_graph_rows=19959
compact_source_diff=PASS
compact_graph_bundle_sha256=0dc77be62ce50e759076465378d6af6f7fda0e8cac9e391b089f412b37d6f8c4
```

### AC-8: Pinned Before-Baseline

Status: **COMPLETE**. Hardware-independent graph and contribution metrics are pinned:

```text
lockfile_packages=717
release_normal=485
release_build=90
direct_feature_rows=115
source_review_rows=30
duplicate_names=41
duplicate_version_rows=94
reverse_closure_rows=8485
release_root_builds=8/8
cargo_bloat_tables=8/8
aggregate_release_artifact_bytes=99275536
```

Evidence: `reachability-summary.txt`, `direct-release-features.tsv`,
`feature-review.tsv`, `duplicate-versions.tsv`, `duplicates/reverse-closure.tsv`,
`release-builds.tsv`, and `bloat-summary.tsv`. No build-server timing or performance
result is retained.

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
Ran 24 tests
OK
public-evidence-scan=PASS files=50
staged-new-lines-scan=PASS
```

The test suite includes negative fixtures for IP addresses, home/workspace/remote/Cargo
paths, SSH authorities, unexpected absolute paths, and wrapper timestamp filtering. It
also proves that modifying a derived workspace-membership flag or Cargo-active edge makes
`audit --check` fail while the compact Cargo graph sources remain unchanged.

## Verification Gates

All Rust commands ran on the remote builder. Private connection configuration is
omitted from these public command renderings.

Commands:

```bash
cargo remote --no-copy-lock -- check --workspace --all-targets --locked
cargo remote --no-copy-lock -- test --workspace --locked
cargo remote --no-copy-lock -- clippy --workspace --all-targets --locked -- -D warnings
```

Output assertions:

```text
remote_check=PASS
remote_test=PASS
remote_clippy=PASS
```

The first workspace-test invocation hit a pre-existing wall-clock assertion in the Gaia
Loop timeout test. The isolated test rerun and the complete workspace rerun both passed;
no timing value from either invocation is retained as audit evidence.

## Not Tested

- No VM access or runtime behavior.
- No deploy, service restart, or production artifact.
- No startup-time, memory, throughput, or latency claim.
- No dependency or feature change from #632.
