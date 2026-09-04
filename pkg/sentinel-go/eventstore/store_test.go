package eventstore

import (
	"database/sql"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	_ "modernc.org/sqlite"
)

func tempDB(t *testing.T) (*Store, string) {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "test.db")
	store, err := Open(path)
	if err != nil {
		t.Fatalf("Open(%q): %v", path, err)
	}
	t.Cleanup(func() { _ = store.Close() })
	return store, path
}

func migrateEventContractForTest(t *testing.T, path string) {
	t.Helper()
	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatalf("open migration fixture: %v", err)
	}
	defer func() { _ = db.Close() }()
	migration, err := os.ReadFile(filepath.Join(
		"..", "..", "..", "crates", "sentinel-limbo", "migrations", "event-store",
		"0001-event-envelope-v2.sql",
	))
	if err != nil {
		t.Fatalf("read canonical event migration: %v", err)
	}
	if _, err := db.Exec(`CREATE TABLE event_schema_migrations (
		version INTEGER PRIMARY KEY,
		name TEXT NOT NULL UNIQUE,
		sha256 TEXT NOT NULL,
		applied_at_ms INTEGER NOT NULL
	)`); err != nil {
		t.Fatalf("create migration ledger: %v", err)
	}
	if _, err := db.Exec(string(migration)); err != nil {
		t.Fatalf("apply canonical event migration: %v", err)
	}
	if _, err := db.Exec(
		`INSERT INTO event_schema_migrations(version, name, sha256, applied_at_ms)
		 VALUES (?, ?, ?, ?)`,
		eventContractSchemaVersion,
		eventContractMigrationName,
		eventContractMigrationSHA,
		time.Now().UnixMilli(),
	); err != nil {
		t.Fatalf("record canonical event migration: %v", err)
	}
}

func makeEvent(opID string) DomainEvent {
	return DomainEvent{
		EventID:          GenerateUUID(),
		EventType:        "agent_move",
		AggregateID:      "AGENT-01",
		Payload:          `{"target":"kueche","emotion":"happy"}`,
		CorrelationID:    "req-001",
		OperationID:      opID,
		Tick:             42,
		TimestampMs:      1707900000000,
		SchemaVersion:    1,
		CompensationType: "none",
	}
}

func TestOpenAndClose(t *testing.T) {
	store, path := tempDB(t)

	// DB file must exist
	if _, err := os.Stat(path); err != nil {
		t.Fatalf("DB file not created: %v", err)
	}

	// Tables must exist (query should not error)
	count, err := store.EventCount()
	if err != nil {
		t.Fatalf("EventCount: %v", err)
	}
	if count != 0 {
		t.Errorf("expected 0 events, got %d", count)
	}

	pending, err := store.PendingOutboxCount()
	if err != nil {
		t.Fatalf("PendingOutboxCount: %v", err)
	}
	if pending != 0 {
		t.Errorf("expected 0 pending outbox, got %d", pending)
	}

	var busyTimeout int
	if err := store.db.QueryRow("PRAGMA busy_timeout").Scan(&busyTimeout); err != nil {
		t.Fatalf("read busy_timeout: %v", err)
	}
	if busyTimeout != sqliteBusyTimeoutMillis {
		t.Fatalf("busy_timeout=%d, want %d", busyTimeout, sqliteBusyTimeoutMillis)
	}
}

func TestOpenCompatibleVerifiesRustMigrationAndDisablesLegacyAppend(t *testing.T) {
	legacy, path := tempDB(t)
	if err := legacy.Close(); err != nil {
		t.Fatalf("close legacy schema authority: %v", err)
	}
	migrateEventContractForTest(t, path)

	store, err := OpenCompatible(path)
	if err != nil {
		t.Fatalf("OpenCompatible(%q): %v", path, err)
	}
	defer func() { _ = store.Close() }()
	status, err := store.EventContractSchemaStatus()
	if err != nil {
		t.Fatalf("EventContractSchemaStatus: %v", err)
	}
	if status.SchemaVersion != eventContractSchemaVersion ||
		status.MigrationSHA256 != eventContractMigrationSHA ||
		status.EventTruthGeneration != 1 || status.NextGlobalPosition != 1 {
		t.Fatalf("unexpected schema status: %+v", status)
	}
	if err := store.appendWithOutbox(makeEvent("forbidden"), "events.agent"); !errors.Is(err, ErrLegacyAppendDisabled) {
		t.Fatalf("AppendWithOutbox error = %v, want %v", err, ErrLegacyAppendDisabled)
	}
}

