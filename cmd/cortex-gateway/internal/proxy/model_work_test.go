package proxy

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/synthesis"
)

func modelWorkMetadata() map[string]string {
	return map[string]string{
		"company_execution_schema": "1", "company_execution_context_digest": strings.Repeat("a", 64),
		"request_id": "company-provider-reservation-test", "reservation_id": "reservation-test",
		"tenant_id": "tenant-test", "project_id": "project-test", "work_item_id": "work-test",
		"assignment_id": "assignment-test", "assignment_version": "1", "reserved_provider": "mock",
		"agent_id": "5", "hierarchy_tier": "2", "personality_type": "I",
		"synth_fp": "H5|E5|B9|S3|C5|SN5|R:buero-dev-1|P:0|CH:0|HR:0|T:10|TMP:0|PE:I|IM:0",
	}
}

func TestModelWorkRequestRequiresAuthenticatedClassAndCompleteBinding(t *testing.T) {
	for _, mutation := range []func(*LLMRequest){
		func(r *LLMRequest) { r.RequestClass = RequestClassExternalCompat },
		func(r *LLMRequest) { r.Stream = true },
		func(r *LLMRequest) { r.Metadata["company_execution_schema"] = "2" },
		func(r *LLMRequest) { delete(r.Metadata, "project_id") },
		func(r *LLMRequest) { r.Metadata["request_id"] = "foreign" },
		func(r *LLMRequest) { r.Metadata["reservation_id"] = "foreign" },
		func(r *LLMRequest) { r.Metadata["company_execution_context_digest"] = strings.Repeat("A", 64) },
	} {
		req := LLMRequest{RequestClass: RequestClassAgentRuntime, MaxTokens: 1024, Metadata: modelWorkMetadata()}
		mutation(&req)
		if _, err := classifyModelWorkRequest(&req, "company-provider-reservation-test"); err == nil {
			t.Fatal("invalid model work request admitted")
		}
	}
}

func TestModelWorkForwardsOnceWithoutSynthesisRegenerationOrLegacyActions(t *testing.T) {
	for _, test := range []struct{ name, content, decision string }{
		{"typed", `{"schema_version":1,"tools":[{"kind":"write_file","path":"a.js","content":"console.log(1)","expected_sha256":null}]}`, "forward"},
		{"fourth_wall", "Ich bin eine KI", "dropped"},
		{"oversized", strings.Repeat("x", maxModelWorkResponseBytes+1), "dropped"},
	} {
		t.Run(test.name, func(t *testing.T) {
			reg := NewRegistry()
			provider := &pipelineMockProvider{name: "mock", resp: &LLMResponse{
				Content: test.content, Model: "mock-tier2", InputTokens: 10, OutputTokens: 20, TokensUsed: 30,
			}}
			reg.Register("mock", provider)
			cfg := control.NewConfig("mock")
			if err := cfg.Update(map[string]interface{}{"synthesis_enabled": true}); err != nil {
				t.Fatal(err)
			}
			ph := newTestPipelineHandler(reg, cfg)
			ph.synthesis = synthesis.NewEngine(true, nil)
			encoded, err := json.Marshal(map[string]any{
				"max_tokens": 128,
				"messages":   []map[string]string{{"role": "user", "content": "Build the assigned site"}},
				"metadata":   modelWorkMetadata(),
			})
			if err != nil {
				t.Fatal(err)
			}
			req := newAgentRuntimeTestRequest(t, string(encoded))
			req.Header.Set("X-Request-ID", "company-provider-reservation-test")
			w := httptest.NewRecorder()
			ph.ServeHTTP(w, req)
			if w.Code != http.StatusOK {
				t.Fatalf("status=%d body=%s", w.Code, w.Body.String())
			}
			var result PipelineResponse
			if err := json.Unmarshal(w.Body.Bytes(), &result); err != nil {
				t.Fatal(err)
			}
			if provider.calls != 1 || result.Decision != test.decision || len(result.Actions) != 0 {
				t.Fatalf("calls=%d decision=%s actions=%v", provider.calls, result.Decision, result.Actions)
			}
			if provider.lastReq.MaxTokens > 128 {
				t.Fatal("work request token ceiling was raised")
			}
			if result.InputTokens != 10 || result.OutputTokens != 20 {
				t.Fatal("usage lost")
			}
			if test.decision == "forward" && result.Content != test.content {
				t.Fatal("proposal was rewritten")
			}
			if test.decision == "dropped" && result.Content != "" {
				t.Fatal("rejected proposal leaked")
			}
		})
	}
}
