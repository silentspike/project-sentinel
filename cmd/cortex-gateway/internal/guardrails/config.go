package guardrails

import (
	"os"
	"strconv"
)

// Config holds all guardrails configuration values.
type Config struct {
	Enabled            bool                  `toml:"enabled"`
	BudgetHourlyTokens int64                 `toml:"budget_hourly_tokens"`
	BudgetDailyTokens  int64                 `toml:"budget_daily_tokens"`
	RateLimitPerAgent  int                   `toml:"rate_limit_per_agent_rpm"`
	RateLimitGlobal    int                   `toml:"rate_limit_global_rpm"`
	BurstMultiple      int                   `toml:"burst_multiple"`
	FallbackProvider   string                `toml:"fallback_provider"`
	ProviderPrices     map[string]PriceTable `toml:"provider_prices"`
}

// DefaultConfig returns a Config with sensible defaults.
func DefaultConfig() Config {
	return Config{
		Enabled:            false,
		BudgetHourlyTokens: 5_000_000,
		BudgetDailyTokens:  50_000_000,
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

// ConfigFromEnv creates a Config from environment variables with sensible defaults.
// Recognized variables:
//
//	SENTINEL_GUARDRAILS_ENABLED          (bool, default: false)
//	SENTINEL_GUARDRAILS_BUDGET_HOURLY    (int64, tokens/hour, default: 5000000)
//	SENTINEL_GUARDRAILS_BUDGET_DAILY     (int64, tokens/day, default: 50000000)
//	SENTINEL_GUARDRAILS_RATE_AGENT_RPM   (int, per-agent calls/min, default: 20)
//	SENTINEL_GUARDRAILS_RATE_GLOBAL_RPM  (int, global calls/min, default: 300)
//	SENTINEL_GUARDRAILS_BURST_MULTIPLE   (int, default: 2)
//	SENTINEL_GUARDRAILS_FALLBACK         (string, default: "ollama")
func ConfigFromEnv() Config {
	cfg := DefaultConfig()

	if v := os.Getenv("SENTINEL_GUARDRAILS_ENABLED"); v == "true" || v == "1" {
		cfg.Enabled = true
	}
	if v, err := strconv.ParseInt(os.Getenv("SENTINEL_GUARDRAILS_BUDGET_HOURLY"), 10, 64); err == nil && v > 0 {
		cfg.BudgetHourlyTokens = v
	}
	if v, err := strconv.ParseInt(os.Getenv("SENTINEL_GUARDRAILS_BUDGET_DAILY"), 10, 64); err == nil && v > 0 {
		cfg.BudgetDailyTokens = v
	}
	if v, err := strconv.Atoi(os.Getenv("SENTINEL_GUARDRAILS_RATE_AGENT_RPM")); err == nil && v > 0 {
		cfg.RateLimitPerAgent = v
	}
	if v, err := strconv.Atoi(os.Getenv("SENTINEL_GUARDRAILS_RATE_GLOBAL_RPM")); err == nil && v > 0 {
		cfg.RateLimitGlobal = v
	}
	if v, err := strconv.Atoi(os.Getenv("SENTINEL_GUARDRAILS_BURST_MULTIPLE")); err == nil && v > 0 {
		cfg.BurstMultiple = v
	}
	if v := os.Getenv("SENTINEL_GUARDRAILS_FALLBACK"); v != "" {
		cfg.FallbackProvider = v
	}

	return cfg
}
