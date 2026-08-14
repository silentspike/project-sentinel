# sentinel-nats-bridge

## Purpose

`sentinel-nats-bridge` mirrors pending outbox events from the SQLite event store into NATS JetStream. It is the Go-side bridge between Limbo-style event persistence and NATS consumers.

## Interfaces

- Polls `outbox` rows from the configured event-store path.
- Drains the backlog immediately at startup and processes consecutive bounded
  batches until no pending row remains.
- Publishes synchronously to JetStream using a stable `Nats-Msg-Id`; only a
  returned PubAck permits the exact outbox row to transition to `published`.
- Health server exposes `GET /health` and `GET /ready` on the configured health port.
- Retries publishes and marks failed entries after the retry limit.

`GET /health` is process liveness. `GET /ready` is fail-closed until the first
successful outbox scan has completed, NATS is connected, and the durable store
reports zero pending, failed, or otherwise non-published rows. The endpoint
returns only stable reason codes and counts, never database or broker errors.

The SQLite adoption step is a compare-and-set bound to the outbox ID, event ID,
operation ID, and `pending` state. If PubAck succeeds but that compare-and-set
does not, the sweep stops. The next poll republishes the same stable message ID,
allowing JetStream deduplication to retain one effective broker event before
the row is adopted locally.

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
