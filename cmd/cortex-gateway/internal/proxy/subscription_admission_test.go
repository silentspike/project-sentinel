package proxy

import (
	"context"
	"crypto/sha256"
	"encoding/json"
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
