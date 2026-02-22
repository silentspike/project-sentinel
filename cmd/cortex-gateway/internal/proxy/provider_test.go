package proxy

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"sort"
	"strings"
	"testing"
)

// mockProvider implements Provider for testing.
type mockProvider struct {
	name string
	resp *LLMResponse
	err  error
}

func (m *mockProvider) Name() string { return m.name }
func (m *mockProvider) Send(_ context.Context, _ *LLMRequest) (*LLMResponse, error) {
	return m.resp, m.err
}
func (m *mockProvider) HealthCheck(_ context.Context) error { return nil }

// --- Registry Tests ---

func TestRegistryRegisterAndGet(t *testing.T) {
	reg := NewRegistry()
	p := &mockProvider{name: "test"}
	reg.Register("test", p)

	got, ok := reg.Get("test")
	if !ok {
		t.Fatal("expected provider to be registered")
	}
	if got.Name() != "test" {
		t.Errorf("expected name %q, got %q", "test", got.Name())
	}

	_, ok = reg.Get("nonexistent")
	if ok {
		t.Fatal("expected nonexistent provider to not be found")
	}
}

func TestRegistryPrimary(t *testing.T) {
	reg := NewRegistry()

	_, err := reg.Primary()
	if err == nil {
		t.Fatal("expected error for empty registry")
	}

	p1 := &mockProvider{name: "first"}
	p2 := &mockProvider{name: "second"}
	reg.Register("first", p1)
	reg.Register("second", p2)

	primary, err := reg.Primary()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if primary.Name() != "first" {
		t.Errorf("expected primary %q, got %q", "first", primary.Name())
	}
}

func TestRegistrySetPrimary(t *testing.T) {
	reg := NewRegistry()
	p1 := &mockProvider{name: "first"}
	p2 := &mockProvider{name: "second"}
	reg.Register("first", p1)
	reg.Register("second", p2)

	if err := reg.SetPrimary("second"); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	primary, err := reg.Primary()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if primary.Name() != "second" {
		t.Errorf("expected primary %q, got %q", "second", primary.Name())
	}
}

func TestRegistrySetPrimary_NotFound(t *testing.T) {
	reg := NewRegistry()
	err := reg.SetPrimary("nonexistent")
	if err == nil {
		t.Fatal("expected error for nonexistent provider")
	}
	if !strings.Contains(err.Error(), "not registered") {
		t.Errorf("expected 'not registered' in error, got: %v", err)
	}
}

func TestRegistryList(t *testing.T) {
	reg := NewRegistry()
	reg.Register("alpha", &mockProvider{name: "alpha"})
	reg.Register("beta", &mockProvider{name: "beta"})
	reg.Register("gamma", &mockProvider{name: "gamma"})

	names := reg.List()
	sort.Strings(names)

	expected := []string{"alpha", "beta", "gamma"}
	if len(names) != len(expected) {
		t.Fatalf("expected %d providers, got %d", len(expected), len(names))
	}
	for i, name := range names {
		if name != expected[i] {
			t.Errorf("expected name %q at index %d, got %q", expected[i], i, name)
		}
	}
}

func TestRegistryRegisterOverwrite(t *testing.T) {
	reg := NewRegistry()
	p1 := &mockProvider{name: "v1"}
	p2 := &mockProvider{name: "v2"}
	reg.Register("provider", p1)
	reg.Register("provider", p2)

	got, ok := reg.Get("provider")
	if !ok {
		t.Fatal("expected provider to be registered")
	}
	if got.Name() != "v2" {
		t.Errorf("expected overwritten provider %q, got %q", "v2", got.Name())
	}
}

// --- Factory Tests ---

func TestNewProviderFromConfig_Claude(t *testing.T) {
	cfg := ProviderConfig{
		Name:    "test-claude",
		Type:    "claude",
		BaseURL: "https://api.anthropic.com",
		APIKey:  "test-key",
		Model:   "claude-sonnet-4-5-20250929",
	}
	p, err := NewProviderFromConfig(cfg)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if p.Name() != "test-claude" {
		t.Errorf("expected name %q, got %q", "test-claude", p.Name())
	}
}

