package capability

import (
	"testing"
)

func TestHasCapability_Claude_ToolUse(t *testing.T) {
	pc := New()
	if !pc.HasCapability("claude", CapToolUse) {
		t.Error("claude should support tool_use")
	}
}

func TestHasCapability_Claude_AllCapabilities(t *testing.T) {
	pc := New()

	caps := []Capability{
		CapStreaming, CapToolUse, CapVision,
		CapSystemPrompt, CapJSONMode, CapFunctionCall,
	}
	for _, cap := range caps {
		if !pc.HasCapability("claude", cap) {
			t.Errorf("claude should support %s", cap)
		}
	}
}

func TestHasCapability_Ollama_ToolUse(t *testing.T) {
	pc := New()
	if pc.HasCapability("ollama", CapToolUse) {
		t.Error("ollama should NOT support tool_use")
	}
}

func TestHasCapability_Ollama_Streaming(t *testing.T) {
	pc := New()
	if !pc.HasCapability("ollama", CapStreaming) {
		t.Error("ollama should support streaming")
	}
}

func TestHasCapability_Ollama_SystemPrompt(t *testing.T) {
	pc := New()
	if !pc.HasCapability("ollama", CapSystemPrompt) {
		t.Error("ollama should support system_prompt")
	}
}

func TestHasCapability_UnknownProvider(t *testing.T) {
	pc := New()
	if pc.HasCapability("mistral", CapToolUse) {
		t.Error("unknown provider should not have any capabilities")
	}
}

func TestGetFallback_NoToolUse(t *testing.T) {
	pc := New()
	fb := pc.GetFallback("ollama", CapToolUse)

	if fb == "" {
		t.Error("expected non-empty fallback for missing tool_use")
	}
	if fb != "parse text response for action patterns" {
		t.Errorf("fallback = %q, want %q", fb, "parse text response for action patterns")
	}
}

func TestGetFallback_NoJSONMode(t *testing.T) {
	pc := New()
	fb := pc.GetFallback("ollama", CapJSONMode)

	if fb == "" {
		t.Error("expected non-empty fallback for missing json_mode")
	}
}

func TestGetFallback_HasCapability(t *testing.T) {
	pc := New()
	fb := pc.GetFallback("claude", CapToolUse)

	if fb != "" {
		t.Errorf("expected empty fallback when capability exists, got %q", fb)
	}
}

func TestSetCapability(t *testing.T) {
	pc := New()

	// Ollama initially has no tool_use
	if pc.HasCapability("ollama", CapToolUse) {
		t.Fatal("ollama should not have tool_use initially")
	}

	// Enable tool_use for ollama
	pc.SetCapability("ollama", CapToolUse, true)

	if !pc.HasCapability("ollama", CapToolUse) {
		t.Error("ollama should have tool_use after SetCapability")
	}
}

func TestSetCapability_NewProvider(t *testing.T) {
	pc := New()

	pc.SetCapability("custom-llm", CapStreaming, true)
	pc.SetCapability("custom-llm", CapSystemPrompt, true)

	if !pc.HasCapability("custom-llm", CapStreaming) {
		t.Error("custom-llm should support streaming after SetCapability")
	}
	if !pc.HasCapability("custom-llm", CapSystemPrompt) {
		t.Error("custom-llm should support system_prompt after SetCapability")
	}
	if pc.HasCapability("custom-llm", CapVision) {
		t.Error("custom-llm should NOT support vision (not set)")
	}
}

func TestListCapabilities(t *testing.T) {
	pc := New()
	caps := pc.ListCapabilities("claude")

	if caps == nil {
		t.Fatal("expected non-nil capabilities for claude")
	}
	if len(caps) != 9 {
		t.Errorf("expected 9 capabilities for claude, got %d", len(caps))
	}
}

func TestListCapabilities_UnknownProvider(t *testing.T) {
	pc := New()
	caps := pc.ListCapabilities("unknown")

	if caps != nil {
		t.Errorf("expected nil capabilities for unknown provider, got %v", caps)
	}
}

