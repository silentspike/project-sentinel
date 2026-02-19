package guardrails

import (
	"testing"
	"time"
)

func newTestConfig() Config {
	return Config{
		Enabled:            true,
		BudgetHourlyTokens: 10000,
		BudgetDailyTokens:  100000,
		RateLimitPerAgent:  20,
		RateLimitGlobal:    300,
		BurstMultiple:      2,
		FallbackProvider:   "ollama",
		ProviderPrices: map[string]PriceTable{
			"claude": {InputPricePerMToken: 3.0, OutputPricePerMToken: 15.0},
			"ollama": {InputPricePerMToken: 0.0, OutputPricePerMToken: 0.0},
		},
	}
}

func TestEnforcer_NewDisabled(t *testing.T) {
	cfg := newTestConfig()
	cfg.Enabled = false
	e := New(cfg)
	if e != nil {
		t.Fatal("expected nil enforcer when disabled")
	}
}

func TestEnforcer_CheckAllowed(t *testing.T) {
	e := New(newTestConfig())
	result := e.Check("AGENT-01", 100)
	if !result.Allowed {
		t.Fatal("expected allowed for normal request")
	}
	if result.RateLimited {
		t.Fatal("should not be rate limited")
	}
	if result.BudgetExhausted {
		t.Fatal("should not have exhausted budget")
	}
}

func TestEnforcer_CheckRateLimited(t *testing.T) {
	cfg := newTestConfig()
	cfg.RateLimitPerAgent = 60 // 1/sec
	cfg.BurstMultiple = 1      // no burst
	e := New(cfg)

	now := time.Now()
	e.rateLimiter.nowFunc = func() time.Time { return now }

	// First call allowed
	r := e.Check("AGENT-01", 100)
	if !r.Allowed {
		t.Fatal("first call should be allowed")
	}

	// Second call rate-limited
	r = e.Check("AGENT-01", 100)
	if r.Allowed {
		t.Fatal("second call should be denied")
	}
	if !r.RateLimited {
		t.Fatal("should be flagged as rate limited")
	}
}

func TestEnforcer_CheckBudgetExhaustedWithFallback(t *testing.T) {
	// AC-1: Budget exhaustion triggers fallback
	cfg := newTestConfig()
	cfg.BudgetHourlyTokens = 100
	e := New(cfg)

	e.budget.Record(50, 50) // 100 used, budget exhausted

	result := e.Check("AGENT-01", 1)
	if !result.Allowed {
		t.Fatal("AC-1: budget exhaustion should still allow (with fallback)")
	}
	if !result.BudgetExhausted {
		t.Fatal("AC-1: should flag budget as exhausted")
	}
	if result.FallbackProvider != "ollama" {
		t.Fatalf("AC-1: expected fallback to ollama, got %q", result.FallbackProvider)
	}
}

func TestEnforcer_Record(t *testing.T) {
	e := New(newTestConfig())
	e.Record("claude", 1000, 500)

	status := e.Status()
	if status.Budget.HourlyUsed != 1500 {
		t.Fatalf("expected 1500 tokens used, got %d", status.Budget.HourlyUsed)
	}
	if status.Cost.Total == 0 {
		t.Fatal("expected non-zero cost after recording claude usage")
	}
}

func TestEnforcer_StatusDisabled(t *testing.T) {
	cfg := newTestConfig()
	cfg.Enabled = false
	// Manually create an enforcer that's disabled
	e := &Enforcer{enabled: false}
	status := e.Status()
	if status.Budget.HourlyLimit != 0 && status.Cost.Total != 0 {
		t.Fatal("disabled enforcer should return empty status")
	}
}

func TestEnforcer_CheckDisabledAlwaysAllows(t *testing.T) {
	e := &Enforcer{enabled: false}
	result := e.Check("AGENT-01", 999999)
	if !result.Allowed {
		t.Fatal("disabled enforcer should always allow")
	}
}

func TestEnforcer_RecordDisabledNoop(t *testing.T) {
	e := &Enforcer{enabled: false}
	// Should not panic
	e.Record("claude", 1000, 500)
}

func TestEnforcer_E2E_RateLimitAndFallback(t *testing.T) {
	// AC-5: 25 calls at limit 20 → some rate-limited or fallback
	cfg := newTestConfig()
	cfg.RateLimitPerAgent = 60 // 1/sec per agent
	cfg.RateLimitGlobal = 600  // 10/sec global (burst=2 → 20 tokens initially)
	cfg.BurstMultiple = 2
	e := New(cfg)

	now := time.Now()
	e.rateLimiter.nowFunc = func() time.Time { return now }

	allowed := 0
	denied := 0
	for i := 0; i < 25; i++ {
		agentID := "AGENT-01"
		if i%5 == 0 {
			agentID = "AGENT-02" // spread across agents
		}
		result := e.Check(agentID, 100)
		if result.Allowed && !result.RateLimited {
			allowed++
		} else {
			denied++
		}
	}

	if denied == 0 {
		t.Fatal("AC-5: expected some calls to be rate-limited with 25 calls")
	}
	if allowed == 0 {
		t.Fatal("AC-5: expected some calls to be allowed")
	}
}

func TestEnforcer_StatusRateInfo(t *testing.T) {
	e := New(newTestConfig())
	status := e.Status()

	if status.RateInfo.PerAgentRPM != 20 {
		t.Fatalf("expected per_agent_rpm=20, got %d", status.RateInfo.PerAgentRPM)
	}
	if status.RateInfo.GlobalRPM != 300 {
		t.Fatalf("expected global_rpm=300, got %d", status.RateInfo.GlobalRPM)
	}
}
