package proxy

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
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

type ollamaTagsResponse struct {
	Models []struct {
		Name   string `json:"name"`
		Model  string `json:"model"`
		Digest string `json:"digest"`
	} `json:"models"`
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
	if req != nil && req.ProviderTimeout > 0 {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, req.ProviderTimeout)
		defer cancel()
	}

	messages := make([]ollamaMessage, len(req.Messages))
	for i, m := range req.Messages {
		messages[i] = ollamaMessage{
			Role:    m.Role,
			Content: m.Content,
		}
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

	endpoint, err := url.JoinPath(p.baseURL, "/api/chat")
	if err != nil {
		return nil, fmt.Errorf("build ollama endpoint URL: %w", err)
	}
	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, bytes.NewReader(body)) //nolint:gosec // G704: baseURL is from trusted daemon config, not user input
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
		InputTokens:  oResp.PromptEvalCount,
		OutputTokens: oResp.EvalCount,
		FinishReason: oResp.DoneReason,
	}, nil
}

// ModelInventory reads Ollama's token-free model inventory. Content digests
// must be present so a later authorized Gate B readback can pin the exact local
// artifacts independently of the immutable catalog's model-ID validation.
func (p *OllamaProvider) ModelInventory(ctx context.Context) ([]string, error) {
	ctx, cancel := context.WithTimeout(ctx, healthCheckTimeout)
	defer cancel()

	endpoint, err := url.JoinPath(p.baseURL, "/api/tags")
	if err != nil {
		return nil, fmt.Errorf("build ollama inventory URL: %w", err)
	}
	httpReq, err := http.NewRequestWithContext(ctx, http.MethodGet, endpoint, nil)
	if err != nil {
		return nil, fmt.Errorf("create ollama inventory request: %w", err)
	}

	resp, err := p.client.Do(httpReq) //nolint:gosec // URL from trusted config
	if err != nil {
		return nil, fmt.Errorf("ollama inventory request: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()
	body, err := io.ReadAll(io.LimitReader(resp.Body, maxResponseBodySize))
	if err != nil {
		return nil, fmt.Errorf("read ollama inventory: %w", err)
	}

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("ollama inventory returned status %d", resp.StatusCode)
	}
	var tags ollamaTagsResponse
	if err := json.Unmarshal(body, &tags); err != nil {
		return nil, fmt.Errorf("decode ollama inventory: %w", err)
	}
	models := make([]string, 0, len(tags.Models))
	for _, item := range tags.Models {
		model := strings.TrimSpace(item.Model)
		if model == "" {
			model = strings.TrimSpace(item.Name)
		}
		if model == "" {
			return nil, fmt.Errorf("ollama inventory contains an empty model id")
		}
		if strings.TrimSpace(item.Digest) == "" {
			return nil, fmt.Errorf("ollama inventory model %q has no content digest", model)
		}
		models = append(models, model)
	}
	return models, nil
}

// HealthCheck verifies that the Ollama API and its inventory response are
// reachable and structurally valid without generating tokens.
func (p *OllamaProvider) HealthCheck(ctx context.Context) error {
	_, err := p.ModelInventory(ctx)
	return err
}
