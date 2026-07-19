# Dependency Reachability and Cost Audit

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
- Linux release reachability contains 499 normal and 91 build/proc-macro packages.
- The lockfile also contains 2 non-release workspace packages, 31 dev/bench-only
  packages, 90 foreign-target-only packages, and 4 disabled optional packages.
- There are 41 duplicate package names covering 94 locked versions. Two dev-only
  groups are ready to align, ten foreign-target groups are explicit `leave` results,
  and the remaining groups require upstream-chain or provider analysis.
- Thirty high-value direct dependencies were checked against release source. The
  resulting table contains ten actionable prune rows, one dev-only alignment row, and
  two investigation rows.
- No VM, deploy, service restart, or runtime assertion belongs to this audit.

All eight explicit root builds, all eight cargo-bloat contribution tables, and three
uncontended clean-target workspace release builds are complete. The shared-target root
build times overlapped other users of the remote builder, so those times are retained as
`CONTENDED` context and are not a performance baseline. Only the three isolated,
load-monitored clean-target runs contribute to the baseline medians.

## Pinned Provenance

| Item | Value |
| --- | --- |
| Base commit | `f622885d7137b8cb334adf655d42749c5aa1d881` |
| `Cargo.lock` SHA-256 | `9ea96b715b709d43b9b90352968c06998111476a0ebb546254db0f43e4034b22` |
| Lockfile packages | 717 |
| Workspace members | 27 |
| Release roots | 8 |
| Native target | `x86_64-unknown-linux-gnu` |
| Remote Rust | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| Remote Cargo | `cargo 1.97.1 (c980f4866 2026-06-30)` |
| cargo-bloat | `0.12.1`, installed under an issue-local remote tool directory |

PR #611 remains open and changes only `Cargo.lock`. If it or any other lockfile change
lands before completion, this branch must rebase and regenerate metadata, trees,
duplicates, feature reviews, binary sizes, bloat, clean-target build measurements, and
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

Package identity is `(name, version, source)`, not name alone. The classifier consumes
locked remote `cargo metadata` for all targets and for the native Linux target. It walks
each root's normal graph, carries build context through build dependencies, classifies
proc-macro packages as build-time, separately walks non-release workspace roots and dev
edges, and compares all-target edges with the native graph for foreign-target secondary
membership.

Each lockfile package receives the first matching primary category:

1. `release-normal`
2. `release-build`
3. `non-release-workspace-normal`
4. `dev-bench-only`
5. `target-only`
6. `optional-disabled`

The classifier fails if a root does not resolve exactly once, metadata cannot map to the
lockfile, any package remains unclassified, or category totals do not equal the lockfile
count. Secondary columns retain dev and foreign-target overlap rather than erasing it.

## Reachability Results

| Primary category | Packages |
| --- | ---: |
| `release-normal` | 499 |
| `release-build` | 91 |
| `non-release-workspace-normal` | 2 |
| `dev-bench-only` | 31 |
| `target-only` | 90 |
| `optional-disabled` | 4 |
| **Total** | **717** |

The four packages present in the lockfile but absent from the all-target default
resolution are `generator 0.8.9`, `io-uring 0.7.11`, `loom 0.7.2`, and
`scoped-tls 1.0.1`. They are classified `optional-disabled`, not falsely counted as
Linux release dependencies.

The complete package table is
[`reachability.tsv`](../../console/evidence/issue-631-live/reachability.tsv). The
machine-checkable invariants are in
[`reachability-summary.txt`](../../console/evidence/issue-631-live/reachability-summary.txt).

## Feature Origin and Source Review

Feature resolution from `cargo metadata` is a workspace union and can include dev
activation. It is therefore recorded only as diagnostic context. Release decisions use
the per-root `cargo tree -e normal,features` output. The direct-feature table separates
`release_features` from `metadata_union_features` so the two cannot be confused.

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
- `sentinel-telemetry`, `sentinel-projection`, and `sentinel-common` contain several
  direct edges whose use differs by root; recommendations split proven unused edges
  from owner-sensitive forwarding contracts.

## Duplicate Versions

The lockfile contains 41 duplicate names and 94 version rows. The full table records
primary reachability, release roots, immediate lockfile forcers, and a decision for each
version: [`duplicate-versions.tsv`](../../console/evidence/issue-631-live/duplicate-versions.tsv).
The native Linux reverse graph is retained in normalized form under
[`duplicates/`](../../console/evidence/issue-631-live/duplicates/).

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

## Binary Contribution and Build Cost

All explicit root builds are incremental release builds against one shared remote target.
Foreign Rust jobs overlapped every run, so their times are retained as structural build
context only and are excluded from the clean-target baseline:

