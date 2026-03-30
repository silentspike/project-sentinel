package guardrails

import (
	"math"
	"testing"

	"github.com/prometheus/client_golang/prometheus"
	dto "github.com/prometheus/client_model/go"
)

func getPlainCounterValue(c prometheus.Counter) float64 {
	m := &dto.Metric{}
	_ = c.(prometheus.Metric).Write(m)
	if m.Counter != nil {
		return m.Counter.GetValue()
	}
	return 0
}

func TestRuntimeCostTracker_RecordForwardCost(t *testing.T) {
	resetRuntimeCostTrackerForTest()

	before := getCounterValue(costUSDTotal, "anthropic-direct")
	cost := RecordRuntimeForwardCost("anthropic-direct", 1_000_000, 1_000_000)
	after := getCounterValue(costUSDTotal, "anthropic-direct")

	if math.Abs(cost-90.0) > 1e-9 {
		t.Fatalf("expected cost 90.0, got %.6f", cost)
	}
	if diff := after - before; diff < 89.99 || diff > 90.01 {
		t.Fatalf("expected metric increase ~90.0, got %.6f", diff)
	}

	snapshot := RuntimeCostSnapshot()
	if snapshot.ForwardCalls != 1 {
		t.Fatalf("expected 1 forward call, got %d", snapshot.ForwardCalls)
	}
	if math.Abs(snapshot.TotalCostUSD-90.0) > 1e-9 {
		t.Fatalf("expected total cost 90.0, got %.6f", snapshot.TotalCostUSD)
	}
	if math.Abs(snapshot.AverageForwardCostUSD-90.0) > 1e-9 {
		t.Fatalf("expected avg cost 90.0, got %.6f", snapshot.AverageForwardCostUSD)
	}
}

func TestRuntimeCostTracker_SynthesisSavingsNeedsForwardBaseline(t *testing.T) {
	resetRuntimeCostTrackerForTest()

	before := getPlainCounterValue(synthesisSavingsUSDTotal)
	savings := RecordRuntimeSynthesisSavings()
	after := getPlainCounterValue(synthesisSavingsUSDTotal)

	if savings != 0 {
		t.Fatalf("expected zero savings without forward baseline, got %.6f", savings)
	}
	if after != before {
		t.Fatalf("expected no metric increase without forward baseline, got before=%.6f after=%.6f", before, after)
	}

	snapshot := RuntimeCostSnapshot()
	if snapshot.SynthesisCount != 1 {
		t.Fatalf("expected synthesis count 1, got %d", snapshot.SynthesisCount)
	}
	if snapshot.TotalSavingsUSD != 0 {
		t.Fatalf("expected zero total savings without baseline, got %.6f", snapshot.TotalSavingsUSD)
	}
}

func TestRuntimeCostTracker_SynthesisSavingsUsesAverageForwardCost(t *testing.T) {
	resetRuntimeCostTrackerForTest()

	RecordRuntimeForwardCost("anthropic-direct", 1_000_000, 0) // $15
	RecordRuntimeForwardCost("anthropic-direct", 0, 1_000_000) // $75

	before := getPlainCounterValue(synthesisSavingsUSDTotal)
	savings := RecordRuntimeSynthesisSavings()
	after := getPlainCounterValue(synthesisSavingsUSDTotal)

	if math.Abs(savings-45.0) > 1e-9 {
		t.Fatalf("expected average savings 45.0, got %.6f", savings)
	}
	if diff := after - before; diff < 44.99 || diff > 45.01 {
		t.Fatalf("expected savings metric increase ~45.0, got %.6f", diff)
	}

	snapshot := RuntimeCostSnapshot()
	if snapshot.ForwardCalls != 2 {
		t.Fatalf("expected 2 forward calls, got %d", snapshot.ForwardCalls)
	}
	if snapshot.SynthesisCount != 1 {
		t.Fatalf("expected synthesis count 1, got %d", snapshot.SynthesisCount)
	}
	if math.Abs(snapshot.TotalSavingsUSD-45.0) > 1e-9 {
		t.Fatalf("expected total savings 45.0, got %.6f", snapshot.TotalSavingsUSD)
	}
	if math.Abs(snapshot.SynthesisRate-(1.0/3.0)) > 1e-9 {
		t.Fatalf("expected synthesis rate 1/3, got %.6f", snapshot.SynthesisRate)
	}
}

func TestRuntimeCostTracker_BackfillsSavingsAfterForwardBaselineArrives(t *testing.T) {
	resetRuntimeCostTrackerForTest()

	before := getPlainCounterValue(synthesisSavingsUSDTotal)
	if got := RecordRuntimeSynthesisSavings(); got != 0 {
		t.Fatalf("expected zero savings before baseline, got %.6f", got)
	}
	if diff := getPlainCounterValue(synthesisSavingsUSDTotal) - before; diff != 0 {
		t.Fatalf("expected no counter increase before baseline, got %.6f", diff)
	}

	RecordRuntimeForwardCost("claude-code", 1_000_000, 0) // $15

	snapshot := RuntimeCostSnapshot()
	if math.Abs(snapshot.TotalSavingsUSD-15.0) > 1e-9 {
		t.Fatalf("expected total savings 15.0 after first forward baseline, got %.6f", snapshot.TotalSavingsUSD)
	}
	if diff := getPlainCounterValue(synthesisSavingsUSDTotal) - before; diff < 14.99 || diff > 15.01 {
		t.Fatalf("expected backfilled counter increase ~15.0, got %.6f", diff)
	}
}
