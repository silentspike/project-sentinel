// Package eventstore provides a Go-native SQLite event store compatible with
// sentinel-limbo's schema. It supports atomic Event+Outbox writes for the
// transactional outbox pattern (Command→Event Mapping, Issue #13 AC-5).
package eventstore

import (
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"fmt"
	"time"

	_ "modernc.org/sqlite"
)

// DomainEvent mirrors sentinel-limbo's DomainEvent (Rust).
// All fields map 1:1 to the events table columns.
type DomainEvent struct {
	EventID          string
	EventType        string
	AggregateID      string
	Payload          string
	CorrelationID    string
	CausationID      string
	OperationID      string
	Tick             int64
	TimestampMs      int64
	SchemaVersion    int
	CompensationType string
}

// OutboxEntry represents a pending publish in the outbox table.
type OutboxEntry struct {
	ID        int64
	EventID   string
	Topic     string
	Payload   string
	Status    string
	CreatedAt int64
}

// Store wraps a SQLite database with the sentinel-limbo event store schema.
type Store struct {
	db *sql.DB
}

const createEvents = `CREATE TABLE IF NOT EXISTS events (
	id INTEGER PRIMARY KEY AUTOINCREMENT,
	event_id TEXT NOT NULL UNIQUE,
	event_type TEXT NOT NULL,
	aggregate_id TEXT NOT NULL,
	payload TEXT NOT NULL,
	correlation_id TEXT NOT NULL,
	causation_id TEXT,
	operation_id TEXT NOT NULL,
	tick INTEGER NOT NULL,
	timestamp_ms INTEGER NOT NULL,
	schema_version INTEGER NOT NULL DEFAULT 1,
	compensation_type TEXT NOT NULL DEFAULT 'none'
)`

const createEventsIndices = `
CREATE INDEX IF NOT EXISTS idx_events_aggregate ON events(aggregate_id, id);
CREATE INDEX IF NOT EXISTS idx_events_type ON events(event_type, id);
CREATE INDEX IF NOT EXISTS idx_events_correlation ON events(correlation_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_events_operation ON events(operation_id)
`

const createOutbox = `CREATE TABLE IF NOT EXISTS outbox (
	id INTEGER PRIMARY KEY AUTOINCREMENT,
	event_id TEXT NOT NULL REFERENCES events(event_id),
	topic TEXT NOT NULL,
	payload TEXT NOT NULL,
	status TEXT NOT NULL DEFAULT 'pending',
	created_at INTEGER NOT NULL,
	published_at INTEGER
)`

const createOutboxIndex = `CREATE INDEX IF NOT EXISTS idx_outbox_pending ON outbox(status) WHERE status = 'pending'`

const pragmas = `
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA mmap_size = 268435456;
PRAGMA page_size = 8192
`

// Open creates or opens a SQLite event store at path.
// It applies WAL mode and creates all required tables/indices.
func Open(path string) (*Store, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("eventstore open: %w", err)
	}

	// Single connection for writes (SQLite concurrency).
	db.SetMaxOpenConns(1)

	if _, err := db.Exec(pragmas); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("eventstore pragmas: %w", err)
	}
	if _, err := db.Exec(createEvents); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("eventstore create events: %w", err)
	}
	if _, err := db.Exec(createEventsIndices); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("eventstore create events indices: %w", err)
	}
	if _, err := db.Exec(createOutbox); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("eventstore create outbox: %w", err)
	}
	if _, err := db.Exec(createOutboxIndex); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("eventstore create outbox index: %w", err)
	}

	return &Store{db: db}, nil
}

// AppendWithOutbox atomically inserts a DomainEvent and an outbox entry
// within a single transaction. Uses INSERT OR IGNORE for idempotency
// via the operation_id UNIQUE index.
func (s *Store) AppendWithOutbox(event DomainEvent, topic string) error {
	tx, err := s.db.Begin()
	if err != nil {
		return fmt.Errorf("eventstore begin tx: %w", err)
	}
	defer func() { _ = tx.Rollback() }()

	now := time.Now().UnixMilli()
	if event.TimestampMs == 0 {
		event.TimestampMs = now
	}
	if event.SchemaVersion == 0 {
		event.SchemaVersion = 1
	}
	if event.CompensationType == "" {
		event.CompensationType = "none"
	}

	res, err := tx.Exec(`INSERT OR IGNORE INTO events
		(event_id, event_type, aggregate_id, payload, correlation_id,
		 causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		event.EventID, event.EventType, event.AggregateID, event.Payload,
		event.CorrelationID, event.CausationID, event.OperationID,
		event.Tick, event.TimestampMs, event.SchemaVersion, event.CompensationType,
	)
	if err != nil {
		return fmt.Errorf("eventstore insert event: %w", err)
	}

	rows, err := res.RowsAffected()
	if err != nil {
		return fmt.Errorf("eventstore rows affected: %w", err)
	}

	// Only insert outbox if event was actually inserted (not a duplicate).
	if rows > 0 {
		if _, err := tx.Exec(`INSERT INTO outbox
			(event_id, topic, payload, status, created_at)
			VALUES (?, ?, ?, 'pending', ?)`,
			event.EventID, topic, event.Payload, now,
		); err != nil {
			return fmt.Errorf("eventstore insert outbox: %w", err)
		}
	}

	return tx.Commit()
}

// EventCount returns the number of events in the store.
func (s *Store) EventCount() (int64, error) {
	var count int64
	err := s.db.QueryRow("SELECT COUNT(*) FROM events").Scan(&count)
	return count, err
}

// PendingOutboxCount returns the number of pending outbox entries.
func (s *Store) PendingOutboxCount() (int64, error) {
	var count int64
	err := s.db.QueryRow("SELECT COUNT(*) FROM outbox WHERE status = 'pending'").Scan(&count)
	return count, err
}

// GetEventByOperationID looks up a single event by operation_id.
// Returns nil if not found.
func (s *Store) GetEventByOperationID(opID string) (*DomainEvent, error) {
	row := s.db.QueryRow(`SELECT event_id, event_type, aggregate_id, payload,
		correlation_id, causation_id, operation_id, tick, timestamp_ms,
		schema_version, compensation_type
		FROM events WHERE operation_id = ?`, opID)

	var e DomainEvent
	var causation sql.NullString
	err := row.Scan(&e.EventID, &e.EventType, &e.AggregateID, &e.Payload,
		&e.CorrelationID, &causation, &e.OperationID, &e.Tick, &e.TimestampMs,
		&e.SchemaVersion, &e.CompensationType)
	if err == sql.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	if causation.Valid {
		e.CausationID = causation.String
	}
	return &e, nil
}

// Close closes the underlying database connection.
func (s *Store) Close() error {
	return s.db.Close()
}

// GenerateUUID returns a new random UUIDv4 string.
func GenerateUUID() string {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic("crypto/rand failed: " + err.Error())
	}
	b[6] = (b[6] & 0x0f) | 0x40 // version 4
	b[8] = (b[8] & 0x3f) | 0x80 // variant 2
	return fmt.Sprintf("%s-%s-%s-%s-%s",
		hex.EncodeToString(b[0:4]),
		hex.EncodeToString(b[4:6]),
		hex.EncodeToString(b[6:8]),
		hex.EncodeToString(b[8:10]),
		hex.EncodeToString(b[10:16]),
	)
}
