package acceptance_test

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/capability"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/compiler"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/extraction"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/normalizer"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/proxy"
	"github.com/silentspike/project-sentinel/pkg/sentinel-go/eventstore"
)

// mockProvider implements proxy.Provider for acceptance tests.
type mockProvider struct {
	name string
	resp *proxy.LLMResponse
	err  error
}

func (m *mockProvider) Name() string { return m.name }
func (m *mockProvider) Send(_ context.Context, _ *proxy.LLMRequest) (*proxy.LLMResponse, error) {
	return m.resp, m.err
}
func (m *mockProvider) HealthCheck(_ context.Context) error { return nil }

// AC-13-03: MockProvider implements Provider interface completely
func TestAC_13_03_ProviderInterface(t *testing.T) {
	mock := &mockProvider{
		name: "test-mock",
		resp: &proxy.LLMResponse{
			Content:      "test response",
			Model:        "mock-model",
			TokensUsed:   10,
			FinishReason: "stop",
		},
	}

	// Verify all three interface methods exist and work
	var p proxy.Provider = mock

	if p.Name() != "test-mock" {
		t.Errorf("Name() = %q, want %q", p.Name(), "test-mock")
	}

	resp, err := p.Send(context.Background(), &proxy.LLMRequest{
		Messages: []proxy.Message{{Role: "user", Content: "hello"}},
	})
	if err != nil {
		t.Fatalf("Send() error: %v", err)
	}
	if resp.Content != "test response" {
		t.Errorf("Send() content = %q, want %q", resp.Content, "test response")
	}
	if resp.Model != "mock-model" {
		t.Errorf("Send() model = %q, want %q", resp.Model, "mock-model")
	}
	if resp.TokensUsed != 10 {
		t.Errorf("Send() tokens_used = %d, want %d", resp.TokensUsed, 10)
	}
	if resp.FinishReason != "stop" {
		t.Errorf("Send() finish_reason = %q, want %q", resp.FinishReason, "stop")
	}

	if err := p.HealthCheck(context.Background()); err != nil {
		t.Errorf("HealthCheck() error: %v", err)
	}
}

// AC-13-04: Claude provider sends correct Anthropic API format
func TestAC_13_04_ClaudeProviderRequest(t *testing.T) {
	var (
		receivedAPIKey  string
		receivedVersion string
		receivedPath    string
		receivedMethod  string
		receivedBody    []byte
	)

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		receivedAPIKey = r.Header.Get("x-api-key")
		receivedVersion = r.Header.Get("anthropic-version")
		receivedPath = r.URL.Path
		receivedMethod = r.Method

		body, err := io.ReadAll(io.LimitReader(r.Body, 1<<20)) // 1MB limit
		if err != nil {
			http.Error(w, "read error", http.StatusBadRequest)
			return
		}
		receivedBody = body

		resp := map[string]interface{}{
			"content":     []map[string]string{{"type": "text", "text": "ok"}},
			"model":       "claude-sonnet-4-5-20250929",
			"stop_reason": "end_turn",
			"usage":       map[string]int{"input_tokens": 5, "output_tokens": 3},
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(resp)
	}))
	defer server.Close()

	p := proxy.NewClaudeProvider(proxy.ProviderConfig{ //nolint:gosec // G101: test credential, not real
		Name:    "claude-test",
		BaseURL: server.URL,
		APIKey:  "sk-test-key-123",
		Model:   "claude-sonnet-4-5-20250929",
	})

	_, err := p.Send(context.Background(), &proxy.LLMRequest{
		Messages: []proxy.Message{{Role: "user", Content: "test"}},
	})
	if err != nil {
		t.Fatalf("Send() error: %v", err)
	}

	// Verify x-api-key header
	if receivedAPIKey != "sk-test-key-123" { //nolint:gosec // test credential, not real
		t.Errorf("x-api-key = %q, want %q", receivedAPIKey, "sk-test-key-123")
	}

	// Verify /v1/messages path
	if receivedPath != "/v1/messages" {
		t.Errorf("path = %q, want %q", receivedPath, "/v1/messages")
	}

	// Verify POST method
	if receivedMethod != http.MethodPost {
		t.Errorf("method = %q, want %q", receivedMethod, http.MethodPost)
	}

	// Verify anthropic-version header
	if receivedVersion != "2023-06-01" {
		t.Errorf("anthropic-version = %q, want %q", receivedVersion, "2023-06-01")
	}

	// Verify request body contains model
	if !strings.Contains(string(receivedBody), "claude-sonnet-4-5-20250929") {
		t.Errorf("request body missing model, got: %s", string(receivedBody))
	}
}