func TestOpenLegacyCompatibleAllowsOnlyTheBoundProducer(t *testing.T) {
	legacy, path := tempDB(t)
	if err := legacy.Close(); err != nil {
		t.Fatalf("close legacy schema authority: %v", err)
	}
	migrateEventContractForTest(t, path)

	store, err := OpenLegacyCompatible(path, LegacyProducerCortexAudit)
	if err != nil {
		t.Fatalf("OpenLegacyCompatible(%q): %v", path, err)
	}
	defer func() { _ = store.Close() }()
	if err := store.LegacyAppendGateway(LegacyProducerCortexAudit).
		AppendWithOutbox(makeEvent("cortex-compatible"), "events.audit"); err != nil {
		t.Fatalf("authorized compatibility append: %v", err)
	}
	if err := store.LegacyAppendGateway(LegacyProducerBenchmark).
		AppendWithOutbox(makeEvent("wrong-producer"), "events.audit"); err == nil {
		t.Fatal("store accepted a legacy producer other than its bound producer")
	}
}

func TestOpenCompatibleRejectsTamperedMigrationDigest(t *testing.T) {
	legacy, path := tempDB(t)
	if err := legacy.Close(); err != nil {
		t.Fatalf("close legacy schema authority: %v", err)
	}
	migrateEventContractForTest(t, path)
	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatalf("open tamper fixture: %v", err)
	}
	if _, err := db.Exec(`UPDATE event_schema_migrations SET sha256 = ? WHERE version = ?`, strings.Repeat("0", 64), eventContractSchemaVersion); err != nil {
		t.Fatalf("tamper migration digest: %v", err)
	}
	_ = db.Close()
	if _, err := OpenCompatible(path); err == nil {
		t.Fatal("OpenCompatible accepted a tampered migration digest")
	}
}

func TestOpenCompatibleDoesNotCreateMissingStore(t *testing.T) {
	path := filepath.Join(t.TempDir(), "missing.db")
	if _, err := OpenCompatible(path); err == nil {
		t.Fatal("OpenCompatible created or accepted a missing store")
	}
	if _, err := os.Stat(path); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("missing compatible store was materialized: %v", err)
	}
}

func TestOpenCompatibleRejectsCallerControlledSQLiteURI(t *testing.T) {
	if _, err := OpenCompatible("file::memory:?cache=shared&_pragma=journal_mode(WAL)"); err == nil {
		t.Fatal("OpenCompatible accepted a caller-controlled SQLite URI")
	}
}

func TestOpenCompatibleRejectsPartialV2Schema(t *testing.T) {
	legacy, path := tempDB(t)
	if err := legacy.Close(); err != nil {
		t.Fatalf("close legacy schema authority: %v", err)
	}
	migrateEventContractForTest(t, path)
	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatalf("open partial schema fixture: %v", err)
	}
	if _, err := db.Exec(`ALTER TABLE events_v2 DROP COLUMN producer`); err != nil {
		_ = db.Close()
		t.Fatalf("remove required V2 column: %v", err)
	}
	_ = db.Close()
	if _, err := OpenCompatible(path); err == nil {
		t.Fatal("OpenCompatible accepted a partial V2 schema")
	}
}

