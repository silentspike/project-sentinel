# sentinel-go

## Purpose

`pkg/sentinel-go` is the shared Go module for Cortex Gateway, Sentinel Judge, and NATS Bridge. It keeps Go-side event-store, messaging, and quality heuristics consistent across services.

## Interfaces

- `eventstore` mirrors the Rust `sentinel-limbo` SQLite schema and supports atomic event + outbox writes.
- `messaging` owns NATS connection setup, JetStream stream definitions, and subject construction/parsing.
- `judge` provides drift, fatigue, quality, and model-swap heuristics.

## Dependencies

- `nats.go` for JetStream.
- `modernc.org/sqlite` for pure-Go SQLite access.
- Standard-library logging, testing, and concurrency primitives.

## Verify

```bash
cd pkg/sentinel-go
go test ./...
go test ./... -bench .
```

Changes here affect multiple Go services. Run the consuming service tests when changing exported contracts.