func TestNewProviderFromConfig_Ollama(t *testing.T) {
	cfg := ProviderConfig{
		Name:    "test-ollama",
		Type:    "ollama",
		BaseURL: "http://localhost:11434",
		Model:   "llama3",
	}
	p, err := NewProviderFromConfig(cfg)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if p.Name() != "test-ollama" {
		t.Errorf("expected name %q, got %q", "test-ollama", p.Name())
	}
}

func TestNewProviderFromConfig_Unknown(t *testing.T) {
	cfg := ProviderConfig{Type: "unknown"}
	_, err := NewProviderFromConfig(cfg)
	if err == nil {
		t.Fatal("expected error for unknown provider type")
	}
	if !strings.Contains(err.Error(), "unknown provider type") {
		t.Errorf("expected 'unknown provider type' in error, got: %v", err)
	}
}

// --- Handler Tests ---

func TestHandlerServeHTTP_NoProvider(t *testing.T) {
	reg := NewRegistry()
	h := NewHandler(reg, newTestLogger())

	reqBody := `{"messages":[{"role":"user","content":"hello"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(reqBody))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()

	h.ServeHTTP(w, req)

	if w.Code != http.StatusServiceUnavailable {
		t.Errorf("expected status %d, got %d", http.StatusServiceUnavailable, w.Code)
	}
}

func TestHandlerServeHTTP_Success(t *testing.T) {
	reg := NewRegistry()
	reg.Register("mock", &mockProvider{
		name: "mock",
		resp: &LLMResponse{
			Content:      "hello back",
			Model:        "test-model",
			TokensUsed:   42,
			FinishReason: "end_turn",
		},
	})
	h := NewHandler(reg, newTestLogger())

	reqBody := `{"messages":[{"role":"user","content":"hello"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(reqBody))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()

	h.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Errorf("expected status %d, got %d", http.StatusOK, w.Code)
	}

	var resp LLMResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}
	if resp.Content != "hello back" {
		t.Errorf("expected content %q, got %q", "hello back", resp.Content)
	}
	if resp.TokensUsed != 42 {
		t.Errorf("expected tokens_used %d, got %d", 42, resp.TokensUsed)
	}
	if resp.FinishReason != "end_turn" {
		t.Errorf("expected finish_reason %q, got %q", "end_turn", resp.FinishReason)
	}
}

