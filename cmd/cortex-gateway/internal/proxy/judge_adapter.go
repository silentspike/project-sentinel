package proxy

import "context"

// JudgeProviderAdapter adaptiert einen proxy.Provider (Send mit *LLMRequest)
// auf das detection.LLMProvider Interface (Send mit einzelnen Parametern).
type JudgeProviderAdapter struct {
	provider Provider
}

// NewJudgeProviderAdapter erstellt einen Adapter fuer Fourth-Wall Judge-Aufrufe.
func NewJudgeProviderAdapter(p Provider) *JudgeProviderAdapter {
	return &JudgeProviderAdapter{provider: p}
}

// Send implementiert detection.LLMProvider.
func (a *JudgeProviderAdapter) Send(ctx context.Context, prompt string, temperature float64, maxTokens int) (string, error) {
	req := &LLMRequest{
		Messages:    []Message{{Role: "user", Content: prompt}},
		Temperature: temperature,
		MaxTokens:   maxTokens,
	}
	resp, err := a.provider.Send(ctx, req)
	if err != nil {
		return "", err
	}
	return resp.Content, nil
}
