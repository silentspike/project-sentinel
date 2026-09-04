// Package eventstore provides a Go-native SQLite event store compatible with
// sentinel-limbo's schema. It supports atomic Event+Outbox writes for the
// transactional outbox pattern (Command→Event Mapping, Issue #13 AC-5).
package eventstore

import (
	"crypto/rand"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"errors"
	"fmt"
	"net/url"
	"path/filepath"
	"strings"
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
	ID          int64
	EventID     string
	Topic       string
	Payload     string
	Status      string
	CreatedAt   int64
	PublishedAt sql.NullInt64
	RetryCount  int
	LastError   sql.NullString
}

// Store wraps a SQLite database with the sentinel-limbo event store schema.
type Store struct {
	db                    *sql.DB
	allowLegacyAppend     bool
	allowedLegacyProducer LegacyProducer
}

var ErrLegacyAppendDisabled = errors.New("legacy event append is disabled for schema-compatible runtime clients")

// LegacyProducer classifies the remaining immutable V1 compatibility writers.
// New authority producers use the Rust-owned EventAppendGateway V2 boundary.
type LegacyProducer string

const (
	LegacyProducerCortexAudit LegacyProducer = "cortex_audit"
	LegacyProducerTestHarness LegacyProducer = "test_harness"
	LegacyProducerBenchmark   LegacyProducer = "benchmark_harness"
)

type LegacyAppendGateway struct {
	store    *Store
	producer LegacyProducer
}

func (s *Store) LegacyAppendGateway(producer LegacyProducer) LegacyAppendGateway {
	return LegacyAppendGateway{store: s, producer: producer}
}

func (g LegacyAppendGateway) AppendWithOutbox(event DomainEvent, topic string) error {
	if g.producer == "" || !g.store.allowLegacyAppend {
		return errors.New("legacy event append requires a classified producer")
	}
	if g.store.allowedLegacyProducer != "" && g.store.allowedLegacyProducer != g.producer {
		return fmt.Errorf("legacy event producer %q is not authorized by this store handle", g.producer)
	}
	return g.store.appendWithOutbox(event, topic)
}

const (
	eventContractSchemaVersion = 2
	eventContractMigrationName = "event-envelope-v2"
	eventContractMigrationSHA  = "472b60a6cd218422b946f03e01e50d3566b563759899a027c02c047519097e86"
	eventContractSchemaSHA     = "d7e51ea21faf194fa85b894534f816cfb6b5ca530be5d73cfabeac3ae22c88b4"
)

var requiredEventContractColumns = map[string][]string{
	"event_truth_metadata":  {"singleton_id", "schema_version", "event_truth_generation", "next_global_position"},
	"event_stream_heads_v2": {"stream_namespace", "stream_revision"},
	"events_v2": {
		"event_id", "event_truth_generation", "stream_namespace", "stream_revision",
		"global_position", "event_type", "schema_version", "payload_codec",
		"payload_digest", "payload", "causal_context_json", "causal_context_digest",
		"producer", "owner_term_json", "tick", "appended_at_ms", "durability",
		"canonical_request_digest", "append_receipt_digest", "sealed_envelope_digest",
	},
	"event_operations_v2": {
		"authority_scope_digest", "operation_id", "canonical_request_digest", "event_id", "outcome_digest",
	},
	"delivery_intents_v2": {
		"intent_id", "event_id", "authority_scope_digest", "causal_context_digest",
		"topic", "payload_digest", "status",
	},
	"local_effect_reservations_v2": {
		"effect_id", "event_id", "authority_scope_digest", "causal_context_digest",
		"effect_kind", "request_digest", "status",
	},
}

