package proxy

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"sync"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/capability"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/compiler"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/detection"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/extraction"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/normalizer"
)

// Provider-Deadline: maximale Wartezeit pro LLM-Call.
const defaultProviderDeadline = 20 * time.Second

// maxRegenAttempts limitiert Fourth-Wall Re-Generierungs-Versuche.
const maxRegenAttempts = 2

var (
	pipelineRequestsTotal = promauto.NewCounterVec(prometheus.CounterOpts{
		Name: "sentinel_pipeline_requests_total",
		Help: "Total pipeline requests by provider and status",
	}, []string{"provider", "status"})

	pipelineLatency = promauto.NewHistogramVec(prometheus.HistogramOpts{
		Name:    "sentinel_pipeline_latency_seconds",
		Help:    "Full pipeline latency by provider",
		Buckets: prometheus.DefBuckets,
	}, []string{"provider"})

	breakerStateGauge = promauto.NewGaugeVec(prometheus.GaugeOpts{
		Name: "sentinel_circuit_breaker_state",
		Help: "Circuit breaker state (0=closed, 1=open, 2=half-open)",
	}, []string{"provider"})
)

// PipelineConfig haelt alle Abhaengigkeiten fuer den PipelineHandler.
type PipelineConfig struct {
	Registry     *Registry
	Config       *control.Config
	Compiler     *compiler.Compiler
	Normalizer   *normalizer.Normalizer
	Extractor    *extraction.Extractor
	Capabilities *capability.ProviderCapabilities
	Logger       *slog.Logger
	BreakerCfg   BreakerConfig
}

// PipelineHandler orchestriert die 7-Step LLM-Pipeline.
type PipelineHandler struct {
	registry *Registry
	config   *control.Config
	compiler *compiler.Compiler
	norm     *normalizer.Normalizer
	ext      *extraction.Extractor
	caps     *capability.ProviderCapabilities
	logger   *slog.Logger

	breakerMu sync.RWMutex
	breakers  map[string]*CircuitBreaker
	breakerCfg BreakerConfig
}

// PipelineResponse ist die erweiterte Antwort mit extrahierten Aktionen.
type PipelineResponse struct {
	Content      string                     `json:"content"`
	Model        string                     `json:"model"`
	Provider     string                     `json:"provider"`
	TokensUsed   int                        `json:"tokens_used"`
	FinishReason string                     `json:"finish_reason"`
	Actions      []extraction.ExtractedAction `json:"actions,omitempty"`
}

// NewPipelineHandler erstellt den Pipeline-Handler.
func NewPipelineHandler(cfg PipelineConfig) *PipelineHandler {
	return &PipelineHandler{
		registry:   cfg.Registry,
		config:     cfg.Config,
		compiler:   cfg.Compiler,
		norm:       cfg.Normalizer,
		ext:        cfg.Extractor,
		caps:       cfg.Capabilities,
		logger:     cfg.Logger,
		breakers:   make(map[string]*CircuitBreaker),
		breakerCfg: cfg.BreakerCfg,
	}
}

// getBreaker gibt den CircuitBreaker fuer einen Provider zurueck (lazy init).
func (ph *PipelineHandler) getBreaker(name string) *CircuitBreaker {
	ph.breakerMu.RLock()
	cb, ok := ph.breakers[name]
	ph.breakerMu.RUnlock()
	if ok {
		return cb
	}

	ph.breakerMu.Lock()
	defer ph.breakerMu.Unlock()
	// Double-check
	if cb, ok = ph.breakers[name]; ok {
		return cb
	}
	cb = NewCircuitBreaker(ph.breakerCfg)
	ph.breakers[name] = cb
	return cb
}

