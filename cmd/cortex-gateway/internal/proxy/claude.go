package proxy

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"
)

const (
	defaultClaudeBaseURL = "https://api.anthropic.com"
	anthropicVersion     = "2023-06-01"

	// defaultClaudeMaxTokens is the default max_tokens if not configured.
	defaultClaudeMaxTokens = 4096

	// maxResponseBodySize limits provider response bodies to 50 MB.
	maxResponseBodySize = 50 * 1024 * 1024

	// defaultHTTPTimeout is the default timeout for HTTP requests to providers.
	// LLM responses can be slow, so we set a generous timeout.
	defaultHTTPTimeout = 5 * time.Minute

	// healthCheckTimeout is the timeout for health check requests.
	healthCheckTimeout = 10 * time.Second
)

// claudeRequest is the Anthropic Messages API request format.
type claudeRequest struct {
	Model       string          `json:"model"`
	MaxTokens   int             `json:"max_tokens"`
	Messages    []claudeMessage `json:"messages"`
	Temperature float64         `json:"temperature,omitempty"`
}

// claudeMessage is a single message in the Anthropic format.
type claudeMessage struct {
	Role    string `json:"role"`
	Content string `json:"content"`
}

// claudeResponse is the Anthropic Messages API response format.
type claudeResponse struct {
	Content []struct {
		Type string `json:"type"`
		Text string `json:"text"`
	} `json:"content"`
	Model      string      `json:"model"`
	StopReason string      `json:"stop_reason"`
	Usage      claudeUsage `json:"usage"`
}

// claudeUsage tracks token usage in the response.
type claudeUsage struct {
	InputTokens  int `json:"input_tokens"`
	OutputTokens int `json:"output_tokens"`
}

// ClaudeProvider implements Provider for the Anthropic Claude API.
type ClaudeProvider struct {
	name      string
	baseURL   string
	apiKey    string
	model     string
	maxTokens int
	client    *http.Client
}

// NewClaudeProvider creates a new Claude API provider.
func NewClaudeProvider(cfg ProviderConfig) *ClaudeProvider {
	baseURL := cfg.BaseURL
	if baseURL == "" {
		baseURL = defaultClaudeBaseURL
	}
	maxTokens := cfg.MaxTokens
	if maxTokens == 0 {
		maxTokens = defaultClaudeMaxTokens
	}
	return &ClaudeProvider{
		name:      cfg.Name,
		baseURL:   baseURL,
		apiKey:    cfg.APIKey,
		model:     cfg.Model,
		maxTokens: maxTokens,
		client:    &http.Client{Timeout: defaultHTTPTimeout},
	}
}

// Name returns the provider name.
func (p *ClaudeProvider) Name() string {
	return p.name
}

// Send forwards an LLMRequest to the Anthropic Messages API.
func (p *ClaudeProvider) Send(ctx context.Context, req *LLMRequest) (*LLMResponse, error) {
	messages := make([]claudeMessage, len(req.Messages))
	for i, m := range req.Messages {
		messages[i] = claudeMessage(m)
	}

	model := p.model
	if req.Model != "" {
		model = req.Model
	}

	maxTokens := p.maxTokens
	if req.MaxTokens > 0 {
		maxTokens = req.MaxTokens
	}

	cReq := claudeRequest{
		Model:       model,
		MaxTokens:   maxTokens,
		Messages:    messages,
		Temperature: req.Temperature,
	}

	body, err := json.Marshal(cReq)
	if err != nil {
		return nil, fmt.Errorf("marshal claude request: %w", err)
	}

	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, p.baseURL+"/v1/messages", bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("create http request: %w", err)
	}

	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("x-api-key", p.apiKey)
	httpReq.Header.Set("anthropic-version", anthropicVersion)

	resp, err := p.client.Do(httpReq)
	if err != nil {
		return nil, fmt.Errorf("claude API call: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()

	respBody, err := io.ReadAll(io.LimitReader(resp.Body, maxResponseBodySize))
	if err != nil {
		return nil, fmt.Errorf("read claude response: %w", err)
	}

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("claude API returned status %d: %s", resp.StatusCode, string(respBody))
	}

	var cResp claudeResponse
	if err := json.Unmarshal(respBody, &cResp); err != nil {
		return nil, fmt.Errorf("unmarshal claude response: %w", err)
	}

	content := ""
	for _, c := range cResp.Content {
		if c.Type == "text" {
			content += c.Text
		}
	}

	return &LLMResponse{
		Content:      content,
		Model:        cResp.Model,
		TokensUsed:   cResp.Usage.InputTokens + cResp.Usage.OutputTokens,
		FinishReason: cResp.StopReason,
	}, nil
}

// HealthCheck verifies that the Claude API is reachable.
func (p *ClaudeProvider) HealthCheck(ctx context.Context) error {
	ctx, cancel := context.WithTimeout(ctx, healthCheckTimeout)
	defer cancel()

	httpReq, err := http.NewRequestWithContext(ctx, http.MethodGet, p.baseURL+"/v1/messages", nil)
	if err != nil {
		return fmt.Errorf("create health check request: %w", err)
	}
	httpReq.Header.Set("x-api-key", p.apiKey)
	httpReq.Header.Set("anthropic-version", anthropicVersion)

	resp, err := p.client.Do(httpReq)
	if err != nil {
		return fmt.Errorf("claude health check: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()
	// Drain the body to allow connection reuse
	_, _ = io.Copy(io.Discard, io.LimitReader(resp.Body, 1024))

	// Any response (even 405 Method Not Allowed) means the API is reachable
	return nil
}