// EventContractSchemaStatus is the compatibility receipt read from the
// Rust-owned migration ledger. Go processes verify it but never create it.
type EventContractSchemaStatus struct {
	SchemaVersion        int
	MigrationName        string
	MigrationSHA256      string
	EventTruthGeneration uint64
	NextGlobalPosition   uint64
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
CREATE INDEX IF NOT EXISTS idx_events_causation ON events(causation_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_events_event_id ON events(event_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_events_operation ON events(operation_id)
`

const createOutbox = `CREATE TABLE IF NOT EXISTS outbox (
	id INTEGER PRIMARY KEY AUTOINCREMENT,
	event_id TEXT NOT NULL REFERENCES events(event_id),
	topic TEXT NOT NULL,
	payload TEXT NOT NULL,
	status TEXT NOT NULL DEFAULT 'pending',
	created_at INTEGER NOT NULL,
	published_at INTEGER,
	retry_count INTEGER NOT NULL DEFAULT 0,
	last_error TEXT
)`

const createOutboxIndex = `
CREATE INDEX IF NOT EXISTS idx_outbox_pending ON outbox(status) WHERE status = 'pending';
CREATE INDEX IF NOT EXISTS idx_outbox_event_id ON outbox(event_id)
`

const sqliteBusyTimeoutMillis = 5000

const pragmas = `
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA mmap_size = 268435456;
PRAGMA page_size = 8192
`

// Open creates a SQLite event store for tests and offline maintenance.
// Production processes must use OpenCompatible or OpenLegacyCompatible; the
// repository boundary gate reserves live schema authority for the Rust daemon.
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
	if _, err := db.Exec(fmt.Sprintf("PRAGMA busy_timeout = %d", sqliteBusyTimeoutMillis)); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("eventstore busy timeout: %w", err)
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
	if err := ensureOutboxColumns(db); err != nil {
		_ = db.Close()
		return nil, err
	}
	if _, err := db.Exec(createOutboxIndex); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("eventstore create outbox index: %w", err)
	}

	return &Store{db: db, allowLegacyAppend: true}, nil
}

// OpenCompatible opens an existing Rust-migrated event store for delivery and
// read-model work. It creates no table, index, or migration and disables the
// legacy event append API on the returned handle.
func OpenCompatible(path string) (*Store, error) {
	return openCompatible(path, "")
}

// OpenLegacyCompatible opens an existing Rust-migrated store and grants exactly
// one classified V1 producer write access without granting schema authority.
func OpenLegacyCompatible(path string, producer LegacyProducer) (*Store, error) {
	if producer == "" || producer == LegacyProducerTestHarness || producer == LegacyProducerBenchmark {
		return nil, fmt.Errorf("invalid production legacy producer %q", producer)
	}
	return openCompatible(path, producer)
}

func openCompatible(path string, legacyProducer LegacyProducer) (*Store, error) {
	dsn, err := compatibleReadWriteDSN(path)
	if err != nil {
		return nil, err
	}
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, fmt.Errorf("eventstore compatible open: %w", err)
	}
	db.SetMaxOpenConns(1)
	if _, err := db.Exec(`
		PRAGMA synchronous = FULL;
		PRAGMA foreign_keys = ON;
		PRAGMA mmap_size = 268435456;
		PRAGMA busy_timeout = 5000`); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("eventstore compatible pragmas: %w", err)
	}
	var journalMode string
	var synchronous, foreignKeys int
	if err := db.QueryRow(`PRAGMA journal_mode`).Scan(&journalMode); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("eventstore journal mode readback: %w", err)
	}
	if err := db.QueryRow(`PRAGMA synchronous`).Scan(&synchronous); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("eventstore synchronous readback: %w", err)
	}
	if err := db.QueryRow(`PRAGMA foreign_keys`).Scan(&foreignKeys); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("eventstore foreign key readback: %w", err)
	}
	if !strings.EqualFold(journalMode, "wal") || synchronous != 2 || foreignKeys != 1 {
		_ = db.Close()
		return nil, fmt.Errorf(
			"eventstore incompatible durability pragmas: journal_mode=%s synchronous=%d foreign_keys=%d",
			journalMode,
			synchronous,
			foreignKeys,
		)
	}
	store := &Store{
		db:                    db,
		allowLegacyAppend:     legacyProducer != "",
		allowedLegacyProducer: legacyProducer,
	}
	if _, err := store.EventContractSchemaStatus(); err != nil {
		_ = db.Close()
		return nil, err
	}
	if err := verifyEventContractSchema(db); err != nil {
		_ = db.Close()
		return nil, err
	}
	for _, table := range []string{"events", "outbox"} {
		var exists bool
		if err := db.QueryRow(
			`SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?)`,
			table,
		).Scan(&exists); err != nil {
			_ = db.Close()
			return nil, fmt.Errorf("eventstore verify table %s: %w", table, err)
		}
		if !exists {
			_ = db.Close()
			return nil, fmt.Errorf("eventstore required compatibility table %s is missing", table)
		}
	}
	return store, nil
}

func compatibleReadWriteDSN(path string) (string, error) {
	if path == "" || path == ":memory:" {
		return "", errors.New("eventstore compatible open requires an existing file-backed database")
	}
	if strings.HasPrefix(path, "file:") {
		return "", errors.New("eventstore compatible open accepts filesystem paths, not caller-controlled SQLite URIs")
	}
	absolute, err := filepath.Abs(path)
	if err != nil {
		return "", fmt.Errorf("eventstore compatible path: %w", err)
	}
	parsed := &url.URL{Scheme: "file", Path: absolute}
	query := parsed.Query()
	query.Set("mode", "rw")
	parsed.RawQuery = query.Encode()
	return parsed.String(), nil
}

func verifyEventContractSchema(db *sql.DB) error {
	fingerprint, err := eventContractSchemaFingerprint(db)
	if err != nil {
		return err
	}
	if fingerprint != eventContractSchemaSHA {
		return fmt.Errorf(
			"eventstore event contract schema object fingerprint mismatch: expected %s, found %s",
			eventContractSchemaSHA,
			fingerprint,
		)
	}
	for table, requiredColumns := range requiredEventContractColumns {
		actual := make(map[string]struct{})
		rows, err := db.Query(fmt.Sprintf(`PRAGMA table_info(%s)`, table))
		if err != nil {
			return fmt.Errorf("eventstore verify table %s: %w", table, err)
		}
		for rows.Next() {
			var cid, notNull, primaryKey int
			var name, columnType string
			var defaultValue any
			if err := rows.Scan(&cid, &name, &columnType, &notNull, &defaultValue, &primaryKey); err != nil {
				_ = rows.Close()
				return fmt.Errorf("eventstore read table %s: %w", table, err)
			}
			actual[name] = struct{}{}
		}
		if err := rows.Err(); err != nil {
			_ = rows.Close()
			return fmt.Errorf("eventstore read table %s: %w", table, err)
		}
		if err := rows.Close(); err != nil {
			return fmt.Errorf("eventstore close table %s readback: %w", table, err)
		}
		for _, column := range requiredColumns {
			if _, ok := actual[column]; !ok {
				return fmt.Errorf("eventstore contract table %s is missing column %s", table, column)
			}
		}
	}
	return nil
}

func eventContractSchemaFingerprint(db *sql.DB) (string, error) {
	rows, err := db.Query(`
		SELECT type, name, tbl_name, COALESCE(sql, '')
		FROM sqlite_schema
		WHERE tbl_name IN (
			'event_schema_migrations',
			'event_truth_metadata',
			'event_stream_heads_v2',
			'events_v2',
			'event_operations_v2',
			'delivery_intents_v2',
			'local_effect_reservations_v2'
		)
		ORDER BY type, name, tbl_name`)
	if err != nil {
		return "", fmt.Errorf("eventstore query event contract schema objects: %w", err)
	}
	defer func() { _ = rows.Close() }()

	hash := sha256.New()
	for rows.Next() {
		var objectType, name, table, statement string
		if err := rows.Scan(&objectType, &name, &table, &statement); err != nil {
			return "", fmt.Errorf("eventstore read event contract schema object: %w", err)
		}
		for _, value := range []string{objectType, name, table, statement} {
			_, _ = fmt.Fprintf(hash, "%d:", len(value))
			_, _ = hash.Write([]byte(value))
			_, _ = hash.Write([]byte{'\n'})
		}
	}
	if err := rows.Err(); err != nil {
		return "", fmt.Errorf("eventstore read event contract schema objects: %w", err)
	}
	return hex.EncodeToString(hash.Sum(nil)), nil
}

// EventContractSchemaStatus verifies the immutable migration identity and
// returns the current generation counters without mutating schema.
func (s *Store) EventContractSchemaStatus() (EventContractSchemaStatus, error) {
	var status EventContractSchemaStatus
	err := s.db.QueryRow(
		`SELECT m.version, m.name, m.sha256,
		        t.event_truth_generation, t.next_global_position
		 FROM event_schema_migrations m
		 JOIN event_truth_metadata t ON t.singleton_id = 1
		 WHERE m.version = ?`,
		eventContractSchemaVersion,
	).Scan(
		&status.SchemaVersion,
		&status.MigrationName,
		&status.MigrationSHA256,
		&status.EventTruthGeneration,
		&status.NextGlobalPosition,
	)
	if err != nil {
		return EventContractSchemaStatus{}, fmt.Errorf("eventstore schema compatibility: %w", err)
	}
	if status.MigrationName != eventContractMigrationName || status.MigrationSHA256 != eventContractMigrationSHA {
		return EventContractSchemaStatus{}, fmt.Errorf("eventstore migration checksum mismatch for version %d", eventContractSchemaVersion)
	}
	if status.EventTruthGeneration == 0 || status.NextGlobalPosition == 0 {
		return EventContractSchemaStatus{}, fmt.Errorf("eventstore invalid truth-generation counters")
	}
	return status, nil
}

func ensureOutboxColumns(db *sql.DB) error {
	if ok, err := tableHasColumn(db, "outbox", "retry_count"); err != nil {
		return err
	} else if !ok {
		if _, err := db.Exec(`ALTER TABLE outbox ADD COLUMN retry_count INTEGER NOT NULL DEFAULT 0`); err != nil {
			return fmt.Errorf("eventstore migrate outbox retry_count: %w", err)
		}
	}
	if ok, err := tableHasColumn(db, "outbox", "last_error"); err != nil {
		return err
	} else if !ok {
		if _, err := db.Exec(`ALTER TABLE outbox ADD COLUMN last_error TEXT`); err != nil {
			return fmt.Errorf("eventstore migrate outbox last_error: %w", err)
		}
	}
	return nil
}

func tableHasColumn(db *sql.DB, table, column string) (bool, error) {
	rows, err := db.Query(fmt.Sprintf(`PRAGMA table_info(%s)`, table))
	if err != nil {
		return false, fmt.Errorf("eventstore table info %s: %w", table, err)
	}
	defer func() { _ = rows.Close() }()

	for rows.Next() {
		var cid int
		var name, typ string
		var notNull int
		var defaultValue any
		var pk int
		if err := rows.Scan(&cid, &name, &typ, &notNull, &defaultValue, &pk); err != nil {
			return false, fmt.Errorf("eventstore scan table info %s: %w", table, err)
		}
		if name == column {
			return true, nil
		}
	}
	return false, rows.Err()
}

// appendWithOutbox atomically inserts a DomainEvent and an outbox entry
// within a single transaction. Uses INSERT OR IGNORE for idempotency
// via the operation_id UNIQUE index.
func (s *Store) appendWithOutbox(event DomainEvent, topic string) error {
	if !s.allowLegacyAppend {
		return ErrLegacyAppendDisabled
	}
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

// GetEventsSince returns up to limit events with id > afterID, ordered by id ascending.
// Used by the NATS bridge to poll for new events.
func (s *Store) GetEventsSince(afterID int64, limit int) ([]DomainEvent, int64, error) {
	if limit <= 0 {
		limit = 100
	}
	rows, err := s.db.Query(`SELECT id, event_id, event_type, aggregate_id, payload,
		correlation_id, causation_id, operation_id, tick, timestamp_ms,
		schema_version, compensation_type
		FROM events WHERE id > ? ORDER BY id ASC LIMIT ?`, afterID, limit)
	if err != nil {
		return nil, afterID, fmt.Errorf("eventstore get events since: %w", err)
	}
	defer func() { _ = rows.Close() }()

	var events []DomainEvent
	maxID := afterID
	for rows.Next() {
		var rowID int64
		var e DomainEvent
		var causation sql.NullString
		if err := rows.Scan(&rowID, &e.EventID, &e.EventType, &e.AggregateID, &e.Payload,
			&e.CorrelationID, &causation, &e.OperationID, &e.Tick, &e.TimestampMs,
			&e.SchemaVersion, &e.CompensationType); err != nil {
			return nil, afterID, fmt.Errorf("eventstore scan row: %w", err)
		}
		if causation.Valid {
			e.CausationID = causation.String
		}
		events = append(events, e)
		if rowID > maxID {
			maxID = rowID
		}
	}
	if err := rows.Err(); err != nil {
		return nil, afterID, fmt.Errorf("eventstore rows iteration: %w", err)
	}
	return events, maxID, nil
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

// EnsureOutboxMigration adds retry_count and last_error columns if missing.
// Idempotent — safe to call on every startup.
func (s *Store) EnsureOutboxMigration() error {
	// SQLite ALTER TABLE ADD COLUMN is idempotent-safe: errors on duplicate column.
	// We ignore "duplicate column" errors.
	for _, stmt := range []string{
		"ALTER TABLE outbox ADD COLUMN retry_count INTEGER NOT NULL DEFAULT 0",
		"ALTER TABLE outbox ADD COLUMN last_error TEXT",
	} {
		if _, err := s.db.Exec(stmt); err != nil {
			// Ignore "duplicate column name" error (format varies by SQLite driver).
			if !strings.Contains(err.Error(), "duplicate column name") {
				return fmt.Errorf("outbox migration: %w", err)
			}
		}
	}
	return nil
}

// OutboxPublishEntry is an outbox entry enriched with event metadata for NATS publishing.
type OutboxPublishEntry struct {
	OutboxID      int64
	EventID       string
	EventType     string
	AggregateID   string
	OperationID   string
	CorrelationID string
	Tick          int64
	Topic         string
	Payload       string
	RetryCount    int
}

// OutboxStatusCounts is the public-safe readiness summary for the durable
// publication boundary. NonPublished includes pending, failed, and any
// unrecognized non-terminal status.
type OutboxStatusCounts struct {
	Pending      int64
	Failed       int64
	NonPublished int64
}

// GetOutboxBatch returns up to limit pending outbox entries joined with event metadata.
func (s *Store) GetOutboxBatch(limit int) ([]OutboxPublishEntry, error) {
	if limit <= 0 {
		limit = 100
	}
	rows, err := s.db.Query(`SELECT o.id, o.event_id, e.event_type, e.aggregate_id,
		e.operation_id, e.correlation_id, e.tick, o.topic, o.payload, o.retry_count
		FROM outbox o JOIN events e ON o.event_id = e.event_id
		WHERE o.status = 'pending' ORDER BY o.id ASC LIMIT ?`, limit)
	if err != nil {
		return nil, fmt.Errorf("outbox get batch: %w", err)
	}
	defer func() { _ = rows.Close() }()

	var entries []OutboxPublishEntry
	for rows.Next() {
		var e OutboxPublishEntry
		if err := rows.Scan(&e.OutboxID, &e.EventID, &e.EventType, &e.AggregateID,
			&e.OperationID, &e.CorrelationID, &e.Tick, &e.Topic, &e.Payload,
			&e.RetryCount); err != nil {
			return nil, fmt.Errorf("outbox scan row: %w", err)
		}
		entries = append(entries, e)
	}
	return entries, rows.Err()
}

// OutboxCounts returns the durable publication state used by bridge readiness.
func (s *Store) OutboxCounts() (OutboxStatusCounts, error) {
	var counts OutboxStatusCounts
	err := s.db.QueryRow(`SELECT
		COALESCE(SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END), 0),
		COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0),
		COALESCE(SUM(CASE WHEN status <> 'published' THEN 1 ELSE 0 END), 0)
		FROM outbox`).Scan(&counts.Pending, &counts.Failed, &counts.NonPublished)
	if err != nil {
		return OutboxStatusCounts{}, fmt.Errorf("outbox count states: %w", err)
	}
	return counts, nil
}

// MarkPublishedCAS acknowledges exactly the pending row and event operation
// that received a synchronous JetStream PubAck.
func (s *Store) MarkPublishedCAS(id int64, eventID, operationID string) error {
	return s.transitionOutboxCAS(
		id,
		eventID,
		operationID,
		`UPDATE outbox SET status = 'published', published_at = ?, last_error = NULL
		 WHERE id = ? AND event_id = ? AND status = 'pending'
		 AND EXISTS (
			SELECT 1 FROM events e
			WHERE e.event_id = outbox.event_id AND e.operation_id = ?
		 )`,
		time.Now().UnixMilli(),
	)
}

// MarkRetryCAS records a retry only while the exact event operation remains
// pending. The supplied reason must be a stable public-safe code.
func (s *Store) MarkRetryCAS(id int64, eventID, operationID, reason string) error {
	return s.transitionOutboxCAS(
		id,
		eventID,
		operationID,
		`UPDATE outbox SET retry_count = retry_count + 1, last_error = ?
		 WHERE id = ? AND event_id = ? AND status = 'pending'
		 AND EXISTS (
			SELECT 1 FROM events e
			WHERE e.event_id = outbox.event_id AND e.operation_id = ?
		 )`,
		reason,
	)
}

// MarkFailedCAS terminally fails only the exact pending event operation.
func (s *Store) MarkFailedCAS(id int64, eventID, operationID, reason string) error {
	return s.transitionOutboxCAS(
		id,
		eventID,
		operationID,
		`UPDATE outbox SET status = 'failed', retry_count = retry_count + 1, last_error = ?
		 WHERE id = ? AND event_id = ? AND status = 'pending'
		 AND EXISTS (
			SELECT 1 FROM events e
			WHERE e.event_id = outbox.event_id AND e.operation_id = ?
		 )`,
		reason,
	)
}

func (s *Store) transitionOutboxCAS(
	id int64,
	eventID string,
	operationID string,
	statement string,
	value any,
) error {
	result, err := s.db.Exec(statement, value, id, eventID, operationID)
	if err != nil {
		return fmt.Errorf("outbox transition id=%d: %w", id, err)
	}
	rows, err := result.RowsAffected()
	if err != nil {
		return fmt.Errorf("outbox transition rows id=%d: %w", id, err)
	}
	if rows != 1 {
		return fmt.Errorf("outbox transition rejected id=%d: expected 1 pending row, changed %d", id, rows)
	}
	return nil
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
