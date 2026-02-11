package proxy

import (
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
)

// maxRequestBodySize limits incoming request bodies to 10 MB.
const maxRequestBodySize = 10 * 1024 * 1024

var (
	proxyRequestsTotal = promauto.NewCounterVec(prometheus.CounterOpts{
		Name: "sentinel_proxy_requests_total",
		Help: "Total proxy requests by provider and status",
	}, []string{"provider", "status"})

	proxyLatency = promauto.NewHistogramVec(prometheus.HistogramOpts{
		Name:    "sentinel_proxy_latency_seconds",
		Help:    "Proxy request latency by provider",
		Buckets: prometheus.DefBuckets,
	}, []string{"provider"})
)

// Handler is the HTTP proxy handler for LLM requests.
type Handler struct {
	registry *Registry
	logger   *slog.Logger
}

// NewHandler creates a new proxy handler.
func NewHandler(registry *Registry, logger *slog.Logger) *Handler {
	return &Handler{registry: registry, logger: logger}
}

// ServeHTTP handles proxy requests at POST /v1/chat/completions.
func (h *Handler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}

	defer func() { _ = r.Body.Close() }()

	limited := io.LimitReader(r.Body, maxRequestBodySize+1)
	body, err := io.ReadAll(limited)
	if err != nil {
		h.logger.Error("failed to read request body", "error", err)
		http.Error(w, "failed to read request body", http.StatusBadRequest)
		return
	}
	if len(body) > maxRequestBodySize {
		h.logger.Warn("request body too large", "size", len(body))
		http.Error(w, "request body too large", http.StatusRequestEntityTooLarge)
		return
	}

	var req LLMRequest
	if err := json.Unmarshal(body, &req); err != nil {
		h.logger.Error("failed to decode request", "error", err)
		http.Error(w, "invalid request body", http.StatusBadRequest)
		return
	}

	provider, err := h.registry.Primary()
	if err != nil {
		h.logger.Error("no provider available", "error", err)
		http.Error(w, "no provider available", http.StatusServiceUnavailable)
		return
	}

	providerName := provider.Name()
	start := time.Now()

	resp, err := provider.Send(r.Context(), &req)
	duration := time.Since(start)

	proxyLatency.WithLabelValues(providerName).Observe(duration.Seconds())

	if err != nil {
		proxyRequestsTotal.WithLabelValues(providerName, "error").Inc()
		h.logger.Error("provider request failed",
			"provider", providerName,
			"duration", duration,
			"error", err,
		)
		http.Error(w, "provider request failed", http.StatusBadGateway)
		return
	}

	proxyRequestsTotal.WithLabelValues(providerName, "ok").Inc()
	h.logger.Info("proxy request completed",
		"provider", providerName,
		"duration", duration,
		"tokens", resp.TokensUsed,
	)

	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(resp); err != nil {
		h.logger.Error("failed to encode response", "error", err)
	}
}
