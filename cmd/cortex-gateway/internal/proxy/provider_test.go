package proxy

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/forwardqueue"
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

type streamRecorderProvider struct {
	name       string
	lastReq    *LLMRequest
	streamBody string
}

func (p *streamRecorderProvider) Name() string { return p.name }
func (p *streamRecorderProvider) Send(_ context.Context, _ *LLMRequest) (*LLMResponse, error) {
	return nil, errors.New("unexpected Send call")
}
func (p *streamRecorderProvider) HealthCheck(_ context.Context) error { return nil }
func (p *streamRecorderProvider) StreamHTTP(_ context.Context, req *LLMRequest, w http.ResponseWriter) error {
	p.lastReq = req
	w.Header().Set("Content-Type", "text/event-stream")
	_, err := w.Write([]byte(p.streamBody))
	return err
}

type timeoutAwareProvider struct {
	name  string
	sleep time.Duration
}

func (p *timeoutAwareProvider) Name() string { return p.name }
func (p *timeoutAwareProvider) Send(ctx context.Context, req *LLMRequest) (*LLMResponse, error) {
	if req != nil && req.ProviderTimeout > 0 {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, req.ProviderTimeout)
		defer cancel()
	}

	select {
	case <-time.After(p.sleep):
		return &LLMResponse{Content: "ok", Model: "test-model", FinishReason: "success"}, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}
func (p *timeoutAwareProvider) HealthCheck(_ context.Context) error { return nil }

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

func TestNewProviderFromConfig_AnthropicDirect(t *testing.T) {
	cfg := ProviderConfig{
		Name:    "test-anthropic-direct",
		Type:    "anthropic-direct",
		BaseURL: "https://api.anthropic.com",
		APIKey:  "test-key",
		Model:   "claude-sonnet-4-5-20250929",
	}
	p, err := NewProviderFromConfig(cfg)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if p.Name() != "test-anthropic-direct" {
		t.Errorf("expected name %q, got %q", "test-anthropic-direct", p.Name())
	}
}

func TestClaudeProviderSendUsesPassthroughHeaders(t *testing.T) {
	t.Helper()

	type capturedRequest struct {
		Authorization    string
		APIKey           string
		AnthropicVersion string
		AnthropicBeta    string
		SystemCount      int
	}

	captured := capturedRequest{}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		defer func() { _ = r.Body.Close() }()

		captured.Authorization = r.Header.Get("Authorization")
		captured.APIKey = r.Header.Get("x-api-key")
		captured.AnthropicVersion = r.Header.Get("anthropic-version")
		captured.AnthropicBeta = r.Header.Get("anthropic-beta")

		var reqBody map[string]any
		if err := json.NewDecoder(r.Body).Decode(&reqBody); err != nil {
			t.Fatalf("decode upstream request: %v", err)
		}
		if system, ok := reqBody["system"].([]any); ok {
			captured.SystemCount = len(system)
		}

		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{
			"content": []map[string]string{{"type": "text", "text": "ok"}},
			"model":   "claude-opus-4-6",
			"usage": map[string]int{
				"input_tokens":  11,
				"output_tokens": 7,
			},
			"stop_reason": "end_turn",
		})
	}))
	defer server.Close()

	provider := NewAnthropicDirectProvider(ProviderConfig{
		Name:    "anthropic-direct",
		Type:    "anthropic-direct",
		BaseURL: server.URL,
		APIKey:  "configured-key",
		Model:   "claude-opus-4-6",
	})

	resp, err := provider.Send(context.Background(), &LLMRequest{
		Model: "claude-opus-4-6",
		SystemBlocks: []SystemBlock{
			{Type: "text", Text: "<agent-identity>...</agent-identity>"},
		},
		Messages: []Message{{Role: "user", Content: "Hallo"}},
		PassthroughHeaders: map[string]string{
			"authorization":     "Bearer passthrough-token",
			"anthropic-version": "2023-06-01",
			"anthropic-beta":    "prompt-caching-2024-07-31",
		},
	})
	if err != nil {
		t.Fatalf("Send() error = %v", err)
	}
	if resp.Content != "ok" {
		t.Fatalf("response content = %q, want ok", resp.Content)
	}
	if captured.Authorization != "Bearer passthrough-token" {
		t.Fatalf("Authorization = %q", captured.Authorization)
	}
	if captured.APIKey != "" {
		t.Fatalf("x-api-key = %q, want empty when Authorization is passed through", captured.APIKey)
	}
	if captured.AnthropicVersion != "2023-06-01" {
		t.Fatalf("anthropic-version = %q", captured.AnthropicVersion)
	}
	if captured.AnthropicBeta != "prompt-caching-2024-07-31" {
		t.Fatalf("anthropic-beta = %q", captured.AnthropicBeta)
	}
	if captured.SystemCount != 1 {
		t.Fatalf("system block count = %d, want 1", captured.SystemCount)
	}
}

