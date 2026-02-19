package guardrails

import (
	"sync"
)

// PriceTable holds per-provider token pricing (USD per 1M tokens).
type PriceTable struct {
	InputPricePerMToken  float64 `toml:"input_price_per_m_token"`
	OutputPricePerMToken float64 `toml:"output_price_per_m_token"`
}

// CostStatus represents cost data for the dashboard.
type CostStatus struct {
	ByProvider map[string]float64 `json:"by_provider"`
	Total      float64            `json:"total_usd"`
}

// CostTracker accumulates costs per provider based on token usage and pricing.
type CostTracker struct {
	mu     sync.Mutex
	prices map[string]PriceTable
	costs  map[string]float64
	total  float64
}

// NewCostTracker creates a cost tracker with the given pricing table.
func NewCostTracker(prices map[string]PriceTable) *CostTracker {
	if prices == nil {
		prices = make(map[string]PriceTable)
	}
	return &CostTracker{
		prices: prices,
		costs:  make(map[string]float64),
	}
}

// Record calculates and accumulates cost for a provider call.
func (ct *CostTracker) Record(provider string, inputTokens, outputTokens int) float64 {
	ct.mu.Lock()
	defer ct.mu.Unlock()

	pt, ok := ct.prices[provider]
	if !ok {
		return 0
	}

	cost := float64(inputTokens)/1_000_000*pt.InputPricePerMToken +
		float64(outputTokens)/1_000_000*pt.OutputPricePerMToken

	ct.costs[provider] += cost
	ct.total += cost
	return cost
}

// CostByProvider returns the accumulated cost for a specific provider.
func (ct *CostTracker) CostByProvider(provider string) float64 {
	ct.mu.Lock()
	defer ct.mu.Unlock()
	return ct.costs[provider]
}

// TotalCost returns the total accumulated cost across all providers.
func (ct *CostTracker) TotalCost() float64 {
	ct.mu.Lock()
	defer ct.mu.Unlock()
	return ct.total
}

// Status returns a snapshot for the dashboard endpoint.
func (ct *CostTracker) Status() CostStatus {
	ct.mu.Lock()
	defer ct.mu.Unlock()

	byProvider := make(map[string]float64, len(ct.costs))
	for k, v := range ct.costs {
		byProvider[k] = v
	}
	return CostStatus{
		ByProvider: byProvider,
		Total:      ct.total,
	}
}
