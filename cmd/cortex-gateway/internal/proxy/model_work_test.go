package proxy

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

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

func salesRequestMetadata() map[string]string {
	metadata := modelWorkMetadata()
	for _, key := range []string{"project_id", "work_item_id", "assignment_id", "assignment_version"} {
		delete(metadata, key)
	}
	metadata["company_execution_schema"] = "2"
	metadata["company_execution_subject"] = "customer_request"
	metadata["customer_request_id"] = "request-test"
	metadata["customer_request_version"] = "1"
	return metadata
}

func adaptiveRequestMetadata() map[string]string {
	metadata := modelWorkMetadata()
	metadata["company_execution_schema"] = "3"
	metadata["adaptive_session_id"] = "01991c34-e03c-70c2-b97e-0591f4be2311"
	metadata["adaptive_effect_id"] = "01991c34-e03c-70c2-b97e-0591f4be2312"
	metadata["adaptive_session_version"] = "1"
	metadata["request_id"] = "company-adaptive-" + metadata["adaptive_session_id"] + "-" + metadata["adaptive_effect_id"]
	return metadata
}

func projectPlanningMetadata() map[string]string {
	metadata := modelWorkMetadata()
	for _, key := range []string{"work_item_id", "assignment_id", "assignment_version"} {
		delete(metadata, key)
	}
	metadata["company_execution_schema"] = "4"
	metadata["company_execution_subject"] = "project_planning"
	metadata["project_version"] = "1"
	metadata["request_id"] = "company-planning-" + metadata["reservation_id"] + "-" + metadata["project_id"]
	return metadata
}

func TestProjectPlanningRequiresCanonicalDisjointSubject(t *testing.T) {
	valid := projectPlanningMetadata()
	req := LLMRequest{RequestClass: RequestClassAgentRuntime, MaxTokens: 1024, Metadata: valid}
	if admitted, err := classifyModelWorkRequest(&req, valid["request_id"]); err != nil || !admitted {
		t.Fatalf("valid project planning request rejected: %v", err)
	}
	mutations := map[string]func(map[string]string){
		"wrong_subject":        func(m map[string]string) { m["company_execution_subject"] = "customer_request" },
		"missing_project":      func(m map[string]string) { delete(m, "project_id") },
		"invalid_project":      func(m map[string]string) { m["project_id"] = "../project" },
		"missing_version":      func(m map[string]string) { delete(m, "project_version") },
		"zero_version":         func(m map[string]string) { m["project_version"] = "0" },
		"noncanonical_version": func(m map[string]string) { m["project_version"] = "01" },
		"wrong_request":        func(m map[string]string) { m["request_id"] = "company-provider-" + m["reservation_id"] },
		"customer_request":     func(m map[string]string) { m["customer_request_id"] = "request-test" },
		"work_item":            func(m map[string]string) { m["work_item_id"] = "work-test" },
		"assignment":           func(m map[string]string) { m["assignment_id"] = "assignment-test" },
	}
	for name, mutate := range mutations {
		t.Run(name, func(t *testing.T) {
			metadata := projectPlanningMetadata()
			mutate(metadata)
			request := LLMRequest{RequestClass: RequestClassAgentRuntime, MaxTokens: 1024, Metadata: metadata}
			if admitted, err := classifyModelWorkRequest(&request, metadata["request_id"]); err == nil || admitted {
				t.Fatal("invalid project planning request admitted")
			}
		})
	}
}

