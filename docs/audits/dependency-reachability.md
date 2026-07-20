# Dependency Reachability and Structural Contribution Audit

Issue #631 establishes the pinned, reproducible before-state for dependency pruning in
#632. This audit changes no manifest, lockfile, Renovate configuration, deployment
artifact, or runtime. It classifies the complete lockfile, separates release and
non-release reachability, identifies feature origins and duplicate forcing chains, and
records evidence-backed `prune-features`, `align-version`, `leave`, and `investigate`
decisions.

## Executive Summary

- `Cargo.lock` contains 717 packages; all 717 have exactly one primary category.
- Eight Rust binary roots contribute to release, helper, or packaged-demo supply-chain
  reachability. `sentinel-gaia-loop` is included; non-deployed workspace binaries are
  not silently treated as services.
- Linux release reachability contains 485 normal and 90 build/proc-macro packages.
- The lockfile also contains 2 non-release workspace packages, 31 dev/bench-only
  packages, 80 foreign-target-only packages, and 29 disabled optional packages.
- Secondary context is retained independently: 165 packages participate in at least
  one native dev path and 162 participate in at least one foreign-target path, including
  packages whose primary category is release reachability.
- There are 41 duplicate package names covering 94 locked versions. Two dev-only
  groups are ready to align, ten foreign-target groups are explicit `leave` results,
  and the remaining groups require upstream-chain or provider analysis.
- Thirty high-value direct dependencies were checked against release source. The
  resulting table contains ten actionable prune rows, one dev-only alignment row, and
  two investigation rows.
- No VM, deploy, service restart, or runtime assertion belongs to this audit.

All eight explicit root builds and all eight cargo-bloat contribution tables are complete.
The audit retains release-artifact bytes and SHA-256 values plus crate contribution data.
Build-server timing and performance data are deliberately excluded.

## Pinned Provenance

| Item | Value |
| --- | --- |
| Base commit | `94134b14c380e0cdc55c34222cd74698f97cf555` |
| `Cargo.lock` SHA-256 | `29b97c217ff9694e116e0e6ce856e5ab761b808d5b2289bd56cb255373e14b93` |
| Lockfile packages | 717 |
| Workspace members | 27 |
| Release roots | 8 |
| Native target | `x86_64-unknown-linux-gnu` |
| Remote Rust | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| Remote Cargo | `cargo 1.97.1 (c980f4866 2026-06-30)` |
| cargo-bloat | `0.12.1`, installed under an issue-local remote tool directory |

The dependency graph was fully regenerated after PR #640 at
`7b24caeb9dd7d687018f35f0aa96a478d533b2b3`. The later mainline changes through the
pinned base changed neither `Cargo.lock` nor any `Cargo.toml`; source-dependent daemon
and dashboard artifact and cargo-bloat rows were refreshed on the pinned base.

PR #611 remains open and changes only `Cargo.lock`. If it or any other lockfile change
lands before completion, this branch must rebase and regenerate metadata, trees,
duplicates, feature reviews, binary sizes, bloat, and
recommendations. Results from different lockfile hashes must never be mixed.

## Release Artifact Inventory

The audit uses three tiers. Tier A is a direct systemd release service. Tier B is a
runtime child or operator helper. Tier C is a packaged one-shot/demo artifact. The union
is relevant to supply-chain and compile-cost analysis, while the tiers prevent packaged
artifacts from being misreported as deployed services.

| Tier | Cargo package | Binary | Verified repository consumer | Boundary |
| --- | --- | --- | --- | --- |
| A | `sentinel-daemon` | `sentinel-daemon` | `deploy/systemd/sentinel-daemon.service:12` | direct service |
| A | `sentinel-projection-service` | `sentinel-projection` | `deploy/systemd/sentinel-projection.service:12` | direct service; omitted by current generator |
| A | `sentinel-dashboard-backend` | `sentinel-dashboard-backend` | `deploy/systemd/sentinel-dashboard-backend.service:14` | direct service |
| A | `sentinel-gaia-loop` | `sentinel-gaia-loop` | `deploy/systemd/sentinel-gaia-loop.service:14` | direct service |
| B | `agent-runtime` | `agent-runtime` | `services/sentinel-daemon/src/config.rs:473-475` | daemon child; no standalone unit |
| B | `sentinel-ctl` | `sentinel-ctl` | `deploy/systemd/sentinel-dashboard-backend.service:35` | operator helper |
| B | `sentinel-gaia` | `sentinel-gaia` | `deploy/systemd/sentinel-dashboard-backend.service:36` | operator helper and library |
| C | `sentinel-nightrun` | `sentinel-nightrun` | `deploy/generate-manifest.sh:16` | packaged/demo binary; host unit calls the daemon API |

