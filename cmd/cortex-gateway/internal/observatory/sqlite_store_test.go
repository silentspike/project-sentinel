package observatory

import (
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"
)

func tempDBPath(t *testing.T) string {
	t.Helper()
	return filepath.Join(t.TempDir(), "observatory_test.db")
}

func TestOpenSqliteStore(t *testing.T) {
	path := tempDBPath(t)
	store, err := OpenSqliteStore(path)
	if err != nil {
		t.Fatalf("OpenSqliteStore: %v", err)
	}
	defer func() { _ = store.Close() }()

	if store.Len() != 0 {
		t.Errorf("empty store Len = %d, want 0", store.Len())
	}

	// Verify DB file exists
	if _, err := os.Stat(path); os.IsNotExist(err) {
		t.Error("DB file was not created")
	}
}

func TestSqliteStoreAdd(t *testing.T) {
	path := tempDBPath(t)
	store, err := OpenSqliteStore(path)
	if err != nil {
		t.Fatalf("OpenSqliteStore: %v", err)
	}
	defer func() { _ = store.Close() }()

	rec := ObservationRecord{
		Timestamp: time.Now(),
		Shift:     1,
		Model:     "claude-sonnet",
		Agent:     "AGENT-01",
		Scenario:  "daily_routine",
		Metrics: MetricsSnapshot{
			InfoPropagation:        0.85,
			GroupPolarization:      0.15,
			CommunicationScore:     0.82,
			PersonalityConsistency: 0.91,
			ResponseCreativity:     0.67,
			EmotionalRange:         0.73,
		},
	}

	store.Add(rec)

	if store.Len() != 1 {
		t.Errorf("Len = %d, want 1", store.Len())
	}

	all := store.AllRecords()
	if len(all) != 1 {
		t.Fatalf("AllRecords len = %d, want 1", len(all))
	}
	if all[0].Model != "claude-sonnet" {
		t.Errorf("Model = %q, want %q", all[0].Model, "claude-sonnet")
	}
	if all[0].Metrics.InfoPropagation != 0.85 {
		t.Errorf("InfoPropagation = %f, want 0.85", all[0].Metrics.InfoPropagation)
	}
}

// TestSqliteStoreReload verifies AC-4: restart resilience.
func TestSqliteStoreReload(t *testing.T) {
	path := tempDBPath(t)

	// Phase 1: Open, add records, close
	store1, err := OpenSqliteStore(path)
	if err != nil {
		t.Fatalf("open 1: %v", err)
	}

	for i := 0; i < 5; i++ {
		store1.Add(ObservationRecord{
			Timestamp: time.Now(),
			Shift:     (i % 3) + 1,
			Model:     "test-model",
			Agent:     "AGENT-01",
			Scenario:  "daily_routine",
			Metrics: MetricsSnapshot{
				InfoPropagation:   float64(i) * 0.1,
				GroupPolarization: 0.2,
			},
		})
	}

	if store1.Len() != 5 {
		t.Fatalf("store1 Len = %d, want 5", store1.Len())
	}
	if err := store1.Close(); err != nil {
		t.Fatalf("close 1: %v", err)
	}

	// Phase 2: Reopen, verify records survived
	store2, err := OpenSqliteStore(path)
	if err != nil {
		t.Fatalf("open 2: %v", err)
	}
	defer func() { _ = store2.Close() }()

	if store2.Len() != 5 {
		t.Errorf("store2 Len after reload = %d, want 5", store2.Len())
	}

	all := store2.AllRecords()
	if len(all) != 5 {
		t.Fatalf("AllRecords after reload = %d, want 5", len(all))
	}
	if all[0].Model != "test-model" {
		t.Errorf("record[0].Model = %q, want %q", all[0].Model, "test-model")
	}
}

func TestSqliteStoreSubmitRun(t *testing.T) {
	path := tempDBPath(t)
	store, err := OpenSqliteStore(path)
	if err != nil {
		t.Fatalf("OpenSqliteStore: %v", err)
	}
	defer func() { _ = store.Close() }()

	runID := "test-run-001"
	configHash := "abc123"
	records := []ObservationRecord{
		{Timestamp: time.Now(), Shift: 1, Model: "claude-sonnet", Agent: "A1", Scenario: "daily_routine",
			Metrics: MetricsSnapshot{InfoPropagation: 0.8, CommunicationScore: 0.7}},
		{Timestamp: time.Now(), Shift: 2, Model: "llama-3.1-70b", Agent: "A1", Scenario: "daily_routine",
			Metrics: MetricsSnapshot{InfoPropagation: 0.6, CommunicationScore: 0.5}},
	}

	if err := store.SubmitRun(runID, configHash, records); err != nil {
		t.Fatalf("SubmitRun: %v", err)
	}

	if store.Len() != 2 {
		t.Errorf("Len = %d, want 2", store.Len())
	}

	// Verify run metadata
	runs, err := store.ListRuns()
	if err != nil {
		t.Fatalf("ListRuns: %v", err)
	}
	if len(runs) != 1 {
		t.Fatalf("ListRuns len = %d, want 1", len(runs))
	}
	if runs[0].RunID != runID {
		t.Errorf("RunID = %q, want %q", runs[0].RunID, runID)
	}
	if runs[0].ConfigHash != configHash {
		t.Errorf("ConfigHash = %q, want %q", runs[0].ConfigHash, configHash)
	}
	if runs[0].RecordCount != 2 {
		t.Errorf("RecordCount = %d, want 2", runs[0].RecordCount)
	}
	if runs[0].Status != "completed" {
		t.Errorf("Status = %q, want %q", runs[0].Status, "completed")
	}
}

