package guardrails

import (
	"strconv"
	"sync"
	"time"
)

const (
	claudeOpusInputPricePerMTok  = 15.0
	claudeOpusOutputPricePerMTok = 75.0
)

type RuntimeCostStatus struct {
	ByProvider               map[string]float64 `json:"by_provider"`
	CostByAgent              map[string]float64 `json:"cost_by_agent"`
	TokensByAgent            map[string]int64   `json:"tokens_by_agent"`
	ByModelTier              map[string]float64 `json:"by_model_tier"`
	ByHierarchyTier          map[string]float64 `json:"by_hierarchy_tier"`
	ByCostSource             map[string]float64 `json:"by_cost_source"`
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
	byAgent         map[string]float64
	tokensByAgent   map[string]int64
	byModelTier     map[string]float64
	byHierarchyTier map[string]float64
	byCostSource    map[string]float64
	totalCostUSD    float64
	totalSavingsUSD float64
	reportedSavings float64
	forwardCalls    int64
	synthesisCount  int64
}

var runtimeCosts = &runtimeCostTracker{
	startedAt:       time.Now(),
	byProvider:      make(map[string]float64),
	byAgent:         make(map[string]float64),
	tokensByAgent:   make(map[string]int64),
	byModelTier:     make(map[string]float64),
	byHierarchyTier: make(map[string]float64),
	byCostSource:    make(map[string]float64),
}

func RecordRuntimeForwardCost(provider string, inputTokens, outputTokens int) float64 {
	cost := calculateForwardCostUSD(provider, inputTokens, outputTokens)
	if cost <= 0 {
		return 0
	}
	RecordRuntimeForwardCostResolved(provider, cost)
	return cost
}

// RecordRuntimeForwardCostResolved accumulates an authoritative cost selected
// by the gateway. A valid zero is retained as a forward call without inventing
// a price-table value.
func RecordRuntimeForwardCostResolved(provider string, cost float64) {
	if cost < 0 {
		return
	}

	runtimeCosts.mu.Lock()
	defer runtimeCosts.mu.Unlock()

	runtimeCosts.byProvider[provider] += cost
	runtimeCosts.totalCostUSD += cost
	runtimeCosts.forwardCalls++
	runtimeCosts.recomputeSavingsLocked()
	recordCostMetric(provider, cost)
}

// RecordRuntimeAgentUsage accumulates per-agent cost + token totals for the
// /control/traffic-stats aggregate (#427 AC-3). The cost is the already-computed
// per-call cost (parity with the /metrics per-agent cost counter); tokens is the
// folded input+output. Zero-cost synthesis/apicp calls still register the agent.
func RecordRuntimeAgentUsage(agentID string, inputTokens, outputTokens int, cost float64) {
	RecordRuntimeAgentUsageDimensions(agentID, "unknown", 0, "unknown", inputTokens, outputTokens, cost)
}

// RecordRuntimeAgentUsageDimensions keeps model/pricing tier and organization
// hierarchy tier in independent runtime aggregates.
func RecordRuntimeAgentUsageDimensions(agentID, modelTier string, hierarchyTier int, costSource string, inputTokens, outputTokens int, cost float64) {
	if agentID == "" {
		agentID = "unknown"
	}
	runtimeCosts.mu.Lock()
	defer runtimeCosts.mu.Unlock()
	if runtimeCosts.byAgent == nil {
		runtimeCosts.byAgent = make(map[string]float64)
	}
	if runtimeCosts.tokensByAgent == nil {
		runtimeCosts.tokensByAgent = make(map[string]int64)
	}
	if runtimeCosts.byModelTier == nil {
		runtimeCosts.byModelTier = make(map[string]float64)
	}
	if runtimeCosts.byHierarchyTier == nil {
		runtimeCosts.byHierarchyTier = make(map[string]float64)
	}
	if runtimeCosts.byCostSource == nil {
		runtimeCosts.byCostSource = make(map[string]float64)
	}
	runtimeCosts.byAgent[agentID] += cost
	runtimeCosts.tokensByAgent[agentID] += int64(inputTokens + outputTokens)
	runtimeCosts.byModelTier[modelTier] += cost
	if hierarchyTier >= 1 && hierarchyTier <= 3 {
		runtimeCosts.byHierarchyTier[strconv.Itoa(hierarchyTier)] += cost
	}
	runtimeCosts.byCostSource[costSource] += cost
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

	byAgent := make(map[string]float64, len(runtimeCosts.byAgent))
	for agent, cost := range runtimeCosts.byAgent {
		byAgent[agent] = cost
	}
	tokensByAgent := make(map[string]int64, len(runtimeCosts.tokensByAgent))
	for agent, tokens := range runtimeCosts.tokensByAgent {
		tokensByAgent[agent] = tokens
	}
	byModelTier := make(map[string]float64, len(runtimeCosts.byModelTier))
	for tier, cost := range runtimeCosts.byModelTier {
		byModelTier[tier] = cost
	}
	byHierarchyTier := make(map[string]float64, len(runtimeCosts.byHierarchyTier))
	for tier, cost := range runtimeCosts.byHierarchyTier {
		byHierarchyTier[tier] = cost
	}
	byCostSource := make(map[string]float64, len(runtimeCosts.byCostSource))
	for source, cost := range runtimeCosts.byCostSource {
		byCostSource[source] = cost
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
		CostByAgent:              byAgent,
		TokensByAgent:            tokensByAgent,
		ByModelTier:              byModelTier,
		ByHierarchyTier:          byHierarchyTier,
		ByCostSource:             byCostSource,
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
	runtimeCosts.byAgent = make(map[string]float64)
	runtimeCosts.tokensByAgent = make(map[string]int64)
	runtimeCosts.byModelTier = make(map[string]float64)
	runtimeCosts.byHierarchyTier = make(map[string]float64)
	runtimeCosts.byCostSource = make(map[string]float64)
	runtimeCosts.totalCostUSD = 0
	runtimeCosts.totalSavingsUSD = 0
	runtimeCosts.reportedSavings = 0
	runtimeCosts.forwardCalls = 0
	runtimeCosts.synthesisCount = 0
}
