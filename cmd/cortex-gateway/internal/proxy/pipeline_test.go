package proxy

import (
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/apicp"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/capability"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/compiler"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/extraction"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/forwardqueue"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/intercept"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/normalizer"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/sequencing"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/synthesis"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/ticksync"
	"github.com/obtFusi/project-sentinel/pkg/sentinel-go/judge"
)

// pipelineMockProvider implementiert Provider fuer Pipeline-Tests.
// Kann Requests aufzeichnen und konfigurierbare Responses/Errors liefern.
type pipelineMockProvider struct {
	name     string
	resp     *LLMResponse
	err      error
	statusErr error
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
func (p *pipelineMockProvider) CurrentProviderError() error        { return p.statusErr }

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

func TestPipelineStructuredSystemBlocksForAnthropicDirect(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "anthropic-direct",
		resp: &LLMResponse{
			Content:      "{\"action_type\":\"THINK\",\"target\":\"\",\"content\":\"ok\"}",
			Model:        "test-model",
			TokensUsed:   5,
			FinishReason: "end_turn",
		},
	}
	reg.Register("anthropic-direct", mock)

	ph := newTestPipelineHandler(reg, nil)

	body := `{"messages":[{"role":"user","content":"Was machst du?"}],"metadata":{"agent_name":"Thomas Mueller","agent_role":"CEO","body":"Hunger: 45%, Energy: 62%","environment":"Buero der Geschaeftsfuehrung (OG)","impulse":"Du musst jetzt in die Kueche gehen.","room_id":"buero-ceo"}}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()

	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected status %d, got %d: %s", http.StatusOK, w.Code, w.Body.String())
	}
	if mock.lastReq == nil {
		t.Fatal("expected provider to have received a request")
	}
	if len(mock.lastReq.SystemBlocks) != 8 {
		t.Fatalf("expected 8 structured system blocks, got %d", len(mock.lastReq.SystemBlocks))
	}
	if len(mock.lastReq.Messages) == 0 || mock.lastReq.Messages[0].Role != "user" {
		t.Fatalf("expected user messages to remain intact, got %+v", mock.lastReq.Messages)
	}
	for _, msg := range mock.lastReq.Messages {
		if msg.Role == "system" {
			t.Fatalf("expected structured provider path to avoid prepended system messages, got %+v", mock.lastReq.Messages)
		}
	}
	if !strings.Contains(mock.lastReq.SystemBlocks[0].Text, "<agent-identity>") {
		t.Errorf("expected first system block to contain tagged identity block, got %q", mock.lastReq.SystemBlocks[0].Text)
	}
}

func TestPipelineSynthesisUsesRuleActions(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		resp: &LLMResponse{
			Content:      "{\"action_type\":\"THINK\",\"target\":\"\",\"content\":\"should not run\"}",
			Model:        "test-model",
			TokensUsed:   5,
			FinishReason: "end_turn",
		},
	}
	reg.Register("mock", mock)

	cfg := control.NewConfig("mock")
	if err := cfg.Update(map[string]interface{}{"synthesis_enabled": true}); err != nil {
		t.Fatalf("update config: %v", err)
	}

	ph := NewPipelineHandler(PipelineConfig{
		Registry:     reg,
		Config:       cfg,
		Compiler:     compiler.New(),
		Normalizer:   normalizer.New(),
		Extractor:    extraction.New(),
		Capabilities: capability.New(),
		Logger:       slog.Default(),
		BreakerCfg:   testConfig(),
		Synthesis:    synthesis.NewEngine(true, nil),
	})

	body := `{"messages":[{"role":"user","content":"Was machst du?"}],"metadata":{"agent_id":"5","personality_type":"I","synth_fp":"H5|E5|B9|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0"}}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()

	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected status %d, got %d: %s", http.StatusOK, w.Code, w.Body.String())
	}
	if mock.calls != 0 {
		t.Fatalf("expected provider send to be skipped on synthesis, got %d calls", mock.calls)
	}

	var resp PipelineResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}
	if resp.Provider != "synthesis" {
		t.Fatalf("provider = %q, want synthesis", resp.Provider)
	}
	if len(resp.Actions) == 0 {
		t.Fatal("expected synthesis actions")
	}

	foundMove := false
	for _, action := range resp.Actions {
		if action.Type == "move" {
			foundMove = true
			if action.Target != "toilette-eg-herren" {
				t.Fatalf("move target = %q, want toilette-eg-herren", action.Target)
			}
		}
	}
	if !foundMove {
		t.Fatal("expected move action from synthesis rule")
	}
}

