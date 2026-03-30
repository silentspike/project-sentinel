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

// recordRateLimited increments the rate-limited counter.
func recordRateLimited(agentID, reason string) {
	rateLimitedTotal.WithLabelValues(agentID, reason).Inc()
}

// updateBudgetGauges sets the current budget usage gauges.
func updateBudgetGauges(status BudgetStatus) {
	budgetUsedGauge.WithLabelValues("hourly").Set(float64(status.HourlyUsed))
	budgetUsedGauge.WithLabelValues("daily").Set(float64(status.DailyUsed))
}