func TestCustomerRequestModelWorkRejectsMixedOrNoncanonicalSubjects(t *testing.T) {
	mutations := map[string]func(map[string]string){
		"missing_schema":       func(m map[string]string) { delete(m, "company_execution_schema") },
		"missing_subject":      func(m map[string]string) { delete(m, "company_execution_subject") },
		"foreign_subject":      func(m map[string]string) { m["company_execution_subject"] = "project" },
		"missing_request":      func(m map[string]string) { delete(m, "customer_request_id") },
		"invalid_request":      func(m map[string]string) { m["customer_request_id"] = "../request" },
		"long_request":         func(m map[string]string) { m["customer_request_id"] = strings.Repeat("a", 129) },
		"missing_version":      func(m map[string]string) { delete(m, "customer_request_version") },
		"zero_version":         func(m map[string]string) { m["customer_request_version"] = "0" },
		"negative_version":     func(m map[string]string) { m["customer_request_version"] = "-1" },
		"noncanonical_version": func(m map[string]string) { m["customer_request_version"] = "01" },
		"overflow_version":     func(m map[string]string) { m["customer_request_version"] = "18446744073709551616" },
		"project":              func(m map[string]string) { m["project_id"] = "project-test" },
		"empty_project":        func(m map[string]string) { m["project_id"] = "" },
		"work_item":            func(m map[string]string) { m["work_item_id"] = "work-test" },
		"assignment":           func(m map[string]string) { m["assignment_id"] = "assignment-test" },
		"assignment_version":   func(m map[string]string) { m["assignment_version"] = "1" },
		"schema_downgrade":     func(m map[string]string) { m["company_execution_schema"] = "1" },
	}
	for name, mutate := range mutations {
		t.Run(name, func(t *testing.T) {
			req := LLMRequest{RequestClass: RequestClassAgentRuntime, MaxTokens: 128, Metadata: salesRequestMetadata()}
			mutate(req.Metadata)
			if admitted, err := classifyModelWorkRequest(&req, req.Metadata["request_id"]); err == nil || admitted {
				t.Fatal("invalid Sales subject was classified as model work")
			}
		})
	}
	for _, metadata := range []map[string]string{modelWorkMetadata(), salesRequestMetadata()} {
		req := LLMRequest{RequestClass: RequestClassAgentRuntime, MaxTokens: 128, Metadata: metadata}
		if admitted, err := classifyModelWorkRequest(&req, metadata["request_id"]); err != nil || !admitted {
			t.Fatalf("valid subject rejected: %v", err)
		}
	}
	legacy := modelWorkMetadata()
	legacy["customer_request_id"] = "request-test"
	if _, err := classifyModelWorkRequest(&LLMRequest{RequestClass: RequestClassAgentRuntime, MaxTokens: 128, Metadata: legacy}, legacy["request_id"]); err == nil {
		t.Fatal("legacy schema accepted a mixed request subject")
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

func TestAdaptiveModelWorkRequiresCanonicalRequestBoundSessionAndEffect(t *testing.T) {
	valid := adaptiveRequestMetadata()
	req := LLMRequest{RequestClass: RequestClassAgentRuntime, MaxTokens: 1024, Metadata: valid}
	if admitted, err := classifyModelWorkRequest(&req, valid["request_id"]); err != nil || !admitted {
		t.Fatalf("valid adaptive request rejected: %v", err)
	}
	mutations := map[string]func(map[string]string){
		"missing_session":  func(m map[string]string) { delete(m, "adaptive_session_id") },
		"missing_effect":   func(m map[string]string) { delete(m, "adaptive_effect_id") },
		"session_mismatch": func(m map[string]string) { m["adaptive_session_id"] = "01991c34-e03c-70c2-b97e-0591f4be2313" },
		"effect_mismatch":  func(m map[string]string) { m["adaptive_effect_id"] = "01991c34-e03c-70c2-b97e-0591f4be2313" },
		"bad_request_uuid": func(m map[string]string) {
			m["request_id"] = "company-adaptive-01991c34-e03c-70c2-b97e-0591f4be2311-01991c34-e03c-00c2-b97e-0591f4be2312"
		},
		"sales_subject": func(m map[string]string) { m["customer_request_id"] = "request-test" },
	}
	for name, mutate := range mutations {
		t.Run(name, func(t *testing.T) {
			metadata := adaptiveRequestMetadata()
			mutate(metadata)
			request := LLMRequest{RequestClass: RequestClassAgentRuntime, MaxTokens: 1024, Metadata: metadata}
			if admitted, err := classifyModelWorkRequest(&request, metadata["request_id"]); err == nil || admitted {
				t.Fatal("invalid adaptive request admitted")
			}
		})
	}
}

func TestModelWorkForwardsOnceWithoutSynthesisRegenerationOrLegacyActions(t *testing.T) {
	for _, test := range []struct {
		name, content, decision string
		sales                   bool
	}{
		{"typed", `{"schema_version":1,"tools":[{"kind":"write_file","path":"a.js","content":"console.log(1)","expected_sha256":null}]}`, "forward", false},
		{"fourth_wall", "Ich bin eine KI", "dropped", false},
		{"oversized", strings.Repeat("x", maxModelWorkResponseBytes+1), "dropped", false},
		{"sales_question", `{"schema_version":1,"decision":{"kind":"ask_question","content":"Welche Inhalte sollen auf die Kontaktseite?"}}`, "forward", true},
		{"sales_fourth_wall", "Ich bin eine KI", "dropped", true},
		{"sales_oversized", strings.Repeat("x", maxModelWorkResponseBytes+1), "dropped", true},
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
			metadata := modelWorkMetadata()
			if test.sales {
				metadata = salesRequestMetadata()
			}
			encoded, err := json.Marshal(map[string]any{
				"max_tokens": 128,
				"messages":   []map[string]string{{"role": "user", "content": "Build the assigned site"}},
				"metadata":   metadata,
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

func TestModelWorkDeadlineBoundsQueueAndProviderWithoutExtendingCaller(t *testing.T) {
	for _, test := range []struct {
		name            string
		providerTimeout time.Duration
		callerTimeout   time.Duration
		wantTimeout     time.Duration
	}{
		{"long_global", 5 * time.Minute, 5 * time.Minute, maxModelWorkDuration},
		{"unset_global", 0, 5 * time.Minute, maxModelWorkDuration},
		{"short_global", 10 * time.Second, 5 * time.Minute, 10 * time.Second},
		{"short_caller", 5 * time.Minute, 5 * time.Second, maxModelWorkDuration},
		{"expired_caller", 5 * time.Minute, -time.Second, maxModelWorkDuration},
	} {
		t.Run(test.name, func(t *testing.T) {
			started := time.Now()
			callerDeadline := started.Add(test.callerTimeout)
			provider := &pipelineMockProvider{name: "mock", sendFunc: func(ctx context.Context, req *LLMRequest) (*LLMResponse, error) {
				deadline, exists := ctx.Deadline()
				if !exists || deadline.After(time.Now().Add(maxModelWorkDuration)) || deadline.After(callerDeadline) {
					t.Fatal("missing or extended whole-attempt deadline")
				}
				if req.ProviderTimeout != test.wantTimeout {
					t.Fatalf("provider timeout=%s, want %s", req.ProviderTimeout, test.wantTimeout)
				}
				return &LLMResponse{Content: "A bounded proposal", Model: "mock-tier2", InputTokens: 1, OutputTokens: 1, TokensUsed: 2}, nil
			}}
			reg := NewRegistry()
			reg.Register("mock", provider)
			ph := newTestPipelineHandler(reg, control.NewConfig("mock"))
			ph.providerDeadline = test.providerTimeout
			encoded, err := json.Marshal(map[string]any{
				"max_tokens": 128,
				"messages":   []map[string]string{{"role": "user", "content": "Perform the assigned work"}},
				"metadata":   modelWorkMetadata(),
			})
			if err != nil {
				t.Fatal(err)
			}
			req := newAgentRuntimeTestRequest(t, string(encoded))
			req.Header.Set("X-Request-ID", "company-provider-reservation-test")
			ctx, cancel := context.WithDeadline(req.Context(), callerDeadline)
			defer cancel()
			w := httptest.NewRecorder()
			ph.ServeHTTP(w, req.WithContext(ctx))
			if test.callerTimeout < 0 {
				if w.Code != http.StatusGatewayTimeout || provider.calls != 0 {
					t.Fatalf("expired request status=%d calls=%d", w.Code, provider.calls)
				}
			} else if w.Code != http.StatusOK || provider.calls != 1 {
				t.Fatalf("bounded request status=%d calls=%d body=%s", w.Code, provider.calls, w.Body.String())
			}
		})
	}
}
