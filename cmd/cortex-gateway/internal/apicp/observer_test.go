package apicp

import (
	"testing"
	"time"
)

func TestRecordAndStats(t *testing.T) {
	o := NewObserver(t.TempDir()+"/apicp.json", 1*time.Hour, nil)
	defer o.Stop()

	o.Record("H5|E7|R:test", "AGENT-01", "hello world", false)
	o.Record("H5|E7|R:test", "AGENT-01", "hello world", false)
	o.Record("H5|E7|R:test", "AGENT-01", "hello world", false)

	stats := o.Stats()
	if stats["patterns_total"].(int) != 1 {
		t.Errorf("patterns = %v, want 1", stats["patterns_total"])
	}
	if stats["buffer_used"].(int) != 3 {
		t.Errorf("buffer_used = %v, want 3", stats["buffer_used"])
	}
}

func TestConfidenceCalculation(t *testing.T) {
	o := NewObserver(t.TempDir()+"/apicp.json", 1*time.Hour, nil)
	defer o.Stop()

	// 9 identical responses + 1 different → confidence = 0.9
	for i := 0; i < 9; i++ {
		o.Record("fp1", "AGENT-01", "same response", false)
	}
	o.Record("fp1", "AGENT-01", "different response", false)

	o.mu.RLock()
	ps := o.stats["fp1"]
	o.mu.RUnlock()

	if ps.Confidence != 0.9 {
		t.Errorf("confidence = %f, want 0.9", ps.Confidence)
	}
}

func TestEvolutionDegradation(t *testing.T) {
	o := NewObserver(t.TempDir()+"/apicp.json", 1*time.Hour, nil)
	defer o.Stop()

	for i := 0; i < 10; i++ {
		o.Record("fp1", "AGENT-01", "same", false)
	}

	o.mu.RLock()
	before := o.stats["fp1"].Confidence
	o.mu.RUnlock()

	// Simulate evolution version change
	o.CheckEvolutionDegradation("AGENT-01", "v1")
	o.CheckEvolutionDegradation("AGENT-01", "v2") // version changed → degrade

	o.mu.RLock()
	after := o.stats["fp1"].Confidence
	o.mu.RUnlock()

	expected := before * degradationFactor
	if after != expected {
		t.Errorf("after degradation = %f, want %f (half of %f)", after, expected, before)
	}
}

func TestSuggestionsThreshold(t *testing.T) {
	o := NewObserver(t.TempDir()+"/apicp.json", 1*time.Hour, nil)
	defer o.Stop()

	// Not enough samples → no suggestion
	for i := 0; i < 10; i++ {
		o.Record("fp1", "AGENT-01", "same", false)
	}
	if len(o.Suggestions()) != 0 {
		t.Error("should have no suggestions with only 10 samples")
	}

	// Enough samples + high confidence → suggestion
	for i := 0; i < 45; i++ {
		o.Record("fp1", "AGENT-01", "same", false)
	}
	suggestions := o.Suggestions()
	if len(suggestions) != 1 {
		t.Errorf("should have 1 suggestion, got %d", len(suggestions))
	}
	if suggestions[0].Confidence < promotionThreshold {
		t.Errorf("confidence = %f, want >= %f", suggestions[0].Confidence, promotionThreshold)
	}
}

func TestShouldProbe(t *testing.T) {
	o := NewObserver(t.TempDir()+"/apicp.json", 1*time.Hour, nil)
	defer o.Stop()

	// Record 99 synth calls → no probe
	for i := 0; i < 99; i++ {
		o.Record("fp1", "AGENT-01", "same", true)
	}
	if o.ShouldProbe() {
		t.Error("should not probe at 99 synth calls")
	}

	// 100th call → probe
	o.Record("fp1", "AGENT-01", "same", true)
	if !o.ShouldProbe() {
		t.Error("should probe at 100 synth calls")
	}
}
