package proxy

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/forwardqueue"
)

type subscriptionTestProvider struct{ calls atomic.Int32 }

func (p *subscriptionTestProvider) Name() string                      { return CodexCLIProviderName }
func (p *subscriptionTestProvider) HealthCheck(context.Context) error { return nil }
func (p *subscriptionTestProvider) Send(ctx context.Context, req *LLMRequest) (*LLMResponse, error) {
	if ctx.Err() != nil {
		return nil, ctx.Err()
	}
	p.calls.Add(1)
	return &LLMResponse{Content: "result", Model: req.Model, InputTokens: 10, OutputTokens: 2}, nil
}

func subscriptionTestRequest() *LLMRequest {
	return &LLMRequest{Model: "model-a", EffectiveModel: "model-a", CallerRole: CallerRoleAgentRuntime,
		RequestClass: RequestClassAgentRuntime, AuthorityRequestDigest: strings.Repeat("d", 64),
		Metadata: map[string]string{"agent_id": "6", "request_id": "company-provider-subscription-test",
			"reservation_id": "subscription-test", "subscription_allowance_id": "subscription-test",
			"reserved_provider": CodexCLIProviderName, "company_execution_schema": "1",
			"subscription_catalog_digest": strings.Repeat("c", 64), "company_execution_context_digest": strings.Repeat("b", 64)},
	}
}

func salesSubscriptionTestRequest() *LLMRequest {
	req := subscriptionTestRequest()
	for key, value := range salesRequestMetadata() {
		if key == "request_id" || key == "reservation_id" || key == "reserved_provider" || key == "company_execution_context_digest" {
			continue
		}
		req.Metadata[key] = value
	}
	req.MaxTokens = 128
	return req
}

func dynamicSalesSubscriptionTestRequest() *LLMRequest {
	req := salesSubscriptionTestRequest()
	req.Metadata["subscription_allowance_id"] = "subscription-sales-request-test"
	req.Metadata["reservation_id"] = req.Metadata["subscription_allowance_id"]
	req.Metadata["request_id"] = "company-provider-" + req.Metadata["reservation_id"]
	return req
}

func TestSalesSubscriptionDynamicAllowanceRequiresExplicitOptIn(t *testing.T) {
	for _, enabled := range []bool{false, true} {
		t.Run(fmt.Sprint(enabled), func(t *testing.T) {
			var callbacks atomic.Int32
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				callbacks.Add(1)
				w.WriteHeader(http.StatusForbidden)
			}))
			defer server.Close()
			admission, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), server.URL, "test-operator")
			if enabled {
				admission, err = NewSubscriptionAdmissionWithSalesAutonomy("subscription-test", strings.Repeat("c", 64), server.URL, "test-operator", true)
			}
			if err != nil {
				t.Fatal(err)
			}
			provider := &subscriptionTestProvider{}
			wrapped := NewSubscriptionQueuedProvider(provider, forwardqueue.NewManager(1), admission)
			if _, err := wrapped.Send(context.Background(), dynamicSalesSubscriptionTestRequest()); err == nil {
				t.Fatal("unclaimed dynamic Sales request admitted")
			}
			planning := projectPlanningSubscriptionTestRequest()
			planning.Metadata["subscription_allowance_id"] = "subscription-sales-request-test"
			planning.Metadata["reservation_id"] = planning.Metadata["subscription_allowance_id"]
			planning.Metadata["request_id"] = "company-planning-" + planning.Metadata["reservation_id"] + "-project-test"
			if _, err := wrapped.Send(context.Background(), planning); err == nil {
				t.Fatal("Sales opt-in admitted a foreign planning allowance")
			}
			wantCallbacks := int32(0)
			if enabled {
				wantCallbacks = 1
			}
			if callbacks.Load() != wantCallbacks || provider.calls.Load() != 0 {
				t.Fatalf("authority calls=%d provider calls=%d", callbacks.Load(), provider.calls.Load())
			}
		})
	}
}

func autonomousSalesClaimMatches(claim subscriptionDispatch) bool {
	wantSubject := customerRequestExecutionSubject{Kind: "customer_request", RequestID: "request-test", RequestVersion: 1}
	return claim.SchemaVersion == 2 && claim.AllowanceID == "subscription-sales-request-test" && claim.AgentID == 5 &&
		claim.RequestID == "company-provider-subscription-sales-request-test" && claim.Subject != nil &&
		*claim.Subject == wantSubject && claim.Provider == CodexCLIProviderName && claim.Model == "model-a" &&
		claim.CatalogDigest == strings.Repeat("c", 64) && claim.ContextDigest == strings.Repeat("b", 64) && claim.RequestDigest == strings.Repeat("d", 64)
}