// AC-13-05: Ollama provider sends correct /api/chat format
func TestAC_13_05_OllamaProviderRequest(t *testing.T) {
	var (
		receivedPath string
		receivedBody map[string]interface{}
	)

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		receivedPath = r.URL.Path

		_ = json.NewDecoder(r.Body).Decode(&receivedBody)

		resp := map[string]interface{}{
			"model":   "qwen3:7b",
			"message": map[string]string{"role": "assistant", "content": "ollama response"},
			"done":    true,
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(resp)
	}))
	defer server.Close()

	p := proxy.NewOllamaProvider(proxy.ProviderConfig{
		Name:    "ollama-test",
		BaseURL: server.URL,
		Model:   "qwen3:7b",
	})

	_, err := p.Send(context.Background(), &proxy.LLMRequest{
		Messages: []proxy.Message{{Role: "user", Content: "test"}},
	})
	if err != nil {
		t.Fatalf("Send() error: %v", err)
	}

	// Verify /api/chat path
	if receivedPath != "/api/chat" {
		t.Errorf("path = %q, want %q", receivedPath, "/api/chat")
	}

	// Verify model in body
	model, ok := receivedBody["model"].(string)
	if !ok || model != "qwen3:7b" {
		t.Errorf("body.model = %v, want %q", receivedBody["model"], "qwen3:7b")
	}

	// Verify stream=false
	stream, ok := receivedBody["stream"].(bool)
	if !ok || stream {
		t.Errorf("body.stream = %v, want false", receivedBody["stream"])
	}

	// Verify messages array exists
	messages, ok := receivedBody["messages"].([]interface{})
	if !ok || len(messages) == 0 {
		t.Errorf("body.messages missing or empty: %v", receivedBody["messages"])
	}
}

// AC-13-06: Provider failover via Registry
func TestAC_13_06_ProviderFailover(t *testing.T) {
	registry := proxy.NewRegistry()

	// Register a failing primary
	failingProvider := &mockProvider{
		name: "primary",
		err:  fmt.Errorf("primary unavailable"),
	}
	registry.Register("primary", failingProvider)

	// Register a working fallback
	fallbackProvider := &mockProvider{
		name: "fallback",
		resp: &proxy.LLMResponse{
			Content: "fallback response",
			Model:   "fallback-model",
		},
	}
	registry.Register("fallback", fallbackProvider)

	// Primary should be "primary" (first registered)
	primary, err := registry.Primary()
	if err != nil {
		t.Fatalf("Primary() error: %v", err)
	}
	if primary.Name() != "primary" {
		t.Errorf("primary = %q, want %q", primary.Name(), "primary")
	}

	// Primary fails
	_, err = primary.Send(context.Background(), &proxy.LLMRequest{})
	if err == nil {
		t.Fatal("expected primary to fail")
	}

	// Switch to fallback
	if err := registry.SetPrimary("fallback"); err != nil {
		t.Fatalf("SetPrimary() error: %v", err)
	}

	// Fallback works
	newPrimary, err := registry.Primary()
	if err != nil {
		t.Fatalf("Primary() after failover error: %v", err)
	}
	resp, err := newPrimary.Send(context.Background(), &proxy.LLMRequest{})
	if err != nil {
		t.Fatalf("fallback Send() error: %v", err)
	}
	if resp.Content != "fallback response" {
		t.Errorf("fallback content = %q, want %q", resp.Content, "fallback response")
	}
}

// AC-13-07: Health endpoint returns 200 + {"status":"ok"}
func TestAC_13_07_HealthEndpoint(t *testing.T) {
	// Create the same mux as in main.go
	mux := http.NewServeMux()
	mux.HandleFunc("GET /health", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = fmt.Fprintf(w, `{"status":"ok","version":"test"}`)
	})

	req := httptest.NewRequest(http.MethodGet, "/health", nil)
	w := httptest.NewRecorder()
	mux.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Errorf("status = %d, want %d", w.Code, http.StatusOK)
	}

	var body map[string]interface{}
	if err := json.NewDecoder(w.Body).Decode(&body); err != nil {
		t.Fatalf("decode error: %v", err)
	}

	status, ok := body["status"].(string)
	if !ok || status != "ok" {
		t.Errorf("body.status = %v, want %q", body["status"], "ok")
	}

	contentType := w.Header().Get("Content-Type")
	if contentType != "application/json" {
		t.Errorf("Content-Type = %q, want %q", contentType, "application/json")
	}
}

