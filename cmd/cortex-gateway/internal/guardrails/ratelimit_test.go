package guardrails

import (
	"sync"
	"testing"
	"time"
)

func TestRateLimiter_AllowBasic(t *testing.T) {
	rl := NewRateLimiter(60, 600, 2) // 60/min per agent, 600/min global
	ok, reason := rl.Allow("AGENT-01")
	if !ok {
		t.Fatalf("expected allowed, got denied: %s", reason)
	}
	if reason != "" {
		t.Fatalf("expected empty reason, got %q", reason)
	}
}

func TestRateLimiter_PerAgentLimit(t *testing.T) {
	// 20 calls/min = ~0.333/sec, burst=2 → max=0.666 tokens
	// So with burst=2 the bucket starts with max=0.666 which rounds to allow 0 burst calls after initial.
	// Use higher limit to make test deterministic.
	rl := NewRateLimiter(60, 6000, 1) // 1 call/sec, burst=1 → max=1
	now := time.Now()
	rl.nowFunc = func() time.Time { return now }

	// First call: consumes the 1 token
	ok, _ := rl.Allow("AGENT-01")
	if !ok {
		t.Fatal("first call should be allowed")
	}

	// Second call at same instant: should be denied (per_agent)
	ok, reason := rl.Allow("AGENT-01")
	if ok {
		t.Fatal("second call at same time should be rate-limited")
	}
	if reason != "per_agent" {
		t.Fatalf("expected per_agent reason, got %q", reason)
	}

	// Different agent should still be allowed
	ok, _ = rl.Allow("AGENT-02")
	if !ok {
		t.Fatal("different agent should be allowed")
	}
}

func TestRateLimiter_GlobalLimit(t *testing.T) {
	rl := NewRateLimiter(600, 60, 1) // per-agent high, global 1/sec, burst=1
	now := time.Now()
	rl.nowFunc = func() time.Time { return now }

	// First call: allowed (consumes global token)
	ok, _ := rl.Allow("AGENT-01")
	if !ok {
		t.Fatal("first call should be allowed")
	}

	// Second call: global depleted
	ok, reason := rl.Allow("AGENT-02")
	if ok {
		t.Fatal("second call should be globally rate-limited")
	}
	if reason != "global" {
		t.Fatalf("expected global reason, got %q", reason)
	}
}

func TestRateLimiter_TokenRefill(t *testing.T) {
	rl := NewRateLimiter(60, 6000, 1) // 1/sec, burst=1
	now := time.Now()
	rl.nowFunc = func() time.Time { return now }

	// Exhaust
	rl.Allow("AGENT-01")
	ok, _ := rl.Allow("AGENT-01")
	if ok {
		t.Fatal("should be rate-limited after exhaustion")
	}

	// Advance time by 1.5 seconds → refill > 1 token
	now = now.Add(1500 * time.Millisecond)
	ok, _ = rl.Allow("AGENT-01")
	if !ok {
		t.Fatal("should be allowed after token refill")
	}
}

func TestRateLimiter_BurstMultiple(t *testing.T) {
	rl := NewRateLimiter(60, 6000, 3) // 1/sec, burst=3 → max=3
	now := time.Now()
	rl.nowFunc = func() time.Time { return now }

	// Should allow 3 calls (burst=3)
	for i := 0; i < 3; i++ {
		ok, _ := rl.Allow("AGENT-01")
		if !ok {
			t.Fatalf("call %d should be allowed with burst=3", i+1)
		}
	}

	// 4th should be denied
	ok, _ := rl.Allow("AGENT-01")
	if ok {
		t.Fatal("4th call should be denied")
	}
}

func TestRateLimiter_EmptyAgentID(t *testing.T) {
	rl := NewRateLimiter(60, 6000, 2)
	// Empty agent ID should skip per-agent check, only global applies
	ok, _ := rl.Allow("")
	if !ok {
		t.Fatal("empty agent ID should be allowed (global only)")
	}
}

func TestRateLimiter_Reset(t *testing.T) {
	rl := NewRateLimiter(60, 6000, 1)
	now := time.Now()
	rl.nowFunc = func() time.Time { return now }

	rl.Allow("AGENT-01")
	rl.Allow("AGENT-01") // depleted

	rl.Reset()

	ok, _ := rl.Allow("AGENT-01")
	if !ok {
		t.Fatal("should be allowed after reset")
	}
}

func TestRateLimiter_Concurrent(t *testing.T) {
	rl := NewRateLimiter(600, 6000, 10)

	var wg sync.WaitGroup
	for i := 0; i < 100; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			rl.Allow("AGENT-01")
		}()
	}
	wg.Wait()
	// No race conditions = pass
}

func TestRateLimiter_DefaultBurstMultiple(t *testing.T) {
	rl := NewRateLimiter(60, 6000, 0) // 0 should default to 2
	now := time.Now()
	rl.nowFunc = func() time.Time { return now }

	// burst=2 → max=2
	ok1, _ := rl.Allow("AGENT-01")
	ok2, _ := rl.Allow("AGENT-01")
	if !ok1 || !ok2 {
		t.Fatal("both calls should be allowed with default burst=2")
	}

	ok3, _ := rl.Allow("AGENT-01")
	if ok3 {
		t.Fatal("3rd call should be denied with burst=2")
	}
}