func TestPipelineAPICPLearnedPatternSynthesizes(t *testing.T) {
	fp := "H5|E5|B5|S5|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0"
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		resp: &LLMResponse{
			Content:      "{\"action_type\":\"THINK\",\"target\":\"\",\"content\":\"provider\"}",
			Model:        "test-model",
			TokensUsed:   5,
			FinishReason: "end_turn",
		},
	}
	reg.Register("mock", mock)

	cfg := control.NewConfig("mock")
	if err := cfg.Update(map[string]interface{}{
		"synthesis_enabled": false,
		"apicp_enabled":     true,
	}); err != nil {
		t.Fatalf("update config: %v", err)
	}

	observer := apicp.NewObserver(apicp.Config{}, nil)
	defer observer.Stop()
	for i := 0; i < 50; i++ {
		observer.Record(fp, "1", "{\"action_type\":\"THINK\",\"target\":\"\",\"content\":\"learned\"}", false)
	}

	ph := NewPipelineHandler(PipelineConfig{
		Registry:     reg,
		Config:       cfg,
		Compiler:     compiler.New(),
		Normalizer:   normalizer.New(),
		Extractor:    extraction.New(),
		Capabilities: capability.New(),
		Logger:       slog.Default(),
		BreakerCfg:   testConfig(),
		Observer:     observer,
	})

	body := `{"messages":[{"role":"user","content":"Bitte antworte."}],"metadata":{"agent_id":"1","synth_fp":"` + fp + `"}}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()

	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected status %d, got %d: %s", http.StatusOK, w.Code, w.Body.String())
	}
	if mock.calls != 0 {
		t.Fatalf("expected provider send to be skipped on APICP synth, got %d calls", mock.calls)
	}

	var resp PipelineResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("decode response: %v", err)
	}
	if resp.Provider != "apicp" {
		t.Fatalf("provider = %q, want apicp", resp.Provider)
	}
	if !strings.Contains(resp.Content, "learned") {
		t.Fatalf("content = %q, want learned content", resp.Content)
	}
}

func TestPipelineAPICPProbeForwardsAndDegrades(t *testing.T) {
	fp := "H5|E5|B5|S5|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0"
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		resp: &LLMResponse{
			Content:      "{\"action_type\":\"THINK\",\"target\":\"\",\"content\":\"different\"}",
			Model:        "test-model",
			TokensUsed:   5,
			FinishReason: "end_turn",
		},
	}
	reg.Register("mock", mock)

	cfg := control.NewConfig("mock")
	if err := cfg.Update(map[string]interface{}{
		"synthesis_enabled": false,
		"apicp_enabled":     true,
	}); err != nil {
		t.Fatalf("update config: %v", err)
	}

	observer := apicp.NewObserver(apicp.Config{}, nil)
	defer observer.Stop()
	for i := 0; i < 50; i++ {
		observer.Record(fp, "1", "{\"action_type\":\"THINK\",\"target\":\"\",\"content\":\"learned\"}", false)
	}
	for i := 0; i < 99; i++ {
		observer.MarkSynthesisCandidate()
	}

	ph := NewPipelineHandler(PipelineConfig{
		Registry:     reg,
		Config:       cfg,
		Compiler:     compiler.New(),
		Normalizer:   normalizer.New(),
		Extractor:    extraction.New(),
		Capabilities: capability.New(),
		Logger:       slog.Default(),
		BreakerCfg:   testConfig(),
		Observer:     observer,
	})

	body := `{"messages":[{"role":"user","content":"Bitte antworte."}],"metadata":{"agent_id":"1","synth_fp":"` + fp + `"}}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()

	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected status %d, got %d: %s", http.StatusOK, w.Code, w.Body.String())
	}
	if mock.calls != 1 {
		t.Fatalf("expected probe to forward real provider call, got %d calls", mock.calls)
	}

	if learned, ok := observer.LearnedPatternFor("1", fp); ok && learned.Confidence >= 1.0 {
		t.Fatalf("expected probe mismatch to degrade confidence, got %f", learned.Confidence)
	}
}

