package guardrails

import (
	"testing"
)

func TestDefaultConfig(t *testing.T) {
	cfg := DefaultConfig()

	if cfg.Enabled {
		t.Fatal("default config should be disabled")
	}
	if cfg.BudgetHourlyTokens != 5_000_000 {
		t.Fatalf("expected 5M hourly, got %d", cfg.BudgetHourlyTokens)
	}
	if cfg.BudgetDailyTokens != 50_000_000 {
		t.Fatalf("expected 50M daily, got %d", cfg.BudgetDailyTokens)
	}
	if cfg.RateLimitPerAgent != 20 {
		t.Fatalf("expected 20 rpm, got %d", cfg.RateLimitPerAgent)
	}
	if cfg.RateLimitGlobal != 300 {
		t.Fatalf("expected 300 global rpm, got %d", cfg.RateLimitGlobal)
	}
	if cfg.FallbackProvider != "ollama" {
		t.Fatalf("expected ollama fallback, got %q", cfg.FallbackProvider)
	}
}

func TestConfigFromEnv_Defaults(t *testing.T) {
	// Ensure env vars are not set
	for _, key := range []string{
		"SENTINEL_GUARDRAILS_ENABLED",
		"SENTINEL_GUARDRAILS_BUDGET_HOURLY",
		"SENTINEL_GUARDRAILS_BUDGET_DAILY",
		"SENTINEL_GUARDRAILS_RATE_AGENT_RPM",
		"SENTINEL_GUARDRAILS_RATE_GLOBAL_RPM",
		"SENTINEL_GUARDRAILS_BURST_MULTIPLE",
		"SENTINEL_GUARDRAILS_FALLBACK",
	} {
		t.Setenv(key, "") // t.Setenv restores after test
	}

	cfg := ConfigFromEnv()
	def := DefaultConfig()

	if cfg.Enabled != def.Enabled {
		t.Fatal("ConfigFromEnv should match defaults when no env vars set")
	}
	if cfg.BudgetHourlyTokens != def.BudgetHourlyTokens {
		t.Fatal("hourly budget should match default")
	}
}

func TestConfigFromEnv_OverrideEnabled(t *testing.T) {
	t.Setenv("SENTINEL_GUARDRAILS_ENABLED", "true")

	cfg := ConfigFromEnv()
	if !cfg.Enabled {
		t.Fatal("expected enabled=true")
	}
}

func TestConfigFromEnv_OverrideBudget(t *testing.T) {
	t.Setenv("SENTINEL_GUARDRAILS_BUDGET_HOURLY", "1000000")
	t.Setenv("SENTINEL_GUARDRAILS_BUDGET_DAILY", "20000000")

	cfg := ConfigFromEnv()
	if cfg.BudgetHourlyTokens != 1_000_000 {
		t.Fatalf("expected 1M hourly, got %d", cfg.BudgetHourlyTokens)
	}
	if cfg.BudgetDailyTokens != 20_000_000 {
		t.Fatalf("expected 20M daily, got %d", cfg.BudgetDailyTokens)
	}
}

func TestConfigFromEnv_OverrideRateLimits(t *testing.T) {
	t.Setenv("SENTINEL_GUARDRAILS_RATE_AGENT_RPM", "10")
	t.Setenv("SENTINEL_GUARDRAILS_RATE_GLOBAL_RPM", "150")
	t.Setenv("SENTINEL_GUARDRAILS_BURST_MULTIPLE", "3")

	cfg := ConfigFromEnv()
	if cfg.RateLimitPerAgent != 10 {
		t.Fatalf("expected 10, got %d", cfg.RateLimitPerAgent)
	}
	if cfg.RateLimitGlobal != 150 {
		t.Fatalf("expected 150, got %d", cfg.RateLimitGlobal)
	}
	if cfg.BurstMultiple != 3 {
		t.Fatalf("expected 3, got %d", cfg.BurstMultiple)
	}
}

func TestConfigFromEnv_OverrideFallback(t *testing.T) {
	t.Setenv("SENTINEL_GUARDRAILS_FALLBACK", "bitnet")

	cfg := ConfigFromEnv()
	if cfg.FallbackProvider != "bitnet" {
		t.Fatalf("expected bitnet, got %q", cfg.FallbackProvider)
	}
}

func TestConfigFromEnv_InvalidValues(t *testing.T) {
	t.Setenv("SENTINEL_GUARDRAILS_BUDGET_HOURLY", "notanumber")
	t.Setenv("SENTINEL_GUARDRAILS_RATE_AGENT_RPM", "-5")

	cfg := ConfigFromEnv()
	// Invalid values should keep defaults
	if cfg.BudgetHourlyTokens != 5_000_000 {
		t.Fatalf("expected default 5M, got %d", cfg.BudgetHourlyTokens)
	}
	// Negative number: our check is v > 0, so -5 is rejected
	if cfg.RateLimitPerAgent != 20 {
		t.Fatalf("expected default 20, got %d", cfg.RateLimitPerAgent)
	}
}
