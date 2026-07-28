# Benchmark Results

## Issue 633 - Cargo Duplicate-Version Bans

Date: 2026-07-24

Runtime target class: `NONE`

This CI policy change has no runtime benchmark. Its drift baseline is structural
and is taken directly from the finished Issue #632 handoff:

| Structural metric | Result |
| --- | ---: |
| Duplicate package names | 39 |
| Version rows across duplicate groups | 89 |
| Exact lower-version skip entries | 50 |
| Unskipped highest-version baselines | 39 |

The positive gate uses `cargo-deny 0.19.0` through `cargo remote -c --`. No VM,
performance measurement, build-server timing, or runtime claim is included.

## Issue 442 - Gaia Console Readiness And Native Sessions

Date: 2026-07-18

Infrastructure:

- Remote Rust gates and release builds ran through `cargo remote` on `.155`.
- Runtime benchmarks ran directly on `ubuntu@10.0.0.241`; `.240` was not used.
- Native Claude Code `2.1.214` (ELF) was used; Node.js/npm were absent.

Results:

| Benchmark | Result |
| --- | ---: |
| Readiness idle duration / samples | 3602 s / 61 |
| Readiness average CPU | 0.000833% |
| Readiness RSS min / avg / max | 9368 / 9368 / 9368 KiB |
| Claude processes across idle samples | 0 |
| Event to in-console alert latency | 22 s |
| Deep + resume | 31,496 token accounting units / USD 0.0206505 |
| Complete setup | 12,267 token accounting units / USD 0.0197928 |
| Dashboard stream proof | 4,119 token accounting units / USD 0.004407 |

Native installation smokes cost USD 0.027488 and the final environment-hardening
smoke cost USD 0.004439. Accepted native verification cost USD 0.0767773 in
total. A failed pre-fix diagnostic cost USD 0.0642571 and is reported but
excluded from accepted-session metrics. Command/output evidence and the raw
61-row idle sample set are committed under `console/evidence/issue-442-live/`.

## Issue 443 - Gaia Console Memory

Date: 2026-06-27

Scope:

- `crates/sentinel-gaia-memory`
- Gaia Console Memory graph insert/query/supersede paths
- Read-only rehydration context assembly with `events_replayed=0`, `event_rows_loaded=0`, and `event_copy_count=0`

Infrastructure:

- Build artifact: `cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- bench -p sentinel-gaia-memory --no-run`
- Benchmark VM: `ubuntu@10.0.0.241`
- Benchmark command: `./gaia_memory_bench --bench --noplot --format pretty`
- `.240` was not used for benchmark execution.
- `cargo-remote` was not used to execute benchmarks.

Results:

| Benchmark | Median | Range |
| --- | ---: | ---: |
| `gaia_console_memory.graph_insert_fact` | 6.5244 ms | 5.2763 ms - 8.1790 ms |
| `gaia_console_memory.graph_query_current_1k` | 5.2903 us | 5.2398 us - 5.3456 us |
| `gaia_console_memory.graph_supersede_fact` | 11.592 ms | 11.169 ms - 11.892 ms |
| `gaia_console_memory.rehydrate_readonly_zero_replay` | 554.99 us | 554.63 us - 555.42 us |

Rehydration benchmark invariant:

- Uses immutable read-only SQLite URI opens for `events.db` and `projection.db` so live verification does not create WAL/SHM side files under `/opt/sentinel/data`.
- Uses read-only Hippocampus access.
- Does not replay or copy event rows.
- Asserts `events_replayed=0`, `event_rows_loaded=0`, and `event_copy_count=0` inside the benchmark loop.

Evidence snippets and command output are committed under `console/evidence/issue-443-live/`.

## Issue 473 - Dashboard TLS Modes

Date: 2026-06-27

Scope:

- `sentinel-dashboard-backend` live on `ubuntu@10.0.0.240`
- TLS handshake latency through `curl` `time_appconnect`
- WebTransport connect latency through browser `WebTransport.ready`
- Zero-Config mode with self-signed certificate hash pinning
- Production mode with provided CA-signed certificate and no certificate hash pinning

Infrastructure:

- Build artifact: `cargo remote -H root@10.0.0.155 -t /tmp/builds -c -- build -p sentinel-dashboard-backend --release`
- Benchmark VM: `ubuntu@10.0.0.240`
- `cargo-remote` was not used to execute benchmarks.
- Browser for benchmark: Playwright 1.61.1, Chrome for Testing 149.0.7827.55.
- Production test CA root was imported into `sql:/tmp/issue-473-live/chromium-ca-home/.pki/nssdb` and `/usr/local/share/ca-certificates/sentinel-issue473-root.crt`.
- Production browser run used `--use-system-ca --webtransport-developer-mode` and did not use `ignoreHTTPSErrors` or `--ignore-certificate-errors`.
- Zero-Config benchmark used `curl -k` and `ignore_https_errors=true` only to load the expected self-signed HTTPS origin; WebTransport still used `serverCertificateHashes`.

Results:

| Mode | Cert hash | TLS handshake p50 | TLS handshake p95 | WT connect p50 | WT connect p95 | Samples |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Zero-Config | present | 6.30 ms | 7.43 ms | 9.00 ms | 10.20 ms | 20 |
| Production | null | 5.65 ms | 6.58 ms | 10.50 ms | 12.20 ms | 20 |

Notes:

- Zero-Config WebTransport max was `19537.40 ms` in the warm run while p95 stayed `10.20 ms`; raw JSON is kept in evidence.
- Production WebTransport max was `12.30 ms`.
- Production `/api/cert-hash` returned `{"algorithm":"sha-256","hash":null}`.
- Production `curl --cacert /tmp/issue-473-live/production-ca/root.pem https://localhost:8001/api/health` returned HTTP 200.

Evidence snippets, screenshots, setup scripts, and raw benchmark JSON are committed under `console/evidence/issue-473-live/`.