func autonomousSalesTestReceipt(claim subscriptionDispatch, mode string) subscriptionDispatchReceipt {
	receipt := subscriptionDispatchReceipt{SchemaVersion: claim.SchemaVersion, AllowanceID: claim.AllowanceID,
		RequestID: claim.RequestID, RequestDigest: claim.RequestDigest, DeadlineUnixMS: time.Now().Add(time.Minute).UnixMilli()}
	switch mode {
	case "schema-mismatch":
		receipt.SchemaVersion = 1
	case "allowance-mismatch":
		receipt.AllowanceID = "subscription-test"
	case "request-mismatch":
		receipt.RequestID = "company-provider-subscription-test"
	case "digest-mismatch":
		receipt.RequestDigest = strings.Repeat("e", 64)
	case "expired":
		receipt.DeadlineUnixMS = time.Now().Add(-time.Second).UnixMilli()
	}
	return receipt
}

func autonomousSalesAuthorityHandler(t *testing.T, mode string, provider *subscriptionTestProvider, queue *forwardqueue.Manager, callbacks *atomic.Int32) http.Handler {
	t.Helper()
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		callbacks.Add(1)
		if provider.calls.Load() != 0 || queue.Stats().Active != 1 {
			t.Error("authority claim must follow queue lease and precede provider I/O")
			w.WriteHeader(http.StatusForbidden)
			return
		}
		if r.Method != http.MethodPost || r.URL.Path != "/operator/workflow/subscription-dispatch" || r.Header.Get("Authorization") != "Bearer test-operator" {
			t.Error("wrong authority transport or credential")
			w.WriteHeader(http.StatusUnauthorized)
			return
		}
		var claim subscriptionDispatch
		decoder := json.NewDecoder(r.Body)
		decoder.DisallowUnknownFields()
		if err := decoder.Decode(&claim); err != nil {
			t.Error(err)
			w.WriteHeader(http.StatusBadRequest)
			return
		}
		if !autonomousSalesClaimMatches(claim) {
			if mode != "cross-subject" && mode != "forged-model" && mode != "forged-digest" {
				t.Error("unexpected authority claim binding")
			}
			w.WriteHeader(http.StatusForbidden)
			return
		}
		switch mode {
		case "rejected":
			w.WriteHeader(http.StatusForbidden)
			return
		case "claim-lost":
			connection, _, err := w.(http.Hijacker).Hijack()
			if err != nil {
				t.Error(err)
				return
			}
			_ = connection.Close()
			return
		case "lost":
			_, _ = io.WriteString(w, "{")
			return
		}
		_ = json.NewEncoder(w).Encode(autonomousSalesTestReceipt(claim, mode))
	})
}

func TestSalesSubscriptionAutonomousDispatchRequiresExactAuthorityReceipt(t *testing.T) {
	for _, mode := range []string{"approved", "rejected", "cross-subject", "forged-model", "forged-digest", "schema-mismatch", "allowance-mismatch", "request-mismatch", "digest-mismatch", "expired", "lost", "claim-lost"} {
		t.Run(mode, func(t *testing.T) {
			provider := &subscriptionTestProvider{}
			queue := forwardqueue.NewManager(1)
			var callbacks atomic.Int32
			server := httptest.NewServer(autonomousSalesAuthorityHandler(t, mode, provider, queue, &callbacks))
			defer server.Close()
			admission, err := NewSubscriptionAdmissionWithSalesAutonomy("subscription-test", strings.Repeat("c", 64), server.URL, "test-operator", true)
			if err != nil {
				t.Fatal(err)
			}
			req := dynamicSalesSubscriptionTestRequest()
			switch mode {
			case "cross-subject":
				req.Metadata["customer_request_id"] = "request-other"
			case "forged-model":
				req.Model, req.EffectiveModel = "model-other", "model-other"
			case "forged-digest":
				req.AuthorityRequestDigest = strings.Repeat("e", 64)
			}
			_, err = NewSubscriptionQueuedProvider(provider, queue, admission).Send(context.Background(), req)
			wantCalls := int32(0)
			if mode == "approved" {
				wantCalls = 1
			}
			if (err == nil) != (mode == "approved") || provider.calls.Load() != wantCalls || callbacks.Load() != 1 || queue.Stats().Active != 0 {
				t.Fatalf("result=%v provider calls=%d authority calls=%d queue=%+v", err, provider.calls.Load(), callbacks.Load(), queue.Stats())
			}
		})
	}
}