// ServeHTTP implementiert die vollstaendige 7-Step Pipeline.
func (ph *PipelineHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}

	defer func() { _ = r.Body.Close() }()
	start := time.Now()

	// --- Step 0: Request lesen + validieren ---
	limited := io.LimitReader(r.Body, maxRequestBodySize+1)
	body, err := io.ReadAll(limited)
	if err != nil {
		ph.logger.Error("failed to read request body", "error", err)
		http.Error(w, "failed to read request body", http.StatusBadRequest)
		return
	}
	if len(body) > maxRequestBodySize {
		ph.logger.Warn("request body too large", "size", len(body))
		http.Error(w, "request body too large", http.StatusRequestEntityTooLarge)
		return
	}

	var req LLMRequest
	if err := json.Unmarshal(body, &req); err != nil {
		ph.logger.Error("failed to decode request", "error", err)
		http.Error(w, "invalid request body", http.StatusBadRequest)
		return
	}

	// --- Step 1: Config-Snapshot ---
	snap := ph.config.Get()

	// --- Step 2: Provider bestimmen (Runtime-switchable via Control Plane) ---
	provider, providerName := ph.resolveProvider(snap.PrimaryProvider)
	if provider == nil {
		ph.logger.Error("no provider available", "requested", snap.PrimaryProvider)
		http.Error(w, "no provider available", http.StatusServiceUnavailable)
		return
	}

	// --- Step 3: Circuit Breaker ---
	breaker := ph.getBreaker(providerName)
	if !breaker.Allow() {
		// Failover: versuche anderen Provider
		provider, providerName = ph.failover(providerName)
		if provider == nil {
			ph.logger.Warn("all providers circuit-broken")
			http.Error(w, "service unavailable (circuit breaker open)", http.StatusServiceUnavailable)
			return
		}
		breaker = ph.getBreaker(providerName)
		if !breaker.Allow() {
			http.Error(w, "service unavailable (all circuit breakers open)", http.StatusServiceUnavailable)
			return
		}
	}

	ph.updateBreakerGauge(providerName, breaker)

	// --- Step 4: Config-Werte anwenden ---
	req.Temperature = snap.Temperature
	if snap.MaxTokens > 0 {
		req.MaxTokens = snap.MaxTokens
	}

	// --- Step 5: Perception Injection ---
	agentName := req.Metadata["agent_name"]
	agentRole := req.Metadata["agent_role"]
	perception := req.Metadata["perception"]

	if perception != "" && agentName != "" {
		modelKey := ph.modelKey(providerName)
		systemPrompt := ph.compiler.Compile(modelKey, agentName, agentRole, perception)
		req.Messages = prependSystemMessage(req.Messages, systemPrompt)
	}

	// --- Step 6: Provider.Send() mit Deadline ---
	ctx, cancel := context.WithTimeout(r.Context(), defaultProviderDeadline)
	defer cancel()

	resp, err := provider.Send(ctx, &req)
	breaker.Record(err)

	if err != nil {
		duration := time.Since(start)
		pipelineRequestsTotal.WithLabelValues(providerName, "error").Inc()
		pipelineLatency.WithLabelValues(providerName).Observe(duration.Seconds())
		ph.logger.Error("provider request failed",
			"provider", providerName,
			"duration", duration,
			"error", err,
		)
		http.Error(w, "provider request failed", http.StatusBadGateway)
		return
	}

	// --- Step 7: Fourth-Wall Detection ---
	content := resp.Content
	if agentName != "" {
		content = ph.fourthWallCheck(ctx, content, agentName, agentRole, provider, &req)
	}

	// --- Step 8: Action Extraction ---
	actions := ph.ext.Extract(content)

	// --- Step 9: Response ---
	duration := time.Since(start)
	pipelineRequestsTotal.WithLabelValues(providerName, "ok").Inc()
	pipelineLatency.WithLabelValues(providerName).Observe(duration.Seconds())

	ph.logger.Info("pipeline request completed",
		"provider", providerName,
		"duration", duration,
		"tokens", resp.TokensUsed,
		"actions", len(actions),
	)

	pipelineResp := PipelineResponse{
		Content:      content,
		Model:        resp.Model,
		Provider:     providerName,
		TokensUsed:   resp.TokensUsed,
		FinishReason: resp.FinishReason,
		Actions:      actions,
	}

	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(pipelineResp); err != nil {
		ph.logger.Error("failed to encode response", "error", err)
	}
}

