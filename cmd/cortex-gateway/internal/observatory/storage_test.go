package observatory

import (
	"sync"
	"testing"
	"time"
)

func makeRecord(shift int, model, agent, scenario string, ts time.Time) ObservationRecord {
	return ObservationRecord{
		Timestamp: ts,
		Shift:     shift,
		Model:     model,
		Agent:     agent,
		Scenario:  scenario,
		Metrics: MetricsSnapshot{
			InfoPropagation:        0.5,
			GroupPolarization:      0.1,
			CommunicationScore:     0.7,
			PersonalityConsistency: 0.9,
			ResponseCreativity:     0.6,
			EmotionalRange:         0.5,
		},
	}
}

func TestNewObservationStore(t *testing.T) {
	store := NewObservationStore()
	if store.Len() != 0 {
		t.Errorf("new store should be empty, got %d records", store.Len())
	}
}

func TestStoreAddAndAllRecords(t *testing.T) {
	store := NewObservationStore()
	now := time.Now()

	store.Add(makeRecord(1, "claude-sonnet", "Agent-01", "daily_routine", now))
	store.Add(makeRecord(2, "llama-3.1-70b", "Agent-16", "meeting", now.Add(time.Hour)))

	records := store.AllRecords()
	if len(records) != 2 {
		t.Fatalf("expected 2 records, got %d", len(records))
	}
	if records[0].Agent != "Agent-01" {
		t.Errorf("first record agent = %s, want Agent-01", records[0].Agent)
	}
	if records[1].Agent != "Agent-16" {
		t.Errorf("second record agent = %s, want Agent-16", records[1].Agent)
	}
}

func TestStoreAllRecordsReturnsCopy(t *testing.T) {
	store := NewObservationStore()
	now := time.Now()
	store.Add(makeRecord(1, "claude-sonnet", "Agent-01", "daily_routine", now))

	records := store.AllRecords()
	records[0].Agent = "MODIFIED"

	original := store.AllRecords()
	if original[0].Agent == "MODIFIED" {
		t.Error("AllRecords should return a copy, not a reference to internal data")
	}
}

func TestStoreQueryByShift(t *testing.T) {
	store := NewObservationStore()
	now := time.Now()
	store.Add(makeRecord(1, "claude-sonnet", "Agent-01", "daily_routine", now))
	store.Add(makeRecord(2, "claude-sonnet", "Agent-16", "daily_routine", now))
	store.Add(makeRecord(1, "claude-sonnet", "Agent-02", "meeting", now))

	shift := 1
	results := store.Query(QueryFilter{Shift: &shift})
	if len(results) != 2 {
		t.Errorf("expected 2 shift-1 records, got %d", len(results))
	}
}

func TestStoreQueryByModel(t *testing.T) {
	store := NewObservationStore()
	now := time.Now()
	store.Add(makeRecord(1, "claude-sonnet", "Agent-01", "daily_routine", now))
	store.Add(makeRecord(1, "llama-3.1-70b", "Agent-02", "daily_routine", now))

	model := "claude-sonnet"
	results := store.Query(QueryFilter{Model: &model})
	if len(results) != 1 {
		t.Errorf("expected 1 claude-sonnet record, got %d", len(results))
	}
}

func TestStoreQueryByAgent(t *testing.T) {
	store := NewObservationStore()
	now := time.Now()
	store.Add(makeRecord(1, "claude-sonnet", "Agent-01", "daily_routine", now))
	store.Add(makeRecord(1, "claude-sonnet", "Agent-02", "meeting", now))

	agent := "Agent-01"
	results := store.Query(QueryFilter{Agent: &agent})
	if len(results) != 1 {
		t.Errorf("expected 1 Agent-01 record, got %d", len(results))
	}
}

func TestStoreQueryByScenario(t *testing.T) {
	store := NewObservationStore()
	now := time.Now()
	store.Add(makeRecord(1, "claude-sonnet", "Agent-01", "daily_routine", now))
	store.Add(makeRecord(1, "claude-sonnet", "Agent-02", "meeting", now))

	scenario := "meeting"
	results := store.Query(QueryFilter{Scenario: &scenario})
	if len(results) != 1 {
		t.Errorf("expected 1 meeting record, got %d", len(results))
	}
}

func TestStoreQueryByTimeRange(t *testing.T) {
	store := NewObservationStore()
	base := time.Date(2026, 2, 12, 10, 0, 0, 0, time.UTC)

	store.Add(makeRecord(1, "claude-sonnet", "Agent-01", "daily_routine", base))
	store.Add(makeRecord(1, "claude-sonnet", "Agent-02", "daily_routine", base.Add(2*time.Hour)))
	store.Add(makeRecord(1, "claude-sonnet", "Agent-03", "daily_routine", base.Add(4*time.Hour)))

	from := base.Add(1 * time.Hour)
	to := base.Add(3 * time.Hour)
	results := store.Query(QueryFilter{From: &from, To: &to})
	if len(results) != 1 {
		t.Errorf("expected 1 record in time range, got %d", len(results))
	}
}

func TestStoreQueryCombinedFilters(t *testing.T) {
	store := NewObservationStore()
	now := time.Now()

	store.Add(makeRecord(1, "claude-sonnet", "Agent-01", "daily_routine", now))
	store.Add(makeRecord(1, "llama-3.1-70b", "Agent-01", "daily_routine", now))
	store.Add(makeRecord(2, "claude-sonnet", "Agent-01", "daily_routine", now))

	shift := 1
	model := "claude-sonnet"
	results := store.Query(QueryFilter{Shift: &shift, Model: &model})
	if len(results) != 1 {
		t.Errorf("expected 1 record matching shift+model, got %d", len(results))
	}
}

func TestStoreQueryEmptyFilter(t *testing.T) {
	store := NewObservationStore()
	now := time.Now()
	store.Add(makeRecord(1, "claude-sonnet", "Agent-01", "daily_routine", now))
	store.Add(makeRecord(2, "llama-3.1-70b", "Agent-16", "meeting", now))

	// Empty filter should return all
	results := store.Query(QueryFilter{})
	if len(results) != 2 {
		t.Errorf("expected 2 records with empty filter, got %d", len(results))
	}
}

func TestStoreQueryNoMatch(t *testing.T) {
	store := NewObservationStore()
	now := time.Now()
	store.Add(makeRecord(1, "claude-sonnet", "Agent-01", "daily_routine", now))

	model := "nonexistent"
	results := store.Query(QueryFilter{Model: &model})
	if len(results) != 0 {
		t.Errorf("expected 0 records for nonexistent model, got %d", len(results))
	}
}

func TestStoreConcurrentAccess(t *testing.T) {
	store := NewObservationStore()
	now := time.Now()

	var wg sync.WaitGroup
	// 10 concurrent writers
	for i := 0; i < 10; i++ {
		wg.Add(1)
		go func(shift int) {
			defer wg.Done()
			for j := 0; j < 100; j++ {
				store.Add(makeRecord(shift%3+1, "claude-sonnet", "Agent-01", "daily_routine", now))
			}
		}(i)
	}

	// 5 concurrent readers
	for i := 0; i < 5; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := 0; j < 50; j++ {
				_ = store.AllRecords()
				_ = store.Query(QueryFilter{})
				_ = store.Len()
			}
		}()
	}

	wg.Wait()

	if store.Len() != 1000 {
		t.Errorf("expected 1000 records after concurrent writes, got %d", store.Len())
	}
}
