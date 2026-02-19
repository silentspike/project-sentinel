package guardrails

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestHandler_StatusEndpoint(t *testing.T) {
	cfg := newTestConfig()
	e := New(cfg)
	e.Record("claude", 1000, 500)

	h := NewHandler(e)
	mux := http.NewServeMux()
	h.RegisterRoutes(mux)

	req := httptest.NewRequest(http.MethodGet, "/api/guardrails/status", nil)
	w := httptest.NewRecorder()
	mux.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d", w.Code)
	}

	ct := w.Header().Get("Content-Type")
	if ct != "application/json" {
		t.Fatalf("expected application/json, got %q", ct)
	}

	var status GuardrailsStatus
	if err := json.NewDecoder(w.Body).Decode(&status); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}

	if status.Budget.HourlyUsed != 1500 {
		t.Fatalf("expected hourly used 1500, got %d", status.Budget.HourlyUsed)
	}
	if status.Budget.HourlyLimit != 10000 {
		t.Fatalf("expected hourly limit 10000, got %d", status.Budget.HourlyLimit)
	}
	if status.Cost.Total == 0 {
		t.Fatal("expected non-zero cost")
	}
	if status.RateInfo.PerAgentRPM != 20 {
		t.Fatalf("expected per_agent_rpm 20, got %d", status.RateInfo.PerAgentRPM)
	}
}
