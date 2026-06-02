# sentinel-judge

## Purpose

`sentinel-judge` is the Go quality and drift analysis service. It consumes NATS JetStream events, runs heuristic plus optional LLM-backed analysis, stores evolution data, emits alerts, and exposes a batch API used by nightrun-style workflows.

## Interfaces

- HTTP endpoints: `GET /health`, `GET /ready`, `GET /metrics`, `POST /api/v1/analyze`.
- NATS consumers read event and eBPF streams configured in `config/judge.toml`.
- The batch API accepts agent messages and returns analysis results through `services/sentinel-judge/api`.
- Internal packages cover config, analyzer, gateway client, persistence, alerter, and service loops.

## Dependencies

- `pkg/sentinel-go/messaging` and `pkg/sentinel-go/judge`.
- `nats.go`, `modernc.org/sqlite`, `prometheus/client_golang`, and `BurntSushi/toml`.
- Cortex Gateway URL/config for LLM-backed analysis.

## Verify

```bash
cd services/sentinel-judge
go test ./...
go build ./...
```

LLM-backed runtime verification must keep token use explicit; unit tests cover the handler and heuristic paths without provider calls.
