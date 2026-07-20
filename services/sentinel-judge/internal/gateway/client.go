// Package gateway provides an HTTP client to the Cortex Gateway for LLM analysis.
package gateway

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"
)

// Client communicates with the Cortex Gateway HTTP API.
type Client struct {
	baseURL     string
	model       string
	temperature float64
	maxTokens   int
	httpClient  *http.Client
	credential  string
}

// ClientConfig configures the gateway client.
type ClientConfig struct {
	URL         string
	Model       string
	Temperature float64
	MaxTokens   int
	Timeout     time.Duration
	Credential  string
}

// NewClient creates a gateway client for judge LLM analysis.
func NewClient(cfg ClientConfig) *Client {
	timeout := cfg.Timeout
	if timeout <= 0 {
		timeout = 30 * time.Second
	}
	return &Client{
		baseURL:     cfg.URL,
		model:       cfg.Model,
		temperature: cfg.Temperature,
		maxTokens:   cfg.MaxTokens,
		httpClient:  &http.Client{Timeout: timeout},
		credential:  cfg.Credential,
	}
}

// ChatRequest is the request to the Cortex Gateway.
type ChatRequest struct {
	Messages    []Message         `json:"messages"`
	Model       string            `json:"model,omitempty"`
	Temperature float64           `json:"temperature,omitempty"`
	MaxTokens   int               `json:"max_tokens,omitempty"`
	Metadata    map[string]string `json:"metadata,omitempty"`
}

// Message represents a chat message.
type Message struct {
	Role    string `json:"role"`
	Content string `json:"content"`
}

// ChatResponse is the response from the Cortex Gateway.
type ChatResponse struct {
	Content    string `json:"content"`
	Model      string `json:"model"`
	TokensUsed int    `json:"tokens_used"`
}

// Chat sends a chat request to the gateway and returns the response content.
func (c *Client) Chat(ctx context.Context, systemPrompt, userPrompt string) (string, error) {
	if c.credential == "" {
		return "", fmt.Errorf("gateway caller credential is required")
	}
	req := ChatRequest{
		Messages: []Message{
			{Role: "system", Content: systemPrompt},
			{Role: "user", Content: userPrompt},
		},
		Model:       c.model,
		Temperature: c.temperature,
		MaxTokens:   c.maxTokens,
		Metadata: map[string]string{
			"agent_name": "sentinel-judge",
		},
	}

	body, err := json.Marshal(req)
	if err != nil {
		return "", fmt.Errorf("gateway marshal: %w", err)
	}

	// Judge traffic uses the internal gateway contract and must not be routed
	// through the public Anthropic-compatible MITM endpoint.
	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost,
		c.baseURL+"/internal/llm", bytes.NewReader(body))
	if err != nil {
		return "", fmt.Errorf("gateway new request: %w", err)
	}
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("Authorization", "Bearer "+c.credential)

	resp, err := c.httpClient.Do(httpReq)
	if err != nil {
		return "", fmt.Errorf("gateway request: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()

	respBody, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20)) // 1MB limit
	if err != nil {
		return "", fmt.Errorf("gateway read response: %w", err)
	}

	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("gateway status %d: %s", resp.StatusCode, string(respBody))
	}

	var chatResp ChatResponse
	if err := json.Unmarshal(respBody, &chatResp); err != nil {
		return "", fmt.Errorf("gateway unmarshal: %w", err)
	}

	return chatResp.Content, nil
}
