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
