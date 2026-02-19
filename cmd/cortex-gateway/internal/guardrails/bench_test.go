package guardrails

import (
	"fmt"
	"testing"
)

func BenchmarkRateLimitCheck(b *testing.B) {
	rl := NewRateLimiter(20, 300, 2)
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		rl.Allow(fmt.Sprintf("AGENT-%02d", i%54))
	}
}

func BenchmarkBudgetCheck(b *testing.B) {
	bt := NewBudgetTracker(5_000_000, 50_000_000)
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		bt.Allow(4096)
	}
}

func BenchmarkCostRecord(b *testing.B) {
	prices := map[string]PriceTable{
		"claude": {InputPricePerMToken: 3.0, OutputPricePerMToken: 15.0},
	}
	ct := NewCostTracker(prices)
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		ct.Record("claude", 1000, 500)
	}
}

func BenchmarkEnforcerCheck(b *testing.B) {
	cfg := Config{
		Enabled:            true,
		BudgetHourlyTokens: 5_000_000,
		BudgetDailyTokens:  50_000_000,
		RateLimitPerAgent:  20,
		RateLimitGlobal:    300,
		BurstMultiple:      2,
		FallbackProvider:   "ollama",
		ProviderPrices: map[string]PriceTable{
			"claude": {InputPricePerMToken: 3.0, OutputPricePerMToken: 15.0},
		},
	}
	e := New(cfg)
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		e.Check(fmt.Sprintf("AGENT-%02d", i%54), 4096)
	}
}

func BenchmarkEnforcerRecord(b *testing.B) {
	cfg := Config{
		Enabled:            true,
		BudgetHourlyTokens: 50_000_000_000, // huge limit to avoid exhaustion
		BudgetDailyTokens:  500_000_000_000,
		RateLimitPerAgent:  200,
		RateLimitGlobal:    3000,
		BurstMultiple:      2,
		FallbackProvider:   "ollama",
		ProviderPrices: map[string]PriceTable{
			"claude": {InputPricePerMToken: 3.0, OutputPricePerMToken: 15.0},
		},
	}
	e := New(cfg)
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		e.Record("claude", 1000, 500)
	}
}