func TestSalesSubscriptionAutonomousInvalidBindingsNeverReachAuthority(t *testing.T) {
	var callbacks atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		callbacks.Add(1)
		w.WriteHeader(http.StatusForbidden)
	}))
	defer server.Close()
	admission, err := NewSubscriptionAdmissionWithSalesAutonomy("subscription-test", strings.Repeat("c", 64), server.URL, "test-operator", true)
	if err != nil {
		t.Fatal(err)
	}
	provider := &subscriptionTestProvider{}
	wrapped := NewSubscriptionQueuedProvider(provider, forwardqueue.NewManager(1), admission)
	mutations := map[string]func(*LLMRequest){
		"mixed_subject": func(r *LLMRequest) { r.Metadata["project_id"] = "project-test" },
		"wrong_subject": func(r *LLMRequest) { r.Metadata["company_execution_subject"] = "project_planning" },
		"version":       func(r *LLMRequest) { r.Metadata["customer_request_version"] = "01" },
		"allowance":     func(r *LLMRequest) { r.Metadata["subscription_allowance_id"] = "../forged" },
		"reservation":   func(r *LLMRequest) { r.Metadata["reservation_id"] = "subscription-other" },
		"request":       func(r *LLMRequest) { r.Metadata["request_id"] = "company-provider-subscription-other" },
		"agent":         func(r *LLMRequest) { r.Metadata["agent_id"] = "0" },
		"class":         func(r *LLMRequest) { r.RequestClass = RequestClassExternalCompat },
		"caller":        func(r *LLMRequest) { r.CallerRole = CallerRolePlatformControlplane },
		"provider":      func(r *LLMRequest) { r.Metadata["reserved_provider"] = "local-loop" },
		"catalog":       func(r *LLMRequest) { r.Metadata["subscription_catalog_digest"] = strings.Repeat("e", 64) },
		"model":         func(r *LLMRequest) { r.EffectiveModel = "model-other" },
		"digest":        func(r *LLMRequest) { r.AuthorityRequestDigest = "forged" },
		"context":       func(r *LLMRequest) { r.Metadata["company_execution_context_digest"] = "forged" },
		"stream":        func(r *LLMRequest) { r.Stream = true },
	}
	for name, mutate := range mutations {
		t.Run(name, func(t *testing.T) {
			req := dynamicSalesSubscriptionTestRequest()
			mutate(req)
			if _, err := wrapped.Send(context.Background(), req); err == nil {
				t.Fatal("invalid dynamic Sales binding admitted")
			}
		})
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := wrapped.Send(ctx, dynamicSalesSubscriptionTestRequest()); err == nil {
		t.Fatal("cancelled queue lease admitted an authority claim")
	}
	if callbacks.Load() != 0 || provider.calls.Load() != 0 {
		t.Fatalf("authority calls=%d provider calls=%d", callbacks.Load(), provider.calls.Load())
	}
}

func adaptiveSubscriptionTestRequest() *LLMRequest {
	req := subscriptionTestRequest()
	for key, value := range adaptiveRequestMetadata() {
		if key == "reservation_id" || key == "reserved_provider" || key == "company_execution_context_digest" {
			continue
		}
		req.Metadata[key] = value
	}
	req.Metadata["reservation_id"] = "subscription-test"
	req.Metadata["subscription_allowance_id"] = "subscription-test"
	req.Metadata["subscription_catalog_digest"] = strings.Repeat("c", 64)
	req.MaxTokens = 128
	return req
}

func projectPlanningSubscriptionTestRequest() *LLMRequest {
	req := subscriptionTestRequest()
	for key, value := range projectPlanningMetadata() {
		if key == "reservation_id" || key == "reserved_provider" || key == "company_execution_context_digest" {
			continue
		}
		req.Metadata[key] = value
	}
	req.Metadata["reservation_id"] = "subscription-test"
	req.Metadata["subscription_allowance_id"] = "subscription-test"
	req.Metadata["subscription_catalog_digest"] = strings.Repeat("c", 64)
	req.Metadata["request_id"] = "company-planning-subscription-test-project-test"
	req.MaxTokens = 128
	return req
}