func TestHandlerServeHTTP_ProviderError(t *testing.T) {
	reg := NewRegistry()
	reg.Register("failing", &mockProvider{
		name: "failing",
		err:  fmt.Errorf("upstream timeout"),
	})
	h := NewHandler(reg, newTestLogger())

	reqBody := `{"messages":[{"role":"user","content":"hello"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(reqBody))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()

	h.ServeHTTP(w, req)

	if w.Code != http.StatusBadGateway {
		t.Errorf("expected status %d, got %d", http.StatusBadGateway, w.Code)
	}
}

func TestHandlerServeHTTP_MethodNotAllowed(t *testing.T) {
	reg := NewRegistry()
	h := NewHandler(reg, newTestLogger())

	req := httptest.NewRequest(http.MethodGet, "/v1/chat/completions", nil)
	w := httptest.NewRecorder()

	h.ServeHTTP(w, req)

	if w.Code != http.StatusMethodNotAllowed {
		t.Errorf("expected status %d, got %d", http.StatusMethodNotAllowed, w.Code)
	}
}

func TestHandlerServeHTTP_InvalidJSON(t *testing.T) {
	reg := NewRegistry()
	h := NewHandler(reg, newTestLogger())

	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader("not json"))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()

	h.ServeHTTP(w, req)

	if w.Code != http.StatusBadRequest {
		t.Errorf("expected status %d, got %d", http.StatusBadRequest, w.Code)
	}
}

func TestHandlerServeHTTP_BodyTooLarge(t *testing.T) {
	reg := NewRegistry()
	reg.Register("mock", &mockProvider{name: "mock", resp: &LLMResponse{}})
	h := NewHandler(reg, newTestLogger())

	// Create a body larger than maxRequestBodySize (10 MB)
	largeBody := strings.Repeat("x", maxRequestBodySize+100)
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(largeBody))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()

	h.ServeHTTP(w, req)

	if w.Code != http.StatusRequestEntityTooLarge {
		t.Errorf("expected status %d, got %d", http.StatusRequestEntityTooLarge, w.Code)
	}
}

// --- Provider Unit Tests ---

func TestClaudeProvider_Name(t *testing.T) {
	p := NewClaudeProvider(ProviderConfig{Name: "my-claude"})
	if p.Name() != "my-claude" {
		t.Errorf("expected name %q, got %q", "my-claude", p.Name())
	}
}

func TestOllamaProvider_Name(t *testing.T) {
	p := NewOllamaProvider(ProviderConfig{Name: "my-ollama"})
	if p.Name() != "my-ollama" {
		t.Errorf("expected name %q, got %q", "my-ollama", p.Name())
	}
}

func TestClaudeProvider_DefaultBaseURL(t *testing.T) {
	p := NewClaudeProvider(ProviderConfig{Name: "claude"})
	if p.baseURL != defaultClaudeBaseURL {
		t.Errorf("expected default base URL %q, got %q", defaultClaudeBaseURL, p.baseURL)
	}
}

func TestClaudeProvider_DefaultMaxTokens(t *testing.T) {
	p := NewClaudeProvider(ProviderConfig{Name: "claude"})
	if p.maxTokens != defaultClaudeMaxTokens {
		t.Errorf("expected default max tokens %d, got %d", defaultClaudeMaxTokens, p.maxTokens)
	}
}

func TestClaudeProvider_CustomConfig(t *testing.T) {
	p := NewClaudeProvider(ProviderConfig{
		Name:      "custom",
		BaseURL:   "https://custom.api.example.com",
		MaxTokens: 8192,
		Model:     "claude-opus-4-6",
	})
	if p.baseURL != "https://custom.api.example.com" {
		t.Errorf("expected custom base URL, got %q", p.baseURL)
	}
	if p.maxTokens != 8192 {
		t.Errorf("expected max tokens 8192, got %d", p.maxTokens)
	}
	if p.model != "claude-opus-4-6" {
		t.Errorf("expected model %q, got %q", "claude-opus-4-6", p.model)
	}
}

func TestOllamaProvider_DefaultBaseURL(t *testing.T) {
	p := NewOllamaProvider(ProviderConfig{Name: "ollama"})
	if p.baseURL != defaultOllamaBaseURL {
		t.Errorf("expected default base URL %q, got %q", defaultOllamaBaseURL, p.baseURL)
	}
}

// --- Integration Tests with httptest.Server ---

func TestClaudeProvider_Send_Integration(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			t.Errorf("expected POST, got %s", r.Method)
		}
		if r.URL.Path != "/v1/messages" {
			t.Errorf("expected path /v1/messages, got %s", r.URL.Path)
		}
		if r.Header.Get("x-api-key") != "test-key" {
			t.Errorf("expected x-api-key header 'test-key', got %q", r.Header.Get("x-api-key"))
		}
		if r.Header.Get("anthropic-version") != anthropicVersion {
			t.Errorf("expected anthropic-version %q, got %q", anthropicVersion, r.Header.Get("anthropic-version"))
		}
		if r.Header.Get("Content-Type") != "application/json" {
			t.Errorf("expected Content-Type application/json, got %q", r.Header.Get("Content-Type"))
		}

		var cReq claudeRequest
		if err := json.NewDecoder(r.Body).Decode(&cReq); err != nil {
			t.Fatalf("failed to decode request body: %v", err)
		}
		if cReq.Model != "claude-sonnet-4-5-20250929" {
			t.Errorf("expected model %q, got %q", "claude-sonnet-4-5-20250929", cReq.Model)
		}
		if len(cReq.Messages) != 1 || cReq.Messages[0].Content != "hello" {
			t.Errorf("unexpected messages: %+v", cReq.Messages)
		}

		resp := claudeResponse{
			Content: []struct {
				Type string `json:"type"`
				Text string `json:"text"`
			}{{Type: "text", Text: "world"}},
			Model:      "claude-sonnet-4-5-20250929",
			StopReason: "end_turn",
			Usage:      claudeUsage{InputTokens: 10, OutputTokens: 5},
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(resp)
	}))
	defer server.Close()

	p := NewClaudeProvider(ProviderConfig{
		Name:    "test-claude",
		BaseURL: server.URL,
		APIKey:  "test-key",
		Model:   "claude-sonnet-4-5-20250929",
	})

	resp, err := p.Send(context.Background(), &LLMRequest{
		Messages: []Message{{Role: "user", Content: "hello"}},
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if resp.Content != "world" {
		t.Errorf("expected content %q, got %q", "world", resp.Content)
	}
	if resp.TokensUsed != 15 {
		t.Errorf("expected tokens_used 15, got %d", resp.TokensUsed)
	}
	if resp.FinishReason != "end_turn" {
		t.Errorf("expected finish_reason %q, got %q", "end_turn", resp.FinishReason)
	}
}

func TestClaudeProvider_Send_APIError(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusTooManyRequests)
		_, _ = w.Write([]byte(`{"error":{"message":"rate limited"}}`))
	}))
	defer server.Close()

	p := NewClaudeProvider(ProviderConfig{
		Name:    "test-claude",
		BaseURL: server.URL,
		APIKey:  "test-key",
		Model:   "test-model",
	})

	_, err := p.Send(context.Background(), &LLMRequest{
		Messages: []Message{{Role: "user", Content: "hello"}},
	})
	if err == nil {
		t.Fatal("expected error for 429 response")
	}
	if !strings.Contains(err.Error(), "429") {
		t.Errorf("expected status code in error, got: %v", err)
	}
}

func TestClaudeProvider_Send_ModelOverride(t *testing.T) {
	var receivedModel string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var cReq claudeRequest
		_ = json.NewDecoder(r.Body).Decode(&cReq)
		receivedModel = cReq.Model

		resp := claudeResponse{
			Content: []struct {
				Type string `json:"type"`
				Text string `json:"text"`
			}{{Type: "text", Text: "ok"}},
			Model: cReq.Model,
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(resp)
	}))
	defer server.Close()

	p := NewClaudeProvider(ProviderConfig{
		Name:    "test",
		BaseURL: server.URL,
		APIKey:  "key",
		Model:   "default-model",
	})

	_, err := p.Send(context.Background(), &LLMRequest{
		Messages: []Message{{Role: "user", Content: "test"}},
		Model:    "override-model",
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if receivedModel != "override-model" {
		t.Errorf("expected model override %q, got %q", "override-model", receivedModel)
	}
}

func TestOllamaProvider_Send_Integration(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			t.Errorf("expected POST, got %s", r.Method)
		}
		if r.URL.Path != "/api/chat" {
			t.Errorf("expected path /api/chat, got %s", r.URL.Path)
		}

		var oReq ollamaRequest
		if err := json.NewDecoder(r.Body).Decode(&oReq); err != nil {
			t.Fatalf("failed to decode request body: %v", err)
		}
		if oReq.Model != "llama3" {
			t.Errorf("expected model %q, got %q", "llama3", oReq.Model)
		}
		if oReq.Stream {
			t.Error("expected stream=false")
		}

		resp := ollamaResponse{
			Model:           "llama3",
			Message:         ollamaMessage{Role: "assistant", Content: "hello from ollama"},
			Done:            true,
			DoneReason:      "stop",
			EvalCount:       20,
			PromptEvalCount: 10,
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(resp)
	}))
	defer server.Close()

	p := NewOllamaProvider(ProviderConfig{
		Name:    "test-ollama",
		BaseURL: server.URL,
		Model:   "llama3",
	})

	resp, err := p.Send(context.Background(), &LLMRequest{
		Messages: []Message{{Role: "user", Content: "hello"}},
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if resp.Content != "hello from ollama" {
		t.Errorf("expected content %q, got %q", "hello from ollama", resp.Content)
	}
	if resp.TokensUsed != 30 {
		t.Errorf("expected tokens_used 30, got %d", resp.TokensUsed)
	}
	if resp.FinishReason != "stop" {
		t.Errorf("expected finish_reason %q, got %q", "stop", resp.FinishReason)
	}
}

func TestOllamaProvider_Send_WithOptions(t *testing.T) {
	var receivedOpts *ollamaOptions
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var oReq ollamaRequest
		_ = json.NewDecoder(r.Body).Decode(&oReq)
		receivedOpts = oReq.Options

		resp := ollamaResponse{
			Model:   "llama3",
			Message: ollamaMessage{Role: "assistant", Content: "ok"},
			Done:    true,
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(resp)
	}))
	defer server.Close()

	p := NewOllamaProvider(ProviderConfig{
		Name:    "test",
		BaseURL: server.URL,
		Model:   "llama3",
	})

	_, err := p.Send(context.Background(), &LLMRequest{
		Messages:    []Message{{Role: "user", Content: "test"}},
		Temperature: 0.7,
		MaxTokens:   512,
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if receivedOpts == nil {
		t.Fatal("expected options to be set")
	}
	if receivedOpts.Temperature != 0.7 {
		t.Errorf("expected temperature 0.7, got %f", receivedOpts.Temperature)
	}
	if receivedOpts.NumPredict != 512 {
		t.Errorf("expected num_predict 512, got %d", receivedOpts.NumPredict)
	}
}

func TestClaudeProvider_HealthCheck_Integration(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/messages" {
			t.Errorf("expected path /v1/messages, got %s", r.URL.Path)
		}
		w.WriteHeader(http.StatusMethodNotAllowed)
	}))
	defer server.Close()

	p := NewClaudeProvider(ProviderConfig{
		Name:    "test",
		BaseURL: server.URL,
		APIKey:  "key",
	})

	err := p.HealthCheck(context.Background())
	if err != nil {
		t.Errorf("expected no error for reachable API (even 405), got: %v", err)
	}
}

func TestOllamaProvider_HealthCheck_Integration(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/api/tags" {
			t.Errorf("expected path /api/tags, got %s", r.URL.Path)
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"models":[]}`))
	}))
	defer server.Close()

	p := NewOllamaProvider(ProviderConfig{
		Name:    "test",
		BaseURL: server.URL,
	})

	err := p.HealthCheck(context.Background())
	if err != nil {
		t.Errorf("expected no error, got: %v", err)
	}
}

