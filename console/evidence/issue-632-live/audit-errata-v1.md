# Issue 632 Audit Errata Overlay v1

Issue #631 remains an immutable, closed audit baseline. Its canonical report and
machine-readable tables are not rewritten by this implementation. Consumers must
evaluate the Issue #631 baseline together with the append-only overlay in
`audit-errata-v1.tsv`.

## Pinned Baseline

```text
source_commit=13e87b663cc3b47223a2b0052db1cc6c274e66c8
merged_report_sha256=67ad1d87d7023cee7a5c5d16a8cc79ee2b626a18f6907bb28d60fcadc64d4723
recommendations_tsv_sha256=7eab424c1188c36dcfa6f8dccb56ba64e2179be903726c495ff688aac7730a6d
feature_review_tsv_sha256=983af979a0b7adfdc2bd55cc090e3908c98c64c1c06a423c9531800dfb8e3595
overlay_tsv_sha256=fa7def31cc30c4f4f418373395a3e9478647d3f6e27e72d92a07409a6320bc3a
```

The baseline contains 10 `prune-features`, one `align-version`, and two
`investigate` recommendations. This overlay changes no historical bytes. It records
two implementation-time falsifications:

- `DEP-002`: removing tracing-subscriber JSON makes the release library fail to
  compile because `sentinel-telemetry` calls `fmt::layer().json()`.
- `DEP-004`: pruning only the dashboard's direct zstd declaration does not remove
  zstd defaults from the release graph because `sentinel-console-plane` reaches the
  default-enabled `sentinel-fs` edge.

The effective implementation decision set is therefore eight `prune-features`, one
`align-version`, two `leave`, and two `investigate` rows.

## Application Rule

Apply rows by stable `row_id`. A matching row in `audit-errata-v1.tsv` supersedes only
the baseline decision for Issue #632 and later consumers. All other baseline fields
remain historical evidence. The TSV binds each correction to the original table and
row digests, the falsifying command and normalized result, and the correcting commit.