// AC-3: Extended capability detection for all providers
func TestHasCapability_Claude_Caching(t *testing.T) {
	pc := New()
	if !pc.HasCapability("claude", CapCaching) {
		t.Error("claude should support caching")
	}
}

func TestHasCapability_Claude_NoPredictedOut(t *testing.T) {
	pc := New()
	if pc.HasCapability("claude", CapPredictedOut) {
		t.Error("claude should NOT support predicted_output")
	}
}

func TestHasCapability_OpenAI_AllCapabilities(t *testing.T) {
	pc := New()

	expected := map[Capability]bool{
		CapStreaming:    true,
		CapToolUse:      true,
		CapVision:       true,
		CapSystemPrompt: true,
		CapJSONMode:     true,
		CapFunctionCall: true,
		CapCaching:      false,
		CapPredictedOut: true,
		CapKVRetention:  false,
	}

	for cap, want := range expected {
		got := pc.HasCapability("openai", cap)
		if got != want {
			t.Errorf("openai %s = %v, want %v", cap, got, want)
		}
	}
}

func TestHasCapability_Ollama_KVRetention(t *testing.T) {
	pc := New()
	if !pc.HasCapability("ollama", CapKVRetention) {
		t.Error("ollama should support kv_retention")
	}
}

func TestHasCapability_Ollama_NoCaching(t *testing.T) {
	pc := New()
	if pc.HasCapability("ollama", CapCaching) {
		t.Error("ollama should NOT support caching")
	}
}

func TestGetFallback_NoCaching(t *testing.T) {
	pc := New()
	fb := pc.GetFallback("ollama", CapCaching)
	if fb == "" {
		t.Error("expected non-empty fallback for missing caching")
	}
}

func TestGetFallback_NoPredictedOut(t *testing.T) {
	pc := New()
	fb := pc.GetFallback("claude", CapPredictedOut)
	if fb == "" {
		t.Error("expected non-empty fallback for missing predicted_output")
	}
}

func TestGetFallback_NoKVRetention(t *testing.T) {
	pc := New()
	fb := pc.GetFallback("claude", CapKVRetention)
	if fb == "" {
		t.Error("expected non-empty fallback for missing kv_retention")
	}
}

func TestListCapabilities_OpenAI(t *testing.T) {
	pc := New()
	caps := pc.ListCapabilities("openai")
	if caps == nil {
		t.Fatal("expected non-nil capabilities for openai")
	}
	if len(caps) != 9 {
		t.Errorf("expected 9 capabilities for openai, got %d", len(caps))
	}
}

func TestHasCapability_ClaudeCode_AllCapabilities(t *testing.T) {
	pc := New()

	expected := map[Capability]bool{
		CapStreaming:    true,
		CapToolUse:      true,
		CapVision:       true,
		CapSystemPrompt: true,
		CapJSONMode:     true,
		CapFunctionCall: true,
		CapCaching:      false,
		CapPredictedOut: false,
		CapKVRetention:  false,
	}

	for cap, want := range expected {
		got := pc.HasCapability("claude-code", cap)
		if got != want {
			t.Errorf("claude-code %s = %v, want %v", cap, got, want)
		}
	}
}

func TestListCapabilities_ClaudeCode(t *testing.T) {
	pc := New()
	caps := pc.ListCapabilities("claude-code")
	if caps == nil {
		t.Fatal("expected non-nil capabilities for claude-code")
	}
	if len(caps) != 9 {
		t.Errorf("expected 9 capabilities for claude-code, got %d", len(caps))
	}
}

func TestGetFallback_ClaudeCode_NoCaching(t *testing.T) {
	pc := New()
	fb := pc.GetFallback("claude-code", CapCaching)
	if fb == "" {
		t.Error("expected non-empty fallback for claude-code missing caching")
	}
}

func TestListCapabilities_IsCopy(t *testing.T) {
	pc := New()
	caps := pc.ListCapabilities("claude")

	// Mutating the returned map should not affect internal state
	caps[CapToolUse] = false

	if !pc.HasCapability("claude", CapToolUse) {
		t.Error("modifying returned map should not affect internal state")
	}
}
