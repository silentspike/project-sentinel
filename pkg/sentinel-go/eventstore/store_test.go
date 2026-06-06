package eventstore

import (
	"database/sql"
	"os"
	"path/filepath"
	"sync"
	"testing"

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
	if err := store.AppendWithOutbox(event, "sentinel/cortex/events/AGENT-01"); err != nil {
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
	if err := store.AppendWithOutbox(event1, "test/topic"); err != nil {
		t.Fatalf("first append: %v", err)
	}

	// Second append with same operation_id but different event_id
	event2 := makeEvent("op-idempotent-001")
	if err := store.AppendWithOutbox(event2, "test/topic"); err != nil {
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
			if err := store.AppendWithOutbox(event, "test/concurrent"); err != nil {
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
