package guardrails

import (
	"sync"
	"time"
)

const (
	claudeOpusInputPricePerMTok  = 15.0
	claudeOpusOutputPricePerMTok = 75.0
)

type RuntimeCostStatus struct {
	ByProvider               map[string]float64 `json:"by_provider"`
	TotalCostUSD             float64            `json:"total_cost_usd"`
	TotalSavingsUSD          float64            `json:"total_savings_usd"`
	AverageForwardCostUSD    float64            `json:"average_forward_cost_usd"`
	ProjectedDailyCostUSD    float64            `json:"projected_daily_cost_usd"`
	ProjectedDailySavingsUSD float64            `json:"projected_daily_savings_usd"`
	ForwardCalls             int64              `json:"forward_calls"`
	SynthesisCount           int64              `json:"synthesis_count"`
	SynthesisRate            float64            `json:"synthesis_rate"`
}

type runtimeCostTracker struct {
	mu              sync.Mutex
	startedAt       time.Time
	byProvider      map[string]float64
	totalCostUSD    float64
	totalSavingsUSD float64
	reportedSavings float64
	forwardCalls    int64
	synthesisCount  int64
}

var runtimeCosts = &runtimeCostTracker{
	startedAt:  time.Now(),
	byProvider: make(map[string]float64),
}

func RecordRuntimeForwardCost(provider string, inputTokens, outputTokens int) float64 {
	cost := calculateForwardCostUSD(provider, inputTokens, outputTokens)
	if cost <= 0 {
		return 0
	}

	runtimeCosts.mu.Lock()
	defer runtimeCosts.mu.Unlock()

	runtimeCosts.byProvider[provider] += cost
	runtimeCosts.totalCostUSD += cost
	runtimeCosts.forwardCalls++
	runtimeCosts.recomputeSavingsLocked()
	recordCostMetric(provider, cost)
	return cost
}

func RecordRuntimeSynthesisSavings() float64 {
	runtimeCosts.mu.Lock()
	defer runtimeCosts.mu.Unlock()

	runtimeCosts.synthesisCount++
	runtimeCosts.recomputeSavingsLocked()
	if runtimeCosts.forwardCalls == 0 {
		return 0
	}
	return runtimeCosts.totalSavingsUSD / float64(runtimeCosts.synthesisCount)
}

func RuntimeCostSnapshot() RuntimeCostStatus {
	runtimeCosts.mu.Lock()
	defer runtimeCosts.mu.Unlock()

	byProvider := make(map[string]float64, len(runtimeCosts.byProvider))
	for provider, cost := range runtimeCosts.byProvider {
		byProvider[provider] = cost
	}

	var avgForwardCost float64
	if runtimeCosts.forwardCalls > 0 {
		avgForwardCost = runtimeCosts.totalCostUSD / float64(runtimeCosts.forwardCalls)
	}

	var synthesisRate float64
	totalCalls := runtimeCosts.forwardCalls + runtimeCosts.synthesisCount
	if totalCalls > 0 {
		synthesisRate = float64(runtimeCosts.synthesisCount) / float64(totalCalls)
	}

	uptimeHours := time.Since(runtimeCosts.startedAt).Hours()
	projectedCost := runtimeCosts.totalCostUSD
	projectedSavings := runtimeCosts.totalSavingsUSD
	if uptimeHours > 0 {
		projectedCost = runtimeCosts.totalCostUSD / uptimeHours * 24
		projectedSavings = runtimeCosts.totalSavingsUSD / uptimeHours * 24
	}

	return RuntimeCostStatus{
		ByProvider:               byProvider,
		TotalCostUSD:             runtimeCosts.totalCostUSD,
		TotalSavingsUSD:          runtimeCosts.totalSavingsUSD,
		AverageForwardCostUSD:    avgForwardCost,
		ProjectedDailyCostUSD:    projectedCost,
		ProjectedDailySavingsUSD: projectedSavings,
		ForwardCalls:             runtimeCosts.forwardCalls,
		SynthesisCount:           runtimeCosts.synthesisCount,
		SynthesisRate:            synthesisRate,
	}
}

func calculateForwardCostUSD(provider string, inputTokens, outputTokens int) float64 {
	switch provider {
	case "anthropic-direct", "claude-code", "claude":
		return (float64(inputTokens)*claudeOpusInputPricePerMTok +
			float64(outputTokens)*claudeOpusOutputPricePerMTok) / 1_000_000
	default:
		return 0
	}
}

func (r *runtimeCostTracker) recomputeSavingsLocked() {
	if r.forwardCalls == 0 || r.synthesisCount == 0 {
		r.totalSavingsUSD = 0
		return
	}

	avgForwardCost := r.totalCostUSD / float64(r.forwardCalls)
	r.totalSavingsUSD = avgForwardCost * float64(r.synthesisCount)
	if diff := r.totalSavingsUSD - r.reportedSavings; diff > 0 {
		synthesisSavingsUSDTotal.Add(diff)
		r.reportedSavings = r.totalSavingsUSD
	}
}

func resetRuntimeCostTrackerForTest() {
	runtimeCosts.mu.Lock()
	defer runtimeCosts.mu.Unlock()

	runtimeCosts.startedAt = time.Now()
	runtimeCosts.byProvider = make(map[string]float64)
	runtimeCosts.totalCostUSD = 0
	runtimeCosts.totalSavingsUSD = 0
	runtimeCosts.reportedSavings = 0
	runtimeCosts.forwardCalls = 0
	runtimeCosts.synthesisCount = 0
}