The nightrun unit executes `curl`, not `sentinel-nightrun`
(`deploy/systemd/sentinel-nightrun.service:8-12`). The generated release manifest also
has inventory drift: `deploy/generate-manifest.sh:15-20` includes six Rust binaries but
omits `sentinel-projection` and `agent-runtime`; the versioned manifest still contains
the projection binary. This audit reports that drift and does not repair deployment
files.

## Classification Method

Package identity is `(name, version, source)`, not name alone. Reachability comes from
feature-resolved remote `cargo tree` output, never from the workspace-unified dependency
list in `cargo metadata`. Each release root has a native `normal` tree and a native
`normal,build` tree. Proc-macro nodes and their descendants are build context; packages
present only in the combined tree are build-only. Three workspace trees provide exact
native normal/build, native all-edge, and all-target all-edge sets. Set differences
separate dev/bench-only, target-only, and optional-disabled packages.

A fourth native dev-only tree seeds dev context, which is then propagated through only
Cargo-active native edges. Foreign-target context is propagated through active
all-target edges when the edge's target specification is absent from the native
metadata resolution. This preserves real overlaps without allowing inactive optional
edges to affect primary release membership.

Locked metadata remains useful only for stable package identity, direct manifest feature
requests, and dependency kind/target labels on edges that Cargo proved active. This
boundary prevents disabled optional declarations from becoming release dependencies.

Each lockfile package receives the first matching primary category:

1. `release-normal`
2. `release-build`
3. `non-release-workspace-normal`
4. `dev-bench-only`
5. `target-only`
6. `optional-disabled`

The classifier fails if a root does not resolve exactly once, a normal tree is not a
subset of its normal/build tree, workspace tree roots differ from the 27 metadata
members, package-set nesting is invalid, an active edge lacks metadata annotation, any
package remains unclassified, or committed tables differ byte-for-byte from recomputed
results. Secondary columns retain dev and foreign-target overlap rather than erasing it.

## Reachability Results

| Primary category | Packages |
| --- | ---: |
| `release-normal` | 485 |
| `release-build` | 90 |
| `non-release-workspace-normal` | 2 |
| `dev-bench-only` | 31 |
| `target-only` | 80 |
| `optional-disabled` | 29 |
| **Total** | **717** |

Twenty-nine packages are present in the lockfile but absent from Cargo's all-target,
all-edge workspace tree. They are classified `optional-disabled`, not falsely counted
as release dependencies. This includes `embedded-io 0.4.0` and `embedded-io 0.6.1`,
which are optional declarations in resolved metadata but are absent from every claimed
release-root tree.

The complete package table is
[`reachability.tsv`](../../console/evidence/issue-631-live/reachability.tsv). The
machine-checkable invariants are in
[`reachability-summary.txt`](../../console/evidence/issue-631-live/reachability-summary.txt),
with the exact workspace set inputs in
[`workspace-reachability-sets.tsv`](../../console/evidence/issue-631-live/workspace-reachability-sets.tsv).

## Feature Origin and Source Review

Feature resolution from `cargo metadata` is a workspace union and can include dev
activation. It is therefore recorded only as diagnostic context. Release decisions use
the per-root `cargo tree -e normal,features` output. The direct-feature table separates
`release_features` from `metadata_union_features` so the two cannot be confused.
Feature trees use Cargo's deduplicated rendering: activated feature nodes remain visible,
while repeated transitive subtrees are marked with `(*)` to keep the public evidence
bounded. Reachability classification uses the separate non-deduplicated normal trees.

The deterministic high-value review selected Tier-A direct dependencies first, then
multi-root dependencies, security/runtime/database dependencies, broad feature sets,
and stable alphabetical tie-breaking. Thirty entries were checked against actual source
imports and API calls. Full details are in
[`feature-review.tsv`](../../console/evidence/issue-631-live/feature-review.tsv) and
[`direct-release-features.tsv`](../../console/evidence/issue-631-live/direct-release-features.tsv).

Key findings:

- `tokio/full` activates `fs`, `io-std`, and `test-util` although release source uses no
  corresponding API. Required paths include macros, runtime, net, process, signal,
  sync, time, and async IO utilities.
- All three direct `futures` consumers import only `StreamExt`; the default
  `futures-executor` activation has no release consumer.
- `tracing-subscriber` release roots use `fmt` and `EnvFilter`, but do not enable JSON
  subscriber formatting in code.
- Dashboard zstd calls are limited to `encode_all` and `decode_all`; default legacy,
  arrays, and dictionary-builder features have no source consumer.
- Dashboard WebTransport loads PEM files generated by the directly used `rcgen`; the
  `wtransport/self-signed` helper feature is not called.
- Direct `tower` APIs are test-only. Axum still owns its transitive release dependency.
- `sha2 0.10.9` is now a direct release dependency of `sentinel-common` and a dev
  dependency of `sentinel-redb`; the release edge is required by owner-snapshot checksum
  generation (`crates/sentinel-common/src/fencing.rs:18,224`).
