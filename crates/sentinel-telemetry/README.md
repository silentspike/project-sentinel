# sentinel-telemetry

## Purpose

`sentinel-telemetry` is the shared observability layer. It provides structured logging setup, metrics primitives, health snapshots, context propagation, exporters, and error classification.

## Interfaces

- `MetricsRegistry`, `Counter`, `Gauge`, `Histogram`, and `MetricsSnapshot` expose in-process metrics.
- `HealthRegistry`, `HealthSnapshot`, and `SubsystemHealth` track service readiness.
- `TraceContext` and telemetry constants propagate correlation context.
- `TelemetryExporter` sends metrics through configured transports.
- `init_logging` and `init_logging_dev` are available when the `telemetry` feature is active.

## Dependencies

- `tracing`, `tracing-subscriber`, `serde`, `serde_json`, and `uuid`.
- `sentinel-common` for shared context types.

## Verify

```bash
cargo remote -c -- test -p sentinel-telemetry
```

Telemetry changes should also be sampled in the service that consumes the metric or health path.
