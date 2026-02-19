package compiler

import (
	"strings"
	"testing"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/capability"
)

// AC-2: Static blocks before dynamic blocks
func TestOrderForCache_StaticFirst(t *testing.T) {
	blocks := []PromptBlock{
		{Label: "perception", Content: "dynamic", Static: false},
		{Label: "dna", Content: "static1", Static: true},
		{Label: "evolution", Content: "static2", Static: true},
	}

	ordered := OrderForCache(blocks)

	if len(ordered) != 3 {
		t.Fatalf("expected 3 blocks, got %d", len(ordered))
	}

	// First two should be static
	if !ordered[0].Static {
		t.Error("block 0 should be static")
	}
	if !ordered[1].Static {
		t.Error("block 1 should be static")
	}
	if ordered[2].Static {
		t.Error("block 2 should be dynamic")
	}
}

func TestOrderForCache_AllStatic(t *testing.T) {
	blocks := []PromptBlock{
		{Label: "a", Static: true},
		{Label: "b", Static: true},
	}

	ordered := OrderForCache(blocks)
	if len(ordered) != 2 {
		t.Fatalf("expected 2 blocks, got %d", len(ordered))
	}
}

func TestOrderForCache_AllDynamic(t *testing.T) {
	blocks := []PromptBlock{
		{Label: "a", Static: false},
		{Label: "b", Static: false},
	}

	ordered := OrderForCache(blocks)
	if len(ordered) != 2 {
		t.Fatalf("expected 2 blocks, got %d", len(ordered))
	}
}

func TestOrderForCache_Empty(t *testing.T) {
	ordered := OrderForCache(nil)
	if len(ordered) != 0 {
		t.Errorf("expected 0 blocks, got %d", len(ordered))
	}
}

func TestFormatForProvider_WithCacheBoundary(t *testing.T) {
	caps := capability.New()
	blocks := []PromptBlock{
		{Label: "dna", Content: "identity", Static: true},
		{Label: "perception", Content: "live data", Static: false},
	}

	result := FormatForProvider(blocks, "claude", caps)

	// Claude supports caching → should have boundary marker
	if !strings.Contains(result, "---") {
		t.Error("claude output should contain cache boundary marker")
	}
}

func TestFormatForProvider_WithoutCacheBoundary(t *testing.T) {
	caps := capability.New()
	blocks := []PromptBlock{
		{Label: "dna", Content: "identity", Static: true},
		{Label: "perception", Content: "live data", Static: false},
	}

	result := FormatForProvider(blocks, "ollama", caps)

	// Ollama does not support caching → no boundary marker
	if strings.Contains(result, "---") {
		t.Error("ollama output should NOT contain cache boundary marker")
	}
}

func TestEstimateTokens(t *testing.T) {
	tests := []struct {
		input    string
		minToken int
		maxToken int
	}{
		{"", 0, 0},
		{"Hallo", 1, 3},
		{"Ein laengerer deutscher Satz mit Umlauten und Sonderzeichen.", 10, 20},
	}

	for _, tc := range tests {
		tokens := EstimateTokens(tc.input)
		if tokens < tc.minToken || tokens > tc.maxToken {
			t.Errorf("EstimateTokens(%q) = %d, expected [%d, %d]",
				tc.input, tokens, tc.minToken, tc.maxToken)
		}
	}
}
