package normalizer

import (
	"encoding/json"
	"fmt"
	"strings"
)

// NormalizedResponse is the unified response format across all LLM providers.
type NormalizedResponse struct {
	Content      string            `json:"content"`
	Role         string            `json:"role"`
	Model        string            `json:"model"`
	Provider     string            `json:"provider"`
	TokensUsed   int               `json:"tokens_used"`
	FinishReason string            `json:"finish_reason"`
	Metadata     map[string]string `json:"metadata,omitempty"`
}

// claudeAPIResponse is the Anthropic Messages API response format.
type claudeAPIResponse struct {
	Content []struct {
		Type string `json:"type"`
		Text string `json:"text"`
	} `json:"content"`
	Model      string `json:"model"`
	Role       string `json:"role"`
	StopReason string `json:"stop_reason"`
	Usage      struct {
		InputTokens  int `json:"input_tokens"`
		OutputTokens int `json:"output_tokens"`
	} `json:"usage"`
}

// ollamaAPIResponse is the Ollama /api/chat response format.
type ollamaAPIResponse struct {
	Model   string `json:"model"`
	Message struct {
		Role    string `json:"role"`
		Content string `json:"content"`
	} `json:"message"`
	Done         bool `json:"done"`
	TotalDuration int64 `json:"total_duration"`
	EvalCount     int   `json:"eval_count"`
}

// Normalizer converts provider-specific responses to NormalizedResponse.
type Normalizer struct{}

// New creates a new Normalizer.
func New() *Normalizer { return &Normalizer{} }

// NormalizeClaude converts a Claude API response to NormalizedResponse.
func (n *Normalizer) NormalizeClaude(raw []byte) (*NormalizedResponse, error) {
	var resp claudeAPIResponse
	if err := json.Unmarshal(raw, &resp); err != nil {
		return nil, fmt.Errorf("unmarshal claude response: %w", err)
	}

	var b strings.Builder
	for _, block := range resp.Content {
		if block.Type == "text" {
			b.WriteString(block.Text)
		}
	}
	content := b.String()

	role := resp.Role
	if role == "" {
		role = "assistant"
	}

	return &NormalizedResponse{
		Content:      content,
		Role:         role,
		Model:        resp.Model,
		Provider:     "claude",
		TokensUsed:   resp.Usage.InputTokens + resp.Usage.OutputTokens,
		FinishReason: resp.StopReason,
	}, nil
}

// NormalizeOllama converts an Ollama API response to NormalizedResponse.
func (n *Normalizer) NormalizeOllama(raw []byte) (*NormalizedResponse, error) {
	var resp ollamaAPIResponse
	if err := json.Unmarshal(raw, &resp); err != nil {
		return nil, fmt.Errorf("unmarshal ollama response: %w", err)
	}

	finishReason := ""
	if resp.Done {
		finishReason = "stop"
	}

	role := resp.Message.Role
	if role == "" {
		role = "assistant"
	}

	return &NormalizedResponse{
		Content:      resp.Message.Content,
		Role:         role,
		Model:        resp.Model,
		Provider:     "ollama",
		TokensUsed:   resp.EvalCount,
		FinishReason: finishReason,
	}, nil
}

// Normalize auto-detects the provider format and normalizes the response.
func (n *Normalizer) Normalize(raw []byte, provider string) (*NormalizedResponse, error) {
	switch provider {
	case "claude":
		return n.NormalizeClaude(raw)
	case "ollama":
		return n.NormalizeOllama(raw)
	default:
		return nil, fmt.Errorf("unknown provider: %s", provider)
	}
}
