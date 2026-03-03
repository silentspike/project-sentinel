package proxy

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"os"
	"strconv"
	"sync"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/capability"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/compiler"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/detection"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/extraction"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/guardrails"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/mapping"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/normalizer"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/resilience"
	"github.com/obtFusi/project-sentinel/pkg/sentinel-go/eventstore"
)

// defaultProviderDeadline ist die maximale Wartezeit pro LLM-Call (ENV-konfigurierbar).
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

	breakerTripsTotal = promauto.NewCounterVec(prometheus.CounterOpts{
		Name: "sentinel_breaker_trips_total",
		Help: "Total circuit breaker trips to open state by provider",
	}, []string{"provider"})
)

// PipelineConfig haelt alle Abhaengigkeiten fuer den PipelineHandler.
type PipelineConfig struct {
	Registry         *Registry
	Config           *control.Config
	Compiler         *compiler.Compiler
	Normalizer       *normalizer.Normalizer
	Extractor        *extraction.Extractor
	Capabilities     *capability.ProviderCapabilities
	Logger           *slog.Logger
	BreakerCfg       BreakerConfig
	EventStore       *eventstore.Store       // optional: nil disables event persistence
	Guardrails       *guardrails.Enforcer    // optional: nil disables guardrails
	InFlight         *resilience.InFlightMap // optional: nil disables query tracking
	ProviderDeadline time.Duration           // 0 = defaultProviderDeadline (20s)
}

// ProviderDeadlineFromEnv liest die Provider-Deadline aus ENV.
// Range: 10-30s, Default: 20s.
func ProviderDeadlineFromEnv() time.Duration {
	v := os.Getenv("SENTINEL_CORTEX_PROVIDER_DEADLINE_SECONDS")
	if v == "" {
		return defaultProviderDeadline
	}
	n, err := strconv.Atoi(v)
	if err != nil || n < 10 || n > 30 {
		return defaultProviderDeadline
	}
	return time.Duration(n) * time.Second
}

// PipelineHandler orchestriert die 7-Step LLM-Pipeline.
type PipelineHandler struct {
	registry         *Registry
	config           *control.Config
	compiler         *compiler.Compiler
	norm             *normalizer.Normalizer
	ext              *extraction.Extractor
	caps             *capability.ProviderCapabilities
	logger           *slog.Logger
	eventStore       *eventstore.Store
	guardrails       *guardrails.Enforcer
	inflight         *resilience.InFlightMap
	providerDeadline time.Duration

	breakerMu  sync.RWMutex
	breakers   map[string]*CircuitBreaker
	breakerCfg BreakerConfig
}

// PipelineResponse ist die erweiterte Antwort mit extrahierten Aktionen.
type PipelineResponse struct {
	Content      string                       `json:"content"`
	Model        string                       `json:"model"`
	Provider     string                       `json:"provider"`
	TokensUsed   int                          `json:"tokens_used"`
	FinishReason string                       `json:"finish_reason"`
	Actions      []extraction.ExtractedAction `json:"actions,omitempty"`
	RequestID    string                       `json:"request_id"`
}

