# Duplicate Alignment and Skip-List Handoff

## Aligned Group

The Issue #631 baseline contained two Criterion and criterion-plot versions:

```text
criterion 0.5.1 <- sentinel-telemetry, sentinel-zenoh
criterion 0.8.2 <- workspace benchmark consumers
criterion-plot 0.5.0 <- criterion 0.5.1
criterion-plot 0.8.2 <- criterion 0.8.2
```

After alignment, the lockfile contains only:

```text
criterion 0.8.2
criterion-plot 0.8.2
```

The old Criterion branch also removed its exclusive `is-terminal 0.4.17` and
`itertools 0.10.5` packages. `cargo remote -c -- tree --workspace --duplicates` emits
neither Criterion name because each now has one version.

## Structural Delta

```text
lockfile packages:       717 -> 713 (-4)
duplicate names:          41 -> 39  (-2)
duplicate version rows:   94 -> 89  (-5)
```

## Remaining Groups

`remaining-duplicates.tsv` is the finished handoff for the cargo-deny gate issue. It
lists every remaining duplicate name, all current versions, its canonical Issue #631
decision, and the immediate forcing chain for each version. No entry is promoted from
`investigate` or `leave` by this issue.
