package guardrails

import (
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
)

var (
	tokensTotal = promauto.NewCounterVec(prometheus.CounterOpts{
		Name: "sentinel_tokens_total",
		Help: "Total tokens processed by provider and direction",
	}, []string{"provider", "direction"})

	costUSDTotal = promauto.NewCounterVec(prometheus.CounterOpts{
		Name: "sentinel_cost_usd_total",
		Help: "Total accumulated cost in USD by provider",
	}, []string{"provider"})

	// #427: per-agent/tier token + cost counters. direction is cache-aware
	// (input/output/cache_read/cache_creation). These live alongside the
	// per-provider counters on the same passive /metrics endpoint — both are
	// derived from the same provider response (1:n: the response is the SSOT).
	tokensByAgentTotal = promauto.NewCounterVec(prometheus.CounterOpts{
		Name: "sentinel_tokens_by_agent_total",
		Help: "Total tokens by agent, tier and cache-aware direction",
	}, []string{"agent_id", "tier", "direction"})

	costByAgentUSDTotal = promauto.NewCounterVec(prometheus.CounterOpts{
		Name: "sentinel_cost_by_agent_usd_total",
		Help: "Total accumulated cost in USD by agent and tier",
	}, []string{"agent_id", "tier"})

	synthesisSavingsUSDTotal = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_synthesis_savings_usd_total",
		Help: "Estimated USD saved by synthesis responses versus real forward calls",
	})

	rateLimitedTotal = promauto.NewCounterVec(prometheus.CounterOpts{
		Name: "sentinel_rate_limited_total",
		Help: "Total rate-limited requests by agent and reason",
	}, []string{"agent_id", "reason"})

	budgetUsedGauge = promauto.NewGaugeVec(prometheus.GaugeOpts{
		Name: "sentinel_budget_used_tokens",
		Help: "Current token budget usage by time window",
	}, []string{"window"})

	budgetExhaustedTotal = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_budget_exhausted_total",
		Help: "Total times budget was exhausted triggering fallback",
	})
)

// recordTokenMetrics updates prometheus counters for a provider call.
func recordTokenMetrics(provider string, inputTokens, outputTokens int) {
	tokensTotal.WithLabelValues(provider, "input").Add(float64(inputTokens))
	tokensTotal.WithLabelValues(provider, "output").Add(float64(outputTokens))
}

// recordCostMetric updates the cost counter for a provider.
func recordCostMetric(provider string, cost float64) {
	costUSDTotal.WithLabelValues(provider).Add(cost)
}

// RecordAgentUsage records cache-aware per-agent/tier token counters and the
// derived per-agent/tier cost (#427). rawInput is the fresh (non-cached) input;
// the folded input used for cost parity with the per-provider cost is
// rawInput+cacheRead+cacheCreation. A token direction series is only created
// when its value is non-zero, so cache directions appear exactly when cache
// traffic occurred (AC-2). The cost series is always touched so an agent is
// visible even on a zero-cost synthesis/apicp call. Returns the computed cost.
func RecordAgentUsage(agentID, tier, provider string, rawInput, output, cacheRead, cacheCreation int) float64 {
	if rawInput != 0 {
		tokensByAgentTotal.WithLabelValues(agentID, tier, "input").Add(float64(rawInput))
	}
	if output != 0 {
		tokensByAgentTotal.WithLabelValues(agentID, tier, "output").Add(float64(output))
	}
	if cacheRead != 0 {
		tokensByAgentTotal.WithLabelValues(agentID, tier, "cache_read").Add(float64(cacheRead))
	}
	if cacheCreation != 0 {
		tokensByAgentTotal.WithLabelValues(agentID, tier, "cache_creation").Add(float64(cacheCreation))
	}
	cost := calculateForwardCostUSD(provider, rawInput+cacheRead+cacheCreation, output)
	costByAgentUSDTotal.WithLabelValues(agentID, tier).Add(cost)
	return cost
}

// recordRateLimited increments the rate-limited counter.
func recordRateLimited(agentID, reason string) {
	rateLimitedTotal.WithLabelValues(agentID, reason).Inc()
}

// updateBudgetGauges sets the current budget usage gauges.
func updateBudgetGauges(status BudgetStatus) {
	budgetUsedGauge.WithLabelValues("hourly").Set(float64(status.HourlyUsed))
	budgetUsedGauge.WithLabelValues("daily").Set(float64(status.DailyUsed))
}