- `sentinel-telemetry`, `sentinel-projection`, and `sentinel-common` contain several
  direct edges whose use differs by root; recommendations split proven unused edges
  from owner-sensitive forwarding contracts.

## Duplicate Versions

The lockfile contains 41 duplicate names and 94 version rows. The full table records
primary reachability, immediate forcers, every reachable workspace/release root, closure
size, closure basis, and a decision for each version:
[`duplicate-versions.tsv`](../../console/evidence/issue-631-live/duplicate-versions.tsv).
All 94 rows have a complete reverse closure: 85 use only active all-target Cargo edges;
nine optional-disabled rows use explicitly labelled metadata-constraint edges. The
8,485 target-annotated closure rows are in
[`duplicates/reverse-closure.tsv`](../../console/evidence/issue-631-live/duplicates/reverse-closure.tsv),
backed by the deduplicated edge inventory in
[`workspace-all-target-edges.tsv`](../../console/evidence/issue-631-live/workspace-all-target-edges.tsv).
Every edge records dependency kind, target expression, and whether Cargo activated it,
so inactive declarations cannot masquerade as release reachability.

Decision summary by duplicate name:

- `align-version`: `criterion` and `criterion-plot`. Two crates pin Criterion 0.5 while
  the workspace uses 0.8.2; the requested `html_reports` and `async_tokio` features
  exist in 0.8.2.
- `leave`: foreign-target-only Redox and Windows package families. Their versions are
  upstream target support, not Linux release duplication.
- `investigate`: the remaining 29 groups are forced by independent upstream families, including
  Wasmtime, Zenoh, WebTransport, parsing, crypto, and collection stacks. No semver
  compatibility is inferred merely from package names.

## Recommendations for #632

The canonical recommendation table is
[`recommendations.tsv`](../../console/evidence/issue-631-live/recommendations.tsv).
Stable row IDs must be preserved by #632. Current decisions are:

| Decision | Rows | Meaning |
| --- | ---: | --- |
| `prune-features` | 10 | source and feature origin support a bounded removal or explicit feature set |
| `align-version` | 1 | dev-only Criterion pair can converge on the workspace version |
| `investigate` | 2 | provider or public-feature ownership needs proof before mutation |

#632 may implement only `prune-features` and `align-version` rows. `investigate` and
`leave` are explicit no-change outcomes until their revisit condition is satisfied.
Every implementation must still run its own compile, test, binary-size, and behavior
gates; this audit is decision input, not proof that a future manifest edit compiles.

## Binary and Crate Contribution

All explicit roots were built remotely to materialize the audited release artifacts.
Only deterministic artifact bytes and SHA-256 values are retained:

| Root | Bytes | SHA-256 |
| --- | ---: | --- |
| `sentinel-daemon` | 54,671,272 | `9066107775377130adb9b3b54f6f7849d9051b441065187dfdd281540e012940` |
| `sentinel-projection-service` | 4,399,432 | `edf0cdec0e099415b7909a68923ea356d01825d1cf50814c62fd1f551ee22f93` |
| `sentinel-dashboard-backend` | 18,966,712 | `132d003dfb9c0f5901dc710d3de79b788651b4f1747f582e376c3805e2eff446` |
| `sentinel-gaia-loop` | 8,905,560 | `a8b8e0d2e98d2971002fb9bf07b75d911cdb788df6601391511ee12b855aeb47` |
| `agent-runtime` | 369,776 | `12dda746c12e5685258f15111ec7ccc08201cbe28d5cc4898b3fc356a50a1f0c` |
| `sentinel-ctl` | 4,192,976 | `20da53f6f6749c6982b4461baca27ced106d044097e2de3c91ca4694736cc604` |
| `sentinel-gaia` | 2,126,144 | `f08e9da7ae4ed638d8862e71e3aed62ac5e8fd57b21967a740fed7c237085cce` |
| `sentinel-nightrun` | 5,643,664 | `bdb54dceb9a41291bcd2e1aa464b803531a11b65e07e2cbf6686bb8f135bd790` |

The canonical machine-readable artifact table is
[`release-builds.tsv`](../../console/evidence/issue-631-live/release-builds.tsv).

`cargo-bloat 0.12.1` builds symbol-rich analysis artifacts, whose file sizes differ from
the stripped release artifacts above. The report therefore keeps actual release bytes,
analysis `.text`, and analysis-file size in separate columns:

