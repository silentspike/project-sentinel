package guardrails

// FallbackHandler decides which provider to use when budget is exhausted.
type FallbackHandler struct {
	fallbackProvider string
}

// NewFallbackHandler creates a handler that falls back to the given provider.
func NewFallbackHandler(fallbackProvider string) *FallbackHandler {
	if fallbackProvider == "" {
		fallbackProvider = "ollama"
	}
	return &FallbackHandler{fallbackProvider: fallbackProvider}
}

// ShouldFallback returns the fallback provider name if budget is exhausted.
// Returns ("", false) if no fallback is needed.
func (fh *FallbackHandler) ShouldFallback(budgetExhausted bool) (string, bool) {
	if !budgetExhausted {
		return "", false
	}
	return fh.fallbackProvider, true
}