func TestClaudeProviderSendReturnsProviderErrorOnNon200(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusUnauthorized)
		_, _ = w.Write([]byte(`{"type":"error","error":{"type":"authentication_error","message":"x-api-key header is required"}}`))
	}))
	defer server.Close()

	provider := NewAnthropicDirectProvider(ProviderConfig{
		Name:    "anthropic-direct",
		Type:    "anthropic-direct",
		BaseURL: server.URL,
		Model:   "claude-opus-4-6",
	})

	_, err := provider.Send(context.Background(), &LLMRequest{
		Model:     "claude-opus-4-6",
		MaxTokens: 64,
		Messages:  []Message{{Role: "user", Content: "Hallo"}},
	})
	if err == nil {
		t.Fatal("expected provider error")
	}

	var provErr *ProviderError
	if !errors.As(err, &provErr) {
		t.Fatalf("expected ProviderError, got %v", err)
	}
	if provErr.StatusCode != http.StatusUnauthorized {
		t.Fatalf("status = %d, want %d", provErr.StatusCode, http.StatusUnauthorized)
	}
	if !strings.Contains(provErr.Message, "authentication_error") {
		t.Fatalf("message = %q", provErr.Message)
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

func TestQueuedProvider_QueueWaitDoesNotConsumeProviderTimeout(t *testing.T) {
	queue := forwardqueue.NewManager(1)
	provider := NewQueuedProvider(&timeoutAwareProvider{name: "timeout-aware", sleep: 10 * time.Millisecond}, queue)

	firstDone := make(chan struct{})
	go func() {
		release, err := queue.Acquire(context.Background())
		if err != nil {
			t.Errorf("pre-acquire queue slot: %v", err)
			close(firstDone)
			return
		}
		time.Sleep(80 * time.Millisecond)
		release()
		close(firstDone)
	}()

	time.Sleep(10 * time.Millisecond)

	resp, err := provider.Send(context.Background(), &LLMRequest{ProviderTimeout: 50 * time.Millisecond})
	if err != nil {
		t.Fatalf("queued provider send: %v", err)
	}
	if resp == nil || resp.Content != "ok" {
		t.Fatalf("unexpected response: %+v", resp)
	}

	select {
	case <-firstDone:
	case <-time.After(200 * time.Millisecond):
		t.Fatal("timed out waiting for queued slot holder to finish")
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
			Content: []json.RawMessage{anthropicTextBlockRaw("world")},
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

func TestClaudeProvider_Send_WithStructuredSystemBlocks(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var cReq claudeRequest
		if err := json.NewDecoder(r.Body).Decode(&cReq); err != nil {
			t.Fatalf("failed to decode request body: %v", err)
		}

		if len(cReq.System) != 2 {
			t.Fatalf("expected 2 system blocks, got %d", len(cReq.System))
		}
		if cReq.System[0].Text != "<agent-identity>\nDu bist Thomas Mueller.\n</agent-identity>" {
			t.Errorf("unexpected first system block: %+v", cReq.System[0])
		}
		if cReq.System[0].CacheControl == nil || cReq.System[0].CacheControl.Type != "ephemeral" {
			t.Errorf("expected cache_control=ephemeral on first system block, got %+v", cReq.System[0].CacheControl)
		}
		if cReq.System[1].Text != "legacy system message" {
			t.Errorf("unexpected second system block: %+v", cReq.System[1])
		}
		if len(cReq.Messages) != 1 || cReq.Messages[0].Role != "user" || cReq.Messages[0].Content != "hello" {
			t.Errorf("unexpected non-system messages: %+v", cReq.Messages)
		}

		resp := claudeResponse{
			Content: []json.RawMessage{anthropicTextBlockRaw("world")},
			Model:      "claude-sonnet-4-5-20250929",
			StopReason: "end_turn",
			Usage:      claudeUsage{InputTokens: 10, OutputTokens: 5},
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(resp)
	}))
	defer server.Close()

	p := NewAnthropicDirectProvider(ProviderConfig{
		Name:    "test-anthropic-direct",
		BaseURL: server.URL,
		APIKey:  "test-key",
		Model:   "claude-sonnet-4-5-20250929",
	})

	resp, err := p.Send(context.Background(), &LLMRequest{
		SystemBlocks: []SystemBlock{
			{
				Type: "text",
				Text: "<agent-identity>\nDu bist Thomas Mueller.\n</agent-identity>",
				CacheControl: &CacheControl{
					Type: "ephemeral",
				},
			},
		},
		Messages: []Message{
			{Role: "system", Content: "legacy system message"},
			{Role: "user", Content: "hello"},
		},
	})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if resp.Content != "world" {
		t.Errorf("expected content %q, got %q", "world", resp.Content)
	}
}

func TestSplitAnthropicMessages_PreservesStructuredSystemAndFiltersSystemMessages(t *testing.T) {
	systemBlocks, messages := splitAnthropicMessages(&LLMRequest{
		SystemBlocks: []SystemBlock{
			{
				Type: "text",
				Text: "<agent-identity>\nDu bist Thomas Mueller.\n</agent-identity>",
				CacheControl: &CacheControl{
					Type: "ephemeral",
				},
			},
		},
		Messages: []Message{
			{Role: "system", Content: "legacy system"},
			{Role: "user", Content: "hello"},
			{Role: "assistant", Content: "hi"},
		},
	})

	if len(systemBlocks) != 2 {
		t.Fatalf("expected 2 system blocks, got %d", len(systemBlocks))
	}
	if systemBlocks[0].Type != "text" || systemBlocks[0].Text == "" {
		t.Errorf("unexpected structured system block: %+v", systemBlocks[0])
	}
	if systemBlocks[0].CacheControl == nil || systemBlocks[0].CacheControl.Type != "ephemeral" {
		t.Errorf("expected cache_control on structured system block, got %+v", systemBlocks[0].CacheControl)
	}
	if systemBlocks[1].Text != "legacy system" {
		t.Errorf("expected legacy system message to be lifted into system blocks, got %+v", systemBlocks[1])
	}
	if len(messages) != 2 {
		t.Fatalf("expected 2 non-system messages, got %d", len(messages))
	}
	if messages[0].Role != "user" || messages[1].Role != "assistant" {
		t.Errorf("unexpected message roles: %+v", messages)
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
			Content: []json.RawMessage{anthropicTextBlockRaw("ok")},
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

func TestClaudeProviderSendPreservesRawContentBlocks(t *testing.T) {
	var upstreamContent any
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var payload map[string]any
		if err := json.NewDecoder(r.Body).Decode(&payload); err != nil {
			t.Fatalf("decode request: %v", err)
		}

		messages, ok := payload["messages"].([]any)
		if !ok || len(messages) != 1 {
			t.Fatalf("unexpected messages payload: %#v", payload["messages"])
		}
		first, ok := messages[0].(map[string]any)
		if !ok {
			t.Fatalf("unexpected first message: %#v", messages[0])
		}
		upstreamContent = first["content"]

		resp := claudeResponse{
			Content: []json.RawMessage{
				json.RawMessage(`{"type":"thinking","thinking":"chain","signature":"sig"}`),
				anthropicTextBlockRaw("done"),
			},
			Model:      "claude-sonnet-4-5-20250929",
			StopReason: "end_turn",
			Usage:      claudeUsage{InputTokens: 10, OutputTokens: 5},
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(resp)
	}))
	defer server.Close()

	p := NewAnthropicDirectProvider(ProviderConfig{
		Name:    "anthropic-direct",
		BaseURL: server.URL,
		APIKey:  "test-key",
		Model:   "claude-sonnet-4-5-20250929",
	})

	resp, err := p.Send(context.Background(), &LLMRequest{
		Messages: []Message{{
			Role: "user",
			ContentBlocks: []json.RawMessage{
				json.RawMessage(`{"type":"text","text":"hello"}`),
				json.RawMessage(`{"type":"tool_result","tool_use_id":"tool-1","content":"ok"}`),
			},
		}},
	})
	if err != nil {
		t.Fatalf("Send() error = %v", err)
	}

	blocks, ok := upstreamContent.([]any)
	if !ok || len(blocks) != 2 {
		t.Fatalf("upstream content = %#v, want 2 raw blocks", upstreamContent)
	}
	if resp.Content != "done" {
		t.Fatalf("response content = %q, want done", resp.Content)
	}
	if len(resp.ContentBlocks) != 2 {
		t.Fatalf("response content blocks = %d, want 2", len(resp.ContentBlocks))
	}
	if !strings.Contains(string(resp.ContentBlocks[0]), `"thinking"`) {
		t.Fatalf("expected thinking block, got %s", string(resp.ContentBlocks[0]))
	}
}

func TestClaudeProviderStreamHTTPRelaysSSE(t *testing.T) {
	var gotStream bool
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		defer func() { _ = r.Body.Close() }()

		var payload map[string]any
		if err := json.NewDecoder(r.Body).Decode(&payload); err != nil {
			t.Fatalf("decode stream request: %v", err)
		}
		gotStream, _ = payload["stream"].(bool)
		w.Header().Set("Content-Type", "text/event-stream")
		w.Header().Set("Cache-Control", "no-cache")
		_, _ = w.Write([]byte("event: message_start\ndata: {\"type\":\"message_start\"}\n\n"))
	}))
	defer server.Close()

	p := NewAnthropicDirectProvider(ProviderConfig{
		Name:    "anthropic-direct",
		BaseURL: server.URL,
		APIKey:  "test-key",
		Model:   "claude-sonnet-4-5-20250929",
	})

	rec := httptest.NewRecorder()
	err := p.StreamHTTP(context.Background(), &LLMRequest{
		Messages: []Message{{Role: "user", Content: "hello"}},
		Stream:   true,
	}, rec)
	if err != nil {
		t.Fatalf("StreamHTTP() error = %v", err)
	}
	if !gotStream {
		t.Fatal("expected upstream request to set stream=true")
	}
	if got := rec.Header().Get("Content-Type"); got != "text/event-stream" {
		t.Fatalf("Content-Type = %q, want text/event-stream", got)
	}
	if !strings.Contains(rec.Body.String(), "message_start") {
		t.Fatalf("expected SSE payload, got %q", rec.Body.String())
	}
}