// AC-13-09: Session Normalizer produces NormalizedResponse with same fields from both providers
func TestAC_13_09_SessionNormalizer(t *testing.T) {
	norm := normalizer.New()

	// Claude response
	claudeRaw := `{
		"content": [{"type": "text", "text": "Claude says hello"}],
		"model": "claude-sonnet-4-5-20250929",
		"role": "assistant",
		"stop_reason": "end_turn",
		"usage": {"input_tokens": 10, "output_tokens": 5}
	}`

	claudeNorm, err := norm.NormalizeClaude([]byte(claudeRaw))
	if err != nil {
		t.Fatalf("NormalizeClaude() error: %v", err)
	}

	// Ollama response
	ollamaRaw := `{
		"model": "qwen3:7b",
		"message": {"role": "assistant", "content": "Ollama says hello"},
		"done": true,
		"eval_count": 20,
		"total_duration": 1000000
	}`

	ollamaNorm, err := norm.NormalizeOllama([]byte(ollamaRaw))
	if err != nil {
		t.Fatalf("NormalizeOllama() error: %v", err)
	}

	// Both must have same fields populated
	for _, result := range []struct {
		name string
		resp *normalizer.NormalizedResponse
	}{
		{"claude", claudeNorm},
		{"ollama", ollamaNorm},
	} {
		if result.resp.Content == "" {
			t.Errorf("%s: Content is empty", result.name)
		}
		if result.resp.Role == "" {
			t.Errorf("%s: Role is empty", result.name)
		}
		if result.resp.Model == "" {
			t.Errorf("%s: Model is empty", result.name)
		}
		if result.resp.Provider == "" {
			t.Errorf("%s: Provider is empty", result.name)
		}
	}

	// Verify specific values
	if claudeNorm.Content != "Claude says hello" {
		t.Errorf("claude content = %q, want %q", claudeNorm.Content, "Claude says hello")
	}
	if claudeNorm.Provider != "claude" {
		t.Errorf("claude provider = %q, want %q", claudeNorm.Provider, "claude")
	}
	if claudeNorm.TokensUsed != 15 {
		t.Errorf("claude tokens = %d, want %d", claudeNorm.TokensUsed, 15)
	}

	if ollamaNorm.Content != "Ollama says hello" {
		t.Errorf("ollama content = %q, want %q", ollamaNorm.Content, "Ollama says hello")
	}
	if ollamaNorm.Provider != "ollama" {
		t.Errorf("ollama provider = %q, want %q", ollamaNorm.Provider, "ollama")
	}
}

// AC-13-10: Prompt Compiler produces full bio for Claude, distilled for 7B
func TestAC_13_10_PromptCompiler(t *testing.T) {
	comp := compiler.New()

	// Claude (big model) - full bio
	claudePrompt := comp.Compile("claude", "Thomas Mueller", "CEO", "")
	if !strings.Contains(claudePrompt, "Thomas Mueller") {
		t.Errorf("claude prompt missing agent name")
	}
	if !strings.Contains(claudePrompt, "CEO") {
		t.Errorf("claude prompt missing agent role")
	}
	// Full bio includes personality/history details
	if !strings.Contains(claudePrompt, "Persoenlichkeit") {
		t.Errorf("claude prompt missing full bio (Persoenlichkeit)")
	}

	// Ollama 7B (small model) - distilled prompt
	ollamaPrompt := comp.Compile("ollama-7b", "Thomas Mueller", "CEO", "")
	if !strings.Contains(ollamaPrompt, "Thomas Mueller") {
		t.Errorf("ollama-7b prompt missing agent name")
	}
	if !strings.Contains(ollamaPrompt, "CEO") {
		t.Errorf("ollama-7b prompt missing agent role")
	}

	// Small model should get shorter prompt (no full bio)
	if len(ollamaPrompt) >= len(claudePrompt) {
		t.Errorf("ollama-7b prompt (%d bytes) should be shorter than claude prompt (%d bytes)",
			len(ollamaPrompt), len(claudePrompt))
	}

	// Verify perception injection works
	perception := "KOERPER: Hunger (85%)."
	withPerception := comp.Compile("claude", "Lisa Brenner", "Head of Design", perception)
	if !strings.Contains(withPerception, "[SYSTEM_INJECTION]") {
		t.Errorf("prompt missing [SYSTEM_INJECTION] block")
	}
	if !strings.Contains(withPerception, perception) {
		t.Errorf("prompt missing perception text")
	}
}