func TestProjectPlanningSubscriptionClaimsExactSubjectBeforeProvider(t *testing.T) {
	var callbacks atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		callbacks.Add(1)
		var request subscriptionDispatch
		decoder := json.NewDecoder(r.Body)
		decoder.DisallowUnknownFields()
		if err := decoder.Decode(&request); err != nil {
			t.Error(err)
			w.WriteHeader(http.StatusBadRequest)
			return
		}
		want := customerRequestExecutionSubject{Kind: "project_planning", ProjectID: "project-test", ProjectVersion: 1}
		if request.SchemaVersion != 4 || request.Subject == nil || *request.Subject != want || callbacks.Load() > 1 {
			w.WriteHeader(http.StatusForbidden)
			return
		}
		_ = json.NewEncoder(w).Encode(subscriptionDispatchReceipt{
			SchemaVersion: 4, AllowanceID: request.AllowanceID, RequestID: request.RequestID,
			RequestDigest: request.RequestDigest, DeadlineUnixMS: time.Now().Add(time.Minute).UnixMilli(),
		})
	}))
	defer server.Close()
	admission, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), server.URL, "test-operator")
	if err != nil {
		t.Fatal(err)
	}
	provider := &subscriptionTestProvider{}
	if _, err := admission.send(context.Background(), provider, projectPlanningSubscriptionTestRequest()); err != nil {
		t.Fatal(err)
	}
	if _, err := admission.send(context.Background(), provider, projectPlanningSubscriptionTestRequest()); err == nil {
		t.Fatal("consumed project planning authority replay admitted another provider call")
	}
	if provider.calls.Load() != 1 || callbacks.Load() != 2 {
		t.Fatalf("provider calls=%d callbacks=%d", provider.calls.Load(), callbacks.Load())
	}
}

func TestAdaptiveSubscriptionClaimsExactSubjectBeforeProvider(t *testing.T) {
	var callbacks atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		callbacks.Add(1)
		var request subscriptionDispatch
		decoder := json.NewDecoder(r.Body)
		decoder.DisallowUnknownFields()
		if err := decoder.Decode(&request); err != nil {
			t.Error(err)
			w.WriteHeader(http.StatusBadRequest)
			return
		}
		want := customerRequestExecutionSubject{
			Kind: "adaptive_session", SessionID: "01991c34-e03c-70c2-b97e-0591f4be2311",
			EffectID: "01991c34-e03c-70c2-b97e-0591f4be2312", SessionVersion: 1,
		}
		if request.SchemaVersion != 3 || request.Subject == nil || *request.Subject != want || callbacks.Load() > 1 {
			w.WriteHeader(http.StatusForbidden)
			return
		}
		_ = json.NewEncoder(w).Encode(subscriptionDispatchReceipt{
			SchemaVersion: 3, AllowanceID: request.AllowanceID, RequestID: request.RequestID,
			RequestDigest: request.RequestDigest, DeadlineUnixMS: time.Now().Add(time.Minute).UnixMilli(),
		})
	}))
	defer server.Close()
	admission, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), server.URL, "test-operator")
	if err != nil {
		t.Fatal(err)
	}
	provider := &subscriptionTestProvider{}
	if _, err := admission.send(context.Background(), provider, adaptiveSubscriptionTestRequest()); err != nil {
		t.Fatal(err)
	}
	if _, err := admission.send(context.Background(), provider, adaptiveSubscriptionTestRequest()); err == nil {
		t.Fatal("consumed adaptive authority replay admitted another provider call")
	}
	if provider.calls.Load() != 1 || callbacks.Load() != 2 {
		t.Fatalf("provider calls=%d callbacks=%d", provider.calls.Load(), callbacks.Load())
	}
}

func TestAdaptiveSubscriptionRejectsChangedIdentityBeforeAuthorityHTTP(t *testing.T) {
	admission, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), "http://127.0.0.1:1", "test-operator")
	if err != nil {
		t.Fatal(err)
	}
	for _, mutate := range []func(*LLMRequest){
		func(r *LLMRequest) { r.Metadata["adaptive_session_id"] = "01991c34-e03c-70c2-b97e-0591f4be2313" },
		func(r *LLMRequest) { r.Metadata["adaptive_effect_id"] = "01991c34-e03c-70c2-b97e-0591f4be2313" },
		func(r *LLMRequest) { r.Metadata["adaptive_session_version"] = "01" },
		func(r *LLMRequest) { r.Metadata["request_id"] = "company-adaptive-not-a-uuid-not-a-uuid" },
	} {
		req := adaptiveSubscriptionTestRequest()
		mutate(req)
		if _, err := admission.dispatchRequest(&subscriptionTestProvider{}, req); err == nil {
			t.Fatal("changed adaptive identity reached durable admission")
		}
	}
}

