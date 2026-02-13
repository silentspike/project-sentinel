package proxy

import (
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/capability"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/compiler"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/extraction"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/normalizer"
)

// pipelineMockProvider implementiert Provider fuer Pipeline-Tests.
// Kann Requests aufzeichnen und konfigurierbare Responses/Errors liefern.
type pipelineMockProvider struct {
	name      string
	resp      *LLMResponse
	err       error
	calls     int
	lastReq   *LLMRequest
	sendFunc  func(ctx context.Context, req *LLMRequest) (*LLMResponse, error)
}

func (p *pipelineMockProvider) Name() string { return p.name }
func (p *pipelineMockProvider) Send(ctx context.Context, req *LLMRequest) (*LLMResponse, error) {
	p.calls++
	p.lastReq = req
	if p.sendFunc != nil {
		return p.sendFunc(ctx, req)
	}
	return p.resp, p.err
}
func (p *pipelineMockProvider) HealthCheck(_ context.Context) error { return nil }

func newTestPipelineHandler(registry *Registry, controlCfg *control.Config) *PipelineHandler {
	if controlCfg == nil {
		controlCfg = control.NewConfig("mock")
	}
	return NewPipelineHandler(PipelineConfig{
		Registry:     registry,
		Config:       controlCfg,
		Compiler:     compiler.New(),
		Normalizer:   normalizer.New(),
		Extractor:    extraction.New(),
		Capabilities: capability.New(),
		Logger:       slog.Default(),
		BreakerCfg:   testConfig(),
	})
}

