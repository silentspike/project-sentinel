package capability

import "sync"

// Capability represents a feature that a provider may or may not support.
type Capability string

const (
	CapStreaming     Capability = "streaming"
	CapToolUse       Capability = "tool_use"
	CapVision        Capability = "vision"
	CapSystemPrompt  Capability = "system_prompt"
	CapJSONMode      Capability = "json_mode"
	CapFunctionCall  Capability = "function_calling"
	CapCaching       Capability = "caching"
	CapPredictedOut  Capability = "predicted_output"
	CapKVRetention   Capability = "kv_retention"
)

// ProviderCapabilities maps providers to their supported capabilities.
type ProviderCapabilities struct {
	mu           sync.RWMutex
	capabilities map[string]map[Capability]bool
}

// New creates a ProviderCapabilities with default known capabilities.
func New() *ProviderCapabilities {
	return &ProviderCapabilities{
		capabilities: map[string]map[Capability]bool{
			"claude": {
				CapStreaming:    true,
				CapToolUse:      true,
				CapVision:       true,
				CapSystemPrompt: true,
				CapJSONMode:     true,
				CapFunctionCall: true,
				CapCaching:      true,
				CapPredictedOut: false,
				CapKVRetention:  false,
			},
			"openai": {
				CapStreaming:    true,
				CapToolUse:      true,
				CapVision:       true,
				CapSystemPrompt: true,
				CapJSONMode:     true,
				CapFunctionCall: true,
				CapCaching:      false,
				CapPredictedOut: true,
				CapKVRetention:  false,
			},
			"ollama": {
				CapStreaming:    true,
				CapToolUse:      false,
				CapVision:       false,
				CapSystemPrompt: true,
				CapJSONMode:     false,
				CapFunctionCall: false,
				CapCaching:      false,
				CapPredictedOut: false,
				CapKVRetention:  true,
			},
		},
	}
}

// HasCapability checks if a provider supports a specific capability.
func (pc *ProviderCapabilities) HasCapability(provider string, cap Capability) bool {
	pc.mu.RLock()
	defer pc.mu.RUnlock()

	caps, ok := pc.capabilities[provider]
	if !ok {
		return false
	}
	return caps[cap]
}

// GetFallback returns an alternative approach when a capability is missing.
func (pc *ProviderCapabilities) GetFallback(provider string, cap Capability) string {
	if pc.HasCapability(provider, cap) {
		return ""
	}

	switch cap {
	case CapToolUse:
		return "parse text response for action patterns"
	case CapJSONMode:
		return "use regex extraction on plain text response"
	case CapVision:
		return "convert image to text description before sending"
	case CapFunctionCall:
		return "embed function signatures in system prompt"
	case CapStreaming:
		return "use synchronous request and poll for completion"
	case CapSystemPrompt:
		return "prepend system instructions to first user message"
	case CapCaching:
		return "no prefix caching available, send full prompt each request"
	case CapPredictedOut:
		return "no predicted output, use standard completion"
	case CapKVRetention:
		return "no KV-cache retention, prompt is reprocessed each request"
	default:
		return "capability not available, no fallback defined"
	}
}

// SetCapability allows runtime capability updates for a provider.
func (pc *ProviderCapabilities) SetCapability(provider string, cap Capability, supported bool) {
	pc.mu.Lock()
	defer pc.mu.Unlock()

	if pc.capabilities[provider] == nil {
		pc.capabilities[provider] = make(map[Capability]bool)
	}
	pc.capabilities[provider][cap] = supported
}

// ListCapabilities returns all capabilities for a given provider.
func (pc *ProviderCapabilities) ListCapabilities(provider string) map[Capability]bool {
	pc.mu.RLock()
	defer pc.mu.RUnlock()

	caps, ok := pc.capabilities[provider]
	if !ok {
		return nil
	}

	result := make(map[Capability]bool, len(caps))
	for k, v := range caps {
		result[k] = v
	}
	return result
}