func TestSalesSubscriptionRequiresMatchingDurableAdmissionBeforeProvider(t *testing.T) {
	for _, mode := range []string{"approved", "rejected", "legacy-receipt"} {
		t.Run(mode, func(t *testing.T) {
			var callbacks atomic.Int32
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				callbacks.Add(1)
				var request subscriptionDispatch
				decoder := json.NewDecoder(r.Body)
				decoder.DisallowUnknownFields()
				if err := decoder.Decode(&request); err != nil {
					t.Error(err)
					w.WriteHeader(http.StatusBadRequest)
					return
				}
				if request.SchemaVersion != 2 || request.Subject == nil || *request.Subject != (customerRequestExecutionSubject{Kind: "customer_request", RequestID: "request-test", RequestVersion: 1}) {
					t.Error("request subject was lost at the authority boundary")
					w.WriteHeader(http.StatusBadRequest)
					return
				}
				if mode == "rejected" || callbacks.Load() > 1 {
					w.WriteHeader(http.StatusForbidden)
					return
				}
				schema := request.SchemaVersion
				if mode == "legacy-receipt" {
					schema = 1
				}
				_ = json.NewEncoder(w).Encode(subscriptionDispatchReceipt{SchemaVersion: schema, AllowanceID: request.AllowanceID, RequestID: request.RequestID, RequestDigest: request.RequestDigest, DeadlineUnixMS: time.Now().Add(time.Minute).UnixMilli()})
			}))
			defer server.Close()
			admission, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), server.URL, "test-operator")
			if err != nil {
				t.Fatal(err)
			}
			provider := &subscriptionTestProvider{}
			_, err = admission.send(context.Background(), provider, salesSubscriptionTestRequest())
			if (err == nil) != (mode == "approved") {
				t.Fatalf("unexpected admission result: %v", err)
			}
			if _, err := admission.send(context.Background(), provider, salesSubscriptionTestRequest()); err == nil {
				t.Fatal("consumed authority replay admitted another provider call")
			}
			wantCalls := int32(0)
			if mode == "approved" {
				wantCalls = 1
			}
			if provider.calls.Load() != wantCalls || callbacks.Load() != 2 {
				t.Fatalf("provider calls=%d callbacks=%d", provider.calls.Load(), callbacks.Load())
			}
		})
	}
}

func TestSalesSubscriptionRejectsMixedSubjectBeforeAuthorityHTTP(t *testing.T) {
	admission, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), "http://127.0.0.1:1", "test-operator")
	if err != nil {
		t.Fatal(err)
	}
	for _, mutate := range []func(*LLMRequest){
		func(r *LLMRequest) { r.Metadata["project_id"] = "project-test" },
		func(r *LLMRequest) { r.Metadata["customer_request_version"] = "0" },
		func(r *LLMRequest) { r.Metadata["agent_id"] = "65536" },
		func(r *LLMRequest) { r.RequestClass = RequestClassExternalCompat },
		func(r *LLMRequest) { r.Stream = true },
	} {
		req := salesSubscriptionTestRequest()
		mutate(req)
		if _, err := admission.dispatchRequest(&subscriptionTestProvider{}, req); err == nil {
			t.Fatal("invalid Sales request reached durable admission")
		}
	}
}