func TestOllamaProvider_HealthCheck_Unhealthy(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
	}))
	defer server.Close()

	p := NewOllamaProvider(ProviderConfig{
		Name:    "test",
		BaseURL: server.URL,
	})

	err := p.HealthCheck(context.Background())
	if err == nil {
		t.Fatal("expected error for unhealthy server")
	}
	if !strings.Contains(err.Error(), "500") {
		t.Errorf("expected status code in error, got: %v", err)
	}
}

// --- Claude Code Provider Tests ---

func TestClaudeCodeProvider_Name(t *testing.T) {
	p := NewClaudeCodeProvider(ProviderConfig{Name: "my-claude-code"}, nil)
	if p.Name() != "my-claude-code" {
		t.Errorf("expected name %q, got %q", "my-claude-code", p.Name())
	}
}

func TestClaudeCodeProvider_DefaultConfig(t *testing.T) {
	p := NewClaudeCodeProvider(ProviderConfig{Name: "cc"}, nil)
	if p.binary != defaultClaudeCodeBinary {
		t.Errorf("expected default binary %q, got %q", defaultClaudeCodeBinary, p.binary)
	}
	if p.model != defaultClaudeCodeModel {
		t.Errorf("expected default model %q, got %q", defaultClaudeCodeModel, p.model)
	}
	if p.logger == nil {
		t.Error("expected non-nil logger with nil input")
	}
}

