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
	"github.com/obtFusi/project-sentinel/pkg/sentinel-go/judge"
)

// pipelineMockProvider implementiert Provider fuer Pipeline-Tests.
// Kann Requests aufzeichnen und konfigurierbare Responses/Errors liefern.
type pipelineMockProvider struct {
	name     string
	resp     *LLMResponse
	err      error
	calls    int
	lastReq  *LLMRequest
	sendFunc func(ctx context.Context, req *LLMRequest) (*LLMResponse, error)
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

func TestBreakerStatesEmpty(t *testing.T) {
	reg := NewRegistry()
	ph := newTestPipelineHandler(reg, nil)

	states := ph.BreakerStates()
	if len(states) != 0 {
		t.Errorf("BreakerStates() = %v, want empty map", states)
	}
}

func TestBreakerStatesReflectsState(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "test-provider",
		resp: &LLMResponse{Content: "ok", Model: "m", TokensUsed: 1, FinishReason: "end_turn"},
	}
	reg.Register("test-provider", mock)
	ph := newTestPipelineHandler(reg, nil)

	// Trigger breaker creation by calling getBreaker
	cb := ph.getBreaker("test-provider")
	if cb == nil {
		t.Fatal("getBreaker returned nil")
	}

	states := ph.BreakerStates()
	if len(states) != 1 {
		t.Fatalf("BreakerStates() has %d entries, want 1", len(states))
	}
	if got := states["test-provider"]; got != "closed" {
		t.Errorf("BreakerStates()[test-provider] = %q, want %q", got, "closed")
	}

	// Trip the breaker
	for i := 0; i < 3; i++ {
		cb.Allow()
		cb.Record(errors.New("fail"))
	}

	states = ph.BreakerStates()
	if got := states["test-provider"]; got != "open" {
		t.Errorf("BreakerStates()[test-provider] = %q, want %q after trip", got, "open")
	}
}

// newTestPipelineHandlerWithDrift creates a PipelineHandler with DriftDetector + QualityScorer.
func newTestPipelineHandlerWithDrift(registry *Registry, controlCfg *control.Config, drift *judge.DriftDetector, quality *judge.QualityScorer) *PipelineHandler {
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
		Drift:        drift,
		Quality:      quality,
	})
}

func TestPersonalityGuardDisabledByDefault(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		resp: &LLMResponse{Content: "!!!!!!! SUPER EXCITED !!!!!!!", Model: "m", TokensUsed: 10, FinishReason: "stop"},
	}
	reg.Register("mock", mock)

	drift := judge.NewDriftDetector()
	drift.RegisterProfile("AGENT-01", judge.PersonalityProfile{
		Role:         "Developer",
		Extraversion: 0.1, // introvert — exclamations = high drift
		Neuroticism:  0.3,
	})
	quality := judge.NewQualityScorer(drift)

	// Guard disabled by default
	ph := newTestPipelineHandlerWithDrift(reg, nil, drift, quality)

	body := `{"messages":[{"role":"user","content":"test"}],"metadata":{"agent_id":"1","agent_name":"AGENT-01"}}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("want 200, got %d: %s", w.Code, w.Body.String())
	}

	// Provider should have been called exactly once (no re-gen since guard is disabled)
	if mock.calls != 1 {
		t.Errorf("provider calls: want 1, got %d", mock.calls)
	}
}

func TestPersonalityGuardDetectsDrift(t *testing.T) {
	reg := NewRegistry()
	callCount := 0
	mock := &pipelineMockProvider{
		name: "mock",
		sendFunc: func(_ context.Context, _ *LLMRequest) (*LLMResponse, error) {
			callCount++
			if callCount == 1 {
				// First call: high-drift response (many exclamations for an introvert)
				return &LLMResponse{Content: "!!!!!!! SUPER AUFGEREGT !!!! WOW !!!!", Model: "m", TokensUsed: 10, FinishReason: "stop"}, nil
			}
			// Re-gen call: calmer response
			return &LLMResponse{Content: "Ich arbeite ruhig an meinem Schreibtisch.", Model: "m", TokensUsed: 10, FinishReason: "stop"}, nil
		},
	}
	reg.Register("mock", mock)

	drift := judge.NewDriftDetector()
	drift.RegisterProfile("AGENT-01", judge.PersonalityProfile{
		Role:         "Developer",
		Extraversion: 0.1, // introvert
		Neuroticism:  0.3,
	})
	quality := judge.NewQualityScorer(drift)

	cfg := control.NewConfig("mock")
	_ = cfg.Update(map[string]interface{}{"personality_guard_enabled": true, "drift_threshold": 0.5})

	ph := newTestPipelineHandlerWithDrift(reg, cfg, drift, quality)

	body := `{"messages":[{"role":"user","content":"Was machst du?"}],"metadata":{"agent_id":"1","agent_name":"AGENT-01"}}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("want 200, got %d: %s", w.Code, w.Body.String())
	}

	// Should have called provider twice (original + re-gen)
	if callCount < 2 {
		t.Errorf("expected at least 2 provider calls (original + re-gen), got %d", callCount)
	}

	var resp PipelineResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("decode: %v", err)
	}
	// Response should be the re-generated calmer version
	if !strings.Contains(resp.Content, "ruhig") {
		t.Errorf("expected re-generated content with 'ruhig', got %q", resp.Content)
	}
}