// NewPipelineHandler erstellt den Pipeline-Handler.
func NewPipelineHandler(cfg PipelineConfig) *PipelineHandler {
	deadline := cfg.ProviderDeadline
	if deadline == 0 {
		deadline = defaultProviderDeadline
	}
	return &PipelineHandler{
		registry:         cfg.Registry,
		config:           cfg.Config,
		compiler:         cfg.Compiler,
		norm:             cfg.Normalizer,
		ext:              cfg.Extractor,
		caps:             cfg.Capabilities,
		logger:           cfg.Logger,
		eventStore:       cfg.EventStore,
		guardrails:       cfg.Guardrails,
		inflight:         cfg.InFlight,
		providerDeadline: deadline,
		breakers:         make(map[string]*CircuitBreaker),
		breakerCfg:       cfg.BreakerCfg,
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

// parseRequest reads, validates and decodes the incoming LLM request body.
// Returns the decoded request and a request ID, or writes an HTTP error and returns false.
func (ph *PipelineHandler) parseRequest(w http.ResponseWriter, r *http.Request) (LLMRequest, string, bool) {
	// Request-ID fuer Traceability + Idempotenz
	requestID := r.Header.Get("X-Request-ID")
	if requestID == "" {
		requestID = eventstore.GenerateUUID()
	}

	limited := io.LimitReader(r.Body, maxRequestBodySize+1)
	body, err := io.ReadAll(limited)
	if err != nil {
		ph.logger.Error("failed to read request body", "error", err)
		http.Error(w, "failed to read request body", http.StatusBadRequest)
		return LLMRequest{}, "", false
	}
	if len(body) > maxRequestBodySize {
		ph.logger.Warn("request body too large", "size", len(body))
		http.Error(w, "request body too large", http.StatusRequestEntityTooLarge)
		return LLMRequest{}, "", false
	}

	var req LLMRequest
	if err := json.Unmarshal(body, &req); err != nil {
		ph.logger.Error("failed to decode request", "error", err)
		http.Error(w, "invalid request body", http.StatusBadRequest)
		return LLMRequest{}, "", false
	}

	return req, requestID, true
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
	req, requestID, ok := ph.parseRequest(w, r)
	if !ok {
		return
	}

	// --- Step 1: Config-Snapshot ---
	snap := ph.config.Get()

	// --- Step 2: Provider bestimmen (Runtime-switchable via Control Plane) ---
	// Per-Agent override check: daemon sends agent_id (numeric) or agent_name
	resolvedProviderName := snap.PrimaryProvider
	if agentID := req.Metadata["agent_id"]; agentID != "" {
		// Convert numeric agent_id (e.g. "8") to AGENT-XX format for override lookup
		if n, err := strconv.Atoi(agentID); err == nil {
			canonicalID := fmt.Sprintf("AGENT-%02d", n)
			if override, ok := snap.AgentOverrides[canonicalID]; ok {
				resolvedProviderName = override
				ph.logger.Info("per-agent provider override active", "agent", canonicalID, "provider", override)
			}
		}
	}
	if agentName := req.Metadata["agent_name"]; agentName != "" {
		if override, ok := snap.AgentOverrides[agentName]; ok {
			resolvedProviderName = override
			ph.logger.Info("per-agent provider override active", "agent", agentName, "provider", override)
		}
	}
	provider, providerName := ph.resolveProvider(resolvedProviderName)
	if provider == nil {
		ph.logger.Error("no provider available", "requested", resolvedProviderName)
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

	// --- Step 3b: Guardrails Check ---
	var guardrailsRejected bool
	provider, providerName, guardrailsRejected = ph.applyGuardrails(w, &req, snap.MaxTokens, provider, providerName)
	if guardrailsRejected {
		return
	}

	// --- Step 4: Config-Werte anwenden ---
	req.Temperature = snap.Temperature
	if snap.MaxTokens > 0 {
		req.MaxTokens = snap.MaxTokens
	}

	// --- Step 5: Perception Injection (3-Source Assembly) ---
	agentName := req.Metadata["agent_name"]
	if agentName == "" {
		// Daemon sends "agent_id" (numeric, e.g. "8") — convert to AGENT-XX format.
		if id := req.Metadata["agent_id"]; id != "" {
			n, _ := strconv.Atoi(id)
			agentName = fmt.Sprintf("AGENT-%02d", n)
		}
	}
	agentRole := req.Metadata["agent_role"]
	ph.injectPerception(&req, agentName, agentRole, providerName)

	// --- Step 6: Provider.Send() mit Deadline ---
	ctx, cancel := context.WithTimeout(r.Context(), ph.providerDeadline)
	defer cancel()

	ph.trackInflight(requestID, req.Metadata)

	prevState := breaker.State()
	resp, err := provider.Send(ctx, &req)
	breaker.Record(err)

	// Track breaker trips (Closed/HalfOpen → Open)
	if newState := breaker.State(); newState == "open" && prevState != "open" {
		breakerTripsTotal.WithLabelValues(providerName).Inc()
	}

	if err != nil {
		ph.cancelInflight(requestID)
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

	// --- Step 6b: InFlight Accept (stale/expired check) ---
	if !ph.acceptInflight(requestID, req.Metadata) {
		pipelineRequestsTotal.WithLabelValues(providerName, "stale").Inc()
		pipelineLatency.WithLabelValues(providerName).Observe(time.Since(start).Seconds())
		ph.logger.Warn("query response rejected (stale/expired)",
			"request_id", requestID,
			"provider", providerName,
		)
		http.Error(w, "query expired or stale response", http.StatusGatewayTimeout)
		return
	}

	// --- Step 6c: Guardrails Record ---
	if ph.guardrails != nil {
		ph.guardrails.Record(providerName, resp.InputTokens, resp.OutputTokens)
	}

	// --- Step 7: Fourth-Wall Detection ---
	content := resp.Content
	if agentName != "" {
		content = ph.fourthWallCheck(ctx, content, agentName, agentRole, provider, &req)
	}

	// --- Step 8: Action Extraction ---
	actions := ph.ext.Extract(content)

	// --- Step 8b: Persist extracted actions as events (AC-5) ---
	ph.persistActions(actions, agentName, requestID, &req)

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
		RequestID:    requestID,
	}

	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(pipelineResp); err != nil {
		ph.logger.Error("failed to encode response", "error", err)
	}
}

// buildSystemPrompt assembles the system prompt via 3-source assembly or fallback.
func (ph *PipelineHandler) buildSystemPrompt(req *LLMRequest, agentName, agentRole, providerName string) string {
	perception := req.Metadata["perception"]
	agentIDStr := req.Metadata["agent_id"]
	agentID, parseErr := strconv.Atoi(agentIDStr)

	if parseErr == nil && agentID > 0 {
		evolution := compiler.EvolutionFromMetadata(req.Metadata)
		compiled, compileErr := ph.compiler.CompileFromSources(agentID, providerName, evolution, perception)
		if compileErr != nil {
			ph.logger.Warn("3-source assembly failed, using fallback",
				"agent_id", agentID,
				"error", compileErr,
			)
			modelKey := ph.modelKey(providerName)
			return ph.compiler.Compile(modelKey, agentName, agentRole, perception)
		}
		if !evolution.IsEmpty() {
			ph.logger.Info("evolution injected",
				"agent_id", agentID,
				"has_voice", evolution.VoiceStyle != "",
				"has_notes", evolution.BehavioralNotes != "",
				"has_narrative", evolution.NarrativeSummary != "",
			)
		}
		return compiled
	}

	if perception != "" {
		modelKey := ph.modelKey(providerName)
		return ph.compiler.Compile(modelKey, agentName, agentRole, perception)
	}
	return ""
}

// applyGuardrails runs rate-limit and budget checks. Returns the (possibly replaced)
// provider/name and whether the request was rejected (HTTP 429 already sent).
// Safe to call when guardrails is nil (returns inputs unchanged).
func (ph *PipelineHandler) applyGuardrails(w http.ResponseWriter, req *LLMRequest, maxTokens int, provider Provider, providerName string) (Provider, string, bool) {
	if ph.guardrails == nil {
		return provider, providerName, false
	}
	agentID := req.Metadata["agent_id"]
	result := ph.guardrails.Check(agentID, maxTokens)
	if result.RateLimited {
		http.Error(w, "rate limit exceeded", http.StatusTooManyRequests)
		return nil, "", true
	}
	if result.BudgetExhausted && result.FallbackProvider != "" {
		if fbProvider, ok := ph.registry.Get(result.FallbackProvider); ok {
			return fbProvider, result.FallbackProvider, false
		}
	}
	return provider, providerName, false
}

// injectPerception assembles and prepends the system prompt when an agent name is present.
func (ph *PipelineHandler) injectPerception(req *LLMRequest, agentName, agentRole, providerName string) {
	if agentName == "" {
		return
	}
	if systemPrompt := ph.buildSystemPrompt(req, agentName, agentRole, providerName); systemPrompt != "" {
		req.Messages = prependSystemMessage(req.Messages, systemPrompt)
	}
}

// trackInflight records a query in the InFlightMap if enabled. No-op when inflight is nil.
func (ph *PipelineHandler) trackInflight(requestID string, metadata map[string]string) {
	if ph.inflight != nil {
		ph.inflight.Track(requestID, parseTick(metadata))
	}
}

// cancelInflight cancels an in-flight query on provider error. No-op when inflight is nil.
func (ph *PipelineHandler) cancelInflight(requestID string) {
	if ph.inflight != nil {
		ph.inflight.Cancel(requestID)
	}
}

// acceptInflight accepts a query response, returning false if the response is stale/expired.
// Returns true (accept) when inflight tracking is disabled (nil).
func (ph *PipelineHandler) acceptInflight(requestID string, metadata map[string]string) bool {
	if ph.inflight == nil {
		return true
	}
	return ph.inflight.Accept(requestID, parseTick(metadata))
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

// persistActions writes extracted actions as domain events to the event store (AC-5).
func (ph *PipelineHandler) persistActions(actions []extraction.ExtractedAction, agentName, requestID string, req *LLMRequest) {
	if ph.eventStore == nil || len(actions) == 0 || agentName == "" {
		return
	}
	meta := mapping.ActionMeta{
		AgentName: agentName,
		RequestID: requestID,
		Tick:      parseTick(req.Metadata),
	}
	domainEvents := mapping.MapActions(actions, meta)
	for _, evt := range domainEvents {
		topic := fmt.Sprintf("sentinel/cortex/events/%s", agentName)
		if err := ph.eventStore.AppendWithOutbox(evt, topic); err != nil {
			ph.logger.Warn("event store write failed",
				"error", err,
				"request_id", requestID,
				"agent", agentName,
			)
		}
	}
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

// parseTick extrahiert den Tick-Wert aus Request-Metadata.
func parseTick(metadata map[string]string) int64 {
	if v, ok := metadata["tick"]; ok {
		if tick, err := strconv.ParseInt(v, 10, 64); err == nil {
			return tick
		}
	}
	return 0
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
