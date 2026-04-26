package api

import (
	"bytes"
	"encoding/json"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/silentspike/project-sentinel/services/sentinel-judge/internal/config"
	"github.com/silentspike/project-sentinel/services/sentinel-judge/internal/service"
)

func newTestHandler(t *testing.T) *Handler {
	t.Helper()
	cfg := &config.Config{
		Thresholds: config.ThresholdConfig{
			DriftAlertSeverity:   "moderate",
			QualityAlertMinScore: 2,
			FatigueAlertMinScore: 0.6,
		},
	}
	batch := service.NewBatchHandler(nil, cfg, slog.Default())
	return NewHandler(batch, slog.Default())
}

func TestHealthEndpoint(t *testing.T) {
	h := newTestHandler(t)
	mux := http.NewServeMux()
	h.RegisterRoutes(mux)

	req := httptest.NewRequest(http.MethodGet, "/health", nil)
	rec := httptest.NewRecorder()
	mux.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Errorf("health status = %d, want 200", rec.Code)
	}

	var body map[string]string
	json.NewDecoder(rec.Body).Decode(&body)
	if body["status"] != "ok" {
		t.Errorf("health status = %q, want ok", body["status"])
	}
}

func TestReadyEndpointNotReady(t *testing.T) {
	h := newTestHandler(t)
	mux := http.NewServeMux()
	h.RegisterRoutes(mux)

	req := httptest.NewRequest(http.MethodGet, "/ready", nil)
	rec := httptest.NewRecorder()
	mux.ServeHTTP(rec, req)

	if rec.Code != http.StatusServiceUnavailable {
		t.Errorf("ready status = %d, want 503", rec.Code)
	}
}

func TestReadyEndpointReady(t *testing.T) {
	h := newTestHandler(t)
	h.SetReady(true)
	mux := http.NewServeMux()
	h.RegisterRoutes(mux)

	req := httptest.NewRequest(http.MethodGet, "/ready", nil)
	rec := httptest.NewRecorder()
	mux.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Errorf("ready status = %d, want 200", rec.Code)
	}
}

func TestAnalyzeEndpoint(t *testing.T) {
	h := newTestHandler(t)
	mux := http.NewServeMux()
	h.RegisterRoutes(mux)

	body := service.BatchRequest{
		AgentID:   "AGENT-07",
		AgentRole: "designer",
		Messages: []string{
			"Das sieht gut aus.",
			"Ich arbeite daran.",
			"Fertig.",
		},
		AnalysisTypes: []string{"drift", "quality", "fatigue"},
	}
	jsonBody, _ := json.Marshal(body)

	req := httptest.NewRequest(http.MethodPost, "/api/v1/analyze", bytes.NewReader(jsonBody))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	mux.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Errorf("analyze status = %d, want 200", rec.Code)
	}

	var resp service.BatchResponse
	if err := json.NewDecoder(rec.Body).Decode(&resp); err != nil {
		t.Fatalf("decode response: %v", err)
	}
	if resp.AgentID != "AGENT-07" {
		t.Errorf("agent_id = %q, want AGENT-07", resp.AgentID)
	}
	if resp.Drift == nil {
		t.Error("expected drift result")
	}
	if resp.Quality == nil {
		t.Error("expected quality result")
	}
	if resp.Fatigue == nil {
		t.Error("expected fatigue result")
	}
}

func TestAnalyzeEndpointMissingAgent(t *testing.T) {
	h := newTestHandler(t)
	mux := http.NewServeMux()
	h.RegisterRoutes(mux)

	body := `{"messages":["hello"],"analysis_types":["drift"]}`
	req := httptest.NewRequest(http.MethodPost, "/api/v1/analyze", bytes.NewBufferString(body))
	rec := httptest.NewRecorder()
	mux.ServeHTTP(rec, req)

	if rec.Code != http.StatusBadRequest {
		t.Errorf("status = %d, want 400", rec.Code)
	}
}

func TestAnalyzeEndpointMissingMessages(t *testing.T) {
	h := newTestHandler(t)
	mux := http.NewServeMux()
	h.RegisterRoutes(mux)

	body := `{"agent_id":"AGENT-01","analysis_types":["drift"]}`
	req := httptest.NewRequest(http.MethodPost, "/api/v1/analyze", bytes.NewBufferString(body))
	rec := httptest.NewRecorder()
	mux.ServeHTTP(rec, req)

	if rec.Code != http.StatusBadRequest {
		t.Errorf("status = %d, want 400", rec.Code)
	}
}