func TestQualityGateDisabledByDefault(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		resp: &LLMResponse{Content: "ok", Model: "m", TokensUsed: 1, FinishReason: "stop"},
	}
	reg.Register("mock", mock)

	drift := judge.NewDriftDetector()
	quality := judge.NewQualityScorer(drift)

	ph := newTestPipelineHandlerWithDrift(reg, nil, drift, quality)

	body := `{"messages":[{"role":"user","content":"test"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("want 200, got %d", w.Code)
	}
	// "ok" is very short (score=1) but gate is disabled, so no re-gen
	if mock.calls != 1 {
		t.Errorf("provider calls: want 1, got %d", mock.calls)
	}
}

func TestQualityGateTriggersRegen(t *testing.T) {
	reg := NewRegistry()
	callCount := 0
	mock := &pipelineMockProvider{
		name: "mock",
		sendFunc: func(_ context.Context, _ *LLMRequest) (*LLMResponse, error) {
			callCount++
			if callCount == 1 {
				return &LLMResponse{Content: "ja", Model: "m", TokensUsed: 1, FinishReason: "stop"}, nil
			}
			return &LLMResponse{Content: "Ja, ich werde das Projekt-Meeting um 14:00 Uhr vorbereiten und die Praesentation fuer den Kunden fertigstellen.", Model: "m", TokensUsed: 20, FinishReason: "stop"}, nil
		},
	}
	reg.Register("mock", mock)

	drift := judge.NewDriftDetector()
	quality := judge.NewQualityScorer(drift)

	cfg := control.NewConfig("mock")
	_ = cfg.Update(map[string]interface{}{"quality_gate_enabled": true, "quality_threshold": float64(2)})

	ph := newTestPipelineHandlerWithDrift(reg, cfg, drift, quality)

	body := `{"messages":[{"role":"user","content":"Was hast du vor?"}],"metadata":{"agent_id":"1","agent_name":"AGENT-01"}}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("want 200, got %d: %s", w.Code, w.Body.String())
	}

	// Should have re-generated (original "ja" scores very low)
	if callCount < 2 {
		t.Errorf("expected at least 2 provider calls (original + re-gen), got %d", callCount)
	}
}

func TestQualityGateMaxRegen(t *testing.T) {
	reg := NewRegistry()
	callCount := 0
	mock := &pipelineMockProvider{
		name: "mock",
		sendFunc: func(_ context.Context, _ *LLMRequest) (*LLMResponse, error) {
			callCount++
			// Always return a short response (low quality)
			return &LLMResponse{Content: "ja", Model: "m", TokensUsed: 1, FinishReason: "stop"}, nil
		},
	}
	reg.Register("mock", mock)

	drift := judge.NewDriftDetector()
	quality := judge.NewQualityScorer(drift)

	cfg := control.NewConfig("mock")
	_ = cfg.Update(map[string]interface{}{
		"quality_gate_enabled": true,
		"quality_threshold":    float64(2),
		"quality_max_regen":    float64(1),
	})

	ph := newTestPipelineHandlerWithDrift(reg, cfg, drift, quality)

	body := `{"messages":[{"role":"user","content":"test"}],"metadata":{"agent_id":"1","agent_name":"AGENT-01"}}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("want 200, got %d", w.Code)
	}

	// 1 original + 1 re-gen = 2 calls max (max_regen=1)
	if callCount > 2 {
		t.Errorf("expected max 2 provider calls (1 original + 1 re-gen), got %d", callCount)
	}
}

func TestNarrativeNudgeInjection(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		resp: &LLMResponse{Content: "Alles klar.", Model: "m", TokensUsed: 5, FinishReason: "stop"},
	}
	reg.Register("mock", mock)

	cfg := control.NewConfig("mock")
	_ = cfg.Update(map[string]interface{}{"narrative_nudge": "Fokus heute: Teamwork"})

	ph := newTestPipelineHandlerWithDrift(reg, cfg, nil, nil)

	body := `{"messages":[{"role":"user","content":"Was machst du?"}],"metadata":{"agent_name":"Max Mueller","agent_role":"Developer","perception":"CIRCADIAN: 11:42"}}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("want 200, got %d: %s", w.Code, w.Body.String())
	}

	// Check that system prompt contains the nudge
	if mock.lastReq == nil {
		t.Fatal("provider received no request")
	}
	if len(mock.lastReq.Messages) == 0 {
		t.Fatal("no messages in request")
	}
	systemMsg := mock.lastReq.Messages[0]
	if systemMsg.Role != "system" {
		t.Fatalf("first message role: want 'system', got %q", systemMsg.Role)
	}
	if !strings.Contains(systemMsg.Content, "[NARRATIVE_NUDGE]") {
		t.Error("system prompt should contain [NARRATIVE_NUDGE] tag")
	}
	if !strings.Contains(systemMsg.Content, "Fokus heute: Teamwork") {
		t.Error("system prompt should contain nudge text")
	}
}

func TestNarrativeNudgeEmpty(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		resp: &LLMResponse{Content: "Alles klar.", Model: "m", TokensUsed: 5, FinishReason: "stop"},
	}
	reg.Register("mock", mock)

	// No nudge configured (default empty)
	ph := newTestPipelineHandlerWithDrift(reg, nil, nil, nil)

	body := `{"messages":[{"role":"user","content":"Was machst du?"}],"metadata":{"agent_name":"Max Mueller","agent_role":"Developer","perception":"CIRCADIAN: 11:42"}}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("want 200, got %d: %s", w.Code, w.Body.String())
	}

	if mock.lastReq == nil {
		t.Fatal("provider received no request")
	}
	for _, msg := range mock.lastReq.Messages {
		if msg.Role == "system" && strings.Contains(msg.Content, "[NARRATIVE_NUDGE]") {
			t.Error("system prompt should NOT contain NARRATIVE_NUDGE when nudge is empty")
		}
	}
}