func TestClaudeCodeProvider_CustomConfig(t *testing.T) {
	logger := newTestLogger()
	p := NewClaudeCodeProvider(ProviderConfig{
		Name:    "custom-cc",
		BaseURL: "/usr/local/bin/claude",
		Model:   "claude-sonnet-4-6",
	}, logger)
	if p.binary != "/usr/local/bin/claude" {
		t.Errorf("expected custom binary %q, got %q", "/usr/local/bin/claude", p.binary)
	}
	if p.model != "claude-sonnet-4-6" {
		t.Errorf("expected model %q, got %q", "claude-sonnet-4-6", p.model)
	}
}

func TestNewProviderFromConfig_ClaudeCode(t *testing.T) {
	cfg := ProviderConfig{
		Name: "test-claude-code",
		Type: "claude-code",
	}
	p, err := NewProviderFromConfig(cfg)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if p.Name() != "test-claude-code" {
		t.Errorf("expected name %q, got %q", "test-claude-code", p.Name())
	}
}

func TestBuildPrompt_SingleMessage(t *testing.T) {
	msgs := []Message{{Role: "user", Content: "hello world"}}
	got := buildPrompt(msgs)
	if got != "hello world" {
		t.Errorf("expected %q, got %q", "hello world", got)
	}
}

func TestBuildPrompt_MultiMessage(t *testing.T) {
	msgs := []Message{
		{Role: "system", Content: "You are helpful."},
		{Role: "user", Content: "What is Go?"},
		{Role: "assistant", Content: "Go is a programming language."},
		{Role: "user", Content: "Tell me more."},
	}
	got := buildPrompt(msgs)
	if !strings.Contains(got, "You are helpful.") {
		t.Error("expected system message in prompt")
	}
	if !strings.Contains(got, "What is Go?") {
		t.Error("expected user message in prompt")
	}
	if !strings.Contains(got, "[Previous response: Go is a programming language.]") {
		t.Error("expected assistant message with prefix in prompt")
	}
	if !strings.Contains(got, "Tell me more.") {
		t.Error("expected second user message in prompt")
	}
}

