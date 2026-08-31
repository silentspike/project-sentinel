package proxy

import (
	"math"
	"time"
)

const (
	CostSourceProviderReported = "provider_reported"
	CostSourceUsagePriceTable  = "usage_price_table"
	CostSourcePricingUnknown   = "pricing_unknown"
	CostSourceNonProviderZero  = "non_provider_zero"
)

type CostResult struct {
	USD    float64
	Source string
}

type modelPrice struct {
	inputPerMTok        float64
	outputPerMTok       float64
	cacheReadPerMTok    float64
	cacheWrite5mPerMTok float64
}

// modelPriceAt returns standard global Claude API prices. Sources and dates:
//   - Opus 4.8: https://www.anthropic.com/news/claude-opus-4-8,
//     effective 2026-05-28, $5/$25 per MTok.
//   - Sonnet 5: Anthropic launch announcement, $2/$10 through 2026-08-31 and
//     announced $3/$15 from 2026-09-01:
//     https://www.anthropic.com/news/claude-sonnet-5.
//   - Haiku 4.5: https://www.anthropic.com/news/claude-haiku-4-5,
//     effective 2025-10-15, $1/$5.
//
// Anthropic's pricing documentation specifies 5-minute cache writes at 1.25x
// base input and cache reads at 0.1x. Usage does not expose a 1-hour-cache flag,
// so this fallback never claims the 1-hour rate:
// https://docs.anthropic.com/en/docs/about-claude/pricing.
func modelPriceAt(model string, at time.Time) (modelPrice, bool) {
	var input, output float64
	switch model {
	case "claude-opus-4-8":
		input, output = 5, 25
	case "claude-sonnet-5":
		if at.UTC().Before(time.Date(2026, time.September, 1, 0, 0, 0, 0, time.UTC)) {
			input, output = 2, 10
		} else {
			input, output = 3, 15
		}
	case "claude-haiku-4-5-20251001":
		input, output = 1, 5
	default:
		return modelPrice{}, false
	}
	return modelPrice{
		inputPerMTok:        input,
		outputPerMTok:       output,
		cacheReadPerMTok:    input * 0.1,
		cacheWrite5mPerMTok: input * 1.25,
	}, true
}

// openAIModelPrice returns the public OpenAI API rate-card equivalent for the
// Codex models. A ChatGPT-plan invocation does not expose a per-request billed
// amount, so callers must retain CostSourceUsagePriceTable rather than treating
// this estimate as provider-reported spend. The public model pages also state
// that requests above 272K input tokens use 2x input and 1.5x output rates for
// the complete request. Sources, accessed 2026-08-31:
// https://developers.openai.com/api/docs/models/gpt-5.6-sol
// https://developers.openai.com/api/docs/models/gpt-5.6-terra
// https://developers.openai.com/api/docs/models/gpt-5.6-luna
func openAIModelPrice(model string) (modelPrice, bool) {
	var input, output, cacheRead float64
	switch model {
	case "gpt-5.6-sol":
		input, output, cacheRead = 4, 20, 0.4
	case "gpt-5.6-terra":
		input, output, cacheRead = 2, 12, 0.2
	case "gpt-5.6-luna":
		input, output, cacheRead = 0.2, 1.2, 0.02
	default:
		return modelPrice{}, false
	}
	return modelPrice{
		inputPerMTok:        input,
		outputPerMTok:       output,
		cacheReadPerMTok:    cacheRead,
		cacheWrite5mPerMTok: input * 1.25,
	}, true
}

func resolveResponseCostAt(resp PipelineResponse, at time.Time) CostResult {
	if resp.ReportedCostUSD != nil {
		if !math.IsNaN(*resp.ReportedCostUSD) && !math.IsInf(*resp.ReportedCostUSD, 0) && *resp.ReportedCostUSD >= 0 {
			return CostResult{USD: *resp.ReportedCostUSD, Source: CostSourceProviderReported}
		}
		// A malformed reported value is not the same as an absent value. Do not
		// silently replace provider telemetry with a local estimate.
		return CostResult{Source: CostSourcePricingUnknown}
	}

	switch resp.Provider {
	case LocalLoopProviderName, "mock", "synthesis", "apicp", "intercept":
		return CostResult{Source: CostSourceNonProviderZero}
	case "ollama":
		// Local execution has no provider-reported USD value, but zero vendor
		// price is not a verified total-cost price table for the host runtime.
		return CostResult{Source: CostSourcePricingUnknown}
	case "anthropic-direct", "claude-code":
		price, ok := modelPriceAt(resp.EffectiveModel, at)
		if !ok {
			return CostResult{Source: CostSourcePricingUnknown}
		}
		freshInput := nonNegative(resp.InputTokens - resp.CacheRead - resp.CacheCreation)
		usd := (float64(freshInput)*price.inputPerMTok +
			float64(nonNegative(resp.OutputTokens))*price.outputPerMTok +
			float64(nonNegative(resp.CacheRead))*price.cacheReadPerMTok +
			float64(nonNegative(resp.CacheCreation))*price.cacheWrite5mPerMTok) / 1_000_000
		return CostResult{USD: usd, Source: CostSourceUsagePriceTable}
	case CodexCLIProviderName:
		price, ok := openAIModelPrice(resp.EffectiveModel)
		if !ok {
			return CostResult{Source: CostSourcePricingUnknown}
		}
		outputMultiplier := 1.0
		if resp.InputTokens > 272_000 {
			price.inputPerMTok *= 2
			price.cacheReadPerMTok *= 2
			price.cacheWrite5mPerMTok *= 2
			outputMultiplier = 1.5
		}
		freshInput := nonNegative(resp.InputTokens - resp.CacheRead - resp.CacheCreation)
		usd := (float64(freshInput)*price.inputPerMTok +
			float64(nonNegative(resp.OutputTokens))*price.outputPerMTok*outputMultiplier +
			float64(nonNegative(resp.CacheRead))*price.cacheReadPerMTok +
			float64(nonNegative(resp.CacheCreation))*price.cacheWrite5mPerMTok) / 1_000_000
		return CostResult{USD: usd, Source: CostSourceUsagePriceTable}
	default:
		return CostResult{Source: CostSourcePricingUnknown}
	}
}

func nonNegative(value int) int {
	if value < 0 {
		return 0
	}
	return value
}
