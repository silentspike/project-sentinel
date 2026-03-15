package proxy

import (
	"context"
	"errors"
	"testing"
	"time"
)

func testConfig() BreakerConfig {
	return BreakerConfig{
		WindowSeconds:    10,
		MinRequests:      5,
		FailureRatio:     0.5,
		FailureThreshold: 3,
		OpenSeconds:      5,
		HalfOpenProbes:   2,
		Enabled:          true,
	}
}

func TestCircuitBreakerStartsClosed(t *testing.T) {
	cb := NewCircuitBreaker(testConfig())
	if got := cb.State(); got != "closed" {
		t.Errorf("State() = %q, want %q", got, "closed")
	}
	if !cb.Allow() {
		t.Error("Allow() = false, want true in closed state")
	}
}

func TestConsecutiveFailuresTripsBreaker(t *testing.T) {
	cb := NewCircuitBreaker(testConfig())
	now := time.Now()
	cb.now = func() time.Time { return now }

	// 3 consecutive failures → trip
	for i := 0; i < 3; i++ {
		cb.Allow()
		cb.Record(errors.New("transport error"))
	}

	if got := cb.State(); got != "open" {
		t.Errorf("State() = %q, want %q after 3 consecutive failures", got, "open")
	}
	if cb.Allow() {
		t.Error("Allow() = true, want false in open state")
	}
}

func TestFailureRatioTripsBreaker(t *testing.T) {
	cfg := testConfig()
	cfg.FailureThreshold = 100 // Deaktiviere consecutive check
	cb := NewCircuitBreaker(cfg)
	now := time.Now()
	cb.now = func() time.Time { return now }

	// 5 Requests: 3 failures, 2 successes → ratio 0.6 > 0.5
	for i := 0; i < 5; i++ {
		cb.Allow()
		if i < 3 {
			cb.Record(errors.New("fail"))
		} else {
			cb.Record(nil)
		}
	}

	if got := cb.State(); got != "open" {
		t.Errorf("State() = %q, want %q (failure ratio 0.6)", got, "open")
	}
}

func TestMinRequestsBeforeRatioCheck(t *testing.T) {
	cfg := testConfig()
	cfg.FailureThreshold = 100 // Deaktiviere consecutive check
	cb := NewCircuitBreaker(cfg)
	now := time.Now()
	cb.now = func() time.Time { return now }

	// 4 Requests (< MinRequests=5): alle failures → ratio 1.0 aber zu wenig Requests
	for i := 0; i < 4; i++ {
		cb.Allow()
		cb.Record(errors.New("fail"))
	}

	if got := cb.State(); got != "closed" {
		t.Errorf("State() = %q, want %q (below MinRequests)", got, "closed")
	}
}

func TestOpenToHalfOpenTransition(t *testing.T) {
	cb := NewCircuitBreaker(testConfig())
	now := time.Now()
	cb.now = func() time.Time { return now }

	// Trip breaker
	for i := 0; i < 3; i++ {
		cb.Allow()
		cb.Record(errors.New("fail"))
	}
	if got := cb.State(); got != "open" {
		t.Fatalf("State() = %q, want open", got)
	}

	// Advance time past OpenSeconds
	now = now.Add(6 * time.Second)

	// Allow should transition to half-open
	if !cb.Allow() {
		t.Error("Allow() = false, want true after open timeout")
	}
	if got := cb.State(); got != "half-open" {
		t.Errorf("State() = %q, want %q", got, "half-open")
	}
}

func TestHalfOpenSuccessCloses(t *testing.T) {
	cb := NewCircuitBreaker(testConfig())
	now := time.Now()
	cb.now = func() time.Time { return now }

	// Trip → Open
	for i := 0; i < 3; i++ {
		cb.Allow()
		cb.Record(errors.New("fail"))
	}

	// → Half-Open
	now = now.Add(6 * time.Second)
	cb.Allow()

	// 2 successful probes → Closed
	cb.Record(nil)
	cb.Record(nil)

	if got := cb.State(); got != "closed" {
		t.Errorf("State() = %q, want %q after successful probes", got, "closed")
	}
}

func TestHalfOpenFailureReopens(t *testing.T) {
	cb := NewCircuitBreaker(testConfig())
	now := time.Now()
	cb.now = func() time.Time { return now }

	// Trip → Open
	for i := 0; i < 3; i++ {
		cb.Allow()
		cb.Record(errors.New("fail"))
	}

	// → Half-Open
	now = now.Add(6 * time.Second)
	cb.Allow()

	// Failure in half-open → back to Open
	cb.Record(errors.New("still broken"))

	if got := cb.State(); got != "open" {
		t.Errorf("State() = %q, want %q after half-open failure", got, "open")
	}
}

func TestSemantic4xxNotCounted(t *testing.T) {
	cb := NewCircuitBreaker(testConfig())
	now := time.Now()
	cb.now = func() time.Time { return now }

	// 10 semantic 4xx errors → should NOT trip
	for i := 0; i < 10; i++ {
		cb.Allow()
		cb.Record(&ProviderError{StatusCode: 400, Message: "bad request"})
	}

	if got := cb.State(); got != "closed" {
		t.Errorf("State() = %q, want %q (4xx should not trip)", got, "closed")
	}
}