| Root | Bytes | SHA-256 | Cargo `Finished in` | cargo-remote E2E | Status |
| --- | ---: | --- | ---: | ---: | --- |
| `sentinel-daemon` | 54,171,624 | `83bf73fceb0f27a1e51641df3e4e4eae585b11df4e6be9841005b618f7cc82d8` | 13m 08s | 824.49s | `CONTENDED` |
| `sentinel-projection-service` | 4,389,800 | `4612103f7cba3e29920f9d35a21da20e4ae17282ce06caaeeb24763a8d6328e4` | 2m 58s | 201.14s | `CONTENDED` |
| `sentinel-dashboard-backend` | 18,958,424 | `b47167117aec7e76bff1817fc0cd4e8aed0e2e0f2677227bbb63aa4eee884d1e` | 3m 10s | 202.83s | `CONTENDED` |
| `sentinel-gaia-loop` | 8,907,120 | `9438c93f9393cf455d1f06707f9a30d266838022631e5154f16439527273ca8f` | 3m 25s | 221.45s | `CONTENDED` |
| `agent-runtime` | 369,776 | `12dda746c12e5685258f15111ec7ccc08201cbe28d5cc4898b3fc356a50a1f0c` | 1.44s | 4.26s | `CONTENDED` |
| `sentinel-ctl` | 4,192,976 | `20da53f6f6749c6982b4461baca27ced106d044097e2de3c91ca4694736cc604` | 41.93s | 52.78s | `CONTENDED` |
| `sentinel-gaia` | 2,119,184 | `d06e5f6d59a0edaf1668b8f3b7a2631c22a6b5a75148b379507ab16a1d621f41` | 1m 00s | 69.93s | `CONTENDED` |
| `sentinel-nightrun` | 5,631,560 | `cb7e9c5fe58da8c73da41d3c7e3fb9eac262925154e1a2fdfbd25eafded2589d` | 14.15s | 18.50s | `CONTENDED` |

The canonical machine-readable artifact table is
[`release-builds.tsv`](../../console/evidence/issue-631-live/release-builds.tsv).

`cargo-bloat 0.12.1` builds symbol-rich analysis artifacts, whose file sizes differ from
the stripped release artifacts above. The report therefore keeps actual release bytes,
analysis `.text`, and analysis-file size in separate columns:

| Root | Release bytes | Analysis `.text` | Analysis file | Largest reported crate |
| --- | ---: | ---: | ---: | --- |
| `sentinel-daemon` | 54,171,624 | 37.8 MiB | 82.2 MiB | `cranelift_codegen`, 3.1 MiB / 8.1% of `.text` |
| `sentinel-projection-service` | 4,389,800 | 3.2 MiB | 9.2 MiB | `[Unknown]`, 1.4 MiB / 43.3% |
| `sentinel-dashboard-backend` | 18,958,424 | 12.6 MiB | 30.2 MiB | `[Unknown]`, 2.2 MiB / 17.5% |
| `sentinel-gaia-loop` | 8,907,120 | 5.7 MiB | 17.1 MiB | `[Unknown]`, 1.5 MiB / 25.6% |
| `agent-runtime` | 369,776 | 260.7 KiB | 4.2 MiB | `std`, 254.7 KiB / 97.7% |
| `sentinel-ctl` | 4,192,976 | 2.7 MiB | 11.2 MiB | `rustls`, 459.9 KiB / 16.8% |
| `sentinel-gaia` | 2,119,184 | 1.5 MiB | 6.6 MiB | `std`, 378.8 KiB / 24.1% |
| `sentinel-nightrun` | 5,631,560 | 4.2 MiB | 10.8 MiB | `[Unknown]`, 1.4 MiB / 33.5% |

The normalized top-20 tables are in
[`bloat/`](../../console/evidence/issue-631-live/bloat/); their deterministic summary is
[`bloat-summary.tsv`](../../console/evidence/issue-631-live/bloat-summary.tsv).

Three uncontended clean-target workspace release builds used empty unique remote
project/target directories while retaining registry, source, and toolchain caches.
Cargo's `Finished in` time and cargo-remote end-to-end wall time are reported separately:

| Run | Cargo `Finished in` | cargo-remote E2E | Load samples | Foreign overlap markers |
| --- | ---: | ---: | ---: | ---: |
| C05 | 7m 18s | 466.91s | 86 | 0 |
| C06 | 7m 17s | 459.00s | 85 | 0 |
| C07 | 7m 17s | 458.55s | 85 | 0 |
| **Median** | **7m 17s** | **459.00s** |  |  |

Each included run acquired the issue-local builder lease, passed a 60-90 second idle
preflight, and sampled remote Cargo/rustc process groups every five seconds. Four prior
attempts were retained but excluded after the fail-closed monitor observed foreign load
or could not classify a race safely. This is a structural clean-target build-cost
baseline, not a cache-purged or runtime-performance benchmark. The complete attempt log
is [`clean-builds.tsv`](../../console/evidence/issue-631-live/clean-builds.tsv).

## Public Evidence Sanitization

Unmodified remote output remains internal and untracked. Committed metadata summaries,
trees, duplicate chains, build excerpts, and bloat output are deterministic normalized
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
cargo remote -c -- metadata --locked --format-version 1
cargo remote -c -- metadata --locked --format-version 1 --filter-platform x86_64-unknown-linux-gnu
cargo remote -c -- tree -p <ROOT> --target x86_64-unknown-linux-gnu -e normal --prefix depth --no-dedupe
cargo remote -c -- tree -p <ROOT> --target x86_64-unknown-linux-gnu -e build --prefix depth --no-dedupe
cargo remote -c -- tree -p <ROOT> --target x86_64-unknown-linux-gnu -e normal,features --prefix depth --no-dedupe
cargo remote -c -- tree --workspace --target x86_64-unknown-linux-gnu --duplicates -e normal,build,dev
cargo remote -c -- build --release --locked -p <PACKAGE> --bin <BINARY>
cargo remote -c -- bloat --release --locked -p <PACKAGE> --bin <BINARY> --crates -n 20
```

Classifier and sanitization checks are Python-only and do not invoke local Rust tools:

```bash
python3 -m unittest scripts.tests.test_dependency_reachability_audit
python3 scripts/dependency-reachability-audit.py audit --check --lock Cargo.lock --metadata-all <RAW_ALL> --metadata-native <RAW_NATIVE>
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