func TestSqliteStoreGetRunRecords(t *testing.T) {
	path := tempDBPath(t)
	store, err := OpenSqliteStore(path)
	if err != nil {
		t.Fatalf("OpenSqliteStore: %v", err)
	}
	defer func() { _ = store.Close() }()

	records := []ObservationRecord{
		{Timestamp: time.Now(), Shift: 1, Model: "claude-sonnet", Agent: "A1", Scenario: "daily_routine",
			Metrics: MetricsSnapshot{InfoPropagation: 0.8}},
		{Timestamp: time.Now(), Shift: 2, Model: "llama-70b", Agent: "A2", Scenario: "crisis_response",
			Metrics: MetricsSnapshot{InfoPropagation: 0.6}},
		{Timestamp: time.Now(), Shift: 1, Model: "claude-sonnet", Agent: "A3", Scenario: "daily_routine",
			Metrics: MetricsSnapshot{InfoPropagation: 0.9}},
	}

	if err := store.SubmitRun("run-filter", "hash", records); err != nil {
		t.Fatalf("SubmitRun: %v", err)
	}

	// Get all records for the run
	all, err := store.GetRunRecords("run-filter", QueryFilter{})
	if err != nil {
		t.Fatalf("GetRunRecords: %v", err)
	}
	if len(all) != 3 {
		t.Errorf("all records = %d, want 3", len(all))
	}

	// Filter by shift
	shift1 := 1
	filtered, err := store.GetRunRecords("run-filter", QueryFilter{Shift: &shift1})
	if err != nil {
		t.Fatalf("GetRunRecords filtered: %v", err)
	}
	if len(filtered) != 2 {
		t.Errorf("shift=1 records = %d, want 2", len(filtered))
	}

	// Filter by model
	model := "llama-70b"
	byModel, err := store.GetRunRecords("run-filter", QueryFilter{Model: &model})
	if err != nil {
		t.Fatalf("GetRunRecords by model: %v", err)
	}
	if len(byModel) != 1 {
		t.Errorf("model=llama records = %d, want 1", len(byModel))
	}
}

func TestSqliteStoreQueryFilter(t *testing.T) {
	path := tempDBPath(t)
	store, err := OpenSqliteStore(path)
	if err != nil {
		t.Fatalf("OpenSqliteStore: %v", err)
	}
	defer func() { _ = store.Close() }()

	now := time.Now()
	store.Add(ObservationRecord{Timestamp: now, Shift: 1, Model: "claude", Agent: "A1", Scenario: "daily"})
	store.Add(ObservationRecord{Timestamp: now, Shift: 2, Model: "llama", Agent: "A2", Scenario: "crisis"})
	store.Add(ObservationRecord{Timestamp: now, Shift: 1, Model: "claude", Agent: "A3", Scenario: "daily"})

	shift1 := 1
	results := store.Query(QueryFilter{Shift: &shift1})
	if len(results) != 2 {
		t.Errorf("shift=1 query = %d, want 2", len(results))
	}

	model := "llama"
	results = store.Query(QueryFilter{Model: &model})
	if len(results) != 1 {
		t.Errorf("model=llama query = %d, want 1", len(results))
	}
}

func TestSqliteStoreConcurrent(t *testing.T) {
	path := tempDBPath(t)
	store, err := OpenSqliteStore(path)
	if err != nil {
		t.Fatalf("OpenSqliteStore: %v", err)
	}
	defer func() { _ = store.Close() }()

	var wg sync.WaitGroup
	const writers = 5
	const recsPerWriter = 20

	// Concurrent writers
	for w := 0; w < writers; w++ {
		wg.Add(1)
		go func(shift int) {
			defer wg.Done()
			for i := 0; i < recsPerWriter; i++ {
				store.Add(ObservationRecord{
					Timestamp: time.Now(),
					Shift:     (shift % 3) + 1,
					Model:     "concurrent-model",
					Agent:     "A1",
					Scenario:  "daily",
				})
			}
		}(w)
	}

	// Concurrent readers
	for r := 0; r < 3; r++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for i := 0; i < 10; i++ {
				_ = store.Len()
				_ = store.AllRecords()
			}
		}()
	}

	wg.Wait()

	total := store.Len()
	want := writers * recsPerWriter
	if total != want {
		t.Errorf("total records = %d, want %d", total, want)
	}
}

func TestSqliteStoreListRunsEmpty(t *testing.T) {
	path := tempDBPath(t)
	store, err := OpenSqliteStore(path)
	if err != nil {
		t.Fatalf("OpenSqliteStore: %v", err)
	}
	defer func() { _ = store.Close() }()

	runs, err := store.ListRuns()
	if err != nil {
		t.Fatalf("ListRuns: %v", err)
	}
	if len(runs) != 0 {
		t.Errorf("ListRuns empty = %d, want 0", len(runs))
	}
}
