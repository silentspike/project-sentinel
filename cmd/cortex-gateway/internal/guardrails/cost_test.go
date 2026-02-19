package guardrails

import (
	"math"
	"testing"
)

func TestCostTracker_RecordClaude(t *testing.T) {
	// AC-3: Claude 1000 input + 500 output
	// Cost = 1000/1M * $3.0 + 500/1M * $15.0 = $0.003 + $0.0075 = $0.0105
	prices := map[string]PriceTable{
		"claude": {InputPricePerMToken: 3.0, OutputPricePerMToken: 15.0},
	}
	ct := NewCostTracker(prices)

	cost := ct.Record("claude", 1000, 500)
	expected := 0.0105
	if math.Abs(cost-expected) > 1e-9 {
		t.Fatalf("AC-3: expected cost %.6f, got %.6f", expected, cost)
	}
}

func TestCostTracker_RecordOllamaFree(t *testing.T) {
	// AC-3: Ollama should be $0
	prices := map[string]PriceTable{
		"ollama": {InputPricePerMToken: 0.0, OutputPricePerMToken: 0.0},
	}
	ct := NewCostTracker(prices)

	cost := ct.Record("ollama", 10000, 5000)
	if cost != 0 {
		t.Fatalf("AC-3: expected ollama cost 0, got %f", cost)
	}
}

func TestCostTracker_UnknownProviderZeroCost(t *testing.T) {
	ct := NewCostTracker(nil)
	cost := ct.Record("unknown", 1000, 500)
	if cost != 0 {
		t.Fatalf("expected 0 for unknown provider, got %f", cost)
	}
}

func TestCostTracker_AccumulatesAcrossRecords(t *testing.T) {
	prices := map[string]PriceTable{
		"claude": {InputPricePerMToken: 3.0, OutputPricePerMToken: 15.0},
	}
	ct := NewCostTracker(prices)

	ct.Record("claude", 1_000_000, 0)  // $3.0
	ct.Record("claude", 0, 1_000_000)  // $15.0
	total := ct.TotalCost()
	expected := 18.0
	if math.Abs(total-expected) > 1e-9 {
		t.Fatalf("expected total %.2f, got %.6f", expected, total)
	}
}

func TestCostTracker_CostByProvider(t *testing.T) {
	prices := map[string]PriceTable{
		"claude": {InputPricePerMToken: 3.0, OutputPricePerMToken: 15.0},
		"ollama": {InputPricePerMToken: 0.0, OutputPricePerMToken: 0.0},
	}
	ct := NewCostTracker(prices)

	ct.Record("claude", 1_000_000, 0) // $3.0
	ct.Record("ollama", 1_000_000, 1_000_000) // $0

	claudeCost := ct.CostByProvider("claude")
	if math.Abs(claudeCost-3.0) > 1e-9 {
		t.Fatalf("expected claude cost 3.0, got %f", claudeCost)
	}

	ollamaCost := ct.CostByProvider("ollama")
	if ollamaCost != 0 {
		t.Fatalf("expected ollama cost 0, got %f", ollamaCost)
	}
}

func TestCostTracker_Status(t *testing.T) {
	prices := map[string]PriceTable{
		"claude": {InputPricePerMToken: 3.0, OutputPricePerMToken: 15.0},
	}
	ct := NewCostTracker(prices)
	ct.Record("claude", 1_000_000, 500_000)

	status := ct.Status()
	if len(status.ByProvider) != 1 {
		t.Fatalf("expected 1 provider in status, got %d", len(status.ByProvider))
	}

	expected := 3.0 + 7.5 // 10.5
	if math.Abs(status.Total-expected) > 1e-9 {
		t.Fatalf("expected total %.2f, got %.6f", expected, status.Total)
	}
}
