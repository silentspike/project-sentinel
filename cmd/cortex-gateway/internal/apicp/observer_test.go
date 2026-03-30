package apicp

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"
)

func newTestObserver(t *testing.T) *Observer {
	t.Helper()
	o := NewObserver(Config{}, nil)
	t.Cleanup(o.Stop)
	return o
}

func TestRecordAndStats(t *testing.T) {
	o := newTestObserver(t)

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
	o := newTestObserver(t)

	for i := 0; i < 9; i++ {
		o.Record("fp1", "AGENT-01", "same response", false)
	}
	o.Record("fp1", "AGENT-01", "different response", false)

	o.mu.RLock()
	ps := o.stats[patternKey("AGENT-01", "fp1")]
	o.mu.RUnlock()

	if ps.Confidence != 0.9 {
		t.Errorf("confidence = %f, want 0.9", ps.Confidence)
	}
}

func TestEvolutionDegradation(t *testing.T) {
	o := newTestObserver(t)

	for i := 0; i < 10; i++ {
		o.Record("fp1", "AGENT-01", "same", false)
	}

	o.mu.RLock()
	before := o.stats[patternKey("AGENT-01", "fp1")].Confidence
	o.mu.RUnlock()

	o.CheckEvolutionDegradation("AGENT-01", "v1")
	o.CheckEvolutionDegradation("AGENT-01", "v2")

	o.mu.RLock()
	after := o.stats[patternKey("AGENT-01", "fp1")].Confidence
	o.mu.RUnlock()

	expected := before * degradationFactor
	if after != expected {
		t.Errorf("after degradation = %f, want %f (half of %f)", after, expected, before)
	}
}

func TestEvolutionDegradationOnlyAffectsMatchingAgent(t *testing.T) {
	o := newTestObserver(t)

	for i := 0; i < 50; i++ {
		o.Record("fp1", "AGENT-01", "same", false)
		o.Record("fp1", "AGENT-02", "same", false)
	}

	o.mu.RLock()
	beforeAgent1 := *o.stats[patternKey("AGENT-01", "fp1")]
	beforeAgent2 := *o.stats[patternKey("AGENT-02", "fp1")]
	o.mu.RUnlock()

	o.CheckEvolutionDegradation("AGENT-01", "v1")
	o.CheckEvolutionDegradation("AGENT-02", "v1")
	o.CheckEvolutionDegradation("AGENT-01", "v2")

	o.mu.RLock()
	afterAgent1, ok1 := o.stats[patternKey("AGENT-01", "fp1")]
	afterAgent2, ok2 := o.stats[patternKey("AGENT-02", "fp1")]
	o.mu.RUnlock()
	if !ok1 {
		t.Fatal("expected AGENT-01 pattern to remain present after degradation")
	}
	if !ok2 {
		t.Fatal("expected AGENT-02 pattern to remain present after unrelated degradation")
	}

	if afterAgent1.Confidence != beforeAgent1.Confidence*degradationFactor {
		t.Fatalf("AGENT-01 confidence = %f, want %f", afterAgent1.Confidence, beforeAgent1.Confidence*degradationFactor)
	}
	if afterAgent2.Confidence != beforeAgent2.Confidence {
		t.Fatalf("AGENT-02 confidence = %f, want unchanged %f", afterAgent2.Confidence, beforeAgent2.Confidence)
	}
}

func TestSuggestionsThreshold(t *testing.T) {
	o := newTestObserver(t)

	for i := 0; i < 10; i++ {
		o.Record("fp1", "AGENT-01", "same", false)
	}
	if len(o.Suggestions()) != 0 {
		t.Error("should have no suggestions with only 10 samples")
	}

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
	if suggestions[0].AgentID != "AGENT-01" {
		t.Errorf("agent = %q, want AGENT-01", suggestions[0].AgentID)
	}
}

func TestShouldProbe(t *testing.T) {
	o := newTestObserver(t)

	for i := 0; i < 99; i++ {
		o.Record("fp1", "AGENT-01", "same", true)
	}
	if o.ShouldProbe() {
		t.Error("should not probe at 99 synth calls")
	}

	o.Record("fp1", "AGENT-01", "same", true)
	if !o.ShouldProbe() {
		t.Error("should probe at 100 synth calls")
	}
}

