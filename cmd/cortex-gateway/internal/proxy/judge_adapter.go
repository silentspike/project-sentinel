package proxy

import "context"

// JudgeProviderAdapter adaptiert einen proxy.Provider (Send mit *LLMRequest)
// auf das detection.LLMProvider Interface (Send mit einzelnen Parametern).
type JudgeProviderAdapter struct {
	provider Provider
	baseReq  *LLMRequest
}

// NewJudgeProviderAdapter erstellt einen Adapter fuer Fourth-Wall Judge-Aufrufe.
func NewJudgeProviderAdapter(p Provider, baseReq *LLMRequest) *JudgeProviderAdapter {
	return &JudgeProviderAdapter{provider: p, baseReq: baseReq}
}

// Send implementiert detection.LLMProvider.
func (a *JudgeProviderAdapter) Send(ctx context.Context, prompt string, temperature float64, maxTokens int) (string, error) {
	req := &LLMRequest{
		Messages:           []Message{{Role: "user", Content: prompt}},
		SystemBlocks:       cloneSystemBlocks(a.baseReq),
		Temperature:        temperature,
		MaxTokens:          maxTokens,
		Model:              cloneModel(a.baseReq),
		Format:             cloneFormat(a.baseReq),
		PreferredProvider:  clonePreferredProvider(a.baseReq),
		PassthroughHeaders: clonePassthroughHeaders(clonePassthroughSource(a.baseReq)),
	}
	resp, err := a.provider.Send(ctx, req)
	if err != nil {
		return "", err
	}
	return resp.Content, nil
}

func cloneSystemBlocks(req *LLMRequest) []SystemBlock {
	if req == nil || len(req.SystemBlocks) == 0 {
		return nil
	}
	blocks := make([]SystemBlock, len(req.SystemBlocks))
	copy(blocks, req.SystemBlocks)
	return blocks
}

func cloneModel(req *LLMRequest) string {
	if req == nil {
		return ""
	}
	return req.Model
}

func cloneFormat(req *LLMRequest) RequestFormat {
	if req == nil {
		return RequestFormatInternal
	}
	return req.Format
}

func clonePreferredProvider(req *LLMRequest) string {
	if req == nil {
		return ""
	}
	return req.PreferredProvider
}

func clonePassthroughSource(req *LLMRequest) map[string]string {
	if req == nil {
		return nil
	}
	return req.PassthroughHeaders
}
