package guardrails

import (
	"encoding/json"
	"net/http"
)

// Handler exposes guardrails status via HTTP.
type Handler struct {
	enforcer *Enforcer
}

// NewHandler creates an HTTP handler for guardrails status.
func NewHandler(enforcer *Enforcer) *Handler {
	return &Handler{enforcer: enforcer}
}

// RegisterRoutes registers guardrails endpoints on the given mux.
func (h *Handler) RegisterRoutes(mux *http.ServeMux) {
	mux.HandleFunc("GET /api/guardrails/status", h.handleStatus)
}

func (h *Handler) handleStatus(w http.ResponseWriter, _ *http.Request) {
	status := h.enforcer.Status()
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(status)
}
