# cortex-gateway

## Purpose

`cmd/cortex-gateway` is the Go LLM gateway. It proxies provider calls, assembles agent prompts, applies synthesis/interception rules, validates extracted actions, enforces capability policy, exposes control-plane endpoints, and records optional event-store telemetry.

## Interfaces

- Public proxy server on `CORTEX_PORT` (default `8080`).
- Control plane on `CORTEX_CONTROL_PORT` (default `8081`).
- `/metrics` exposes Prometheus metrics.
- Internal packages under `internal/` implement provider routing, guardrails, capability policy, extraction, synthesis, sequencing, traffic control, APICP, and observability.
- Optional event persistence is enabled by `SENTINEL_CORTEX_EVENT_STORE_PATH`.

## Dependencies

- `pkg/sentinel-go/eventstore` and `pkg/sentinel-go/judge`.
- `modernc.org/sqlite`, `prometheus/client_golang`, and `BurntSushi/toml`.
- External provider credentials are supplied through environment variables; no provider call is required for unit tests.

## Verify

```bash
cd cmd/cortex-gateway
go test ./...
go build ./...
```

Gateway runtime or provider-path changes require deploy-VM verification with the gateway intentionally started only when the issue scope allows LLM traffic.