func TestPipelineManualInterceptModify(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		resp: &LLMResponse{
			Content:      "{\"action_type\":\"THINK\",\"target\":\"\",\"content\":\"ok\"}",
			Model:        "test-model",
			TokensUsed:   5,
			FinishReason: "end_turn",
		},
		sendFunc: func(ctx context.Context, req *LLMRequest) (*LLMResponse, error) {
			if got := req.Messages[len(req.Messages)-1].Content; !strings.Contains(got, "[KONTEXT] manuell freigegeben [/KONTEXT]") {
				t.Fatalf("expected manual context injection, got %q", got)
			}
			return &LLMResponse{
				Content:      "{\"action_type\":\"THINK\",\"target\":\"\",\"content\":\"ok\"}",
				Model:        "test-model",
				TokensUsed:   5,
				FinishReason: "end_turn",
			}, nil
		},
	}
	reg.Register("mock", mock)

	cfg := control.NewConfig("mock")
	if err := cfg.Update(map[string]interface{}{
		"intercept_mode": "manual",
		"p3_timeout_ms":  1000,
	}); err != nil {
		t.Fatalf("update config: %v", err)
	}

	interceptor := intercept.NewManager()
	ph := NewPipelineHandler(PipelineConfig{
		Registry:     reg,
		Config:       cfg,
		Compiler:     compiler.New(),
		Normalizer:   normalizer.New(),
		Extractor:    extraction.New(),
		Capabilities: capability.New(),
		Logger:       slog.Default(),
		BreakerCfg:   testConfig(),
		Interceptor:  interceptor,
	})

	go func() {
		for {
			pending := interceptor.Pending()
			if len(pending) > 0 {
				_ = interceptor.ResolveRequest(pending[0].ID, intercept.Modify("manual", "\n[KONTEXT] manuell freigegeben [/KONTEXT]"))
				return
			}
			time.Sleep(10 * time.Millisecond)
		}
	}()

	body := `{"messages":[{"role":"user","content":"Bitte antworte."}],"metadata":{"agent_id":"5"}}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()

	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected status %d, got %d: %s", http.StatusOK, w.Code, w.Body.String())
	}
	if mock.calls != 1 {
		t.Fatalf("expected provider send after manual approval, got %d calls", mock.calls)
	}
}

func TestPipelineSequencingInjectsP1Context(t *testing.T) {
	reg := NewRegistry()
	p3Content := make(chan string, 1)
	mock := &pipelineMockProvider{
		name: "mock",
		sendFunc: func(ctx context.Context, req *LLMRequest) (*LLMResponse, error) {
			if req.Metadata["is_directly_addressed"] == "true" {
				time.Sleep(80 * time.Millisecond)
				return &LLMResponse{
					Content:      "{\"action_type\":\"Chat\",\"target\":\"\",\"content\":\"Ich kuemmere mich darum.\"}",
					Model:        "test-model",
					TokensUsed:   5,
					FinishReason: "end_turn",
				}, nil
			}
			p3Content <- req.Messages[len(req.Messages)-1].Content
			return &LLMResponse{
				Content:      "{\"action_type\":\"Think\",\"target\":\"\",\"content\":\"ok\"}",
				Model:        "test-model",
				TokensUsed:   5,
				FinishReason: "end_turn",
			}, nil
		},
	}
	reg.Register("mock", mock)

	cfg := control.NewConfig("mock")
	if err := cfg.Update(map[string]interface{}{
		"sequencing_enabled": true,
		"p3_timeout_ms":      500,
	}); err != nil {
		t.Fatalf("update config: %v", err)
	}

	ph := NewPipelineHandler(PipelineConfig{
		Registry:     reg,
		Config:       cfg,
		Compiler:     compiler.New(),
		Normalizer:   normalizer.New(),
		Extractor:    extraction.New(),
		Capabilities: capability.New(),
		Logger:       slog.Default(),
		BreakerCfg:   testConfig(),
		Sequencer:    sequencing.NewSequencer(500*time.Millisecond, true, nil),
	})

	p1Done := make(chan *httptest.ResponseRecorder, 1)
	go func() {
		body := `{"messages":[{"role":"user","content":"Thomas, bitte uebernimm das."}],"metadata":{"agent_id":"1","room_id":"room-1","heard":"Thomas, bitte uebernimm das.","is_directly_addressed":"true"}}`
		req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
		req.Header.Set("Content-Type", "application/json")
		w := httptest.NewRecorder()
		ph.ServeHTTP(w, req)
		p1Done <- w
	}()

	time.Sleep(20 * time.Millisecond)

	p3Body := `{"messages":[{"role":"user","content":"Was ist hier los?"}],"metadata":{"agent_id":"2","room_id":"room-1","heard":"Thomas, bitte uebernimm das.","is_directly_addressed":"false"}}`
	p3Req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(p3Body))
	p3Req.Header.Set("Content-Type", "application/json")
	p3W := httptest.NewRecorder()
	ph.ServeHTTP(p3W, p3Req)

	if p3W.Code != http.StatusOK {
		t.Fatalf("expected P3 status %d, got %d: %s", http.StatusOK, p3W.Code, p3W.Body.String())
	}

	select {
	case content := <-p3Content:
		if !strings.Contains(content, "[KONTEXT] AGENT-01 hat gerade gesagt") {
			t.Fatalf("expected injected context in P3 request, got %q", content)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for P3 provider call")
	}

	select {
	case w := <-p1Done:
		if w.Code != http.StatusOK {
			t.Fatalf("expected P1 status %d, got %d: %s", http.StatusOK, w.Code, w.Body.String())
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for P1 completion")
	}
}

func TestPipelineSequencingTimeoutForwardsWithoutContext(t *testing.T) {
	reg := NewRegistry()
	p3Content := make(chan string, 1)
	mock := &pipelineMockProvider{
		name: "mock",
		sendFunc: func(ctx context.Context, req *LLMRequest) (*LLMResponse, error) {
			if req.Metadata["is_directly_addressed"] == "true" {
				time.Sleep(120 * time.Millisecond)
				return &LLMResponse{
					Content:      "{\"action_type\":\"Chat\",\"target\":\"\",\"content\":\"Zu spaet\"}",
					Model:        "test-model",
					TokensUsed:   5,
					FinishReason: "end_turn",
				}, nil
			}
			p3Content <- req.Messages[len(req.Messages)-1].Content
			return &LLMResponse{
				Content:      "{\"action_type\":\"Think\",\"target\":\"\",\"content\":\"ok\"}",
				Model:        "test-model",
				TokensUsed:   5,
				FinishReason: "end_turn",
			}, nil
		},
	}
	reg.Register("mock", mock)

	cfg := control.NewConfig("mock")
	if err := cfg.Update(map[string]interface{}{
		"sequencing_enabled": true,
		"p3_timeout_ms":      50,
	}); err != nil {
		t.Fatalf("update config: %v", err)
	}

	ph := NewPipelineHandler(PipelineConfig{
		Registry:     reg,
		Config:       cfg,
		Compiler:     compiler.New(),
		Normalizer:   normalizer.New(),
		Extractor:    extraction.New(),
		Capabilities: capability.New(),
		Logger:       slog.Default(),
		BreakerCfg:   testConfig(),
		Sequencer:    sequencing.NewSequencer(50*time.Millisecond, true, nil),
	})

	p1Done := make(chan *httptest.ResponseRecorder, 1)
	go func() {
		body := `{"messages":[{"role":"user","content":"Thomas, bitte uebernimm das."}],"metadata":{"agent_id":"1","room_id":"room-1","heard":"Thomas, bitte uebernimm das.","is_directly_addressed":"true"}}`
		req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
		req.Header.Set("Content-Type", "application/json")
		w := httptest.NewRecorder()
		ph.ServeHTTP(w, req)
		p1Done <- w
	}()

	time.Sleep(20 * time.Millisecond)

	p3Body := `{"messages":[{"role":"user","content":"Was ist hier los?"}],"metadata":{"agent_id":"2","room_id":"room-1","heard":"Thomas, bitte uebernimm das.","is_directly_addressed":"false"}}`
	p3Req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(p3Body))
	p3Req.Header.Set("Content-Type", "application/json")
	p3W := httptest.NewRecorder()
	ph.ServeHTTP(p3W, p3Req)

	if p3W.Code != http.StatusOK {
		t.Fatalf("expected P3 status %d, got %d: %s", http.StatusOK, p3W.Code, p3W.Body.String())
	}

	select {
	case content := <-p3Content:
		if strings.Contains(content, "[KONTEXT]") {
			t.Fatalf("expected timeout release without injected context, got %q", content)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for P3 provider call")
	}

	select {
	case <-p1Done:
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for P1 completion")
	}
}

func TestPipelineManualResponseReplace(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		resp: &LLMResponse{
			Content:      "{\"action_type\":\"Think\",\"target\":\"\",\"content\":\"original\"}",
			Model:        "test-model",
			TokensUsed:   5,
			FinishReason: "end_turn",
		},
	}
	reg.Register("mock", mock)

	cfg := control.NewConfig("mock")
	if err := cfg.Update(map[string]interface{}{
		"intercept_mode": "manual",
		"p3_timeout_ms":  1000,
	}); err != nil {
		t.Fatalf("update config: %v", err)
	}

	requestInterceptor := intercept.NewManager()
	responseInterceptor := intercept.NewResponseManager()
	ph := NewPipelineHandler(PipelineConfig{
		Registry:            reg,
		Config:              cfg,
		Compiler:            compiler.New(),
		Normalizer:          normalizer.New(),
		Extractor:           extraction.New(),
		Capabilities:        capability.New(),
		Logger:              slog.Default(),
		BreakerCfg:          testConfig(),
		Interceptor:         requestInterceptor,
		ResponseInterceptor: responseInterceptor,
	})

	go func() {
		for {
			if pending := requestInterceptor.Pending(); len(pending) > 0 {
				_ = requestInterceptor.ResolveRequest(pending[0].ID, intercept.Forward("manual forward"))
			}
			if pending := responseInterceptor.Pending(); len(pending) > 0 {
				_ = responseInterceptor.Resolve(pending[0].ID, intercept.ResponseDecision{
					Action:  intercept.ResponseReplace,
					Reason:  "manual replace",
					Content: "{\"action_type\":\"Chat\",\"target\":\"\",\"content\":\"manuell ersetzt\"}",
				})
				return
			}
			time.Sleep(10 * time.Millisecond)
		}
	}()

	body := `{"messages":[{"role":"user","content":"Bitte antworte."}],"metadata":{"agent_id":"5"}}`
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
	if !strings.Contains(resp.Content, "manuell ersetzt") {
		t.Fatalf("expected replaced response content, got %q", resp.Content)
	}
}

func TestPipelineSynthesisAndForwardShareOutboundResponsePath(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		resp: &LLMResponse{
			Content:      "{\"action_type\":\"Think\",\"target\":\"\",\"content\":\"original forward\"}",
			Model:        "test-model",
			TokensUsed:   5,
			FinishReason: "end_turn",
		},
	}
	reg.Register("mock", mock)

	cfg := control.NewConfig("mock")
	if err := cfg.Update(map[string]interface{}{
		"synthesis_enabled": true,
		"intercept_mode":    "manual",
		"p3_timeout_ms":     1000,
	}); err != nil {
		t.Fatalf("update config: %v", err)
	}

	responseInterceptor := intercept.NewResponseManager()
	responseLogs := NewResponseLogBuffer(10)
	ph := NewPipelineHandler(PipelineConfig{
		Registry:            reg,
		Config:              cfg,
		Compiler:            compiler.New(),
		Normalizer:          normalizer.New(),
		Extractor:           extraction.New(),
		Capabilities:        capability.New(),
		Logger:              slog.Default(),
		BreakerCfg:          testConfig(),
		Synthesis:           synthesis.NewEngine(true, nil),
		ResponseInterceptor: responseInterceptor,
		ResponseLogs:        responseLogs,
	})

	resolveNext := func(content string) {
		t.Helper()
		go func() {
			for {
				pending := responseInterceptor.Pending()
				if len(pending) > 0 {
					_ = responseInterceptor.Resolve(pending[0].ID, intercept.ResponseDecision{
						Action:  intercept.ResponseReplace,
						Reason:  "test replace",
						Content: content,
					})
					return
				}
				time.Sleep(10 * time.Millisecond)
			}
		}()
	}

	resolveNext("{\"action_type\":\"Chat\",\"target\":\"\",\"content\":\"synthetisch ersetzt\"}")
	synthBody := `{"messages":[{"role":"user","content":"Bitte handle."}],"metadata":{"agent_id":"5","synth_fp":"H5|E5|B9|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0"}}`
	synthReq := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(synthBody))
	synthReq.Header.Set("Content-Type", "application/json")
	synthW := httptest.NewRecorder()
	ph.ServeHTTP(synthW, synthReq)

	if synthW.Code != http.StatusOK {
		t.Fatalf("expected synthesis status %d, got %d: %s", http.StatusOK, synthW.Code, synthW.Body.String())
	}

	var synthResp PipelineResponse
	if err := json.NewDecoder(synthW.Body).Decode(&synthResp); err != nil {
		t.Fatalf("decode synthesis response: %v", err)
	}
	if synthResp.Provider != "synthesis" {
		t.Fatalf("synthesis provider = %q, want synthesis", synthResp.Provider)
	}
	if !strings.Contains(synthResp.Content, "synthetisch ersetzt") {
		t.Fatalf("expected synthesis content to be replaced via outbound path, got %q", synthResp.Content)
	}

	resolveNext("{\"action_type\":\"Chat\",\"target\":\"\",\"content\":\"forward ersetzt\"}")
	forwardBody := `{"messages":[{"role":"user","content":"Bitte antworte."}],"metadata":{"agent_id":"7","room_id":"room-1"}}`
	forwardReq := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(forwardBody))
	forwardReq.Header.Set("Content-Type", "application/json")
	forwardW := httptest.NewRecorder()
	ph.ServeHTTP(forwardW, forwardReq)

	if forwardW.Code != http.StatusOK {
		t.Fatalf("expected forward status %d, got %d: %s", http.StatusOK, forwardW.Code, forwardW.Body.String())
	}

	var forwardResp PipelineResponse
	if err := json.NewDecoder(forwardW.Body).Decode(&forwardResp); err != nil {
		t.Fatalf("decode forward response: %v", err)
	}
	if forwardResp.Provider != "mock" {
		t.Fatalf("forward provider = %q, want mock", forwardResp.Provider)
	}
	if !strings.Contains(forwardResp.Content, "forward ersetzt") {
		t.Fatalf("expected forward content to be replaced via outbound path, got %q", forwardResp.Content)
	}

	entries := responseLogs.Entries()
	if len(entries) != 2 {
		t.Fatalf("expected 2 response log entries, got %d", len(entries))
	}
	if entries[0].Provider != "synthesis" {
		t.Fatalf("first logged provider = %q, want synthesis", entries[0].Provider)
	}
	if entries[1].Provider != "mock" {
		t.Fatalf("second logged provider = %q, want mock", entries[1].Provider)
	}
}

func TestSynthesisFourthWallCheckFallsBackToForward(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		sendFunc: func(_ context.Context, req *LLMRequest) (*LLMResponse, error) {
			if len(req.Messages) == 0 {
				t.Fatal("expected judge request messages")
			}
			if !strings.Contains(req.Messages[0].Content, "Analysiere folgende Aussage") {
				t.Fatalf("expected judge prompt, got %q", req.Messages[0].Content)
			}
			return &LLMResponse{
				Content:      `{"fourth_wall_break": true, "confidence": 0.99, "reason": "self-disclosure"}`,
				Model:        "judge-model",
				TokensUsed:   5,
				FinishReason: "end_turn",
			}, nil
		},
	}
	reg.Register("mock", mock)

	ph := newTestPipelineHandler(reg, nil)

	shouldForward := ph.shouldForwardAfterSynthesisFourthWallCheck(
		context.Background(),
		`Ich bin eine KI und kann das nicht real ausfuehren.`,
		"AGENT-12",
		"Developer",
		mock,
		"test_rule",
	)
	if !shouldForward {
		t.Fatal("expected synthesis fourth-wall check to force provider fallback")
	}
	if mock.calls != 1 {
		t.Fatalf("expected exactly 1 judge call, got %d", mock.calls)
	}
}

func TestSynthesisFourthWallCheckAllowsCleanContent(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{name: "mock"}
	reg.Register("mock", mock)

	ph := newTestPipelineHandler(reg, nil)

	shouldForward := ph.shouldForwardAfterSynthesisFourthWallCheck(
		context.Background(),
		`AKTION: Move