// resolveProvider holt den Provider nach Name, mit Fallback auf Primary.
func (ph *PipelineHandler) resolveProvider(name string) (Provider, string) {
	p, ok := ph.registry.Get(name)
	if ok {
		return p, name
	}
	// Fallback auf den erstregistrierten Primary
	p, err := ph.registry.Primary()
	if err != nil {
		return nil, ""
	}
	return p, p.Name()
}

// failover versucht einen alternativen Provider zu finden.
func (ph *PipelineHandler) failover(exclude string) (Provider, string) {
	for _, name := range ph.registry.List() {
		if name == exclude {
			continue
		}
		p, ok := ph.registry.Get(name)
		if ok {
			return p, name
		}
	}
	return nil, ""
}

// fourthWallCheck fuehrt die 2-Stage Detection durch und re-generiert bei Bedarf.
func (ph *PipelineHandler) fourthWallCheck(ctx context.Context, content string, agentName, agentRole string, provider Provider, req *LLMRequest) string {
	judgeAdapter := NewJudgeProviderAdapter(provider)

	for attempt := 0; attempt < maxRegenAttempts; attempt++ {
		regenStart := time.Now()
		result, err := detection.HandleFourthWall(ctx, content, agentName, agentRole, judgeAdapter)
		if err != nil {
			ph.logger.Warn("fourth-wall detection error", "error", err)
			return content
		}

		if result.Clean {
			return content
		}

		// Re-generate mit Correction + niedrigerer Temperature
		ph.logger.Info("fourth-wall break detected",
			"pattern", result.Pattern,
			"agent", agentName,
			"attempt", attempt+1,
		)

		regenReq := &LLMRequest{
			Messages:    appendCorrectionMessage(req.Messages, result.Correction),
			Temperature: result.RetryWith,
			MaxTokens:   req.MaxTokens,
		}

		resp, sendErr := provider.Send(ctx, regenReq)
		detection.RegenLatency().Observe(time.Since(regenStart).Seconds())

		if sendErr != nil {
			ph.logger.Error("re-generation failed", "error", sendErr)
			return content
		}

		content = resp.Content
	}
	return content
}

// modelKey mappt Provider-Namen auf Compiler-Config-Keys.
func (ph *PipelineHandler) modelKey(providerName string) string {
	switch providerName {
	case "claude":
		return "claude"
	case "ollama":
		return "ollama-7b"
	default:
		return "claude"
	}
}

// prependSystemMessage fuegt eine System-Message am Anfang ein oder ersetzt sie.
func prependSystemMessage(messages []Message, systemPrompt string) []Message {
	sysMsg := Message{Role: "system", Content: systemPrompt}

	if len(messages) > 0 && messages[0].Role == "system" {
		messages[0] = sysMsg
		return messages
	}
	return append([]Message{sysMsg}, messages...)
}

// appendCorrectionMessage haengt eine Correction-Message an.
func appendCorrectionMessage(messages []Message, correction string) []Message {
	result := make([]Message, len(messages), len(messages)+1)
	copy(result, messages)
	return append(result, Message{Role: "system", Content: correction})
}

// updateBreakerGauge setzt die Prometheus-Gauge fuer den Breaker-State.
func (ph *PipelineHandler) updateBreakerGauge(name string, cb *CircuitBreaker) {
	var val float64
	switch cb.State() {
	case "closed":
		val = 0
	case "open":
		val = 1
	case "half-open":
		val = 2
	}
	breakerStateGauge.WithLabelValues(name).Set(val)
}