func TestOpenCompatibleDoesNotChangePersistentJournalMode(t *testing.T) {
	legacy, path := tempDB(t)
	if err := legacy.Close(); err != nil {
		t.Fatalf("close legacy schema authority: %v", err)
	}
	migrateEventContractForTest(t, path)
	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatalf("open journal mode fixture: %v", err)
	}
	var mode string
	if err := db.QueryRow(`PRAGMA journal_mode = DELETE`).Scan(&mode); err != nil {
		_ = db.Close()
		t.Fatalf("set journal mode: %v", err)
	}
	if !strings.EqualFold(mode, "delete") {
		_ = db.Close()
		t.Fatalf("journal mode=%q, want delete", mode)
	}
	_ = db.Close()

	if _, err := OpenCompatible(path); err == nil {
		t.Fatal("OpenCompatible accepted a non-WAL store")
	}
	db, err = sql.Open("sqlite", path)
	if err != nil {
		t.Fatalf("reopen journal mode fixture: %v", err)
	}
	defer func() { _ = db.Close() }()
	if err := db.QueryRow(`PRAGMA journal_mode`).Scan(&mode); err != nil {
		t.Fatalf("read journal mode: %v", err)
	}
	if !strings.EqualFold(mode, "delete") {
		t.Fatalf("compatible open changed journal mode to %q", mode)
	}
}

func TestAppendWaitsForConcurrentWriter(t *testing.T) {
	store, path := tempDB(t)

	locker, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatalf("open lock holder: %v", err)
	}
	locker.SetMaxOpenConns(1)
	t.Cleanup(func() { _ = locker.Close() })

	tx, err := locker.Begin()
	if err != nil {
		t.Fatalf("begin lock holder: %v", err)
	}
	if _, err := tx.Exec(`INSERT INTO events
		(event_id, event_type, aggregate_id, payload, correlation_id,
		 causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type)
		VALUES ('lock-event', 'lock', 'lock', '{}', 'lock', '', 'lock-operation', 0, 1, 1, 'none')`); err != nil {
		_ = tx.Rollback()
		t.Fatalf("acquire writer lock: %v", err)
	}

	released := make(chan error, 1)
	go func() {
		time.Sleep(100 * time.Millisecond)
		released <- tx.Rollback()
	}()

	if err := store.appendWithOutbox(makeEvent("busy-timeout"), "events.agent"); err != nil {
		t.Fatalf("append while a concurrent writer briefly holds the database: %v", err)
	}
	if err := <-released; err != nil && err != sql.ErrTxDone {
		t.Fatalf("release writer lock: %v", err)
	}
}

func TestSchemaIndexes(t *testing.T) {
	store, _ := tempDB(t)

	for _, name := range []string{"idx_events_event_id", "idx_outbox_event_id"} {
		var count int
		if err := store.db.QueryRow(
			`SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = ?`,
			name,
		).Scan(&count); err != nil {
			t.Fatalf("query index %s: %v", name, err)
		}
		if count != 1 {
			t.Fatalf("expected index %s to exist, found %d", name, count)
		}
	}

	for _, name := range []string{"retry_count", "last_error"} {
		ok, err := tableHasColumn(store.db, "outbox", name)
		if err != nil {
			t.Fatalf("query outbox column %s: %v", name, err)
		}
		if !ok {
			t.Fatalf("expected outbox column %s to exist", name)
		}
	}
}

func TestOpenMigratesLegacyOutboxColumns(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "legacy.db")

	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatalf("open legacy db: %v", err)
	}
	if _, err := db.Exec(`CREATE TABLE events (
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
	);
	CREATE TABLE outbox (
		id INTEGER PRIMARY KEY AUTOINCREMENT,
		event_id TEXT NOT NULL REFERENCES events(event_id),
		topic TEXT NOT NULL,
		payload TEXT NOT NULL,
		status TEXT NOT NULL DEFAULT 'pending',
		created_at INTEGER NOT NULL,
		published_at INTEGER
	);`); err != nil {
		t.Fatalf("create legacy schema: %v", err)
	}
	if err := db.Close(); err != nil {
		t.Fatalf("close legacy db: %v", err)
	}

	store, err := Open(path)
	if err != nil {
		t.Fatalf("Open legacy db: %v", err)
	}
	defer func() { _ = store.Close() }()

	for _, name := range []string{"retry_count", "last_error"} {
		ok, err := tableHasColumn(store.db, "outbox", name)
		if err != nil {
			t.Fatalf("query migrated column %s: %v", name, err)
		}
		if !ok {
			t.Fatalf("expected migrated outbox column %s to exist", name)
		}
	}
}

