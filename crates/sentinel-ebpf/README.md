# sentinel-ebpf

## Purpose

`sentinel-ebpf` is the userspace eBPF monitoring facade. It loads kernel probes when the `ebpf` feature and host capabilities are available, and otherwise provides userspace fallback collectors for agent health, I/O, network, and PSI metrics.

## Interfaces

- `EbpfCollector` returns `MetricsSnapshot` values for daemon and dashboard telemetry.
- `MetricsExporter` renders collected metrics for scraping/export paths.
- `CapabilityReport`, `InitResult`, and `MonitoringMode` describe whether kernel probes or userspace fallback are active.
- Probe-side binaries are built from `crates/sentinel-ebpf-probes`.

## Dependencies

- `sentinel-common` for shared agent/cgroup metadata.
- `tokio`, `tracing`, `serde`, `serde_json`, and `thiserror` for async collection and reporting.
- Optional `aya`, `aya-log`, `libc`, and `object` under the `ebpf` feature.

## Verify

```bash
cargo remote -c -- test -p sentinel-ebpf
cargo remote -c -- check -p sentinel-ebpf --features ebpf
```

Kernel-mode runtime verification requires `CAP_BPF`/BTF support on the target host; normal CI should still keep the userspace fallback path green.