func TestLLMRequestJSONDoesNotMarshalPassthroughHeaders(t *testing.T) {
	body, err := json.Marshal(LLMRequest{
		Messages: []Message{{Role: "user", Content: "hello"}},
		PassthroughHeaders: map[string]string{
			"authorization": "Bearer super-secret-token",
			"x-api-key":     "secret-key",
		},
	})
	if err != nil {
		t.Fatalf("marshal request: %v", err)
	}

	encoded := string(body)
	if strings.Contains(encoded, "super-secret-token") {
		t.Fatalf("authorization leaked into JSON payload: %s", encoded)
	}
	if strings.Contains(encoded, "secret-key") {
		t.Fatalf("x-api-key leaked into JSON payload: %s", encoded)
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

func TestSplitMessages_SystemAndUser(t *testing.T) {
	msgs := []Message{
		{Role: "system", Content: "Du bist Thomas Mueller, CEO."},
		{Role: "user", Content: "Was machst du?"},
	}
	sys, usr := splitMessages(msgs)
	if sys != "Du bist Thomas Mueller, CEO." {
		t.Errorf("expected system prompt, got %q", sys)
	}
	if usr != "Was machst du?" {
		t.Errorf("expected user prompt, got %q", usr)
	}
}

func TestSplitMessages_OnlyUser(t *testing.T) {
	msgs := []Message{
		{Role: "user", Content: "Hello world"},
	}
	sys, usr := splitMessages(msgs)
	if sys != "" {
		t.Errorf("expected empty system prompt, got %q", sys)
	}
	if usr != "Hello world" {
		t.Errorf("expected user prompt, got %q", usr)
	}
}

func TestSplitMessages_OnlySystem(t *testing.T) {
	msgs := []Message{
		{Role: "system", Content: "You are a persona."},
	}
	sys, usr := splitMessages(msgs)
	// When only system message exists, it becomes the user prompt
	if sys != "" {
		t.Errorf("expected empty system prompt for single-message case, got %q", sys)
	}
	if usr != "You are a persona." {
		t.Errorf("expected system content as user prompt, got %q", usr)
	}
}

func TestSplitMessages_MultiSystemAndAssistant(t *testing.T) {
	msgs := []Message{
		{Role: "system", Content: "Persona prompt."},
		{Role: "system", Content: "Perception injection."},
		{Role: "user", Content: "What do you do?"},
		{Role: "assistant", Content: "I chat."},
		{Role: "user", Content: "Tell me more."},
	}
	sys, usr := splitMessages(msgs)
	if !strings.Contains(sys, "Persona prompt.") || !strings.Contains(sys, "Perception injection.") {
		t.Errorf("expected both system messages in system prompt, got %q", sys)
	}
	if !strings.Contains(usr, "What do you do?") {
		t.Error("expected user message in user prompt")
	}
	if !strings.Contains(usr, "[Previous response: I chat.]") {
		t.Error("expected assistant message in user prompt")
	}
	if !strings.Contains(usr, "Tell me more.") {
		t.Error("expected second user message in user prompt")
	}
}

func TestSplitRequest_FlattensStructuredSystemBlocksForClaudeCode(t *testing.T) {
	systemPrompt, userPrompt := splitRequest(&LLMRequest{
		SystemBlocks: []SystemBlock{
			{
				Type: "text",
				Text: "<agent-identity>\nDu bist Thomas Mueller.\n</agent-identity>",
			},
			{
				Type: "text",
				Text: "<company-context>\nPixelPerfekt GmbH.\n</company-context>",
			},
		},
		Messages: []Message{
			{Role: "system", Content: "legacy system"},
			{Role: "user", Content: "Hallo"},
			{Role: "assistant", Content: "Servus"},
		},
	})

	if !strings.Contains(systemPrompt, "<agent-identity>") {
		t.Errorf("expected structured system block in flattened system prompt, got %q", systemPrompt)
	}
	if !strings.Contains(systemPrompt, "<company-context>") {
		t.Errorf("expected second structured system block in flattened system prompt, got %q", systemPrompt)
	}
	if !strings.Contains(systemPrompt, "legacy system") {
		t.Errorf("expected legacy system message in flattened system prompt, got %q", systemPrompt)
	}
	if !strings.Contains(userPrompt, "Hallo") {
		t.Errorf("expected user message in user prompt, got %q", userPrompt)
	}
	if !strings.Contains(userPrompt, "[Previous response: Servus]") {
		t.Errorf("expected assistant message in user prompt, got %q", userPrompt)
	}
}

func TestSplitRequest_UsesSystemPromptAsUserPromptWhenNoUserMessagesExist(t *testing.T) {
	systemPrompt, userPrompt := splitRequest(&LLMRequest{
		SystemBlocks: []SystemBlock{
			{
				Type: "text",
				Text: "<agent-identity>\nDu bist Thomas Mueller.\n</agent-identity>",
			},
		},
	})

	if systemPrompt != "" {
		t.Errorf("expected empty system prompt in single-payload fallback, got %q", systemPrompt)
	}
	if !strings.Contains(userPrompt, "<agent-identity>") {
		t.Errorf("expected structured system block to become user prompt fallback, got %q", userPrompt)
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

func TestDetectClaudeCodeLimitParsesUTCReset(t *testing.T) {
	now := time.Date(2026, time.March, 28, 20, 41, 0, 0, time.UTC)

	msg, until, ok := detectClaudeCodeLimit("claude-code result error: You've hit your limit - resets 9pm (UTC)", now)
	if !ok {
		t.Fatal("expected limit detection")
	}
	if !strings.Contains(msg, "hit your limit") {
		t.Fatalf("message = %q, want limit message", msg)
	}
	want := time.Date(2026, time.March, 28, 21, 0, 0, 0, time.UTC)
	if !until.Equal(want) {
		t.Fatalf("until = %s, want %s", until.Format(time.RFC3339), want.Format(time.RFC3339))
	}
}

func TestClaudeCodeProvider_SendShortCircuitsOnActiveLimitCooldown(t *testing.T) {
	dir := t.TempDir()
	counterPath := filepath.Join(dir, "count.txt")
	scriptPath := filepath.Join(dir, "claude-fake.sh")
	script := fmt.Sprintf(`#!/bin/sh
count=0
if [ -f %q ]; then
  count=$(cat %q)
fi
count=$((count + 1))
printf '%%s' "$count" > %q
printf '%%s\n' '{"type":"result","subtype":"error","is_error":true,"result":"You'\''ve hit your limit - resets 9pm (UTC)"}'
`, counterPath, counterPath, counterPath)
	if err := os.WriteFile(scriptPath, []byte(script), 0o700); err != nil { //nolint:gosec // temp test script must be executable
		t.Fatalf("write fake claude script: %v", err)
	}

	p := NewClaudeCodeProvider(ProviderConfig{
		Name:    "test",
		BaseURL: scriptPath,
	}, nil)

	req := &LLMRequest{Messages: []Message{{Role: "user", Content: "hello"}}}
	_, err := p.Send(context.Background(), req)
	if err == nil {
		t.Fatal("expected first send to fail with cooldown error")
	}
	var provErr *ProviderError
	if !errors.As(err, &provErr) || provErr.StatusCode != http.StatusTooManyRequests {
		t.Fatalf("expected provider 429 error, got %v", err)
	}

	_, err = p.Send(context.Background(), req)
	if err == nil {
		t.Fatal("expected second send to short-circuit on cooldown")
	}
	if !errors.As(err, &provErr) || provErr.StatusCode != http.StatusTooManyRequests {
		t.Fatalf("expected provider 429 error on cooldown, got %v", err)
	}

	data, err := os.ReadFile(counterPath) //nolint:gosec // temp counter path created by test
	if err != nil {
		t.Fatalf("read counter file: %v", err)
	}
	if got := strings.TrimSpace(string(data)); got != "1" {
		t.Fatalf("binary spawn count = %q, want 1", got)
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
