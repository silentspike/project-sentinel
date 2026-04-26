package proxy

import (
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/apicp"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/capability"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/compiler"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/extraction"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/forwardqueue"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/normalizer"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/sequencing"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/synthesis"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/ticksync"
)

func newBenchmarkPipelineHandler(providerName string, provider Provider, synthesisEnabled bool) *PipelineHandler {
	reg := NewRegistry()
	reg.Register(providerName, NewQueuedProvider(provider, forwardqueue.NewManager(3)))

	cfg := control.NewConfig(providerName)
	if err := cfg.Update(map[string]interface{}{
		"synthesis_enabled":       synthesisEnabled,
		"sequencing_enabled":      true,
		"tick_sync_enabled":       true,
		"apicp_enabled":           true,
		"tick_sync_timeout_ms":    2000,
		"p3_timeout_ms":           5000,
		"max_forward_concurrency": 3,
	}); err != nil {
		panic(err)
	}

	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	return NewPipelineHandler(PipelineConfig{
		Registry:     reg,
		Config:       cfg,
		Compiler:     compiler.New(),
		Normalizer:   normalizer.New(),
		Extractor:    extraction.New(),
		Capabilities: capability.New(),
		Logger:       logger,
		BreakerCfg:   testConfig(),
		Synthesis:    synthesis.NewEngine(true, logger),
		Sequencer:    sequencing.NewSequencer(5*time.Second, true, logger),
		Observer:     NewObserverForBench(logger),
		TickSync:     ticksync.NewBuffer(2*time.Second, true, logger),
		ResponseLogs: NewResponseLogBuffer(128),
	})
}

func NewObserverForBench(logger *slog.Logger) *apicp.Observer {
	return apicp.NewObserver(apicp.Config{}, logger)
}

func BenchmarkPipelineForwardPath(b *testing.B) {
	mock := &pipelineMockProvider{
		name: "claude-code",
		resp: &LLMResponse{
			Content:      "{\"action_type\":\"THINK\",\"target\":\"\",\"content\":\"ok\"}",
			Model:        "claude-opus-4-6",
			TokensUsed:   64,
			InputTokens:  32,
			OutputTokens: 32,
			FinishReason: "stop",
		},
	}

	ph := newBenchmarkPipelineHandler("claude-code", mock, true)
	defer ph.tickSync.Stop()
	defer ph.observer.Stop()

	body := `{"messages":[{"role":"user","content":"kurzer test"}],"metadata":{"agent_id":"20","agent_name":"AGENT-20","agent_role":"Developer","room_id":"buero-dev-1","is_directly_addressed":"true","synth_fp":"H3|E6|B2|S2|C4|SN3|R:buero-dev-1|P:3|CH:0|HR:0|T:14|TMP:0|PE:E|IM:0","body":"Hunger: 30%, Energy: 60%","environment":"Buero","presence":"Martin, Fatima"}}`

	b.ReportAllocs()
	b.ResetTimer()

	for i := 0; i < b.N; i++ {
		req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
		req.Header.Set("Content-Type", "application/json")
		w := httptest.NewRecorder()
		ph.ServeHTTP(w, req)
		if w.Code != http.StatusOK {
			b.Fatalf("expected %d, got %d: %s", http.StatusOK, w.Code, w.Body.String())
		}
	}
}

func BenchmarkPipelineSynthesisPath(b *testing.B) {
	mock := &pipelineMockProvider{
		name: "claude-code",
		resp: &LLMResponse{
			Content:      "{\"action_type\":\"THINK\",\"target\":\"\",\"content\":\"unused\"}",
			Model:        "claude-opus-4-6",
			TokensUsed:   64,
			InputTokens:  32,
			OutputTokens: 32,
			FinishReason: "stop",
		},
	}

	ph := newBenchmarkPipelineHandler("claude-code", mock, true)
	defer ph.tickSync.Stop()
	defer ph.observer.Stop()

	body := `{"messages":[{"role":"user","content":"was machst du"}],"metadata":{"agent_id":"5","agent_name":"AGENT-05","agent_role":"Developer","room_id":"buero-dev-1","personality_type":"I","synth_fp":"H9|E5|B3|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0"}}`

	b.ReportAllocs()
	b.ResetTimer()

	for i := 0; i < b.N; i++ {
		req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
		req.Header.Set("Content-Type", "application/json")
		w := httptest.NewRecorder()
		ph.ServeHTTP(w, req)
		if w.Code != http.StatusOK {
			b.Fatalf("expected %d, got %d: %s", http.StatusOK, w.Code, w.Body.String())
		}
	}
	if mock.calls != 0 {
		b.Fatalf("expected synthesis benchmark to avoid provider calls, got %d", mock.calls)
	}
}

