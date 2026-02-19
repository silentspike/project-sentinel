// Package api provides HTTP handlers for the sentinel-judge service.
package api

import (
	"encoding/json"
	"fmt"
	"log/slog"
	"net/http"

	"github.com/prometheus/client_golang/prometheus/promhttp"

	"github.com/obtFusi/project-sentinel/services/sentinel-judge/internal/service"
)

// Handler holds the HTTP endpoints for the judge service.
type Handler struct {
	batch  *service.BatchHandler
	logger *slog.Logger
	ready  bool
}

// NewHandler creates HTTP handlers for the judge service.
func NewHandler(batch *service.BatchHandler, logger *slog.Logger) *Handler {
	return &Handler{
		batch:  batch,
		logger: logger,
	}
}

// SetReady marks the service as ready (NATS consumer connected).
func (h *Handler) SetReady(ready bool) {
	h.ready = ready
}

// RegisterRoutes registers all HTTP endpoints on the given mux.
func (h *Handler) RegisterRoutes(mux *http.ServeMux) {
	mux.HandleFunc("GET /health", h.handleHealth)
	mux.HandleFunc("GET /ready", h.handleReady)
	mux.Handle("GET /metrics", promhttp.Handler())
	mux.HandleFunc("POST /api/v1/analyze", h.handleAnalyze)
}

func (h *Handler) handleHealth(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	fmt.Fprint(w, `{"status":"ok","service":"sentinel-judge"}`)
}

func (h *Handler) handleReady(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	if h.ready {
		fmt.Fprint(w, `{"ready":true}`)
	} else {
		w.WriteHeader(http.StatusServiceUnavailable)
		fmt.Fprint(w, `{"ready":false}`)
	}
}

func (h *Handler) handleAnalyze(w http.ResponseWriter, r *http.Request) {
	var req service.BatchRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, "invalid request body", http.StatusBadRequest)
		return
	}

	if req.AgentID == "" {
		http.Error(w, "agent_id is required", http.StatusBadRequest)
		return
	}
	if len(req.Messages) == 0 {
		http.Error(w, "messages are required", http.StatusBadRequest)
		return
	}

	resp, err := h.batch.Analyze(r.Context(), req)
	if err != nil {
		h.logger.Error("batch analysis failed", "agent", req.AgentID, "error", err)
		http.Error(w, "analysis failed", http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(resp)
}