func TestHTTP429CountsAsFailure(t *testing.T) {
	cb := NewCircuitBreaker(testConfig())
	now := time.Now()
	cb.now = func() time.Time { return now }

	// 3 consecutive 429 → trip
	for i := 0; i < 3; i++ {
		cb.Allow()
		cb.Record(&ProviderError{StatusCode: 429, Message: "rate limited"})
	}

	if got := cb.State(); got != "open" {
		t.Errorf("State() = %q, want %q (429 should trip)", got, "open")
	}
}

func TestHTTP5xxCountsAsFailure(t *testing.T) {
	cb := NewCircuitBreaker(testConfig())
	now := time.Now()
	cb.now = func() time.Time { return now }

	for i := 0; i < 3; i++ {
		cb.Allow()
		cb.Record(&ProviderError{StatusCode: 503, Message: "unavailable"})
	}

	if got := cb.State(); got != "open" {
		t.Errorf("State() = %q, want %q (5xx should trip)", got, "open")
	}
}

func TestTimeoutCountsAsFailure(t *testing.T) {
	cb := NewCircuitBreaker(testConfig())
	now := time.Now()
	cb.now = func() time.Time { return now }

	for i := 0; i < 3; i++ {
		cb.Allow()
		cb.Record(context.DeadlineExceeded)
	}

	if got := cb.State(); got != "open" {
		t.Errorf("State() = %q, want %q (timeout should trip)", got, "open")
	}
}

func TestSuccessResetsConsecutive(t *testing.T) {
	cfg := testConfig()
	cfg.MinRequests = 100 // Deaktiviere ratio-check fuer diesen Test
	cb := NewCircuitBreaker(cfg)
	now := time.Now()
	cb.now = func() time.Time { return now }

	// 2 failures, 1 success, 2 failures → consecutive never reaches 3
	cb.Allow()
	cb.Record(errors.New("fail"))
	cb.Allow()
	cb.Record(errors.New("fail"))
	cb.Allow()
	cb.Record(nil) // Reset consecutive
	cb.Allow()
	cb.Record(errors.New("fail"))
	cb.Allow()
	cb.Record(errors.New("fail"))

	if got := cb.State(); got != "closed" {
		t.Errorf("State() = %q, want %q (consecutive was reset)", got, "closed")
	}
}

func TestSlidingWindowPrune(t *testing.T) {
	cfg := testConfig()
	cfg.FailureThreshold = 100
	cb := NewCircuitBreaker(cfg)
	now := time.Now()
	cb.now = func() time.Time { return now }

	// 3 failures at t=0
	for i := 0; i < 3; i++ {
		cb.Allow()
		cb.Record(errors.New("fail"))
	}

	// Advance past window (10s), add 5 successes → old failures pruned
	now = now.Add(11 * time.Second)
	for i := 0; i < 5; i++ {
		cb.Allow()
		cb.Record(nil)
	}

	if got := cb.State(); got != "closed" {
		t.Errorf("State() = %q, want %q (old failures pruned)", got, "closed")
	}
}

func TestDefaultBreakerConfig(t *testing.T) {
	cfg := DefaultBreakerConfig()
	if cfg.WindowSeconds != 20 {
		t.Errorf("WindowSeconds = %d, want 20", cfg.WindowSeconds)
	}
	if cfg.MinRequests != 20 {
		t.Errorf("MinRequests = %d, want 20", cfg.MinRequests)
	}
	if cfg.FailureRatio != 0.5 {
		t.Errorf("FailureRatio = %f, want 0.5", cfg.FailureRatio)
	}
	if cfg.FailureThreshold != 5 {
		t.Errorf("FailureThreshold = %d, want 5", cfg.FailureThreshold)
	}
	if cfg.OpenSeconds != 30 {
		t.Errorf("OpenSeconds = %d, want 30", cfg.OpenSeconds)
	}
	if cfg.HalfOpenProbes != 3 {
		t.Errorf("HalfOpenProbes = %d, want 3", cfg.HalfOpenProbes)
	}
}

func TestBreakerConfigFromEnv(t *testing.T) {
	t.Setenv("SENTINEL_CORTEX_CB_WINDOW_SECONDS", "30")
	t.Setenv("SENTINEL_CORTEX_CB_FAILURE_THRESHOLD", "10")

	cfg := BreakerConfigFromEnv()
	if cfg.WindowSeconds != 30 {
		t.Errorf("WindowSeconds = %d, want 30", cfg.WindowSeconds)
	}
	if cfg.FailureThreshold != 10 {
		t.Errorf("FailureThreshold = %d, want 10", cfg.FailureThreshold)
	}
	// Andere bleiben Default
	if cfg.MinRequests != 20 {
		t.Errorf("MinRequests = %d, want 20 (default)", cfg.MinRequests)
	}
}

func TestHalfOpenLimitsProbes(t *testing.T) {
	cb := NewCircuitBreaker(testConfig())
	now := time.Now()
	cb.now = func() time.Time { return now }

	// Trip
	for i := 0; i < 3; i++ {
		cb.Allow()
		cb.Record(errors.New("fail"))
	}

	// → Half-Open
	now = now.Add(6 * time.Second)

	// HalfOpenProbes = 2 → Allow 2, deny 3rd
	if !cb.Allow() {
		t.Error("probe 1: Allow() = false, want true")
	}
	cb.Record(nil) // success, probeCount=1

	if !cb.Allow() {
		t.Error("probe 2: Allow() = false, want true")
	}
	cb.Record(nil) // success, probeCount=2 → transition to Closed

	// Should be closed now
	if got := cb.State(); got != "closed" {
		t.Errorf("State() = %q, want closed after all probes succeed", got)
	}
}
