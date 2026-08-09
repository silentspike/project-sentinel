# Release Feature and Direct-Edge Deltas

The before-state is the merged Issue #631 feature and compact graph evidence. The
after-state is generated from the final Issue #632 manifests with remote Cargo.

## DEP-001: Tokio

Before, the Nightrun release tree contained:

```text
tokio feature "full"
tokio feature "fs"
tokio feature "io-std"
```

After:

```bash
cargo remote -c -- tree -p sentinel-nightrun -e features,no-dev -i tokio
```

```text
tokio feature "io-util"
tokio feature "macros"
tokio feature "net"
tokio feature "process"
tokio feature "rt-multi-thread"
tokio feature "signal"
tokio feature "sync"
tokio feature "time"
```

`full`, `fs`, `io-std`, and `test-util` are absent from this deployed service's release
feature graph. Other workspace roots may still activate individual Tokio features
through independent dependencies; no workspace-wide absence is claimed.

## DEP-003: Futures

Before, the Gaia Console Loop release tree contained:

```text
futures feature "default"
futures feature "executor"
futures-executor v0.3.32
```

After:

```bash
cargo remote -c -- tree -p sentinel-gaia-loop -e features,no-dev
```

```text
futures feature "async-await"
futures feature "std"
```

No `futures feature "executor"` or `futures-executor` node remains in the Gaia Console
Loop release graph. Workspace test graphs can still contain the crate through unrelated
consumers.

## DEP-005 and DEP-007: Dashboard Features

Before, the dashboard release tree contained:

```text
axum feature "default"
axum feature "form"
wtransport feature "default"
wtransport feature "ring"
wtransport feature "self-signed"
```

After:

```bash
cargo remote -c -- tree -p sentinel-dashboard-backend -e features,no-dev
```

```text
axum feature "http1"
axum feature "json"
axum feature "matched-path"
axum feature "original-uri"
axum feature "query"
axum feature "tokio"
axum feature "tower-log"
axum feature "tracing"
wtransport feature "ring"
```

Axum `default/form` and WebTransport `default/self-signed` are absent. zstd defaults
remain active through `sentinel-console-plane -> sentinel-fs`; DEP-004 is therefore an
explicit `leave`, not a claimed prune.

## DEP-006, DEP-008, DEP-009, and DEP-010: Direct Edges

The Issue #631 compact normal/build graphs contained these direct edges:

```text
sentinel-dashboard-backend -> tower
sentinel-dashboard-backend -> sentinel-projection
sentinel-projection-service -> sentinel-common
sentinel-nightrun -> sentinel-telemetry
```

After, depth-one normal trees contain none of those edges:

```bash
cargo remote -c -- tree -p sentinel-dashboard-backend -e normal --depth 1
cargo remote -c -- tree -p sentinel-projection-service -e normal --depth 1
cargo remote -c -- tree -p sentinel-nightrun -e normal --depth 1
```

```text
dashboard direct tower=absent
dashboard direct sentinel-projection=absent
projection-service direct sentinel-common=absent
nightrun direct sentinel-telemetry=absent
```

Transitive instances remain where owned by other crates; the claim is limited to the
four audited direct manifest edges.