func TestClaudeCodeProvider_ParseOutputStream_Success(t *testing.T) {
	// Simulate NDJSON output from claude subprocess (real format: content is array of blocks)
	ndjson := strings.Join([]string{
		`{"type":"system","subtype":"init","session_id":"sess-123"}`,
		`{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello "}]}}`,
		`{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"World"}]}}`,
		`{"type":"result","subtype":"success","result":"Final answer.","total_cost_usd":0.01,"duration_ms":1500}`,
	}, "\n")

	p := NewClaudeCodeProvider(ProviderConfig{Name: "test"}, nil)
	resp, err := p.parseOutputStream(strings.NewReader(ndjson))
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if resp == nil {
		t.Fatal("expected non-nil response")
	}
	// Result text should NOT be appended when assistant content exists (avoids duplication)
	expected := "Hello World"
	if resp.Content != expected {
		t.Errorf("expected content %q, got %q", expected, resp.Content)
	}
	if resp.FinishReason != "success" {
		t.Errorf("expected finish_reason %q, got %q", "success", resp.FinishReason)
	}
}

func TestClaudeCodeProvider_ParseOutputStream_Error(t *testing.T) {
	ndjson := `{"type":"result","subtype":"error","result":"authentication failed","is_error":true}`

	p := NewClaudeCodeProvider(ProviderConfig{Name: "test"}, nil)
	_, err := p.parseOutputStream(strings.NewReader(ndjson))
	if err == nil {
		t.Fatal("expected error for error result")
	}
	if !strings.Contains(err.Error(), "authentication failed") {
		t.Errorf("expected error message in error, got: %v", err)
	}
}

func TestClaudeCodeProvider_ParseOutputStream_Empty(t *testing.T) {
	p := NewClaudeCodeProvider(ProviderConfig{Name: "test"}, nil)
	resp, err := p.parseOutputStream(strings.NewReader(""))
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if resp != nil {
		t.Errorf("expected nil response for empty stream, got %+v", resp)
	}
}