func BenchmarkAnthropicDirectRequestAssembly(b *testing.B) {
	req := &LLMRequest{
		Model:       "claude-opus-4-6",
		MaxTokens:   4096,
		Temperature: 0.7,
		SystemBlocks: []SystemBlock{
			{
				Type: "text",
				Text: "<agent-identity>\nDu bist Thomas Mueller, CEO der PixelPerfekt GmbH.\nDu bist ein ECHTER Mensch.\n</agent-identity>",
				CacheControl: &CacheControl{
					Type: "ephemeral",
				},
			},
			{
				Type: "text",
				Text: "<company-context>\nPixelPerfekt GmbH, Webdesign-Agentur, Nuernberg.\n</company-context>",
				CacheControl: &CacheControl{
					Type: "ephemeral",
				},
			},
			{
				Type: "text",
				Text: "<inner-voice>\nDu musst JETZT in die Kueche gehen.\n</inner-voice>",
			},
			{
				Type: "text",
				Text: "<action-format>\nAntworte mit JSON.\n</action-format>",
			},
		},
		Messages: []Message{
			{Role: "user", Content: "Wie fuehlst du dich gerade?"},
			{Role: "assistant", Content: "{\"action_type\":\"Think\",\"target\":\"\",\"content\":\"Ich habe Hunger.\"}"},
			{Role: "user", Content: "Und was machst du jetzt?"},
		},
	}

	b.ReportAllocs()
	b.ResetTimer()

	for i := 0; i < b.N; i++ {
		systemBlocks, messages := splitAnthropicMessages(req)
		payload, err := json.Marshal(claudeRequest{
			Model:       req.Model,
			MaxTokens:   req.MaxTokens,
			System:      systemBlocks,
			Messages:    messages,
			Temperature: req.Temperature,
		})
		if err != nil {
			b.Fatal(err)
		}
		if len(payload) == 0 {
			b.Fatal("expected non-empty anthropic payload")
		}
	}
}

func BenchmarkClassifyRequestAgentRuntime(b *testing.B) {
	req := &LLMRequest{Metadata: map[string]string{
		"agent_id":   "12",
		"agent_name": "Thomas Mueller",
		"room_id":    "buero-ceo",
	}}

	b.ReportAllocs()
	b.ResetTimer()

	for i := 0; i < b.N; i++ {
		if got := ClassifyRequest("/internal/llm", req); got != RequestClassAgentRuntime {
			b.Fatalf("ClassifyRequest() = %q, want %q", got, RequestClassAgentRuntime)
		}
	}
}

func BenchmarkResolveModelPolicyAgentRuntime(b *testing.B) {
	b.ReportAllocs()
	b.ResetTimer()

	for i := 0; i < b.N; i++ {
		got, err := ResolveModelPolicy("claude-code", RequestClassAgentRuntime, "", AgentRuntimeModelPolicyHaiku)
		if err != nil {
			b.Fatalf("ResolveModelPolicy() error: %v", err)
		}
		if got.Model != "haiku" || got.Source != PolicySourceAgentRuntime {
			b.Fatalf("ResolveModelPolicy() = %+v, want haiku/%s", got, PolicySourceAgentRuntime)
		}
	}
}

func BenchmarkResponseLogBufferAdd(b *testing.B) {
	buffer := NewResponseLogBuffer(128)
	entry := ResponseLogEntry{
		RequestID:    "bench-request",
		RequestClass: RequestClassAgentRuntime,
		Provider:     "claude-code",
		Model:        "haiku",
		PolicySource: PolicySourceAgentRuntime,
		AgentID:      "12",
		AgentName:    "Thomas Mueller",
		Content:      "ok",
	}

	b.ReportAllocs()
	b.ResetTimer()

	for i := 0; i < b.N; i++ {
		buffer.Add(entry)
	}
}