func TestAppendWithOutbox(t *testing.T) {
	store, _ := tempDB(t)

	event := makeEvent("op-roundtrip-001")
	if err := store.appendWithOutbox(event, "sentinel/cortex/events/AGENT-01"); err != nil {
		t.Fatalf("AppendWithOutbox: %v", err)
	}

	// Verify event persisted
	count, _ := store.EventCount()
	if count != 1 {
		t.Errorf("expected 1 event, got %d", count)
	}

	// Verify outbox entry
	pending, _ := store.PendingOutboxCount()
	if pending != 1 {
		t.Errorf("expected 1 pending outbox, got %d", pending)
	}

	// Verify roundtrip via operation_id lookup
	got, err := store.GetEventByOperationID("op-roundtrip-001")
	if err != nil {
		t.Fatalf("GetEventByOperationID: %v", err)
	}
	if got == nil {
		t.Fatal("expected event, got nil")
	}
	if got.EventType != "agent_move" {
		t.Errorf("expected event_type=agent_move, got %q", got.EventType)
	}
	if got.AggregateID != "AGENT-01" {
		t.Errorf("expected aggregate_id=AGENT-01, got %q", got.AggregateID)
	}
	if got.Payload != `{"target":"kueche","emotion":"happy"}` {
		t.Errorf("unexpected payload: %q", got.Payload)
	}
}

func TestIdempotency(t *testing.T) {
	store, _ := tempDB(t)

	event1 := makeEvent("op-idempotent-001")
	if err := store.appendWithOutbox(event1, "test/topic"); err != nil {
		t.Fatalf("first append: %v", err)
	}

	// Second append with same operation_id but different event_id
	event2 := makeEvent("op-idempotent-001")
	if err := store.appendWithOutbox(event2, "test/topic"); err != nil {
		t.Fatalf("second append: %v", err)
	}

	// Must still be exactly 1 event (INSERT OR IGNORE)
	count, _ := store.EventCount()
	if count != 1 {
		t.Errorf("expected 1 event after duplicate op_id, got %d", count)
	}

	// Must still be exactly 1 outbox entry
	pending, _ := store.PendingOutboxCount()
	if pending != 1 {
		t.Errorf("expected 1 outbox after duplicate op_id, got %d", pending)
	}
}

func TestConcurrentWrites(t *testing.T) {
	store, _ := tempDB(t)

	const n = 100
	var wg sync.WaitGroup
	errs := make(chan error, n)

	for i := range n {
		wg.Add(1)
		go func(idx int) {
			defer wg.Done()
			event := DomainEvent{
				EventID:          GenerateUUID(),
				EventType:        "agent_chat",
				AggregateID:      "AGENT-01",
				Payload:          `{"msg":"concurrent"}`,
				CorrelationID:    "req-concurrent",
				OperationID:      GenerateUUID(), // unique per goroutine
				Tick:             int64(idx),
				SchemaVersion:    1,
				CompensationType: "none",
			}
			if err := store.appendWithOutbox(event, "test/concurrent"); err != nil {
				errs <- err
			}
		}(i)
	}

	wg.Wait()
	close(errs)

	for err := range errs {
		t.Errorf("concurrent write error: %v", err)
	}

	count, _ := store.EventCount()
	if count != n {
		t.Errorf("expected %d events, got %d", n, count)
	}
}

