package proxy

import (
	"math"
	"testing"
	"time"
)

func TestResolveResponseCostProviderReportedWinsIncludingZero(t *testing.T) {
	for _, reported := range []float64{0, 1.25} {
		result := resolveResponseCostAt(PipelineResponse{
			Provider: "claude-code", EffectiveModel: "claude-opus-4-8",
			InputTokens: 1_000_000, OutputTokens: 1_000_000, ReportedCostUSD: &reported,
		}, time.Date(2026, time.July, 19, 0, 0, 0, 0, time.UTC))
		if result.USD != reported || result.Source != CostSourceProviderReported {
			t.Fatalf("reported=%f result=%+v", reported, result)
		}
	}
}

func TestResolveResponseCostUsesModelAndCacheAwareFallback(t *testing.T) {
	result := resolveResponseCostAt(PipelineResponse{
		Provider: "anthropic-direct", EffectiveModel: "claude-opus-4-8",
		InputTokens: 130, OutputTokens: 5, CacheRead: 20, CacheCreation: 10,
	}, time.Date(2026, time.July, 19, 0, 0, 0, 0, time.UTC))
	want := 0.0006975 // fresh=100 at $5, read=20 at $0.50, write=10 at $6.25, output=5 at $25 / MTok
	if math.Abs(result.USD-want) > 1e-12 || result.Source != CostSourceUsagePriceTable {
		t.Fatalf("result=%+v want_usd=%.10f", result, want)
	}
}

func TestSonnetFiveAnnouncedPriceSchedule(t *testing.T) {
	resp := PipelineResponse{
		Provider: "anthropic-direct", EffectiveModel: "claude-sonnet-5", InputTokens: 1_000_000,
	}
	promo := resolveResponseCostAt(resp, time.Date(2026, time.August, 31, 23, 59, 59, 0, time.UTC))
	standard := resolveResponseCostAt(resp, time.Date(2026, time.September, 1, 0, 0, 0, 0, time.UTC))
	if promo.USD != 2 || standard.USD != 3 {
		t.Fatalf("promo=%+v standard=%+v", promo, standard)
	}
}

func TestResolveResponseCostFailsClosedForInvalidOrUnknownPricing(t *testing.T) {
	invalid := math.Inf(1)
	result := resolveResponseCostAt(PipelineResponse{
		Provider: "claude-code", EffectiveModel: "claude-opus-4-8", InputTokens: 1_000_000,
		ReportedCostUSD: &invalid,
	}, time.Now())
	if result.USD != 0 || result.Source != CostSourcePricingUnknown {
		t.Fatalf("unknown pricing result=%+v", result)
	}
	local := resolveResponseCostAt(PipelineResponse{
		Provider: "ollama", EffectiveModel: "qwen3:8b", InputTokens: 1_000_000,
	}, time.Now())
	if local.USD != 0 || local.Source != CostSourcePricingUnknown {
		t.Fatalf("local provider result=%+v", local)
	}
}
