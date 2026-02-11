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
	if pc.HasCapability("openai", CapToolUse) {
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
	if len(caps) != 6 {
		t.Errorf("expected 6 capabilities for claude, got %d", len(caps))
	}
}

func TestListCapabilities_UnknownProvider(t *testing.T) {
	pc := New()
	caps := pc.ListCapabilities("unknown")

	if caps != nil {
		t.Errorf("expected nil capabilities for unknown provider, got %v", caps)
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
