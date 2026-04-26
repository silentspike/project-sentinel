package compiler

import (
	"strings"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/capability"
)

// OrderForCache sorts prompt blocks: static first, then dynamic.
// This maximizes prefix cache hits on providers that support auto-caching
// (e.g. Anthropic's automatic prefix cache).
func OrderForCache(blocks []PromptBlock) []PromptBlock {
	var staticBlocks, dynamicBlocks []PromptBlock
	for _, b := range blocks {
		if b.Static {
			staticBlocks = append(staticBlocks, b)
		} else {
			dynamicBlocks = append(dynamicBlocks, b)
		}
	}
	result := make([]PromptBlock, 0, len(blocks))
	result = append(result, staticBlocks...)
	result = append(result, dynamicBlocks...)
	return result
}

// FormatForProvider renders prompt blocks into a single string
// optimized for the target provider's capabilities.
func FormatForProvider(blocks []PromptBlock, provider string, caps *capability.ProviderCapabilities) string {
	supportsCaching := caps.HasCapability(provider, capability.CapCaching)

	var b strings.Builder
	for i, block := range blocks {
		if i > 0 {
			b.WriteByte('\n')
		}

		// Insert cache boundary marker for providers that support prefix caching.
		// This helps the provider identify where static content ends.
		if supportsCaching && i > 0 && !block.Static && blocks[i-1].Static {
			b.WriteString("---\n")
		}

		b.WriteString(block.Content)
	}
	return b.String()
}

// EstimateTokens provides a rough token count estimate.
// Uses the common heuristic of ~4 characters per token for German text.
func EstimateTokens(s string) int {
	if len(s) == 0 {
		return 0
	}
	// German text averages ~4.5 chars per token due to compound words
	return (len(s) + 3) / 4
}
