package guardrails

import (
	"testing"

	"github.com/prometheus/client_golang/prometheus"
	dto "github.com/prometheus/client_model/go"
)

func getCounterValue(cv *prometheus.CounterVec, labels ...string) float64 {
	m := &dto.Metric{}
	c, err := cv.GetMetricWithLabelValues(labels...)
	if err != nil {
		return 0
	}
	_ = c.(prometheus.Metric).Write(m)
	if m.Counter != nil {
		return m.Counter.GetValue()
	}
	return 0
}

func getGaugeValue(gv *prometheus.GaugeVec, labels ...string) float64 {
	m := &dto.Metric{}
	g, err := gv.GetMetricWithLabelValues(labels...)
	if err != nil {
		return 0
	}
	_ = g.(prometheus.Metric).Write(m)
	if m.Gauge != nil {
		return m.Gauge.GetValue()
	}
	return 0
}

func TestMetrics_RecordTokens(t *testing.T) {
	// AC-4: Token metrics
	before := getCounterValue(tokensTotal, "test-provider", "input")
	recordTokenMetrics("test-provider", 1000, 500)
	after := getCounterValue(tokensTotal, "test-provider", "input")

	if after-before != 1000 {
		t.Fatalf("AC-4: expected input tokens increase of 1000, got %.0f", after-before)
	}

	outputAfter := getCounterValue(tokensTotal, "test-provider", "output")
	if outputAfter < 500 {
		t.Fatalf("AC-4: expected output tokens >= 500, got %.0f", outputAfter)
	}
}

func TestMetrics_RecordCost(t *testing.T) {
	// AC-4: Cost metrics
	before := getCounterValue(costUSDTotal, "test-cost-provider")
	recordCostMetric("test-cost-provider", 1.23)
	after := getCounterValue(costUSDTotal, "test-cost-provider")

	diff := after - before
	if diff < 1.22 || diff > 1.24 {
		t.Fatalf("AC-4: expected cost increase ~1.23, got %.4f", diff)
	}
}

func TestMetrics_RecordRateLimited(t *testing.T) {
	// AC-4: Rate limit metrics
	before := getCounterValue(rateLimitedTotal, "AGENT-TEST", "per_agent")
	recordRateLimited("AGENT-TEST", "per_agent")
	after := getCounterValue(rateLimitedTotal, "AGENT-TEST", "per_agent")

	if after-before != 1 {
		t.Fatalf("AC-4: expected rate limited counter increase of 1, got %.0f", after-before)
	}
}

func TestMetrics_UpdateBudgetGauges(t *testing.T) {
	// AC-4: Budget gauges
	updateBudgetGauges(BudgetStatus{
		HourlyUsed: 5000,
		DailyUsed:  40000,
	})

	hourly := getGaugeValue(budgetUsedGauge, "hourly")
	if hourly != 5000 {
		t.Fatalf("AC-4: expected hourly gauge 5000, got %.0f", hourly)
	}

	daily := getGaugeValue(budgetUsedGauge, "daily")
	if daily != 40000 {
		t.Fatalf("AC-4: expected daily gauge 40000, got %.0f", daily)
	}
}