func TestOutboxPublishedCASBindsPendingRowEventAndOperation(t *testing.T) {
	store, _ := tempDB(t)
	event := makeEvent("op-cas-001")
	if err := store.appendWithOutbox(event, "test/topic"); err != nil {
		t.Fatalf("AppendWithOutbox: %v", err)
	}
	entries, err := store.GetOutboxBatch(10)
	if err != nil || len(entries) != 1 {
		t.Fatalf("GetOutboxBatch len=%d err=%v, want one", len(entries), err)
	}
	entry := entries[0]

	for name, mutate := range map[string]func(*OutboxPublishEntry){
		"wrong id":        func(value *OutboxPublishEntry) { value.OutboxID++ },
		"wrong event":     func(value *OutboxPublishEntry) { value.EventID = "event-other" },
		"wrong operation": func(value *OutboxPublishEntry) { value.OperationID = "operation-other" },
	} {
		t.Run(name, func(t *testing.T) {
			mismatched := entry
			mutate(&mismatched)
			if err := store.MarkPublishedCAS(mismatched.OutboxID, mismatched.EventID, mismatched.OperationID); err == nil {
				t.Fatal("mismatched CAS unexpectedly succeeded")
			}
			counts, countErr := store.OutboxCounts()
			if countErr != nil || counts.Pending != 1 {
				t.Fatalf("pending=%d err=%v, want unchanged pending row", counts.Pending, countErr)
			}
		})
	}

	if err := store.MarkPublishedCAS(entry.OutboxID, entry.EventID, entry.OperationID); err != nil {
		t.Fatalf("exact CAS: %v", err)
	}
	if err := store.MarkPublishedCAS(entry.OutboxID, entry.EventID, entry.OperationID); err == nil {
		t.Fatal("duplicate CAS unexpectedly succeeded")
	}
	counts, err := store.OutboxCounts()
	if err != nil || counts != (OutboxStatusCounts{}) {
		t.Fatalf("counts=%+v err=%v, want fully published", counts, err)
	}
}

func TestOutboxRetryAndFailureCASRemainExact(t *testing.T) {
	store, _ := tempDB(t)
	event := makeEvent("op-failure-cas-001")
	if err := store.appendWithOutbox(event, "test/topic"); err != nil {
		t.Fatalf("AppendWithOutbox: %v", err)
	}
	entries, err := store.GetOutboxBatch(1)
	if err != nil || len(entries) != 1 {
		t.Fatalf("GetOutboxBatch len=%d err=%v", len(entries), err)
	}
	entry := entries[0]
	if err := store.MarkRetryCAS(entry.OutboxID, entry.EventID, "wrong-operation", "publish_failed"); err == nil {
		t.Fatal("retry accepted wrong operation")
	}
	if err := store.MarkRetryCAS(entry.OutboxID, entry.EventID, entry.OperationID, "publish_failed"); err != nil {
		t.Fatalf("exact retry: %v", err)
	}
	entries, err = store.GetOutboxBatch(1)
	if err != nil || entries[0].RetryCount != 1 {
		t.Fatalf("retry_count=%d err=%v, want 1", entries[0].RetryCount, err)
	}
	if err := store.MarkFailedCAS(entry.OutboxID, "wrong-event", entry.OperationID, "publish_failed"); err == nil {
		t.Fatal("failure accepted wrong event")
	}
	if err := store.MarkFailedCAS(entry.OutboxID, entry.EventID, entry.OperationID, "publish_failed"); err != nil {
		t.Fatalf("exact failure: %v", err)
	}
	counts, err := store.OutboxCounts()
	if err != nil || counts.Pending != 0 || counts.Failed != 1 || counts.NonPublished != 1 {
		t.Fatalf("counts=%+v err=%v, want terminal failed row", counts, err)
	}
}

func TestGenerateUUID(t *testing.T) {
	uuid := GenerateUUID()
	// UUIDv4 format: 8-4-4-4-12 = 36 chars
	if len(uuid) != 36 {
		t.Errorf("expected 36-char UUID, got %d: %q", len(uuid), uuid)
	}
	// Must be unique
	other := GenerateUUID()
	if uuid == other {
		t.Errorf("two UUIDs should differ: %q == %q", uuid, other)
	}
}