func TestSubscriptionAdmissionPersistsPermissionAtAuthorityNotGateway(t *testing.T) {
	var claimed atomic.Bool
	var callbacks atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		callbacks.Add(1)
		if r.URL.Path != "/operator/workflow/subscription-dispatch" || r.Header.Get("Authorization") != "Bearer test-operator" {
			w.WriteHeader(http.StatusUnauthorized)
			return
		}
		var request subscriptionDispatch
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			t.Error(err)
			w.WriteHeader(http.StatusBadRequest)
			return
		}
		if !claimed.CompareAndSwap(false, true) {
			w.WriteHeader(http.StatusForbidden)
			return
		}
		_ = json.NewEncoder(w).Encode(subscriptionDispatchReceipt{SchemaVersion: 1, AllowanceID: request.AllowanceID,
			RequestID: request.RequestID, RequestDigest: request.RequestDigest, DeadlineUnixMS: time.Now().Add(time.Minute).UnixMilli()})
	}))
	defer server.Close()
	provider := &subscriptionTestProvider{}
	for attempt := 0; attempt < 2; attempt++ {
		admission, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), server.URL, "test-operator")
		if err != nil {
			t.Fatal(err)
		}
		wrapped := NewSubscriptionQueuedProvider(provider, forwardqueue.NewManager(1), admission)
		_, err = wrapped.Send(context.Background(), subscriptionTestRequest())
		if (err != nil) != (attempt == 1) {
			t.Fatalf("attempt %d: %v", attempt, err)
		}
	}
	if provider.calls.Load() != 1 || callbacks.Load() != 2 {
		t.Fatal("gateway reconstruction bypassed durable authority")
	}
}

func TestSubscriptionAdmissionCarriesDynamicProjectAllowanceToDurableAuthority(t *testing.T) {
	admission, err := NewSubscriptionAdmission("subscription-bootstrap", strings.Repeat("c", 64), "http://127.0.0.1:1", "test-operator")
	if err != nil {
		t.Fatal(err)
	}
	req := subscriptionTestRequest()
	req.Metadata["subscription_allowance_id"] = "subscription-project-work"
	req.Metadata["reservation_id"] = "subscription-project-work"
	req.Metadata["request_id"] = "company-provider-subscription-project-work"
	dispatch, err := admission.dispatchRequest(&subscriptionTestProvider{}, req)
	if err != nil {
		t.Fatal(err)
	}
	if dispatch.SchemaVersion != 1 || dispatch.AllowanceID != "subscription-project-work" {
		t.Fatalf("unexpected dynamic dispatch: %#v", dispatch)
	}

	planning := projectPlanningSubscriptionTestRequest()
	planning.Metadata["subscription_allowance_id"] = "subscription-project-work"
	planning.Metadata["reservation_id"] = "subscription-project-work"
	if _, err := admission.dispatchRequest(&subscriptionTestProvider{}, planning); err == nil {
		t.Fatal("project planning escaped its exact bootstrap allowance")
	}
}

func TestSubscriptionAdmissionRejectsOtherCallersAndBindingsBeforeHTTP(t *testing.T) {
	admission, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), "http://127.0.0.1:1", "test-operator")
	if err != nil {
		t.Fatal(err)
	}
	mutations := map[string]func(*LLMRequest){
		"background":        func(r *LLMRequest) { r.CallerRole = CallerRole("background") },
		"judge":             func(r *LLMRequest) { r.CallerRole = CallerRole("judge") },
		"gaia":              func(r *LLMRequest) { r.RequestClass = RequestClass("gaia") },
		"missing_digest":    func(r *LLMRequest) { r.AuthorityRequestDigest = "" },
		"foreign_allowance": func(r *LLMRequest) { r.Metadata["subscription_allowance_id"] = "foreign" },
		"changed_catalog":   func(r *LLMRequest) { r.Metadata["subscription_catalog_digest"] = strings.Repeat("e", 64) },
		"changed_model":     func(r *LLMRequest) { r.EffectiveModel = "model-b" },
		"no_work":           func(r *LLMRequest) { r.Metadata["company_execution_schema"] = "" },
	}
	for name, mutate := range mutations {
		t.Run(name, func(t *testing.T) {
			req := subscriptionTestRequest()
			mutate(req)
			if _, err := admission.dispatchRequest(&subscriptionTestProvider{}, req); err == nil {
				t.Fatal("invalid request admitted")
			}
		})
	}
	if _, err := admission.dispatchRequest(&mockProvider{name: "local-loop"}, subscriptionTestRequest()); err == nil {
		t.Fatal("local-loop bypass")
	}
}

