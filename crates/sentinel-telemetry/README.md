# sentinel-telemetry

## Purpose

`sentinel-telemetry` is the shared observability layer. It provides structured logging setup, metrics primitives, health snapshots, context propagation, exporters, and error classification.

## Interfaces

- `MetricsRegistry`, `Counter`, `Gauge`, `Histogram`, and `MetricsSnapshot` expose in-process metrics.
- `HealthRegistry`, `HealthSnapshot`, and `SubsystemHealth` track service readiness.
- `TraceContext` and telemetry constants propagate correlation context.
- `TelemetryExporter` sends metrics through configured transports.
- `init_observability` initializes production JSON logging and optional OTLP trace export.
- `init_logging` and `init_logging_dev` remain available when the `telemetry` feature is active.

## OTLP Trace Export

OTLP is part of the production telemetry path and is disabled by default. `sentinel-daemon` uses
`init_observability("sentinel-daemon")`, so enabling these variables exports real daemon spans.

| Env | Default | Meaning |
| --- | --- | --- |
| `SENTINEL_OTLP_ENABLED` | `false` | Enable OTLP trace export. |
| `SENTINEL_OTLP_PROTOCOL` | `http` | `http` or `grpc`. |
| `SENTINEL_OTLP_ENDPOINT` | protocol default | HTTP: `http://127.0.0.1:4318/v1/traces`; gRPC: `http://127.0.0.1:4317`. |
| `SENTINEL_OTLP_SERVICE_NAME` | caller default | OTel `service.name`. |
| `SENTINEL_OTLP_TIMEOUT_MS` | `3000` | Per-export timeout. |
| `SENTINEL_OTLP_BATCH_MS` | `5000` | Batch processor flush interval. |

Exporter failures are non-fatal. If the collector is down, the service keeps running and the
batch exporter drops/logs export errors instead of panicking.

## Dependencies

- `tracing`, `tracing-subscriber`, `serde`, `serde_json`, and `uuid`.
- `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, and `tracing-opentelemetry`
  when the default `telemetry` feature is active.
- `sentinel-common` for shared context types.

## Verify

```bash
cargo remote -c -- test -p sentinel-telemetry
```

Telemetry changes should also be sampled in the service that consumes the metric, health, or trace
path. For #381, the live smoke is `sentinel-daemon` exporting bootstrap/config-load spans to a local
OTLP receiver with both HTTP and gRPC.
