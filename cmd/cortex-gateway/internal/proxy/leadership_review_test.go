package proxy

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func leadershipReviewTestRequest() *LLMRequest {
	req := subscriptionTestRequest()
	req.MaxTokens = 128
	m := req.Metadata
	m["tenant_id"] = "tenant-test"
	m["project_id"] = "project-test"
	m["work_item_id"] = "work-test"
	m["assignment_id"] = "assignment-test"
	m["assignment_version"] = "1"
	m["company_execution_schema"] = "5"
	m["company_execution_subject"] = "adaptive_leadership_review"
	m["company_execution_output_kind"] = "leadership_decision"
	m["leadership_review_id"] = "01991c34-e03c-70c2-b97e-0591f4be2311"
	m["reservation_id"] = m["leadership_review_id"]
	m["request_id"] = "company-leadership-" + m["leadership_review_id"]
	m["subscription_allowance_id"] = "leadership-allowance-test"
	return req
}

func TestLeadershipReviewClaimsSeparateDurableAuthorityBeforeProvider(t *testing.T) {
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
		want := customerRequestExecutionSubject{Kind: "adaptive_leadership_review", ReviewID: "01991c34-e03c-70c2-b97e-0591f4be2311"}
		if request.SchemaVersion != 5 || request.Subject == nil || *request.Subject != want ||
			request.AllowanceID != "leadership-allowance-test" || callbacks.Load() > 1 {
			w.WriteHeader(http.StatusForbidden)
			return
		}
		_ = json.NewEncoder(w).Encode(subscriptionDispatchReceipt{
			SchemaVersion: 5, AllowanceID: request.AllowanceID, RequestID: request.RequestID,
			RequestDigest: request.RequestDigest, DeadlineUnixMS: time.Now().Add(time.Minute).UnixMilli(),
		})
	}))
	defer server.Close()
	admission, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), server.URL, "test-operator")
	if err != nil {
		t.Fatal(err)
	}
	provider := &subscriptionTestProvider{}
	if _, err := admission.send(context.Background(), provider, leadershipReviewTestRequest()); err != nil {
		t.Fatal(err)
	}
	if _, err := admission.send(context.Background(), provider, leadershipReviewTestRequest()); err == nil {
		t.Fatal("consumed review dispatched twice")
	}
	if provider.calls.Load() != 1 || callbacks.Load() != 2 {
		t.Fatalf("provider calls=%d authority calls=%d", provider.calls.Load(), callbacks.Load())
	}
}

func TestLeadershipReviewRejectsMixedSubjectAndBootstrapGrantBeforeAuthority(t *testing.T) {
	var callbacks atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		callbacks.Add(1)
		w.WriteHeader(http.StatusForbidden)
	}))
	defer server.Close()
	admission, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), server.URL, "test-operator")
	if err != nil {
		t.Fatal(err)
	}
	mutations := map[string]func(map[string]string){
		"bootstrap_grant":         func(m map[string]string) { m["subscription_allowance_id"] = "subscription-test" },
		"reservation_as_grant":    func(m map[string]string) { m["subscription_allowance_id"] = m["reservation_id"] },
		"mismatched_review":       func(m map[string]string) { m["leadership_review_id"] = "01991c34-e03c-70c2-b97e-0591f4be2312" },
		"adaptive_subject":        func(m map[string]string) { m["adaptive_session_id"] = "foreign" },
		"customer_subject":        func(m map[string]string) { m["customer_request_id"] = "request-test" },
		"schema_downgrade":        func(m map[string]string) { m["company_execution_schema"] = "1" },
		"foreign_work":            func(m map[string]string) { m["work_item_id"] = "../work" },
		"noncanonical_assignment": func(m map[string]string) { m["assignment_version"] = "01" },
		"wrong_output":            func(m map[string]string) { m["company_execution_output_kind"] = "tool_plan" },
	}
	for name, mutate := range mutations {
		t.Run(name, func(t *testing.T) {
			req := leadershipReviewTestRequest()
			mutate(req.Metadata)
			provider := &subscriptionTestProvider{}
			if _, err := admission.send(context.Background(), provider, req); err == nil {
				t.Fatal("invalid review admitted")
			}
			if provider.calls.Load() != 0 || callbacks.Load() != 0 {
				t.Fatal("invalid review reached authority or provider")
			}
		})
	}
}