func TestSubscriptionAdmissionFailureDoesNotTripProviderBreaker(t *testing.T) {
	admission, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), "http://127.0.0.1:1", "test-operator")
	if err != nil {
		t.Fatal(err)
	}
	provider := &subscriptionTestProvider{}
	req := subscriptionTestRequest()
	req.CallerRole = CallerRolePlatformControlplane
	_, sendErr := NewSubscriptionQueuedProvider(provider, forwardqueue.NewManager(1), admission).Send(context.Background(), req)
	if sendErr == nil {
		t.Fatal("unauthorized pre-dispatch request succeeded")
	}
	var admissionErr *ProviderAdmissionError
	if !errors.As(sendErr, &admissionErr) {
		t.Fatalf("error type = %T, want ProviderAdmissionError", sendErr)
	}
	breaker := NewCircuitBreaker(BreakerConfig{WindowSeconds: 60, MinRequests: 1, FailureRatio: 1, FailureThreshold: 1, OpenSeconds: 10, HalfOpenProbes: 1, Enabled: true})
	breaker.Record(sendErr)
	if state := breaker.State(); state != "closed" {
		t.Fatalf("pre-dispatch rejection changed provider breaker to %s", state)
	}
	if provider.calls.Load() != 0 {
		t.Fatal("provider was called before authority admission")
	}
}

func TestSubscriptionAdmissionLostOrExpiredReceiptNeverCallsProvider(t *testing.T) {
	for _, mode := range []string{"lost", "expired", "redirect", "extra-json", "mismatch", "oversize"} {
		t.Run(mode, func(t *testing.T) {
			var callbacks atomic.Int32
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				callbacks.Add(1)
				if mode == "lost" {
					w.WriteHeader(http.StatusOK)
					_, _ = io.WriteString(w, "{")
					return
				}
				if mode == "redirect" {
					http.Redirect(w, r, "/other", http.StatusTemporaryRedirect)
					return
				}
				receipt := subscriptionDispatchReceipt{SchemaVersion: 1, AllowanceID: "subscription-test", RequestID: "company-provider-subscription-test", RequestDigest: strings.Repeat("d", 64), DeadlineUnixMS: time.Now().Add(time.Minute).UnixMilli()}
				if mode == "expired" {
					receipt.DeadlineUnixMS = time.Now().Add(-time.Second).UnixMilli()
				}
				if mode == "mismatch" {
					receipt.RequestDigest = strings.Repeat("e", 64)
				}
				_ = json.NewEncoder(w).Encode(receipt)
				if mode == "extra-json" {
					_, _ = io.WriteString(w, "{}")
				}
				if mode == "oversize" {
					_, _ = io.WriteString(w, strings.Repeat(" ", 4096)+"{}")
				}
			}))
			defer server.Close()
			admission, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), server.URL, "test-operator")
			if err != nil {
				t.Fatal(err)
			}
			provider := &subscriptionTestProvider{}
			_, err = NewSubscriptionQueuedProvider(provider, forwardqueue.NewManager(1), admission).Send(context.Background(), subscriptionTestRequest())
			if err == nil || provider.calls.Load() != 0 || callbacks.Load() != 1 {
				t.Fatalf("unsafe %s dispatch/retry: %v %d %d", mode, err, provider.calls.Load(), callbacks.Load())
			}
		})
	}
}

func TestSubscriptionRawBodyDigestCannotComeFromMetadata(t *testing.T) {
	ph := &PipelineHandler{logger: slog.New(slog.NewTextHandler(io.Discard, nil))}
	body := `{"messages":[],"metadata":{"request_digest":"fake"}}`
	for _, header := range []string{"", strings.Repeat("a", 64), fmt.Sprintf("%x", sha256.Sum256([]byte(body)))} {
		request := httptest.NewRequest(http.MethodPost, "/llm/request", strings.NewReader(body))
		request.Header.Set("X-Request-Digest", header)
		parsed, _, ok := ph.parseRequest(httptest.NewRecorder(), request)
		if !ok {
			t.Fatal("parse failed")
		}
		if (parsed.AuthorityRequestDigest != "") != (header == fmt.Sprintf("%x", sha256.Sum256([]byte(body)))) {
			t.Fatal("unchecked body authority")
		}
	}
}

func TestSubscriptionAdmissionRejectsExternalAuthorityAndMissingMode(t *testing.T) {
	for _, endpoint := range []string{"http://example.com", "https://example.com", "http://user:pass@127.0.0.1", "http://127.0.0.1/other", "http://127.0.0.1?query=yes"} {
		if _, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), endpoint, "test-operator"); err == nil {
			t.Fatalf("external or ambiguous authority accepted: %s", endpoint)
		}
	}
	provider := &subscriptionTestProvider{}
	if _, err := NewQueuedProvider(provider, forwardqueue.NewManager(1)).Send(context.Background(), subscriptionTestRequest()); err == nil || provider.calls.Load() != 0 {
		t.Fatal("missing gateway mode bypassed subscription authority")
	}
}