func TestClaudeCodeProvider_ParseOutputStream_AssistantOnly(t *testing.T) {
	// Stream that ends without a result event (EOF)
	ndjson := strings.Join([]string{
		`{"type":"system","subtype":"init","session_id":"sess-456"}`,
		`{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Partial response"}]}}`,
	}, "\n")

	p := NewClaudeCodeProvider(ProviderConfig{Name: "test"}, nil)
	resp, err := p.parseOutputStream(strings.NewReader(ndjson))
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if resp == nil {
		t.Fatal("expected non-nil response for partial stream")
	}
	if resp.Content != "Partial response" {
		t.Errorf("expected content %q, got %q", "Partial response", resp.Content)
	}
	if resp.FinishReason != "eof" {
		t.Errorf("expected finish_reason %q, got %q", "eof", resp.FinishReason)
	}
}

func TestClaudeCodeProvider_ParseOutputStream_SkipsInvalidJSON(t *testing.T) {
	ndjson := strings.Join([]string{
		`not json at all`,
		`{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"valid"}]}}`,
		`{broken json`,
		`{"type":"result","subtype":"success","result":"done"}`,
	}, "\n")

	p := NewClaudeCodeProvider(ProviderConfig{Name: "test"}, nil)
	resp, err := p.parseOutputStream(strings.NewReader(ndjson))
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if resp == nil {
		t.Fatal("expected non-nil response")
	}
	// Result text not appended because assistant content exists
	expected := "valid"
	if resp.Content != expected {
		t.Errorf("expected content %q, got %q", expected, resp.Content)
	}
}

func TestClaudeCodeProvider_ParseOutputStream_EmptyLines(t *testing.T) {
	ndjson := "\n\n" + `{"type":"result","subtype":"success","result":"ok"}` + "\n\n"

	p := NewClaudeCodeProvider(ProviderConfig{Name: "test"}, nil)
	resp, err := p.parseOutputStream(strings.NewReader(ndjson))
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if resp == nil {
		t.Fatal("expected non-nil response")
	}
	if resp.Content != "ok" {
		t.Errorf("expected content %q, got %q", "ok", resp.Content)
	}
}

func TestClaudeCodeProvider_ParseOutputStream_ResultOnly(t *testing.T) {
	// When there are no assistant events, the result text should be used
	ndjson := strings.Join([]string{
		`{"type":"system","subtype":"init","session_id":"sess-789"}`,
		`{"type":"result","subtype":"success","result":"Direct result text"}`,
	}, "\n")

	p := NewClaudeCodeProvider(ProviderConfig{Name: "test"}, nil)
	resp, err := p.parseOutputStream(strings.NewReader(ndjson))
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if resp == nil {
		t.Fatal("expected non-nil response")
	}
	if resp.Content != "Direct result text" {
		t.Errorf("expected content %q, got %q", "Direct result text", resp.Content)
	}
}

func TestClaudeCodeProvider_Send_BinaryNotFound(t *testing.T) {
	p := NewClaudeCodeProvider(ProviderConfig{
		Name:    "test",
		BaseURL: "/nonexistent/binary/path/claude-fake-12345",
	}, nil)

	_, err := p.Send(context.Background(), &LLMRequest{
		Messages: []Message{{Role: "user", Content: "hello"}},
	})
	if err == nil {
		t.Fatal("expected error for nonexistent binary")
	}
	if !strings.Contains(err.Error(), "claude-code start") {
		t.Errorf("expected 'claude-code start' in error, got: %v", err)
	}
}

func TestClaudeCodeProvider_HealthCheck_BinaryNotFound(t *testing.T) {
	p := NewClaudeCodeProvider(ProviderConfig{
		Name:    "test",
		BaseURL: "/nonexistent/binary/path/claude-fake-12345",
	}, nil)

	err := p.HealthCheck(context.Background())
	if err == nil {
		t.Fatal("expected error for nonexistent binary")
	}
	if !strings.Contains(err.Error(), "claude-code health check") {
		t.Errorf("expected 'claude-code health check' in error, got: %v", err)
	}
}

func newTestLogger() *slog.Logger {
	return slog.Default()
}