| Root | Release bytes | Analysis `.text` | Analysis file | Largest reported crate |
| --- | ---: | ---: | ---: | --- |
| `sentinel-daemon` | 54,671,272 | 38.2 MiB | 83.0 MiB | `cranelift_codegen`, 3.1 MiB / 8.0% of `.text` |
| `sentinel-projection-service` | 4,399,432 | 3.2 MiB | 9.2 MiB | `[Unknown]`, 1.4 MiB / 43.2% |
| `sentinel-dashboard-backend` | 18,966,712 | 12.6 MiB | 30.2 MiB | `[Unknown]`, 2.2 MiB / 17.5% |
| `sentinel-gaia-loop` | 8,905,560 | 5.7 MiB | 17.1 MiB | `[Unknown]`, 1.5 MiB / 25.6% |
| `agent-runtime` | 369,776 | 260.7 KiB | 4.2 MiB | `std`, 254.7 KiB / 97.7% |
| `sentinel-ctl` | 4,192,976 | 2.7 MiB | 11.2 MiB | `rustls`, 459.9 KiB / 16.8% |
| `sentinel-gaia` | 2,126,144 | 1.5 MiB | 6.6 MiB | `std`, 378.7 KiB / 24.1% |
| `sentinel-nightrun` | 5,643,664 | 4.2 MiB | 10.8 MiB | `[Unknown]`, 1.4 MiB / 33.4% |

The normalized top-20 tables are in
[`bloat/`](../../console/evidence/issue-631-live/bloat/); their deterministic summary is
[`bloat-summary.tsv`](../../console/evidence/issue-631-live/bloat-summary.tsv).

No build-server timing or performance result is part of this audit. Genuine Sentinel
performance evidence belongs only on authorized runtime VMs or cluster nodes.

## Public Evidence Sanitization

Unmodified remote output remains internal and untracked. Committed metadata summaries,
trees, duplicate chains, release-artifact summaries, and bloat output are deterministic normalized
derivatives. The normalizer replaces private locations and authorities with:

```text
<WORKSPACE>
<REMOTE_PROJECT>
<REMOTE_TARGET>
<CARGO_HOME>
<HOME>
<USER>
<HOST>
```

The fail-closed scanner rejects IP addresses, SSH authorities, usernames, hostnames,
home paths, workspace paths, remote temporary paths, Cargo-home paths, unexpected
absolute paths, and binary evidence. Negative unit fixtures cover each class. A green
scan is required before every commit and push.

## Reproduction Commands

Every Cargo invocation runs remotely. The public commands intentionally omit private
connection configuration:

```bash
cargo remote --no-copy-lock -- metadata --locked --format-version 1
cargo remote --no-copy-lock -- metadata --locked --format-version 1 --filter-platform x86_64-unknown-linux-gnu
cargo remote --no-copy-lock -- tree -p <ROOT> --target x86_64-unknown-linux-gnu -e normal --prefix depth --no-dedupe
cargo remote --no-copy-lock -- tree -p <ROOT> --target x86_64-unknown-linux-gnu -e normal,build --prefix depth
cargo remote --no-copy-lock -- tree -p <ROOT> --target x86_64-unknown-linux-gnu -e normal,features --prefix depth
cargo remote --no-copy-lock -- tree --workspace --target x86_64-unknown-linux-gnu -e normal,build --prefix depth --no-dedupe
cargo remote --no-copy-lock -- tree --workspace --target x86_64-unknown-linux-gnu -e normal,build,dev --prefix depth --no-dedupe
cargo remote --no-copy-lock -- tree --workspace --target x86_64-unknown-linux-gnu -e dev --prefix depth --no-dedupe
cargo remote --no-copy-lock -- tree --workspace --target all -e normal,build,dev --prefix depth --no-dedupe
cargo remote --no-copy-lock -- build --release --locked -p <PACKAGE> --bin <BINARY>
cargo remote --no-copy-lock -- bloat --release --locked -p <PACKAGE> --bin <BINARY> --crates -n 20
```

Classifier and sanitization checks are Python-only and do not invoke local Rust tools:

```bash
python3 -m unittest scripts.tests.test_dependency_reachability_audit
python3 scripts/dependency-reachability-audit.py audit --check \
  --lock Cargo.lock --metadata-all <RAW_ALL> --metadata-native <RAW_NATIVE> \
  --trees-dir console/evidence/issue-631-live/trees \
  --output-dir console/evidence/issue-631-live
python3 scripts/dependency-reachability-audit.py check-public-evidence docs/audits/dependency-reachability.md console/evidence/issue-631-live
python3 scripts/dependency-reachability-audit.py check-staged
```

## Separate Findings and Boundaries

- The Renovate workflow pin is defective. It is a separate finding and is not repaired
  by #631 or #632.
- Release generator inventory drift is documented above and remains unchanged.
- No TOGAF HTML or internal Gaia architecture document is touched.
- No dependency, feature, version, deployment, or runtime mutation occurs in this PR.
- No VM was accessed. No service was deployed, restarted, or probed.
- Runtime behavior, startup time, memory use, and production performance are **not
  tested** by this audit.