func TestLearnedPatternForReturnsTopContent(t *testing.T) {
	o := newTestObserver(t)

	for i := 0; i < 50; i++ {
		o.Record("fp1", "AGENT-01", "same", false)
	}

	learned, ok := o.LearnedPatternFor("AGENT-01", "fp1")
	if !ok {
		t.Fatal("expected promoted learned pattern")
	}
	if learned.Content != "same" {
		t.Fatalf("content = %q, want same", learned.Content)
	}
	if learned.TopHash == 0 {
		t.Fatal("expected non-zero top hash")
	}
}

func TestApplyProbeResultDegradesOnMismatch(t *testing.T) {
	o := newTestObserver(t)

	for i := 0; i < 50; i++ {
		o.Record("fp1", "AGENT-01", "same", false)
	}
	learned, ok := o.LearnedPatternFor("AGENT-01", "fp1")
	if !ok {
		t.Fatal("expected promoted learned pattern")
	}

	before := learned.Confidence
	o.ApplyProbeResult("AGENT-01", "fp1", learned.TopHash, "different")

	after, ok := o.LearnedPatternFor("AGENT-01", "fp1")
	if ok && after.Confidence >= before {
		t.Fatalf("confidence = %f, want lower than %f", after.Confidence, before)
	}
}

func TestPatternLimitIsPerAgent(t *testing.T) {
	o := newTestObserver(t)

	for i := 0; i < maxPatternsPerAgent+5; i++ {
		o.Record("fp-agent-1-"+time.Unix(int64(i), 0).Format(time.RFC3339), "AGENT-01", "same", false)
	}
	for i := 0; i < 3; i++ {
		o.Record("fp-agent-2-"+time.Unix(int64(i), 0).Format(time.RFC3339), "AGENT-02", "same", false)
	}

	o.mu.RLock()
	defer o.mu.RUnlock()
	count1 := 0
	count2 := 0
	for _, ps := range o.stats {
		switch ps.AgentID {
		case "AGENT-01":
			count1++
		case "AGENT-02":
			count2++
		}
	}
	if count1 != maxPatternsPerAgent {
		t.Fatalf("agent 1 patterns = %d, want %d", count1, maxPatternsPerAgent)
	}
	if count2 != 3 {
		t.Fatalf("agent 2 patterns = %d, want 3", count2)
	}
}

func TestSnapshotRoundTripViaHTTP(t *testing.T) {
	var (
		mu       sync.Mutex
		stored   Snapshot
		seenAuth string
	)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		seenAuth = r.Header.Get(operatorKeyHeader)
		switch r.Method {
		case http.MethodGet:
			_ = json.NewEncoder(w).Encode(stored)
		case http.MethodPost:
			if err := json.NewDecoder(r.Body).Decode(&stored); err != nil {
				http.Error(w, err.Error(), http.StatusBadRequest)
				return
			}
			w.WriteHeader(http.StatusOK)
			_, _ = w.Write([]byte(`{"accepted":true}`))
		default:
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		}
	}))
	defer server.Close()

	stored = Snapshot{
		Patterns: []*PatternStats{{
			AgentID:        "AGENT-01",
			Fingerprint:    "fp1",
			Count:          50,
			ResponseHashes: map[uint64]int{42: 50},
			TopHash:        42,
			TopContent:     "same",
			Confidence:     1.0,
			LastSeen:       time.Now().UTC().Round(0),
			Promoted:       true,
		}},
		SynthCount:            11,
		LastEvolutionVersions: map[string]string{"AGENT-01": "v3"},
	}

	o := NewObserver(Config{
		SyncURL:      server.URL,
		SyncInterval: time.Hour,
		SharedSecret: "topsecret",
	}, nil)
	t.Cleanup(o.Stop)

	suggestions := o.Suggestions()
	if len(suggestions) != 1 {
		t.Fatalf("suggestions = %d, want 1", len(suggestions))
	}
	if seenAuth != "topsecret" {
		t.Fatalf("operator auth header = %q, want topsecret", seenAuth)
	}

	o.Record("fp2", "AGENT-02", "hello", true)
	o.syncToRemote()

	mu.Lock()
	defer mu.Unlock()
	if stored.SynthCount != 12 {
		t.Fatalf("stored synth_count = %d, want 12", stored.SynthCount)
	}
	if len(stored.Patterns) != 2 {
		t.Fatalf("stored patterns = %d, want 2", len(stored.Patterns))
	}
}