ZIEL: kueche
INHALT: Ich habe Hunger und gehe in die Kueche.`,
		"AGENT-12",
		"Developer",
		mock,
		"bio_hunger",
	)
	if shouldForward {
		t.Fatal("expected clean synthesis content to remain synthetic")
	}
	if mock.calls != 0 {
		t.Fatalf("expected no judge call on clean content, got %d", mock.calls)
	}
}

func TestPipelineQueueTickSyncAndSequencingIntegrate(t *testing.T) {
	reg := NewRegistry()
	queue := forwardqueue.NewManager(1)
	var mu sync.Mutex
	callOrder := make([]string, 0, 2)
	p3Content := make(chan string, 1)

	baseProvider := &pipelineMockProvider{
		name: "mock",
		sendFunc: func(ctx context.Context, req *LLMRequest) (*LLMResponse, error) {
			label := "p3"
			if req.Metadata["is_directly_addressed"] == "true" {
				label = "p1"
			}
			mu.Lock()
			callOrder = append(callOrder, label)
			mu.Unlock()

			if label == "p1" {
				time.Sleep(60 * time.Millisecond)
				return &LLMResponse{
					Content:      "{\"action_type\":\"Chat\",\"target\":\"\",\"content\":\"P1 antwortet zuerst\"}",
					Model:        "test-model",
					TokensUsed:   5,
					FinishReason: "end_turn",
				}, nil
			}

			p3Content <- req.Messages[len(req.Messages)-1].Content
			return &LLMResponse{
				Content:      "{\"action_type\":\"Think\",\"target\":\"\",\"content\":\"P3 antwortet nach P1\"}",
				Model:        "test-model",
				TokensUsed:   5,
				FinishReason: "end_turn",
			}, nil
		},
	}
	reg.Register("mock", NewQueuedProvider(baseProvider, queue))

	cfg := control.NewConfig("mock")
	if err := cfg.Update(map[string]interface{}{
		"sequencing_enabled":   true,
		"p3_timeout_ms":        500,
		"tick_sync_enabled":    true,
		"tick_sync_timeout_ms": 50,
	}); err != nil {
		t.Fatalf("update config: %v", err)
	}

	tickBuffer := ticksync.NewBuffer(50*time.Millisecond, true, nil)
	defer tickBuffer.Stop()

	ph := NewPipelineHandler(PipelineConfig{
		Registry:     reg,
		Config:       cfg,
		Compiler:     compiler.New(),
		Normalizer:   normalizer.New(),
		Extractor:    extraction.New(),
		Capabilities: capability.New(),
		Logger:       slog.Default(),
		BreakerCfg:   testConfig(),
		Sequencer:    sequencing.NewSequencer(500*time.Millisecond, true, nil),
		TickSync:     tickBuffer,
		ResponseLogs: NewResponseLogBuffer(10),
	})

	p1Done := make(chan *httptest.ResponseRecorder, 1)
	go func() {
		body := `{"messages":[{"role":"user","content":"Thomas, bitte uebernimm das."}],"metadata":{"agent_id":"1","room_id":"room-1","heard":"Thomas, bitte uebernimm das.","is_directly_addressed":"true","tick":"100"}}`
		req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
		req.Header.Set("Content-Type", "application/json")
		w := httptest.NewRecorder()
		ph.ServeHTTP(w, req)
		p1Done <- w
	}()

	time.Sleep(20 * time.Millisecond)

	p3Done := make(chan *httptest.ResponseRecorder, 1)
	go func() {
		body := `{"messages":[{"role":"user","content":"Was ist hier los?"}],"metadata":{"agent_id":"2","room_id":"room-1","heard":"Thomas, bitte uebernimm das.","is_directly_addressed":"false","tick":"100"}}`
		req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
		req.Header.Set("Content-Type", "application/json")
		w := httptest.NewRecorder()
		ph.ServeHTTP(w, req)
		p3Done <- w
	}()

	select {
	case content := <-p3Content:
		if !strings.Contains(content, "[KONTEXT] AGENT-01 hat gerade gesagt") {
			t.Fatalf("expected P3 request to contain injected P1 context, got %q", content)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for queued/sequenced P3 provider call")
	}

	var p1W, p3W *httptest.ResponseRecorder
	select {
	case p1W = <-p1Done:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for tick-synced P1 response")
	}
	select {
	case p3W = <-p3Done:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for tick-synced P3 response")
	}

	deadline := time.Now().Add(2 * time.Second)
	for {
		if tickBuffer.Stats().Pending == 0 && p1W.Body.Len() > 0 && p3W.Body.Len() > 0 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for tick-sync flush: pending=%d p1_body=%d p3_body=%d",
				tickBuffer.Stats().Pending, p1W.Body.Len(), p3W.Body.Len())
		}
		time.Sleep(10 * time.Millisecond)
	}

	if p1W.Code != http.StatusOK {
		t.Fatalf("expected P1 status %d, got %d: %s", http.StatusOK, p1W.Code, p1W.Body.String())
	}
	if p3W.Code != http.StatusOK {
		t.Fatalf("expected P3 status %d, got %d: %s", http.StatusOK, p3W.Code, p3W.Body.String())
	}

	mu.Lock()
	gotOrder := append([]string(nil), callOrder...)
	mu.Unlock()
	if len(gotOrder) != 2 {
		t.Fatalf("expected 2 provider calls, got %v", gotOrder)
	}
	if gotOrder[0] != "p1" || gotOrder[1] != "p3" {
		t.Fatalf("provider call order = %v, want [p1 p3]", gotOrder)
	}

	if stats := queue.Stats(); stats.Active != 0 || stats.Depth != 0 {
		t.Fatalf("expected empty forward queue after completion, got %+v", stats)
	}
	if pending := tickBuffer.Stats().Pending; pending != 0 {
		t.Fatalf("expected tick sync buffer to flush completely, pending=%d", pending)
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

func TestPipelineBreakerOpenDoesNotFailover(t *testing.T) {
	reg := NewRegistry()
	// Primary Provider: fehlerhaft
	failing := &pipelineMockProvider{
		name: "failing",
		err:  errors.New("transport error"),
	}
	reg.Register("failing", failing)

	// Sekundaerer Provider: funktioniert, darf aber NICHT als Failover verwendet werden
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

	// Naechster Request: Primary ist offen, aber es darf KEIN Failover stattfinden
	body := `{"messages":[{"role":"user","content":"test"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusServiceUnavailable {
		t.Fatalf("expected %d (breaker open without failover), got %d: %s", http.StatusServiceUnavailable, w.Code, w.Body.String())
	}
	if fallback.calls != 0 {
		t.Fatalf("expected fallback provider to remain unused, got %d calls", fallback.calls)
	}
	if !strings.Contains(w.Body.String(), "circuit breaker open") {
		t.Errorf("expected circuit breaker message, got %q", w.Body.String())
	}
}

func TestPipelineBreakerOpenUsesProviderStatusError(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name:      "mock",
		err:       errors.New("transport error"),
		statusErr: &ProviderError{StatusCode: http.StatusTooManyRequests, Message: "rate limited"},
	}
	reg.Register("mock", mock)

	cfg := control.NewConfig("mock")
	ph := newTestPipelineHandler(reg, cfg)

	for i := 0; i < 3; i++ {
		body := `{"messages":[{"role":"user","content":"fail"}]}`
		req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
		w := httptest.NewRecorder()
		ph.ServeHTTP(w, req)
	}

	body := `{"messages":[{"role":"user","content":"blocked"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusTooManyRequests {
		t.Fatalf("expected %d, got %d: %s", http.StatusTooManyRequests, w.Code, w.Body.String())
	}
	if !strings.Contains(w.Body.String(), "provider rate limited") {
		t.Fatalf("expected provider rate limited message, got %q", w.Body.String())
	}
	if mock.calls != 3 {
		t.Fatalf("expected breaker-open request to avoid an extra provider send, got %d sends", mock.calls)
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

func TestPipelineProviderRateLimited(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "mock",
		err:  &ProviderError{StatusCode: http.StatusTooManyRequests, Message: "rate limited"},
	}
	reg.Register("mock", mock)

	ph := newTestPipelineHandler(reg, nil)

	body := `{"messages":[{"role":"user","content":"hello"}]}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)

	if w.Code != http.StatusTooManyRequests {
		t.Fatalf("expected %d, got %d", http.StatusTooManyRequests, w.Code)
	}
	if !strings.Contains(w.Body.String(), "provider rate limited") {
		t.Fatalf("expected provider rate limited message, got %q", w.Body.String())
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
