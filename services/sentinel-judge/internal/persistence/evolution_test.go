package persistence

import (
	"path/filepath"
	"testing"
)

func tempEvolution(t *testing.T) *EvolutionStore {
	t.Helper()
	path := filepath.Join(t.TempDir(), "evolution.db")
	store, err := OpenEvolution(path)
	if err != nil {
		t.Fatalf("OpenEvolution: %v", err)
	}
	t.Cleanup(func() { _ = store.Close() })
	return store
}

func TestEvolutionWriteAndRead(t *testing.T) {
	store := tempEvolution(t)

	nmda := 0.85
	entry := EvolutionEntry{
		AgentID:    "AGENT-07",
		Tick:       1000,
		Field:      "voice_style",
		ChangeType: "voice_style",
		OldValue:   "",
		NewValue:   `{"phrases":["Guten Morgen"],"formality":0.7}`,
		Reason:     "voice pattern analysis detected formal greeting style",
		NMDAScore:  &nmda,
		Source:     "batch_judge",
	}

	if err := store.Write(entry); err != nil {
		t.Fatalf("Write: %v", err)
	}

	entries, err := store.GetByAgent("AGENT-07")
	if err != nil {
		t.Fatalf("GetByAgent: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("expected 1 entry, got %d", len(entries))
	}

	got := entries[0]
	if got.AgentID != "AGENT-07" {
		t.Errorf("agent_id = %q, want AGENT-07", got.AgentID)
	}
	if got.Tick != 1000 {
		t.Errorf("tick = %d, want 1000", got.Tick)
	}
	if got.Field != "voice_style" {
		t.Errorf("field = %q, want voice_style", got.Field)
	}
	if got.Source != "batch_judge" {
		t.Errorf("source = %q, want batch_judge", got.Source)
	}
	if got.NMDAScore == nil || *got.NMDAScore != 0.85 {
		t.Errorf("nmda_score = %v, want 0.85", got.NMDAScore)
	}
}

func TestEvolutionCount(t *testing.T) {
	store := tempEvolution(t)

	n, err := store.Count()
	if err != nil {
		t.Fatalf("Count: %v", err)
	}
	if n != 0 {
		t.Errorf("expected 0, got %d", n)
	}

	_ = store.Write(EvolutionEntry{
		AgentID:    "AGENT-01",
		Tick:       1,
		Field:      "drift",
		ChangeType: "drift",
		NewValue:   "0.75",
		Reason:     "test",
	})
	_ = store.Write(EvolutionEntry{
		AgentID:    "AGENT-02",
		Tick:       2,
		Field:      "quality",
		ChangeType: "quality",
		NewValue:   "3",
		Reason:     "test",
	})

	n, err = store.Count()
	if err != nil {
		t.Fatalf("Count: %v", err)
	}
	if n != 2 {
		t.Errorf("expected 2, got %d", n)
	}
}

func TestEvolutionEmptyAgent(t *testing.T) {
	store := tempEvolution(t)

	entries, err := store.GetByAgent("AGENT-99")
	if err != nil {
		t.Fatalf("GetByAgent: %v", err)
	}
	if len(entries) != 0 {
		t.Errorf("expected 0 entries for unknown agent, got %d", len(entries))
	}
}

func TestEvolutionRetentionKeepsNewestIDsNotLargestTicks(t *testing.T) {
	store := tempEvolution(t)

	if err := store.Write(EvolutionEntry{
		AgentID:    "AGENT-01",
		Tick:       1_780_000_000_000,
		Field:      "drift_score",
		ChangeType: "drift",
		NewValue:   "legacy-ms",
		Reason:     "legacy realtime judge row",
		Source:     "realtime_judge",
	}); err != nil {
		t.Fatalf("write legacy: %v", err)
	}

	for i := 0; i < maxRowsPerAgentField; i++ {
		if err := store.Write(EvolutionEntry{
			AgentID:    "AGENT-01",
			Tick:       int64(1_900_000 + i),
			Field:      "drift_score",
			ChangeType: "drift",
			NewValue:   "real-sim",
			Reason:     "post-fix row",
			Source:     "realtime_judge",
		}); err != nil {
			t.Fatalf("write real row %d: %v", i, err)
		}
	}

	var count int64
	var maxTick int64
	if err := store.db.QueryRow(`SELECT count(*), max(tick)
		FROM personality_evolution
		WHERE agent_id = 'AGENT-01' AND field = 'drift_score'`).Scan(&count, &maxTick); err != nil {
		t.Fatalf("query retained rows: %v", err)
	}
	if count != maxRowsPerAgentField {
		t.Fatalf("retained rows = %d, want %d", count, maxRowsPerAgentField)
	}
	if maxTick >= maxPlausibleSimTickExcl {
		t.Fatalf("legacy ms tick was retained: max tick = %d", maxTick)
	}
}

func TestEvolutionRetentionIsPerAgentField(t *testing.T) {
	store := tempEvolution(t)

	for i := 0; i < maxRowsPerAgentField+1; i++ {
		if err := store.Write(EvolutionEntry{
			AgentID:    "AGENT-01",
			Tick:       int64(i),
			Field:      "quality_score",
			ChangeType: "quality",
			NewValue:   "1",
			Reason:     "test",
		}); err != nil {
			t.Fatalf("write quality: %v", err)
		}
		if err := store.Write(EvolutionEntry{
			AgentID:    "AGENT-01",
			Tick:       int64(i),
			Field:      "fatigue_score",
			ChangeType: "fatigue",
			NewValue:   "0.1",
			Reason:     "test",
		}); err != nil {
			t.Fatalf("write fatigue: %v", err)
		}
	}

	rows, err := store.db.Query(`SELECT field, count(*)
		FROM personality_evolution
		WHERE agent_id = 'AGENT-01'
		GROUP BY field`)
	if err != nil {
		t.Fatalf("query groups: %v", err)
	}
	defer func() { _ = rows.Close() }()

	counts := map[string]int64{}
	for rows.Next() {
		var field string
		var count int64
		if err := rows.Scan(&field, &count); err != nil {
			t.Fatalf("scan group: %v", err)
		}
		counts[field] = count
	}
	if err := rows.Err(); err != nil {
		t.Fatalf("rows: %v", err)
	}
	if counts["quality_score"] != maxRowsPerAgentField {
		t.Fatalf("quality rows = %d, want %d", counts["quality_score"], maxRowsPerAgentField)
	}
	if counts["fatigue_score"] != maxRowsPerAgentField {
		t.Fatalf("fatigue rows = %d, want %d", counts["fatigue_score"], maxRowsPerAgentField)
	}
}

func TestDeleteInvalidTicks(t *testing.T) {
	store := tempEvolution(t)

	for _, entry := range []EvolutionEntry{
		{AgentID: "AGENT-01", Tick: 42, Field: "drift_score", ChangeType: "drift", NewValue: "ok", Reason: "test"},
		{AgentID: "AGENT-01", Tick: -1, Field: "drift_score", ChangeType: "drift", NewValue: "bad-negative", Reason: "test"},
		{AgentID: "AGENT-01", Tick: maxPlausibleSimTickExcl, Field: "drift_score", ChangeType: "drift", NewValue: "bad-ms", Reason: "test"},
	} {
		if err := store.Write(entry); err != nil {
			t.Fatalf("write: %v", err)
		}
	}

	deleted, err := store.DeleteInvalidTicks()
	if err != nil {
		t.Fatalf("DeleteInvalidTicks: %v", err)
	}
	if deleted != 2 {
		t.Fatalf("deleted = %d, want 2", deleted)
	}

	entries, err := store.GetByAgent("AGENT-01")
	if err != nil {
		t.Fatalf("GetByAgent: %v", err)
	}
	if len(entries) != 1 || entries[0].Tick != 42 {
		t.Fatalf("remaining entries = %+v, want only tick 42", entries)
	}
}