func TestPipelineFullFlow(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		resp: &LLMResponse{
			Content:      "Ich gehe jetzt in die Kueche. *streckt sich*",
			Model:        "test-model",
			TokensUsed:   50,
			FinishReason: "end_turn",
		},
	}
	reg.Register("mock", mock)

	ph := newTestPipelineHandler(reg, nil)

	body := `{"messages":[{"role":"user","content":"Was machst du?"}],"metadata":{"agent_name":"Max Mueller","agent_role":"Senior Entwickler","perception":"CIRCADIAN: 11:42"}}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()

	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected status %d, got %d: %s", http.StatusOK, w.Code, w.Body.String())
	}

	var resp PipelineResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}

	if resp.Content == "" {
		t.Error("expected non-empty content")
	}
	if resp.Provider != "mock" {
		t.Errorf("expected provider %q, got %q", "mock", resp.Provider)
	}
	if resp.TokensUsed != 50 {
		t.Errorf("expected tokens_used 50, got %d", resp.TokensUsed)
	}

	// Actions sollten extrahiert worden sein (move + emote)
	if len(resp.Actions) == 0 {
		t.Error("expected actions to be extracted")
	}

	// Perception Injection: System-Message sollte am Anfang stehen
	if mock.lastReq == nil {
		t.Fatal("expected provider to have received a request")
	}
	if len(mock.lastReq.Messages) < 1 {
		t.Fatal("expected at least 1 message")
	}
	if mock.lastReq.Messages[0].Role != "system" {
		t.Errorf("expected first message role %q, got %q", "system", mock.lastReq.Messages[0].Role)
	}
	if !strings.Contains(mock.lastReq.Messages[0].Content, "Max Mueller") {
		t.Error("expected system message to contain agent name")
	}
	if !strings.Contains(mock.lastReq.Messages[0].Content, "[SYSTEM_INJECTION]") {
		t.Error("expected system message to contain perception injection")
	}
}

func TestPipelineMethodNotAllowed(t *testing.T) {
	reg := NewRegistry()
	reg.Register("mock", &pipelineMockProvider{name: "mock", resp: &LLMResponse{}})
	ph := newTestPipelineHandler(reg, nil)

	req := httptest.NewRequest(http.MethodGet, "/v1/chat/completions", nil)
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusMethodNotAllowed {
		t.Errorf("expected %d, got %d", http.StatusMethodNotAllowed, w.Code)
	}
}

func TestPipelineInvalidJSON(t *testing.T) {
	reg := NewRegistry()
	reg.Register("mock", &pipelineMockProvider{name: "mock", resp: &LLMResponse{}})
	ph := newTestPipelineHandler(reg, nil)

	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader("not json"))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusBadRequest {
		t.Errorf("expected %d, got %d", http.StatusBadRequest, w.Code)
	}
}

func TestPipelineNoProvider(t *testing.T) {
	reg := NewRegistry()
	ph := newTestPipelineHandler(reg, nil)

	body := `{"messages":[{"role":"user","content":"hello"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusServiceUnavailable {
		t.Errorf("expected %d, got %d", http.StatusServiceUnavailable, w.Code)
	}
}

func TestPipelineConfigOverride(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		resp: &LLMResponse{Content: "ok", Model: "m", TokensUsed: 1, FinishReason: "stop"},
	}
	reg.Register("mock", mock)

	cfg := control.NewConfig("mock")
	// Setze spezifische Werte via Update
	_ = cfg.Update(map[string]interface{}{
		"temperature": 0.3,
		"max_tokens":  float64(512),
	})

	ph := newTestPipelineHandler(reg, cfg)

	body := `{"messages":[{"role":"user","content":"test"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected %d, got %d: %s", http.StatusOK, w.Code, w.Body.String())
	}
	if mock.lastReq.Temperature != 0.3 {
		t.Errorf("expected temperature 0.3, got %f", mock.lastReq.Temperature)
	}
	if mock.lastReq.MaxTokens != 512 {
		t.Errorf("expected max_tokens 512, got %d", mock.lastReq.MaxTokens)
	}
}

func TestPipelineBreakerTrip503(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		err:  errors.New("transport error"),
		resp: nil,
	}
	reg.Register("mock", mock)

	cfg := control.NewConfig("mock")
	ph := newTestPipelineHandler(reg, cfg)

	// Trip breaker: 3 aufeinanderfolgende Failures
	for i := 0; i < 3; i++ {
		body := `{"messages":[{"role":"user","content":"fail"}]}`
		req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
		w := httptest.NewRecorder()
		ph.ServeHTTP(w, req)
		// Sollte 502 sein (Provider-Fehler)
		if w.Code != http.StatusBadGateway {
			t.Fatalf("request %d: expected %d, got %d", i, http.StatusBadGateway, w.Code)
		}
	}

	// Naechster Request sollte 503 sein (Breaker offen, kein Failover)
	body := `{"messages":[{"role":"user","content":"blocked"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusServiceUnavailable {
		t.Errorf("expected %d (circuit breaker open), got %d: %s", http.StatusServiceUnavailable, w.Code, w.Body.String())
	}
}

func TestPipelineFailover(t *testing.T) {
	reg := NewRegistry()
	// Primary Provider: fehlerhaft
	failing := &pipelineMockProvider{
		name: "failing",
		err:  errors.New("transport error"),
	}
	reg.Register("failing", failing)

	// Fallback Provider: funktioniert
	fallback := &pipelineMockProvider{
		name: "fallback",
		resp: &LLMResponse{Content: "fallback ok", Model: "m", TokensUsed: 1, FinishReason: "stop"},
	}
	reg.Register("fallback", fallback)

	cfg := control.NewConfig("failing")
	ph := newTestPipelineHandler(reg, cfg)

	// Trip primary breaker
	for i := 0; i < 3; i++ {
		body := `{"messages":[{"role":"user","content":"fail"}]}`
		req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
		w := httptest.NewRecorder()
		ph.ServeHTTP(w, req)
	}

	// Naechster Request: Primary ist offen, Failover auf "fallback"
	body := `{"messages":[{"role":"user","content":"test"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected %d (failover), got %d: %s", http.StatusOK, w.Code, w.Body.String())
	}

	var resp PipelineResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}
	if resp.Provider != "fallback" {
		t.Errorf("expected provider %q (failover), got %q", "fallback", resp.Provider)
	}
}

func TestPipelineWithoutPerception(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		resp: &LLMResponse{Content: "Hallo Welt", Model: "m", TokensUsed: 5, FinishReason: "stop"},
	}
	reg.Register("mock", mock)

	ph := newTestPipelineHandler(reg, nil)

	// Request ohne metadata → kein Perception Injection
	body := `{"messages":[{"role":"user","content":"hello"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected %d, got %d", http.StatusOK, w.Code)
	}

	// Ohne perception sollte keine System-Message injiziert werden
	if mock.lastReq == nil {
		t.Fatal("expected provider to have received a request")
	}
	for _, msg := range mock.lastReq.Messages {
		if msg.Role == "system" && strings.Contains(msg.Content, "[SYSTEM_INJECTION]") {
			t.Error("expected no perception injection without metadata")
		}
	}
}

func TestPipelineProviderError(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		err:  errors.New("upstream timeout"),
	}
	reg.Register("mock", mock)

	ph := newTestPipelineHandler(reg, nil)

	body := `{"messages":[{"role":"user","content":"hello"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusBadGateway {
		t.Errorf("expected %d, got %d", http.StatusBadGateway, w.Code)
	}
}

func TestPipelineActionExtraction(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		resp: &LLMResponse{
			Content:      "*seufzt* Ich gehe in die Kueche.",
			Model:        "m",
			TokensUsed:   10,
			FinishReason: "stop",
		},
	}
	reg.Register("mock", mock)

	ph := newTestPipelineHandler(reg, nil)

	body := `{"messages":[{"role":"user","content":"test"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected %d, got %d", http.StatusOK, w.Code)
	}

	var resp PipelineResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}

	if len(resp.Actions) == 0 {
		t.Fatal("expected extracted actions")
	}

	// Sollte emote (*seufzt*) und move (gehe in die Kueche) enthalten
	hasEmote := false
	hasMove := false
	for _, a := range resp.Actions {
		switch a.Type {
		case "emote":
			hasEmote = true
		case "move":
			hasMove = true
		}
	}
	if !hasEmote {
		t.Error("expected emote action to be extracted")
	}
	if !hasMove {
		t.Error("expected move action to be extracted")
	}
}

func TestJudgeProviderAdapter(t *testing.T) {
	mock := &pipelineMockProvider{
		name: "mock",
		resp: &LLMResponse{Content: "judge says: clean", Model: "m", TokensUsed: 1},
	}

	adapter := NewJudgeProviderAdapter(mock)

	result, err := adapter.Send(context.Background(), "Is this a fourth wall break?", 0.3, 256)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if result != "judge says: clean" {
		t.Errorf("expected %q, got %q", "judge says: clean", result)
	}
	if mock.calls != 1 {
		t.Errorf("expected 1 call, got %d", mock.calls)
	}
	if mock.lastReq.Temperature != 0.3 {
		t.Errorf("expected temperature 0.3, got %f", mock.lastReq.Temperature)
	}
	if mock.lastReq.MaxTokens != 256 {
		t.Errorf("expected max_tokens 256, got %d", mock.lastReq.MaxTokens)
	}
}

func TestPrependSystemMessage(t *testing.T) {
	t.Run("prepend to empty", func(t *testing.T) {
		msgs := prependSystemMessage(nil, "system prompt")
		if len(msgs) != 1 {
			t.Fatalf("expected 1 message, got %d", len(msgs))
		}
		if msgs[0].Role != "system" || msgs[0].Content != "system prompt" {
			t.Errorf("unexpected message: %+v", msgs[0])
		}
	})

	t.Run("replace existing system", func(t *testing.T) {
		msgs := []Message{
			{Role: "system", Content: "old"},
			{Role: "user", Content: "hello"},
		}
		result := prependSystemMessage(msgs, "new system")
		if len(result) != 2 {
			t.Fatalf("expected 2 messages, got %d", len(result))
		}
		if result[0].Content != "new system" {
			t.Errorf("expected replaced system message, got %q", result[0].Content)
		}
	})

	t.Run("prepend before user", func(t *testing.T) {
		msgs := []Message{
			{Role: "user", Content: "hello"},
		}
		result := prependSystemMessage(msgs, "system prompt")
		if len(result) != 2 {
			t.Fatalf("expected 2 messages, got %d", len(result))
		}
		if result[0].Role != "system" {
			t.Errorf("expected first message to be system, got %q", result[0].Role)
		}
		if result[1].Role != "user" {
			t.Errorf("expected second message to be user, got %q", result[1].Role)
		}
	})
}

func TestAppendCorrectionMessage(t *testing.T) {
	msgs := []Message{
		{Role: "user", Content: "hello"},
		{Role: "assistant", Content: "I am an AI"},
	}
	result := appendCorrectionMessage(msgs, "correction text")

	if len(result) != 3 {
		t.Fatalf("expected 3 messages, got %d", len(result))
	}
	if result[2].Role != "system" {
		t.Errorf("expected correction role %q, got %q", "system", result[2].Role)
	}
	if result[2].Content != "correction text" {
		t.Errorf("expected correction content, got %q", result[2].Content)
	}

	// Original-Slice darf nicht veraendert worden sein
	if len(msgs) != 2 {
		t.Error("original slice was modified")
	}
}
