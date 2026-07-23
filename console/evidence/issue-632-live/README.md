# Issue 632 Dependency Pruning Evidence

This directory records the public-safe structural verification for the dependency
changes selected by the canonical Issue #631 audit plus the append-only Issue #632
errata overlay. Raw cargo-remote wrapper output is not committed. Command excerpts
replace remote paths with stable placeholders and omit hosts, users, timestamps,
transfer progress, and duration data.

`provenance.txt` pins the implementation base, canonical audit source, before/after
lockfile hashes, and the root-isolated artifact method.

## AC Mapping

| AC | Result | Evidence |
| --- | --- | --- |
| AC-1 | Every baseline recommendation row is mapped to its implementation or explicit no-change decision; falsified rows are overlaid without rewriting closed #631 evidence. | `row-commit-map.tsv`, `audit-errata-v1.tsv`, `audit-errata-v1.md` |
| AC-2 | Release feature trees prove the selected Tokio, Futures, Axum, and WebTransport edges left at least one affected service graph; direct depth-one trees prove four unused service edges were removed. | `feature-deltas.md` |
| AC-3 | Criterion 0.5.1 and criterion-plot 0.5.0 were collapsed into 0.8.2. All remaining duplicate groups retain a version-and-forcer handoff. | `duplicates-before-after.md`, `remaining-duplicates.tsv` |
| AC-4 | Remote format, check, test, Clippy, and release-build gates are recorded with terminal outcomes; detached replays bind each logical group to its final green state. | `remote-gates.md`, `group-gates.md` |
| AC-5 | Lockfile and root-isolated release-artifact byte deltas are recorded without timing or performance claims and are registered under stable record `PS-DEP-632-STRUCTURAL-V1`. | `metrics.tsv`, `release-artifacts.tsv`, internal register digest in `provenance.txt` |
| AC-6 | Renovate's Cargo grouping keeps the aligned Criterion declarations together; the separately known workflow defect is not changed here. | `renovate.md` |

## Scope Boundaries

- No VM, cluster node, runtime service, deployment, or benchmark was accessed.
- No startup-time, memory, throughput, latency, or build-duration claim is made.
- The closed Issue #631 report and evidence remain byte-identical to their merged
  digests. Issue #632 corrections are append-only in `audit-errata-v1.tsv`.
- Release artifact comparisons use the same one-package/one-binary command shape as
  the Issue #631 baseline. A combined multi-root artifact is not used for byte deltas
  because Cargo feature unification would make the comparison invalid.
- DEP-002 remains unchanged because `sentinel-telemetry` uses JSON formatting in its
  release library API.
- DEP-004 remains unchanged because `sentinel-console-plane -> sentinel-fs` activates
  zstd defaults in the dashboard release graph, so pruning only the dashboard's direct
  declaration has no effect.
- DEP-012 and DEP-013 remain `investigate`; no provider or public-feature ownership
  decision is inferred.
- No Renovate workflow, cargo-deny policy, toolchain, source patch, or unrelated
  dependency was changed.

## Evidence Digest

The final digest is calculated over the files in this directory after all gates and
metrics are complete:

```bash
(cd console/evidence/issue-632-live && sha256sum * | sha256sum)
```
