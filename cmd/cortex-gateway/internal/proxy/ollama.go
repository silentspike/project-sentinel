package proxy

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
)

const defaultOllamaBaseURL = "http://localhost:11434"

// ollamaRequest is the Ollama /api/chat request format.
type ollamaRequest struct {
	Model    string          `json:"model"`
	Messages []ollamaMessage `json:"messages"`
	Stream   bool            `json:"stream"`
	Options  *ollamaOptions  `json:"options,omitempty"`
}

// ollamaMessage is a single message in the Ollama format.
type ollamaMessage struct {
	Role    string `json:"role"`
	Content string `json:"content"`
}

// ollamaOptions holds optional generation parameters.
type ollamaOptions struct {
	Temperature float64 `json:"temperature,omitempty"`
	NumPredict  int     `json:"num_predict,omitempty"`
}

// ollamaResponse is the Ollama /api/chat response format.
type ollamaResponse struct {
	Model           string        `json:"model"`
	Message         ollamaMessage `json:"message"`
	Done            bool          `json:"done"`
	DoneReason      string        `json:"done_reason"`
	TotalDuration   int64         `json:"total_duration"`
	EvalCount       int           `json:"eval_count"`
	PromptEvalCount int           `json:"prompt_eval_count"`
}

// OllamaProvider implements Provider for Ollama.
type OllamaProvider struct {
	name      string
	baseURL   string
	model     string
	maxTokens int
	client    *http.Client
}

// NewOllamaProvider creates a new Ollama provider.
func NewOllamaProvider(cfg ProviderConfig) *OllamaProvider {
	baseURL := cfg.BaseURL
	if baseURL == "" {
		baseURL = defaultOllamaBaseURL
	}
	return &OllamaProvider{
		name:      cfg.Name,
		baseURL:   baseURL,
		model:     cfg.Model,
		maxTokens: cfg.MaxTokens,
		client:    &http.Client{Timeout: defaultHTTPTimeout},
	}
}

// Name returns the provider name.
func (p *OllamaProvider) Name() string {
	return p.name
}

// Send forwards an LLMRequest to Ollama's /api/chat endpoint.
func (p *OllamaProvider) Send(ctx context.Context, req *LLMRequest) (*LLMResponse, error) {
	messages := make([]ollamaMessage, len(req.Messages))
	for i, m := range req.Messages {
		messages[i] = ollamaMessage(m)
	}

	model := p.model
	if req.Model != "" {
		model = req.Model
	}

	oReq := ollamaRequest{
		Model:    model,
		Messages: messages,
		Stream:   false,
	}

	if req.Temperature > 0 || req.MaxTokens > 0 {
		opts := &ollamaOptions{}
		if req.Temperature > 0 {
			opts.Temperature = req.Temperature
		}
		maxTokens := p.maxTokens
		if req.MaxTokens > 0 {
			maxTokens = req.MaxTokens
		}
		if maxTokens > 0 {
			opts.NumPredict = maxTokens
		}
		oReq.Options = opts
	}

	body, err := json.Marshal(oReq)
	if err != nil {
		return nil, fmt.Errorf("marshal ollama request: %w", err)
	}

	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, p.baseURL+"/api/chat", bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("create http request: %w", err)
	}
	httpReq.Header.Set("Content-Type", "application/json")

	resp, err := p.client.Do(httpReq) //nolint:gosec // URL from trusted config
	if err != nil {
		return nil, fmt.Errorf("ollama API call: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()

	respBody, err := io.ReadAll(io.LimitReader(resp.Body, maxResponseBodySize))
	if err != nil {
		return nil, fmt.Errorf("read ollama response: %w", err)
	}

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("ollama API returned status %d: %s", resp.StatusCode, string(respBody))
	}

	var oResp ollamaResponse
	if err := json.Unmarshal(respBody, &oResp); err != nil {
		return nil, fmt.Errorf("unmarshal ollama response: %w", err)
	}

	return &LLMResponse{
		Content:      oResp.Message.Content,
		Model:        oResp.Model,
		TokensUsed:   oResp.PromptEvalCount + oResp.EvalCount,
		FinishReason: oResp.DoneReason,
	}, nil
}

// HealthCheck verifies that the Ollama API is reachable via GET /api/tags.
func (p *OllamaProvider) HealthCheck(ctx context.Context) error {
	ctx, cancel := context.WithTimeout(ctx, healthCheckTimeout)
	defer cancel()

	httpReq, err := http.NewRequestWithContext(ctx, http.MethodGet, p.baseURL+"/api/tags", nil)
	if err != nil {
		return fmt.Errorf("create health check request: %w", err)
	}

	resp, err := p.client.Do(httpReq) //nolint:gosec // URL from trusted config
	if err != nil {
		return fmt.Errorf("ollama health check: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()
	// Drain the body to allow connection reuse
	_, _ = io.Copy(io.Discard, io.LimitReader(resp.Body, 1024))

	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("ollama health check returned status %d", resp.StatusCode)
	}

	return nil
}