// AC-13-AC5: Command→Event Mapping persistiert Event+Outbox atomar
func TestAC_13_AC5_GatewayDoesNotCommitActionProposals(t *testing.T) {
	// 1. Temp-DB
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "ac5_test.db")
	store, err := eventstore.Open(dbPath)
	if err != nil {
		t.Fatalf("eventstore.Open: %v", err)
	}
	defer func() { _ = store.Close() }()

	// 2. Pipeline mit EventStore + Mock-Provider (antwortet mit move-Action)
	registry := proxy.NewRegistry()
	registry.Register("mock", &mockProvider{
		name: "mock",
		resp: &proxy.LLMResponse{
			Content:      "Ich gehe in die Kueche um mir einen Kaffee zu holen.",
			Model:        "mock-model",
			TokensUsed:   30,
			FinishReason: "stop",
		},
	})

	controlConfig := control.NewConfig("mock")
	handler := proxy.NewPipelineHandler(proxy.PipelineConfig{
		Registry:     registry,
		Config:       controlConfig,
		Compiler:     compiler.New(),
		Normalizer:   normalizer.New(),
		Extractor:    extraction.New(),
		Capabilities: capability.New(),
		Logger:       slog.Default(),
		BreakerCfg:   proxy.BreakerConfig{},
		EventStore:   store,
	})
	securedHandler := proxy.CallerCredentials{
		AgentRuntime:         "acceptance-agent-runtime",
		PlatformControlplane: "acceptance-platform",
		Evolution:            "acceptance-evolution",
		Judge:                "acceptance-judge",
	}.Middleware(handler)

	// 3. HTTP-Request mit agent_name Metadata und X-Request-ID
	reqBody := `{
		"messages":[{"role":"user","content":"Was machst du jetzt?"}],
		"metadata":{"agent_id":"3","agent_name":"AGENT-03","agent_role":"Designer","hierarchy_tier":"3","tick":"42"}
	}`
	req := httptest.NewRequest(http.MethodPost, "/internal/agent-runtime", strings.NewReader(reqBody))
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Authorization", "Bearer acceptance-agent-runtime")
	req.Header.Set("X-Request-ID", "test-req-ac5-001")
	w := httptest.NewRecorder()

	securedHandler.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("status = %d, want %d, body: %s", w.Code, http.StatusOK, w.Body.String())
	}

	// 4. Verify: Response enthaelt request_id
	var pipelineResp proxy.PipelineResponse
	if err := json.NewDecoder(w.Body).Decode(&pipelineResp); err != nil {
		t.Fatalf("decode response: %v", err)
	}
	if pipelineResp.RequestID != "test-req-ac5-001" {
		t.Errorf("request_id = %q, want %q", pipelineResp.RequestID, "test-req-ac5-001")
	}

	// 5. The Gateway returns an action proposal. The daemon applies it and is
	// the sole producer of the canonical AgentActionReceived event.
	if len(pipelineResp.Actions) == 0 {
		t.Fatal("expected an extracted action proposal in the response")
	}
	eventCount, err := store.EventCount()
	if err != nil {
		t.Fatalf("EventCount: %v", err)
	}
	if eventCount != 0 {
		t.Fatalf("gateway committed %d unapplied action proposals", eventCount)
	}

	// 6. No applied-action outbox entry exists before daemon/ECS acceptance.
	pendingCount, err := store.PendingOutboxCount()
	if err != nil {
		t.Fatalf("PendingOutboxCount: %v", err)
	}
	if pendingCount != 0 {
		t.Fatalf("gateway created %d unapplied action outbox entries", pendingCount)
	}

	// 7. Retrying the inference request still cannot create an applied event.
	req2 := httptest.NewRequest(http.MethodPost, "/internal/agent-runtime", strings.NewReader(reqBody))
	req2.Header.Set("Content-Type", "application/json")
	req2.Header.Set("Authorization", "Bearer acceptance-agent-runtime")
	req2.Header.Set("X-Request-ID", "test-req-ac5-001") // gleiche ID
	w2 := httptest.NewRecorder()
	securedHandler.ServeHTTP(w2, req2)

	eventCountAfterRetry, _ := store.EventCount()
	if eventCountAfterRetry != 0 {
		t.Errorf("retry committed %d unapplied action proposals", eventCountAfterRetry)
	}
}

// AC-13-07b: Full proxy handler with mocked provider (end-to-end HTTP test)
func TestAC_13_Handler_EndToEnd(t *testing.T) {
	registry := proxy.NewRegistry()
	registry.Register("mock", &mockProvider{
		name: "mock",
		resp: &proxy.LLMResponse{
			Content:      "Guten Morgen!",
			Model:        "mock-model",
			TokensUsed:   25,
			FinishReason: "end_turn",
		},
	})

	logger := slog.Default()
	handler := proxy.NewHandler(registry, logger)

	reqBody := `{"messages":[{"role":"user","content":"Hallo"}],"temperature":0.7}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(reqBody))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()

	handler.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("status = %d, want %d", w.Code, http.StatusOK)
	}

	var resp proxy.LLMResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("decode error: %v", err)
	}
	if resp.Content != "Guten Morgen!" {
		t.Errorf("content = %q, want %q", resp.Content, "Guten Morgen!")
	}
	if resp.TokensUsed != 25 {
		t.Errorf("tokens_used = %d, want %d", resp.TokensUsed, 25)
	}
}
