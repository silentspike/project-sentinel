package guardrails

import "testing"

// TestRecordAgentUsage_CacheAware proves #427 AC-1/AC-2: per-agent/tier counters
// are recorded with cache-aware directions and the cost matches the per-provider
// formula on the folded input (exact reconciliation, no drift).
func TestRecordAgentUsage_CacheAware(t *testing.T) {
	const agent, tier = "AGENT-42", "high"

	inBefore := getCounterValue(tokensByAgentTotal, agent, tier, "input")
	outBefore := getCounterValue(tokensByAgentTotal, agent, tier, "output")
	crBefore := getCounterValue(tokensByAgentTotal, agent, tier, "cache_read")
	ccBefore := getCounterValue(tokensByAgentTotal, agent, tier, "cache_creation")
	costBefore := getCounterValue(costByAgentUSDTotal, agent, tier)

	// fresh=1000, output=500, cache_read=200, cache_creation=100
	cost := RecordAgentUsage(agent, tier, "claude-code", 1000, 500, 200, 100)

	if got := getCounterValue(tokensByAgentTotal, agent, tier, "input") - inBefore; got != 1000 {
		t.Fatalf("AC-1: input delta = %.0f, want 1000", got)
	}
	if got := getCounterValue(tokensByAgentTotal, agent, tier, "output") - outBefore; got != 500 {
		t.Fatalf("AC-1: output delta = %.0f, want 500", got)
	}
	if got := getCounterValue(tokensByAgentTotal, agent, tier, "cache_read") - crBefore; got != 200 {
		t.Fatalf("AC-2: cache_read delta = %.0f, want 200", got)
	}
	if got := getCounterValue(tokensByAgentTotal, agent, tier, "cache_creation") - ccBefore; got != 100 {
		t.Fatalf("AC-2: cache_creation delta = %.0f, want 100", got)
	}

	// Cost parity: folded input = 1000+200+100 = 1300, opus pricing.
	wantCost := (1300.0*claudeOpusInputPricePerMTok + 500.0*claudeOpusOutputPricePerMTok) / 1_000_000
	if cost < wantCost-1e-9 || cost > wantCost+1e-9 {
		t.Fatalf("returned cost = %.8f, want %.8f", cost, wantCost)
	}
	if got := getCounterValue(costByAgentUSDTotal, agent, tier) - costBefore; got < wantCost-1e-9 || got > wantCost+1e-9 {
		t.Fatalf("cost counter delta = %.8f, want %.8f", got, wantCost)
	}
}

// TestRecordAgentUsage_SynthesisZeroCost proves a 0-token synthesis call honors
// the JEDER-Call policy at zero cost without fabricating cache directions.
func TestRecordAgentUsage_SynthesisZeroCost(t *testing.T) {
	const agent = "AGENT-07"
	cost := RecordAgentUsage(agent, "synthesis", "synthesis", 0, 0, 0, 0)
	if cost != 0 {
		t.Fatalf("synthesis cost = %.8f, want 0", cost)
	}
	if got := getCounterValue(tokensByAgentTotal, agent, "synthesis", "cache_read"); got != 0 {
		t.Fatalf("synthesis must not create a cache_read series, got %.0f", got)
	}
}

// TestRecordRuntimeAgentUsage_TrafficStats proves #427 AC-3: the traffic-stats
// snapshot exposes cost_by_agent + tokens_by_agent accumulated per agent.
func TestRecordRuntimeAgentUsage_TrafficStats(t *testing.T) {
	resetRuntimeCostTrackerForTest()

	RecordRuntimeAgentUsage("AGENT-03", 1000, 500, 0.05)
	RecordRuntimeAgentUsage("AGENT-03", 200, 100, 0.01)
	RecordRuntimeAgentUsage("AGENT-09", 300, 200, 0.02)

	snap := RuntimeCostSnapshot()
	if len(snap.CostByAgent) == 0 {
		t.Fatal("AC-3: cost_by_agent is empty")
	}
	if c := snap.CostByAgent["AGENT-03"]; c < 0.0599 || c > 0.0601 {
		t.Fatalf("AC-3: cost_by_agent[AGENT-03] = %.4f, want ~0.06", c)
	}
	if tok := snap.TokensByAgent["AGENT-03"]; tok != 1800 {
		t.Fatalf("AC-3: tokens_by_agent[AGENT-03] = %d, want 1800", tok)
	}
	if tok := snap.TokensByAgent["AGENT-09"]; tok != 500 {
		t.Fatalf("AC-3: tokens_by_agent[AGENT-09] = %d, want 500", tok)
	}
}

func TestRecordRuntimeAgentUsage_DimensionsStaySemanticallySeparate(t *testing.T) {
	resetRuntimeCostTrackerForTest()
	RecordRuntimeAgentUsageDimensions("AGENT-03", "low", 1, "usage_price_table", 100, 20, 0.25)
	RecordRuntimeAgentUsageDimensions("AGENT-09", "high", 3, "provider_reported", 200, 40, 0.75)

	snapshot := RuntimeCostSnapshot()
	if snapshot.ByModelTier["low"] != 0.25 || snapshot.ByModelTier["high"] != 0.75 {
		t.Fatalf("model-tier cost dimensions drifted: %#v", snapshot.ByModelTier)
	}
	if snapshot.ByHierarchyTier["1"] != 0.25 || snapshot.ByHierarchyTier["3"] != 0.75 {
		t.Fatalf("hierarchy-tier cost dimensions drifted: %#v", snapshot.ByHierarchyTier)
	}
	if snapshot.ByCostSource["provider_reported"] != 0.75 {
		t.Fatalf("cost-source dimensions drifted: %#v", snapshot.ByCostSource)
	}
}