func TestSnapshotRestorePreservesEvolutionVersions(t *testing.T) {
	o := newTestObserver(t)

	for i := 0; i < 50; i++ {
		o.Record("fp1", "AGENT-01", "same", false)
	}
	o.CheckEvolutionDegradation("AGENT-01", "v1")

	snapshot := o.Snapshot()

	restored := newTestObserver(t)
	restored.restore(snapshot)

	restored.mu.RLock()
	defer restored.mu.RUnlock()

	if got := restored.lastEvolutionVersions["AGENT-01"]; got != "v1" {
		t.Fatalf("restored evolution version = %q, want v1", got)
	}
	if _, ok := restored.stats[patternKey("AGENT-01", "fp1")]; !ok {
		t.Fatal("expected restored pattern for AGENT-01/fp1")
	}
}

func TestSnapshotLoadRetriesOnStartup(t *testing.T) {
	oldDelay := bootstrapRetryDelay
	oldAttempts := bootstrapRetryAttempts
	bootstrapRetryDelay = 10 * time.Millisecond
	bootstrapRetryAttempts = 5
	t.Cleanup(func() {
		bootstrapRetryDelay = oldDelay
		bootstrapRetryAttempts = oldAttempts
	})

	var calls int
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		if r.Method != http.MethodGet {
			w.WriteHeader(http.StatusOK)
			_, _ = w.Write([]byte(`{"accepted":true}`))
			return
		}
		if calls == 1 {
			http.Error(w, "not ready", http.StatusServiceUnavailable)
			return
		}
		_ = json.NewEncoder(w).Encode(Snapshot{
			Patterns: []*PatternStats{{
				AgentID:        "AGENT-01",
				Fingerprint:    "fp1",
				Count:          50,
				ResponseHashes: map[uint64]int{42: 50},
				TopHash:        42,
				TopContent:     "same",
				Confidence:     1.0,
				LastSeen:       time.Now().UTC().Round(0),
				Promoted:       true,
			}},
		})
	}))
	defer server.Close()

	o := NewObserver(Config{
		SyncURL:      server.URL,
		SyncInterval: time.Hour,
	}, nil)
	t.Cleanup(o.Stop)

	deadline := time.Now().Add(500 * time.Millisecond)
	for time.Now().Before(deadline) {
		if suggestions := o.Suggestions(); len(suggestions) == 1 {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}

	t.Fatalf("expected bootstrap retry to load snapshot, suggestions=%d calls=%d", len(o.Suggestions()), calls)
}

func TestStopFlushesFinalSnapshot(t *testing.T) {
	var (
		mu     sync.Mutex
		stored Snapshot
		calls  int
	)

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		calls++
		defer func() { _ = r.Body.Close() }()
		if err := json.NewDecoder(r.Body).Decode(&stored); err != nil {
			t.Fatalf("decode stored snapshot: %v", err)
		}
		mu.Lock()
		defer mu.Unlock()
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(`{"accepted":true}`))
	}))
	defer server.Close()

	o := NewObserver(Config{
		SyncURL:      server.URL,
		SyncInterval: time.Hour,
	}, nil)

	for i := 0; i < 3; i++ {
		o.Record("fp-stop", "AGENT-01", "same", i%2 == 0)
	}

	o.Stop()

	mu.Lock()
	defer mu.Unlock()
	if calls != 1 {
		t.Fatalf("stop flush calls = %d, want 1", calls)
	}
	if got := len(stored.Patterns); got != 1 {
		t.Fatalf("stored patterns = %d, want 1", got)
	}
	if stored.SynthCount != 2 {
		t.Fatalf("stored synth_count = %d, want 2", stored.SynthCount)
	}
	if stored.Patterns[0].Fingerprint != "fp-stop" {
		t.Fatalf("stored fingerprint = %q, want fp-stop", stored.Patterns[0].Fingerprint)
	}
}
