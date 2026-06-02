# sentinel-nats-bridge

## Purpose

`sentinel-nats-bridge` mirrors pending outbox events from the SQLite event store into NATS JetStream. It is the Go-side bridge between Limbo-style event persistence and NATS consumers.

## Interfaces

- Polls `outbox` rows from the configured event-store path.
- Publishes to NATS subjects using `pkg/sentinel-go/messaging`.
- Health server exposes `GET /health` and `GET /ready` on the configured health port.
- Retries publishes and marks failed entries after the retry limit.

## Dependencies

- `pkg/sentinel-go/eventstore` and `pkg/sentinel-go/messaging`.
- `nats.go` and `BurntSushi/toml`.
- Indirect `modernc.org/sqlite` through the shared eventstore package.

## Verify

```bash
cd services/sentinel-nats-bridge
go test ./...
go build ./...
```

Bridge runtime changes require NATS connectivity checks and event-store outbox evidence in the environment where the bridge runs.
