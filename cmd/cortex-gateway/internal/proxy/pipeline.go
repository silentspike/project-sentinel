package proxy

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/apicp"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/capability"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/compiler"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/detection"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/extraction"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/guardrails"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/intercept"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/mapping"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/normalizer"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/resilience"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/sequencing"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/synthesis"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/ticksync"
	"github.com/silentspike/project-sentinel/pkg/sentinel-go/eventstore"
	"github.com/silentspike/project-sentinel/pkg/sentinel-go/judge"
)

const (
	// defaultProviderDeadline ist die maximale Wartezeit fuer die echte Provider-Ausfuehrung
	// nach erfolgreichem Queue-Acquire. Claude Code ueber Subscription ist real deutlich
	// langsamer als die alten 20s-Annahmen.
	defaultProviderDeadline = 60 * time.Second
	// defaultInflightDeadline ist die maximale Gesamtlebensdauer eines Requests in der
	// InFlightMap. Dieser Wert muss Queue-Wartezeit + Provider-Ausfuehrung abdecken.
	defaultInflightDeadline = 180 * time.Second
)

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

	personalityGuardDriftTotal = promauto.NewCounterVec(prometheus.CounterOpts{
		Name: "sentinel_personality_guard_drift_total",
		Help: "Personality drift events by agent and severity",
	}, []string{"agent", "severity"})

	qualityGateRegenTotal = promauto.NewCounterVec(prometheus.CounterOpts{
		Name: "sentinel_quality_gate_regen_total",
		Help: "Quality gate re-generation attempts by agent",
	}, []string{"agent"})

	qualityGateScore = promauto.NewHistogramVec(prometheus.HistogramOpts{
		Name:    "sentinel_quality_gate_score",
		Help:    "Quality gate scores by agent",
		Buckets: []float64{1, 2, 3, 4, 5},
	}, []string{"agent"})

	pipelineTokensTotal = promauto.NewCounterVec(prometheus.CounterOpts{
		Name: "sentinel_pipeline_tokens_total",
		Help: "Total tokens consumed per provider and direction (always emitted)",
	}, []string{"provider", "direction"})

	pipelineHierarchyRequestsTotal = promauto.NewCounterVec(prometheus.CounterOpts{
		Name: "sentinel_pipeline_hierarchy_requests_total",
		Help: "Agent-runtime responses by provider, hierarchy tier, effective model, and cost source",
	}, []string{"provider", "hierarchy_tier", "effective_model", "cost_source"})
)

// PipelineConfig haelt alle Abhaengigkeiten fuer den PipelineHandler.
type PipelineConfig struct {
	Registry            *Registry
	Catalog             *ProviderCatalog
	Config              *control.Config
	Compiler            *compiler.Compiler
	Normalizer          *normalizer.Normalizer
	Extractor           *extraction.Extractor
	Capabilities        *capability.ProviderCapabilities
	ActionPolicy        *capability.AgentActionPolicy
	Logger              *slog.Logger
	BreakerCfg          BreakerConfig
	EventStore          *eventstore.Store          // optional: nil disables event persistence
	Guardrails          *guardrails.Enforcer       // optional: nil disables guardrails
	InFlight            *resilience.InFlightMap    // optional: nil disables query tracking
	ProviderDeadline    time.Duration              // 0 = defaultProviderDeadline (60s)
	Drift               *judge.DriftDetector       // optional: nil disables personality guard
	Quality             *judge.QualityScorer       // optional: nil disables quality gate
	Synthesis           *synthesis.Engine          // optional: nil disables synthesis
	Sequencer           *sequencing.Sequencer      // optional: nil disables chat-sequencing
	Observer            *apicp.Observer            // optional: nil disables API-CP learning
	Interceptor         *intercept.Manager         // optional: nil disables manual interception
	ResponseInterceptor *intercept.ResponseManager // optional: nil disables manual response interception
	TickSync            *ticksync.Buffer           // optional: nil disables tick sync
	ResponseLogs        *ResponseLogBuffer         // optional: nil disables response-body ring buffer
}

func durationFromEnvSeconds(primaryKey, legacyKey string, minSeconds, maxSeconds int, fallback time.Duration) time.Duration {
	v := os.Getenv(primaryKey)
	if v == "" && legacyKey != "" {
		v = os.Getenv(legacyKey)
	}
	if v == "" {
		return fallback
	}
	n, err := strconv.Atoi(v)
	if err != nil || n < minSeconds || n > maxSeconds {
		return fallback
	}
	return time.Duration(n) * time.Second
}

// ProviderDeadlineFromEnv liest das Timeout fuer die echte Provider-Ausfuehrung aus ENV.
// Neue ENV: SENTINEL_CORTEX_PROVIDER_TIMEOUT_SECONDS
// Legacy-ENV: SENTINEL_CORTEX_PROVIDER_DEADLINE_SECONDS
// Range: 15-180s, Default: 60s.
func ProviderDeadlineFromEnv() time.Duration {
	return durationFromEnvSeconds(
		"SENTINEL_CORTEX_PROVIDER_TIMEOUT_SECONDS",
		"SENTINEL_CORTEX_PROVIDER_DEADLINE_SECONDS",
		15,
		180,
		defaultProviderDeadline,
	)
}

// InflightDeadlineFromEnv liest die maximale Gesamtlebensdauer eines Requests in der InFlightMap.
// Diese Deadline umfasst Queue-Wartezeit und Provider-Ausfuehrung.
// Range: 30-300s, Default: 180s.
func InflightDeadlineFromEnv() time.Duration {
	return durationFromEnvSeconds(
		"SENTINEL_CORTEX_INFLIGHT_DEADLINE_SECONDS",
		"",
		30,
		300,
		defaultInflightDeadline,
	)
}

// PipelineHandler orchestriert die 7-Step LLM-Pipeline.
type PipelineHandler struct {
	registry            *Registry
	catalog             *ProviderCatalog
	config              *control.Config
	compiler            *compiler.Compiler
	norm                *normalizer.Normalizer
	ext                 *extraction.Extractor
	caps                *capability.ProviderCapabilities
	actionPolicy        *capability.AgentActionPolicy
	logger              *slog.Logger
	eventStore          *eventstore.Store
	guardrails          *guardrails.Enforcer
	inflight            *resilience.InFlightMap
	providerDeadline    time.Duration
	drift               *judge.DriftDetector
	quality             *judge.QualityScorer
	synthesis           *synthesis.Engine
	sequencer           *sequencing.Sequencer
	observer            *apicp.Observer
	interceptor         *intercept.Manager
	responseInterceptor *intercept.ResponseManager
	tickSync            *ticksync.Buffer
	responseLogs        *ResponseLogBuffer

	breakerMu  sync.RWMutex
	breakers   map[string]*CircuitBreaker
	breakerCfg BreakerConfig

	regenMu       sync.Mutex
	regenCooldown map[string]time.Time // agent → last regen time (#240)
}

// BreakerStates gibt den aktuellen State aller bekannten Circuit Breaker zurueck.
func (ph *PipelineHandler) BreakerStates() map[string]string {
	ph.breakerMu.RLock()
	defer ph.breakerMu.RUnlock()

	states := make(map[string]string, len(ph.breakers))
	for name, cb := range ph.breakers {
		states[name] = cb.State()
	}
	return states
}

// PipelineResponse ist die erweiterte Antwort mit extrahierten Aktionen.
type PipelineResponse struct {
	Content       string                       `json:"content"`
	ContentBlocks []json.RawMessage            `json:"-"`
	Model         string                       `json:"model"`
	Provider      string                       `json:"provider"`
	TokensUsed    int                          `json:"tokens_used"`
	InputTokens   int                          `json:"input_tokens,omitempty"`
	OutputTokens  int                          `json:"output_tokens,omitempty"`
	FinishReason  string                       `json:"finish_reason"`
	Actions       []extraction.ExtractedAction `json:"actions,omitempty"`
	RequestID     string                       `json:"request_id"`
	// #429: surfaced into the response log for the Request Inspector.
	Decision   string `json:"decision,omitempty"`
	Rule       string `json:"rule,omitempty"`
	FourthWall string `json:"fourth_wall,omitempty"`
	// #427: cache-aware breakdown + resolved tier + per-call cost, threaded to
	// the daemon so it can emit the AgentLlmUsage event (the daemon does not know
	// the EffectiveModel/cost itself). CostUsd is filled at the response sink.
	CacheRead       int      `json:"cache_read,omitempty"`
	CacheCreation   int      `json:"cache_creation,omitempty"`
	Tier            string   `json:"tier,omitempty"`
	CostUsd         float64  `json:"cost_usd"`
	CostSource      string   `json:"cost_source,omitempty"`
	HierarchyTier   int      `json:"hierarchy_tier,omitempty"`
	EffectiveModel  string   `json:"effective_model,omitempty"`
	ReportedCostUSD *float64 `json:"-"`
}

// NewPipelineHandler erstellt den Pipeline-Handler.
func NewPipelineHandler(cfg PipelineConfig) *PipelineHandler {
	deadline := cfg.ProviderDeadline
	if deadline == 0 {
		deadline = defaultProviderDeadline
	}

	// Pre-initialize token counter for all registered providers so the metric
	// appears in /metrics immediately (not only after the first successful call).
	for _, name := range cfg.Registry.List() {
		pipelineTokensTotal.WithLabelValues(name, "input")
		pipelineTokensTotal.WithLabelValues(name, "output")
	}

	return &PipelineHandler{
		registry:            cfg.Registry,
		catalog:             cfg.Catalog,
		config:              cfg.Config,
		compiler:            cfg.Compiler,
		norm:                cfg.Normalizer,
		ext:                 cfg.Extractor,
		caps:                cfg.Capabilities,
		actionPolicy:        cfg.ActionPolicy,
		logger:              cfg.Logger,
		eventStore:          cfg.EventStore,
		guardrails:          cfg.Guardrails,
		inflight:            cfg.InFlight,
		providerDeadline:    deadline,
		drift:               cfg.Drift,
		quality:             cfg.Quality,
		synthesis:           cfg.Synthesis,
		sequencer:           cfg.Sequencer,
		observer:            cfg.Observer,
		interceptor:         cfg.Interceptor,
		responseInterceptor: cfg.ResponseInterceptor,
		tickSync:            cfg.TickSync,
		responseLogs:        cfg.ResponseLogs,
		breakers:            make(map[string]*CircuitBreaker),
		breakerCfg:          cfg.BreakerCfg,
		regenCooldown:       make(map[string]time.Time),
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
		ph.writePathError(w, r.URL.Path, "failed to read request body", http.StatusBadRequest)
		return LLMRequest{}, "", false
	}
	if len(body) > maxRequestBodySize {
		ph.logger.Warn("request body too large", "size", len(body))
		ph.writePathError(w, r.URL.Path, "request body too large", http.StatusRequestEntityTooLarge)
		return LLMRequest{}, "", false
	}

	var req LLMRequest
	if isAnthropicMessagesPath(r.URL.Path) {
		req, err = decodeAnthropicRequest(body)
		if err != nil {
			ph.logger.Error("failed to decode anthropic request", "error", err)
			ph.writePathError(w, r.URL.Path, "invalid request body", http.StatusBadRequest)
			return LLMRequest{}, "", false
		}
		req.PassthroughHeaders = extractAnthropicPassthroughHeaders(r.Header)
	} else {
		if err := json.Unmarshal(body, &req); err != nil {
			ph.logger.Error("failed to decode request", "error", err)
			ph.writePathError(w, r.URL.Path, "invalid request body", http.StatusBadRequest)
			return LLMRequest{}, "", false
		}
		if req.Format == "" {
			req.Format = RequestFormatInternal
		}
	}
	if req.Metadata == nil {
		req.Metadata = make(map[string]string)
	}

	return req, requestID, true
}

// ServeHTTP implementiert die vollstaendige 7-Step Pipeline.
func (ph *PipelineHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) { //nolint:gocyclo // Pipeline orchestration is genuinely complex
	if r.Method != http.MethodPost {
		ph.writePathError(w, r.URL.Path, "method not allowed", http.StatusMethodNotAllowed)
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
	callerRole, authenticated := callerRoleFromContext(r.Context())
	if !authenticated && strings.HasPrefix(r.URL.Path, "/v1/") {
		// Direct handler composition remains safe: public paths have a fixed
		// non-authoritative class even when the outer middleware is absent.
		callerRole = CallerRoleExternalCompat
		authenticated = true
	}
	if !authenticated {
		ph.writeRequestError(w, &req, "authenticated caller context required", http.StatusUnauthorized)
		return
	}
	var classifyErr error
	req.RequestClass, classifyErr = ClassifyRequest(r.URL.Path, &req, callerRole)
	if classifyErr != nil {
		ph.writeRequestError(w, &req, classifyErr.Error(), http.StatusUnprocessableEntity)
		return
	}
	if req.RequestClass == RequestClassAgentRuntime && req.Stream {
		ph.writeRequestError(w, &req, "agent-runtime wire contract does not support streaming", http.StatusUnprocessableEntity)
		return
	}
	if isLocalLoopActive(snap) {
		req.PreferredProvider = LocalLoopProviderName
	}

	// --- Step 2: Provider bestimmen (Runtime-switchable via Control Plane) ---
	resolvedProviderName := req.PreferredProvider
	if resolvedProviderName == "" {
		resolvedProviderName = ph.resolveAgentProvider(snap, req.Metadata)
	}
	provider, providerName := ph.resolveProvider(resolvedProviderName)
	if provider == nil {
		ph.logger.Error("no provider available", "requested", resolvedProviderName)
		ph.writeRequestError(w, &req, "no provider available", http.StatusServiceUnavailable)
		return
	}

	// --- Step 3: Circuit Breaker (SENTINEL_CORTEX_CB_ENABLED gate, AC-5) ---
	// IM:1 (operator impulse) Requests bypassen den Circuit Breaker.
	// Gaia/Broadcast/Encounter MUESSEN durchkommen — der CB schuetzt vor Provider-Ueberlast,
	// aber Operator-Aktionen haben Vorrang (Enterprise: Operator > Automation).
	isUrgent := strings.Contains(req.Metadata["synth_fp"], "|IM:1") ||
		req.Metadata["is_directly_addressed"] == "true" ||
		req.Metadata["heard"] != ""
	breaker := ph.getBreaker(providerName)
	if ph.breakerCfg.Enabled && !isUrgent && !breaker.Allow() {
		ph.logger.Warn("provider circuit-broken", "provider", providerName)
		ph.updateBreakerGauge(providerName, breaker)
		if reporter, ok := provider.(ProviderStatusReporter); ok {
			if err := reporter.CurrentProviderError(); err != nil {
				var provErr *ProviderError
				if errors.As(err, &provErr) && provErr.StatusCode > 0 {
					errMsg := "provider unavailable"
					switch provErr.StatusCode {
					case http.StatusTooManyRequests:
						errMsg = "provider rate limited"
					case http.StatusServiceUnavailable:
						errMsg = "provider unavailable"
					}
					http.Error(w, errMsg, provErr.StatusCode)
					return
				}
			}
		}
		ph.writeRequestError(w, &req, "service unavailable (circuit breaker open)", http.StatusServiceUnavailable)
		return
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
	ph.injectPerception(&req, agentName, agentRole, providerName, snap)

	// --- Step 7.5: Traffic Control — Synthesis Check ---
	if ph.synthesis != nil && snap.SynthesisEnabled && !isAnthropicStreamingRequest(&req) {
		result := ph.synthesis.Decide(req.Metadata, agentName)
		if result.Decision == synthesis.Synthesize {
			content := result.Content
			if ph.shouldForwardAfterSynthesisFourthWallCheck(r.Context(), content, agentName, agentRole, provider, &req, result.Rule) {
				// Fall through to normal Provider.Send()
			} else {
				content, dropped := ph.applyOutboundInterception(requestID, agentName, "synthesis", &req, content, snap)
				if dropped {
					ph.writePipelineResponse(r.Context(), w, &req, PipelineResponse{
						Content:      "",
						Model:        "sentinel-synth-v1",
						Provider:     "intercept",
						Decision:     "dropped",
						Tier:         "intercept",
						TokensUsed:   0,
						FinishReason: "dropped",
						Actions:      nil,
						RequestID:    requestID,
					})
					return
				}

				if ph.observer != nil && snap.APICPEnabled {
					ph.observer.MarkSynthesisCandidate()
				}
				guardrails.RecordRuntimeSynthesisSavings()

				// For synthesis, rule-provided actions are authoritative because they
				// can encode deterministic targets that plain text extraction would lose.
				actions := buildSynthesisActions(result.Actions, ph.ext, content)
				actions = ph.enforceActionPolicy(actions, agentName, requestID, &req)
				ph.persistActions(actions, agentName, requestID, &req)
				duration := time.Since(start)
				pipelineRequestsTotal.WithLabelValues("synthesis", "ok").Inc()
				pipelineLatency.WithLabelValues("synthesis").Observe(duration.Seconds())
				ph.logger.Info("pipeline request completed",
					"provider", "synthesis",
					"request_class", req.RequestClass,
					"effective_model", "sentinel-synth-v1",
					"policy_source", req.PolicySource,
					"duration", duration,
					"tokens", 0,
					"actions", len(actions),
					"rule", result.Rule,
					"agent_id", req.Metadata["agent_id"],
					"agent_name", agentName,
				)
				ph.writePipelineResponse(r.Context(), w, &req, PipelineResponse{
					Content:      content,
					Model:        "sentinel-synth-v1",
					Provider:     "synthesis",
					Decision:     "synthesize",
					Tier:         "synthesis",
					Rule:         result.Rule,
					FourthWall:   "clean",
					TokensUsed:   0,
					InputTokens:  0,
					OutputTokens: 0,
					FinishReason: "synthetic",
					Actions:      actions,
					RequestID:    requestID,
				})
				return
			}
		}
	}
	if ph.observer != nil && snap.APICPEnabled && !isAnthropicStreamingRequest(&req) {
		fp, ctx, err := synthesis.PrepareInputs(req.Metadata)
		if err == nil && synthesis.CanSynthesize(fp, ctx) {
			agentID := req.Metadata["agent_id"]
			if learned, ok := ph.observer.LearnedPatternFor(agentID, req.Metadata["synth_fp"]); ok {
				if ph.observer.ShouldProbeNext() {
					ph.observer.MarkSynthesisCandidate()
					req.Metadata["apicp_probe_agent_id"] = learned.AgentID
					req.Metadata["apicp_probe_fingerprint"] = learned.Fingerprint
					req.Metadata["apicp_probe_expected_hash"] = strconv.FormatUint(learned.TopHash, 10)
					ph.logger.Info("apicp probe forcing real forward",
						"agent_id", learned.AgentID,
						"fingerprint", learned.Fingerprint,
						"expected_hash", learned.TopHash,
					)
				} else {
					content := learned.Content
					content, dropped := ph.applyOutboundInterception(requestID, agentName, "apicp", &req, content, snap)
					if dropped {
						ph.writePipelineResponse(r.Context(), w, &req, PipelineResponse{
							Content:      "",
							Model:        "sentinel-apicp-v1",
							Provider:     "intercept",
							Decision:     "dropped",
							Tier:         "intercept",
							TokensUsed:   0,
							FinishReason: "dropped",
							Actions:      nil,
							RequestID:    requestID,
						})
						return
					}

					ph.observer.MarkSynthesisCandidate()
					guardrails.RecordRuntimeSynthesisSavings()
					actions := buildSynthesisActions(nil, ph.ext, content)
					actions = ph.enforceActionPolicy(actions, agentName, requestID, &req)
					ph.persistActions(actions, agentName, requestID, &req)
					duration := time.Since(start)
					pipelineRequestsTotal.WithLabelValues("apicp", "ok").Inc()
					pipelineLatency.WithLabelValues("apicp").Observe(duration.Seconds())
					ph.logger.Info("pipeline request completed",
						"provider", "apicp",
						"request_class", req.RequestClass,
						"effective_model", "sentinel-apicp-v1",
						"policy_source", req.PolicySource,
						"duration", duration,
						"tokens", 0,
						"actions", len(actions),
						"fingerprint", learned.Fingerprint,
						"agent_id", req.Metadata["agent_id"],
						"agent_name", agentName,
					)
					ph.writePipelineResponse(r.Context(), w, &req, PipelineResponse{
						Content:      content,
						Model:        "sentinel-apicp-v1",
						Provider:     "apicp",
						Decision:     "apicp",
						Tier:         "apicp",
						Rule:         learned.Fingerprint,
						TokensUsed:   0,
						InputTokens:  0,
						OutputTokens: 0,
						FinishReason: "synthetic",
						Actions:      actions,
						RequestID:    requestID,
					})
					return
				}
			}
		}
	}

	// --- Step 7.6: Chat-Sequencing (P1/P3) ---
	var sequencingRoomID string
	var sequencingP1Active bool
	if intercept.Mode(strings.TrimSpace(snap.InterceptMode)) == intercept.ModeManual || (ph.sequencer != nil && snap.SequencingEnabled) {
		sequencingRoomID = req.Metadata["room_id"]
		sequencingP1Active = req.Metadata["is_directly_addressed"] == "true" && strings.TrimSpace(req.Metadata["heard"]) != ""

		decision := ph.interceptInboundRequest(&req, requestID, agentName, snap)
		switch decision.Action {
		case intercept.RequestModify:
			if len(req.Messages) > 0 {
				req.Messages[len(req.Messages)-1].Content += decision.ContextSuffix
			}
			ph.logger.Info("request modified before provider send",
				"request_id", requestID,
				"reason", decision.Reason,
			)
		case intercept.RequestDrop:
			if req.RequestClass == RequestClassAgentRuntime {
				ph.writePipelineResponse(r.Context(), w, &req, PipelineResponse{
					Model:        "sentinel-intercept-v1",
					Provider:     "intercept",
					Decision:     "dropped",
					Tier:         "intercept",
					FinishReason: "dropped",
					RequestID:    requestID,
				})
			} else {
				w.WriteHeader(http.StatusNoContent)
			}
			return
		}
	}

	var policyResolution ModelPolicyResolution
	var err error
	if ph.catalog != nil {
		policyResolution, err = ph.catalog.ResolvePolicy(providerName, req.RequestClass, req.HierarchyTier, req.Model, snap.AgentRuntimeModelPolicy)
	} else {
		legacyPolicy, legacy := snap.AgentRuntimeModelPolicy.LegacyValue()
		if !legacy {
			err = fmt.Errorf("tiered model policy requires an immutable provider catalog")
		} else {
			policyResolution, err = ResolveModelPolicy(providerName, req.RequestClass, req.Model, legacyPolicy)
		}
	}
	if err != nil {
		ph.logger.Error("model policy rejected",
			"request_id", requestID,
			"request_class", req.RequestClass,
			"provider", providerName,
			"policy", snap.AgentRuntimeModelPolicy,
			"error", err,
		)
		ph.writeRequestError(w, &req, "model policy rejected", http.StatusUnprocessableEntity)
		return
	}
	req.Model = policyResolution.Model
	req.EffectiveModel = policyResolution.Model
	req.PolicySource = policyResolution.Source

	if isAnthropicStreamingRequest(&req) {
		ph.streamAnthropicResponse(r.Context(), w, &req, provider, providerName, breaker, requestID, start)
		return
	}

	// --- Step 8: Provider.Send() ---
	// Queue wait must not consume the provider execution deadline. The wrapped
	// provider applies req.ProviderTimeout only after it has acquired a queue slot.
	ctx := r.Context()
	req.ProviderTimeout = ph.providerDeadline

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
			"request_class", req.RequestClass,
			"effective_model", effectiveModelForLog(&req, ""),
			"policy_source", req.PolicySource,
			"agent_id", req.Metadata["agent_id"],
			"agent_name", agentName,
			"duration", duration,
			"error", err,
		)
		statusCode := http.StatusBadGateway
		errMsg := "provider request failed"
		var provErr *ProviderError
		if errors.As(err, &provErr) {
			if provErr.StatusCode > 0 {
				statusCode = provErr.StatusCode
			}
			switch provErr.StatusCode {
			case http.StatusTooManyRequests:
				errMsg = "provider rate limited"
			case http.StatusServiceUnavailable:
				errMsg = "provider unavailable"
			}
		}
		ph.writeRequestError(w, &req, errMsg, statusCode)
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
		ph.writeRequestError(w, &req, "query expired or stale response", http.StatusGatewayTimeout)
		return
	}

	// --- Step 6c: Token Tracking + Guardrails Record ---
	pipelineTokensTotal.WithLabelValues(providerName, "input").Add(float64(resp.InputTokens))
	pipelineTokensTotal.WithLabelValues(providerName, "output").Add(float64(resp.OutputTokens))
	if ph.guardrails != nil {
		ph.guardrails.Record(providerName, resp.InputTokens, resp.OutputTokens)
	}
	if ph.catalog == nil {
		guardrails.RecordRuntimeForwardCost(providerName, resp.InputTokens, resp.OutputTokens)
	}

	// --- Step 6d: Personality Guard Check ---
	content := resp.Content
	if agentName != "" {
		content = ph.personalityGuardCheck(ctx, content, agentName, provider, &req, snap)
	}

	// --- Step 6e: Quality Gate Check ---
	if agentName != "" {
		content = ph.qualityGateCheck(ctx, content, agentName, provider, &req, snap)
	}

	// --- Step 7: Fourth-Wall Detection ---
	fourthWallVerdict := ""
	if agentName != "" {
		content, fourthWallVerdict = ph.fourthWallCheck(ctx, content, agentName, agentRole, provider, &req)
	}

	content, dropped := ph.applyOutboundInterception(requestID, agentName, providerName, &req, content, snap)
	if dropped {
		ph.writePipelineResponse(r.Context(), w, &req, PipelineResponse{
			Content:         "",
			Model:           resp.Model,
			Provider:        providerName,
			Decision:        "dropped",
			Tier:            "intercept",
			TokensUsed:      resp.TokensUsed,
			InputTokens:     resp.InputTokens,
			OutputTokens:    resp.OutputTokens,
			CacheRead:       resp.CacheRead,
			CacheCreation:   resp.CacheCreation,
			ReportedCostUSD: resp.ReportedCostUSD,
			FinishReason:    "dropped",
			Actions:         nil,
			RequestID:       requestID,
		})
		return
	}

	// --- Step 8: Action Extraction ---
	actions := ph.ext.Extract(content)
	actions = ph.enforceActionPolicy(actions, agentName, requestID, &req)

	// --- Step 8b: Persist extracted actions as events (AC-5) ---
	ph.persistActions(actions, agentName, requestID, &req)

	// --- Step 8c: API-CP Observation (record call for learning) ---
	if ph.observer != nil && snap.APICPEnabled {
		fp := req.Metadata["synth_fp"]
		agentID := req.Metadata["agent_id"]
		if prepFP, prepCtx, err := synthesis.PrepareInputs(req.Metadata); err == nil && synthesis.CanSynthesize(prepFP, prepCtx) {
			signature := apicp.BuildResponseSignature(actions, req.Metadata["room_id"], content)
			ph.observer.Record(fp, agentID, content, false, signature)
		}
		if ev := req.Metadata["evolution_version"]; ev != "" {
			ph.observer.CheckEvolutionDegradation(agentID, ev)
		}
		if expected := req.Metadata["apicp_probe_expected_hash"]; expected != "" {
			if expectedHash, err := strconv.ParseUint(expected, 10, 64); err == nil {
				probeAgent := req.Metadata["apicp_probe_agent_id"]
				if probeAgent == "" {
					probeAgent = agentID
				}
				probeFingerprint := req.Metadata["apicp_probe_fingerprint"]
				if probeFingerprint != "" {
					ph.observer.ApplyProbeResult(probeAgent, probeFingerprint, expectedHash, content)
				}
			}
		}
	}

	// --- Step 8d: Complete P1 for Chat-Sequencing (unblocks waiting P3s) ---
	if ph.sequencer != nil && sequencingRoomID != "" && sequencingP1Active {
		ph.sequencer.CompleteP1(sequencingRoomID, requestID, content)
	}

	// --- Step 9: Response ---
	duration := time.Since(start)
	pipelineRequestsTotal.WithLabelValues(providerName, "ok").Inc()
	pipelineLatency.WithLabelValues(providerName).Observe(duration.Seconds())

	ph.logger.Info("pipeline request completed",
		"provider", providerName,
		"request_class", req.RequestClass,
		"effective_model", effectiveModelForLog(&req, resp.Model),
		"policy_source", req.PolicySource,
		"duration", duration,
		"tokens", resp.TokensUsed,
		"actions", len(actions),
		"agent_id", req.Metadata["agent_id"],
		"agent_name", agentName,
	)

	pipelineResp := PipelineResponse{
		Content:         content,
		ContentBlocks:   pipelineResponseBlocks(content, resp),
		Model:           resp.Model,
		Provider:        providerName,
		Decision:        "forward",
		Tier:            resolveTier(req.EffectiveModel),
		CacheRead:       resp.CacheRead,
		CacheCreation:   resp.CacheCreation,
		FourthWall:      fourthWallVerdict,
		TokensUsed:      resp.TokensUsed,
		InputTokens:     resp.InputTokens,
		OutputTokens:    resp.OutputTokens,
		FinishReason:    resp.FinishReason,
		Actions:         actions,
		RequestID:       requestID,
		ReportedCostUSD: resp.ReportedCostUSD,
	}
	ph.writePipelineResponse(r.Context(), w, &req, pipelineResp)
}

// resolveAgentProvider checks for per-agent provider overrides via Control Plane config.
// Returns the overridden provider name, or the primary provider if no override exists.
func (ph *PipelineHandler) resolveAgentProvider(snap control.ConfigSnapshot, metadata map[string]string) string {
	primary := snap.PrimaryProvider
	if agentID := metadata["agent_id"]; agentID != "" {
		if n, err := strconv.Atoi(agentID); err == nil {
			canonicalID := fmt.Sprintf("AGENT-%02d", n)
			if override, ok := snap.AgentOverrides[canonicalID]; ok {
				ph.logger.Info("per-agent provider override active", "agent", canonicalID, "provider", override)
				return override
			}
		}
	}
	if agentName := metadata["agent_name"]; agentName != "" {
		if override, ok := snap.AgentOverrides[agentName]; ok {
			ph.logger.Info("per-agent provider override active", "agent", agentName, "provider", override)
			return override
		}
	}
	return primary
}

func (ph *PipelineHandler) interceptInboundRequest(req *LLMRequest, requestID, agentName string, snap control.ConfigSnapshot) intercept.RequestDecision {
	roomID := strings.TrimSpace(req.Metadata["room_id"])

	if mode := intercept.Mode(strings.TrimSpace(snap.InterceptMode)); mode == intercept.ModeManual {
		if ph.interceptor == nil {
			return intercept.Forward("manual mode unavailable")
		}
		waitCtx, cancel := context.WithTimeout(context.Background(), time.Duration(snap.P3TimeoutMs)*time.Millisecond)
		defer cancel()

		decision, ok := ph.interceptor.AwaitRequestDecision(waitCtx, intercept.PendingRequest{
			ID:        requestID,
			RoomID:    roomID,
			AgentName: agentName,
			Reason:    "manual_mode",
			CreatedAt: time.Now(),
		})
		if !ok {
			ph.logger.Warn("manual intercept timed out, forwarding request",
				"request_id", requestID,
				"room", roomID,
			)
			return intercept.Forward("manual intercept timeout")
		}
		return decision
	}

	if ph.sequencer == nil || !snap.SequencingEnabled {
		return intercept.Forward("sequencing disabled")
	}
	if roomID == "" {
		return intercept.Forward("missing room_id")
	}

	hasHeard := strings.TrimSpace(req.Metadata["heard"]) != ""
	isP1 := strings.EqualFold(strings.TrimSpace(req.Metadata["is_directly_addressed"]), "true")
	if !hasHeard {
		return intercept.Forward("no heard context")
	}

	if isP1 {
		ph.sequencer.MarkP1Active(roomID, requestID, agentName)
		return intercept.Forward("p1 immediate forward")
	}
	if !ph.sequencer.HasActiveP1(roomID) {
		return intercept.Forward("no active p1")
	}

	p1Content, p1Agent, gotP1 := ph.sequencer.WaitForP1(roomID)
	if !gotP1 || p1Content == "" {
		return intercept.Forward("p1 timeout")
	}

	contextMsg := fmt.Sprintf("\n[KONTEXT] %s hat gerade gesagt: \"%s\" [/KONTEXT]",
		p1Agent, p1Content)
	return intercept.Modify("p3 inject p1 context", contextMsg)
}

func (ph *PipelineHandler) applyOutboundInterception(requestID, agentName, providerName string, req *LLMRequest, content string, snap control.ConfigSnapshot) (string, bool) {
	if intercept.Mode(strings.TrimSpace(snap.InterceptMode)) != intercept.ModeManual || ph.responseInterceptor == nil {
		return content, false
	}

	waitCtx, cancel := context.WithTimeout(context.Background(), time.Duration(snap.P3TimeoutMs)*time.Millisecond)
	defer cancel()

	decision, ok := ph.responseInterceptor.AwaitDecision(waitCtx, intercept.PendingResponse{
		ID:        requestID,
		RoomID:    strings.TrimSpace(req.Metadata["room_id"]),
		AgentName: agentName,
		Provider:  providerName,
		Content:   content,
		CreatedAt: time.Now(),
	})
	if !ok {
		ph.logger.Warn("manual response intercept timed out, forwarding response",
			"request_id", requestID,
			"provider", providerName,
		)
		return content, false
	}

	switch decision.Action {
	case intercept.ResponseModify, intercept.ResponseReplace:
		if decision.Content != "" {
			return decision.Content, false
		}
		return content, false
	case intercept.ResponseDrop:
		return "", true
	default:
		return content, false
	}
}

// buildStructuredPrompt assembles tagged system blocks for providers that support
// structured Anthropic-style system arrays.
func (ph *PipelineHandler) buildStructuredPrompt(req *LLMRequest, agentName, agentRole, providerName string, snap control.ConfigSnapshot) []SystemBlock {
	perception := compiler.StructuredPerception{
		CircadianText:   req.Metadata["circadian"],
		BodyText:        req.Metadata["body"],
		EnvironmentText: req.Metadata["environment"],
		AcousticText:    req.Metadata["acoustic"],
		HeardText:       req.Metadata["heard"],
		PresenceText:    req.Metadata["presence"],
		ImpulseText:     req.Metadata["impulse"],
		RoomID:          req.Metadata["room_id"],
	}
	agentIDStr := req.Metadata["agent_id"]
	agentID, parseErr := strconv.Atoi(agentIDStr)

	var compiled compiler.CompiledPrompt
	if parseErr == nil && agentID > 0 {
		evolution := compiler.EvolutionFromMetadata(req.Metadata)
		result, compileErr := ph.compiler.CompileStructuredFromSources(agentID, providerName, evolution, perception)
		if compileErr != nil {
			ph.logger.Warn("structured assembly failed, using fallback",
				"agent_id", agentID,
				"error", compileErr,
			)
			compiled = ph.compiler.CompileStructured(ph.modelKey(providerName), agentName, agentRole, perception)
		} else {
			compiled = result
		}
	} else {
		compiled = ph.compiler.CompileStructured(ph.modelKey(providerName), agentName, agentRole, perception)
	}

	if nudge := snap.NarrativeNudge; nudge != "" {
		compiled = compiler.AppendNarrativeNudge(compiled, nudge)
		ph.logger.Debug("structured narrative nudge injected", "agent", agentName)
	}

	blocks := make([]SystemBlock, 0, len(compiled.SystemBlocks))
	for _, block := range compiled.SystemBlocks {
		entry := SystemBlock{
			Type: "text",
			Text: block.Text,
		}
		if block.CacheControl != nil {
			entry.CacheControl = &CacheControl{Type: block.CacheControl.Type}
		}
		blocks = append(blocks, entry)
	}
	return blocks
}

// buildSystemPrompt assembles the legacy flat system prompt via 3-source assembly or fallback.
func (ph *PipelineHandler) buildSystemPrompt(req *LLMRequest, agentName, agentRole, providerName string, snap control.ConfigSnapshot) string {
	perception := req.Metadata["perception"]
	agentIDStr := req.Metadata["agent_id"]
	agentID, parseErr := strconv.Atoi(agentIDStr)

	var compiled string

	if parseErr == nil && agentID > 0 {
		evolution := compiler.EvolutionFromMetadata(req.Metadata)
		result, compileErr := ph.compiler.CompileFromSources(agentID, providerName, evolution, perception)
		if compileErr != nil {
			ph.logger.Warn("3-source assembly failed, using fallback",
				"agent_id", agentID,
				"error", compileErr,
			)
			modelKey := ph.modelKey(providerName)
			compiled = ph.compiler.Compile(modelKey, agentName, agentRole, perception)
		} else {
			if !evolution.IsEmpty() {
				ph.logger.Info("evolution injected",
					"agent_id", agentID,
					"has_voice", evolution.VoiceStyle != "",
					"has_notes", evolution.BehavioralNotes != "",
					"has_narrative", evolution.NarrativeSummary != "",
				)
			}
			compiled = result
		}
	} else if perception != "" {
		modelKey := ph.modelKey(providerName)
		compiled = ph.compiler.Compile(modelKey, agentName, agentRole, perception)
	}

	// Narrative Nudge injection (#144 AC-1)
	if nudge := snap.NarrativeNudge; nudge != "" && compiled != "" {
		compiled += "\n\n[NARRATIVE_NUDGE]\n" + nudge + "\n[/NARRATIVE_NUDGE]"
		ph.logger.Debug("narrative nudge injected", "agent", agentName)
	}

	return compiled
}

// applyGuardrails runs rate-limit and budget checks. Returns the (possibly replaced)
// provider/name and whether the request was rejected (HTTP 429 already sent).
// Safe to call when guardrails is nil (returns inputs unchanged).
func (ph *PipelineHandler) applyGuardrails(w http.ResponseWriter, req *LLMRequest, maxTokens int, provider Provider, providerName string) (Provider, string, bool) {
	if ph.guardrails == nil {
		return provider, providerName, false
	}
	if providerName == LocalLoopProviderName {
		return provider, providerName, false
	}
	agentID := req.Metadata["agent_id"]
	result := ph.guardrails.Check(agentID, maxTokens)
	if result.RateLimited {
		ph.writeRequestError(w, req, "rate limit exceeded", http.StatusTooManyRequests)
		return nil, "", true
	}
	if result.BudgetExhausted && result.FallbackProvider != "" {
		if fbProvider, ok := ph.registry.Get(result.FallbackProvider); ok {
			return fbProvider, result.FallbackProvider, false
		}
	}
	return provider, providerName, false
}

func isLocalLoopActive(snap control.ConfigSnapshot) bool {
	return snap.LocalLoopEnabled || strings.EqualFold(strings.TrimSpace(snap.PrimaryProvider), LocalLoopProviderName)
}

// injectPerception assembles and prepends the system prompt when an agent name is present.
func (ph *PipelineHandler) injectPerception(req *LLMRequest, agentName, agentRole, providerName string, snap control.ConfigSnapshot) {
	if req.Format == RequestFormatAnthropic && len(req.SystemBlocks) > 0 {
		return
	}
	if agentName == "" {
		return
	}
	if ph.caps != nil && ph.caps.HasCapability(providerName, capability.CapStructuredSystem) {
		if blocks := ph.buildStructuredPrompt(req, agentName, agentRole, providerName, snap); len(blocks) > 0 {
			req.SystemBlocks = blocks
			return
		}
	}
	if systemPrompt := ph.buildSystemPrompt(req, agentName, agentRole, providerName, snap); systemPrompt != "" {
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

func (ph *PipelineHandler) shouldForwardAfterSynthesisFourthWallCheck(ctx context.Context, content, agentName, agentRole string, provider Provider, req *LLMRequest, rule string) bool {
	judgeAdapter := NewJudgeProviderAdapter(provider, req)
	result, err := detection.HandleFourthWall(ctx, content, agentName, agentRole, judgeAdapter)
	if err != nil {
		ph.logger.Warn("synthesis fourth-wall detection error; falling back to provider",
			"agent", agentName,
			"rule", rule,
			"error", err,
		)
		return true
	}

	if result.Clean {
		ph.logger.Info("synthesis outbound fourth-wall checked",
			"agent", agentName,
			"rule", rule,
			"clean", true,
			"judge_override", result.JudgeOverride,
		)
		return false
	}

	ph.logger.Warn("synthesis aborted: fourth-wall detected",
		"agent", agentName,
		"rule", rule,
		"pattern", result.Pattern,
		"judge_override", result.JudgeOverride,
	)
	return true
}

// fourthWallCheck runs the 2-stage detection and re-generates on a break. It
// returns the (possibly re-generated) content plus a verdict for the Request
// Inspector (#429): "clean", "regenerated" (a break was fixed), "break" (a break
// could not be fixed), or "" (detection errored before any verdict).
func (ph *PipelineHandler) fourthWallCheck(ctx context.Context, content string, agentName, agentRole string, provider Provider, req *LLMRequest) (string, string) {
	judgeAdapter := NewJudgeProviderAdapter(provider, req)
	regenerated := false

	for attempt := 0; attempt < maxRegenAttempts; attempt++ {
		regenStart := time.Now()
		result, err := detection.HandleFourthWall(ctx, content, agentName, agentRole, judgeAdapter)
		if err != nil {
			ph.logger.Warn("fourth-wall detection error", "error", err)
			if regenerated {
				return content, "regenerated"
			}
			return content, ""
		}

		if result.Clean {
			if regenerated {
				return content, "regenerated"
			}
			return content, "clean"
		}

		// Re-generate mit Correction + niedrigerer Temperature
		ph.logger.Info("fourth-wall break detected",
			"pattern", result.Pattern,
			"agent", agentName,
			"attempt", attempt+1,
		)

		regenReq := cloneRegenRequest(req, appendCorrectionMessage(req.Messages, result.Correction), result.RetryWith)

		resp, sendErr := provider.Send(ctx, regenReq)
		detection.RegenLatency().Observe(time.Since(regenStart).Seconds())

		if sendErr != nil {
			ph.logger.Error("re-generation failed", "error", sendErr)
			return content, "break"
		}

		content = resp.Content
		regenerated = true
	}
	// Attempts exhausted while still breaking.
	return content, "break"
}

// personalityGuardCheck runs the DriftDetector on the LLM response.
// Returns the original or re-generated content.
func (ph *PipelineHandler) personalityGuardCheck(ctx context.Context, content, agentName string, provider Provider, req *LLMRequest, snap control.ConfigSnapshot) string {
	if ph.drift == nil || !snap.PersonalityGuardEnabled {
		return content
	}

	result := ph.drift.CheckDrift(agentName, []string{content})
	personalityGuardDriftTotal.WithLabelValues(agentName, result.Severity).Inc()

	if result.DriftScore < snap.DriftThreshold {
		return content
	}

	// Cooldown: max 1 re-generation per agent per 5 minutes (#240)
	ph.regenMu.Lock()
	lastRegen, hasRegen := ph.regenCooldown[agentName]
	if hasRegen && time.Since(lastRegen) < 5*time.Minute {
		ph.regenMu.Unlock()
		ph.logger.Debug("personality guard cooldown active", "agent", agentName)
		return content
	}
	ph.regenCooldown[agentName] = time.Now()
	ph.regenMu.Unlock()

	ph.logger.Warn("personality drift detected",
		"agent", agentName,
		"drift_score", result.DriftScore,
		"severity", result.Severity,
		"details", result.Details,
	)

	// Attempt re-generation with personality correction hint
	correction := fmt.Sprintf("Deine Antwort weicht von deinem Persoenlichkeitsprofil ab (Drift: %.2f, Severity: %s). Bitte antworte staerker im Charakter.", result.DriftScore, result.Severity)
	regenReq := cloneRegenRequest(req, appendCorrectionMessage(req.Messages, correction), req.Temperature*0.8)

	resp, err := provider.Send(ctx, regenReq)
	if err != nil {
		ph.logger.Error("personality guard re-generation failed", "error", err)
		return content
	}

	ph.logger.Info("personality guard re-generated response", "agent", agentName)
	return resp.Content
}

// qualityGateCheck runs the QualityScorer on the LLM response.
// Returns the original or re-generated content.
func (ph *PipelineHandler) qualityGateCheck(ctx context.Context, content, agentName string, provider Provider, req *LLMRequest, snap control.ConfigSnapshot) string {
	if ph.quality == nil || !snap.QualityGateEnabled {
		return content
	}

	result := ph.quality.ScoreMessage(agentName, content, nil)
	qualityGateScore.WithLabelValues(agentName).Observe(float64(result.Score))

	if result.Score > snap.QualityThreshold {
		return content
	}

	ph.logger.Warn("low quality response detected",
		"agent", agentName,
		"score", result.Score,
		"factors", fmt.Sprintf("len=%d spec=%d cons=%d", result.Factors.LengthScore, result.Factors.SpecificityScore, result.Factors.ConsistencyScore),
		"details", result.Details,
	)

	// Re-generation loop (limited by QualityMaxRegen)
	current := content
	for attempt := 0; attempt < snap.QualityMaxRegen; attempt++ {
		qualityGateRegenTotal.WithLabelValues(agentName).Inc()

		correction := fmt.Sprintf("Deine Antwort war zu kurz oder unspezifisch (Qualitaet: %d/5). Bitte antworte ausfuehrlicher und konkreter.", result.Score)
		regenReq := cloneRegenRequest(req, appendCorrectionMessage(req.Messages, correction), req.Temperature)

		resp, err := provider.Send(ctx, regenReq)
		if err != nil {
			ph.logger.Error("quality gate re-generation failed", "error", err, "attempt", attempt+1)
			return current
		}

		current = resp.Content
		recheck := ph.quality.ScoreMessage(agentName, current, nil)
		qualityGateScore.WithLabelValues(agentName).Observe(float64(recheck.Score))

		if recheck.Score > snap.QualityThreshold {
			ph.logger.Info("quality gate re-generation succeeded", "agent", agentName, "new_score", recheck.Score, "attempt", attempt+1)
			return current
		}
	}

	ph.logger.Warn("quality gate max regen reached", "agent", agentName, "max_regen", snap.QualityMaxRegen)
	return current
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

func (ph *PipelineHandler) enforceActionPolicy(actions []extraction.ExtractedAction, agentName, requestID string, req *LLMRequest) []extraction.ExtractedAction {
	if len(actions) == 0 || ph.actionPolicy == nil {
		return actions
	}
	allowed := make([]extraction.ExtractedAction, 0, len(actions))
	for i, action := range actions {
		decision := ph.actionPolicy.Allows(capability.ActionRequest{
			AgentID:    req.Metadata["agent_id"],
			AgentName:  agentName,
			ActionType: action.Type,
			Target:     action.Target,
			Content:    action.Content,
		})
		if decision.Allowed {
			allowed = append(allowed, action)
			continue
		}
		ph.logger.Warn("agent action rejected by capability policy",
			"request_id", requestID,
			"agent_id", req.Metadata["agent_id"],
			"agent_name", agentName,
			"action_type", action.Type,
			"target", action.Target,
			"reason", decision.Reason,
		)
		ph.persistActionRejection(action, decision, i, agentName, requestID, req)
	}
	return allowed
}

func (ph *PipelineHandler) persistActionRejection(action extraction.ExtractedAction, decision capability.ActionDecision, index int, agentName, requestID string, req *LLMRequest) {
	if ph.eventStore == nil {
		return
	}
	aggregateID := decision.AgentKey
	if aggregateID == "" {
		aggregateID = agentName
	}
	if aggregateID == "" {
		aggregateID = "unknown-agent"
	}

	payload := map[string]string{
		"action_type":    action.Type,
		"agent_id":       req.Metadata["agent_id"],
		"agent_name":     agentName,
		"reason":         decision.Reason,
		"security_issue": "prompt_injection_defense",
	}
	if action.Target != "" {
		payload["target"] = action.Target
	}
	if action.Content != "" {
		payload["content"] = action.Content
	}
	if decision.Tool != "" {
		payload["tool"] = decision.Tool
	}
	if decision.Target != "" {
		payload["validated_target"] = decision.Target
	}

	payloadJSON, err := json.Marshal(payload)
	if err != nil {
		payloadJSON = []byte(`{"security_issue":"prompt_injection_defense","error":"marshal_failed"}`)
	}

	evt := eventstore.DomainEvent{
		EventID:          eventstore.GenerateUUID(),
		EventType:        "agent_action_rejected",
		AggregateID:      aggregateID,
		Payload:          string(payloadJSON),
		CorrelationID:    requestID,
		OperationID:      fmt.Sprintf("%s-rejected-%d", requestID, index),
		Tick:             parseTick(req.Metadata),
		TimestampMs:      time.Now().UnixMilli(),
		SchemaVersion:    1,
		CompensationType: "none",
	}
	topic := fmt.Sprintf("sentinel/cortex/audit/%s", aggregateID)
	if err := ph.eventStore.AppendWithOutbox(evt, topic); err != nil {
		ph.logger.Warn("event store rejection audit write failed",
			"error", err,
			"request_id", requestID,
			"agent", aggregateID,
		)
	}
}

func (ph *PipelineHandler) writePipelineResponse(_ context.Context, w http.ResponseWriter, req *LLMRequest, resp PipelineResponse) {
	resp.HierarchyTier = req.HierarchyTier
	resp.EffectiveModel = effectiveModelForLog(req, resp.Model)
	if ph.catalog != nil {
		resp.CostUsd, resp.CostSource = resolveResponseCost(resp)
	}
	if resp.HierarchyTier >= 1 && resp.HierarchyTier <= 3 {
		pipelineHierarchyRequestsTotal.WithLabelValues(
			resp.Provider,
			strconv.Itoa(resp.HierarchyTier),
			resp.EffectiveModel,
			resp.CostSource,
		).Inc()
	}

	if ph.responseLogs != nil {
		ph.responseLogs.Add(ResponseLogEntry{
			RequestID:    resp.RequestID,
			RequestClass: req.RequestClass,
			Provider:     resp.Provider,
			Model:        effectiveModelForLog(req, resp.Model),
			PolicySource: req.PolicySource,
			AgentID:      req.Metadata["agent_id"],
			AgentName:    req.Metadata["agent_name"],
			Content:      resp.Content,
			Decision:     resp.Decision,
			Rule:         resp.Rule,
			FourthWall:   resp.FourthWall,
		})
	}

	// #427: record cache-aware per-agent/tier usage for EVERY response that flows
	// through this single sink (forward, synthesis, apicp, dropped) — one place,
	// so the JEDER-Call policy holds. The fresh input is recovered from the folded
	// InputTokens. CostUsd is computed here and threaded back onto the response so
	// the daemon can emit the AgentLlmUsage event with the gateway-resolved cost.
	agentLabel := canonicalAgentID(req.Metadata)
	rawInput := resp.InputTokens - resp.CacheRead - resp.CacheCreation
	if rawInput < 0 {
		rawInput = 0
	}
	if ph.catalog == nil {
		resp.CostUsd = guardrails.RecordAgentUsage(agentLabel, resp.Tier, resp.Provider, rawInput, resp.OutputTokens, resp.CacheRead, resp.CacheCreation)
	} else {
		guardrails.RecordAgentUsageResolved(agentLabel, resp.Tier, rawInput, resp.OutputTokens, resp.CacheRead, resp.CacheCreation, resp.CostUsd)
		if resp.Decision == "forward" || (resp.Decision == "dropped" && resp.Provider != "intercept") {
			guardrails.RecordRuntimeForwardCostResolved(resp.Provider, resp.CostUsd)
		}
	}
	guardrails.RecordRuntimeAgentUsageDimensions(
		agentLabel,
		resp.Tier,
		resp.HierarchyTier,
		resp.CostSource,
		resp.InputTokens,
		resp.OutputTokens,
		resp.CostUsd,
	)

	wireResponse := resp
	if req.RequestClass != RequestClassAgentRuntime {
		// The usage-v2 wire contract is private to the authenticated agent
		// runtime endpoint. Other callers still contribute to internal metrics,
		// but do not receive hierarchy/cost-source routing internals.
		wireResponse.HierarchyTier = 0
		wireResponse.CostSource = ""
		wireResponse.EffectiveModel = ""
	}
	payload := responsePayloadForRequest(req, wireResponse)

	if ph.tickSync != nil && ph.tickSync.Enabled() {
		if tick := parseTick(req.Metadata); tick > 0 {
			done := ph.tickSync.Hold(
				uint64(tick),
				parseAgentID(req.Metadata),
				parsePriority(req.Metadata),
				resp.RequestID,
				payload,
				w,
			)
			if err := <-done; err != nil {
				ph.logger.Error("tick_sync response write failed", "request_id", resp.RequestID, "error", err)
			}
			return
		}
	}

	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(payload); err != nil {
		ph.logger.Error("failed to encode response", "error", err)
	}
}

func resolveResponseCost(resp PipelineResponse) (float64, string) {
	result := resolveResponseCostAt(resp, time.Now())
	return result.USD, result.Source
}

func effectiveModelForLog(req *LLMRequest, responseModel string) string {
	if strings.TrimSpace(responseModel) != "" {
		return responseModel
	}
	if req == nil {
		return ""
	}
	if strings.TrimSpace(req.EffectiveModel) != "" {
		return req.EffectiveModel
	}
	return strings.TrimSpace(req.Model)
}

func responsePayloadForRequest(req *LLMRequest, resp PipelineResponse) interface{} {
	if req != nil && req.Format == RequestFormatAnthropic {
		return buildAnthropicMessageResponse(resp)
	}
	return resp
}

// resolveTier maps an effective model name to a #427 cost tier. It is the
// fallback used until #395 (explicit tier field) is merged. An empty/unknown
// model resolves to "unknown"; the synthesis/apicp/intercept tiers are set
// directly at their response sites (no real model is involved there).
func resolveTier(effectiveModel string) string {
	m := strings.ToLower(strings.TrimSpace(effectiveModel))
	switch {
	case m == "":
		return "unknown"
	case strings.Contains(m, "haiku"):
		return "low"
	case strings.Contains(m, "sonnet"):
		return "mid"
	case strings.Contains(m, "opus"):
		return "high"
	default:
		return "unknown"
	}
}

// canonicalAgentID normalizes the request metadata into the "AGENT-NN" label so
// the /metrics counters, the AgentLlmUsage event and the projection all speak
// one telemetry language (#429-R3 lesson). Falls back to agent_name, then
// "unknown" when neither is present (e.g. a direct MITM call without an agent).
func canonicalAgentID(metadata map[string]string) string {
	if id := metadata["agent_id"]; id != "" {
		if n, err := strconv.Atoi(id); err == nil {
			return fmt.Sprintf("AGENT-%02d", n)
		}
	}
	if name := metadata["agent_name"]; name != "" {
		return name
	}
	return "unknown"
}

func isAnthropicStreamingRequest(req *LLMRequest) bool {
	return req != nil && req.Format == RequestFormatAnthropic && req.Stream
}

func (ph *PipelineHandler) writePathError(w http.ResponseWriter, path, message string, status int) {
	if isAnthropicMessagesPath(path) {
		writeAnthropicError(w, status, message)
		return
	}
	http.Error(w, message, status)
}

func (ph *PipelineHandler) writeRequestError(w http.ResponseWriter, req *LLMRequest, message string, status int) {
	if req != nil && req.Format == RequestFormatAnthropic {
		writeAnthropicError(w, status, message)
		return
	}
	http.Error(w, message, status)
}

func buildSynthesisActions(actions []synthesis.Action, ext *extraction.Extractor, content string) []extraction.ExtractedAction {
	if len(actions) == 0 {
		if ext == nil {
			return nil
		}
		return ext.Extract(content)
	}

	result := make([]extraction.ExtractedAction, 0, len(actions))
	for _, action := range actions {
		result = append(result, extraction.ExtractedAction{
			Type:    action.Type,
			Content: action.Content,
			Target:  action.Target,
			Emotion: action.Emotion,
		})
	}
	return result
}

func pipelineResponseBlocks(content string, resp *LLMResponse) []json.RawMessage {
	if resp == nil || len(resp.ContentBlocks) == 0 {
		return nil
	}
	if content != resp.Content {
		return nil
	}
	return cloneRawMessages(resp.ContentBlocks)
}

type trackedResponseWriter struct {
	http.ResponseWriter
	wroteHeader bool
}

func (w *trackedResponseWriter) WriteHeader(statusCode int) {
	w.wroteHeader = true
	w.ResponseWriter.WriteHeader(statusCode)
}

func (w *trackedResponseWriter) Write(p []byte) (int, error) {
	w.wroteHeader = true
	return w.ResponseWriter.Write(p)
}

func (w *trackedResponseWriter) Flush() {
	if flusher, ok := w.ResponseWriter.(http.Flusher); ok {
		flusher.Flush()
	}
}

func (ph *PipelineHandler) streamAnthropicResponse(ctx context.Context, w http.ResponseWriter, req *LLMRequest, provider Provider, providerName string, breaker *CircuitBreaker, requestID string, start time.Time) {
	streamer, ok := provider.(StreamingProvider)
	if !ok {
		ph.writeRequestError(w, req, "streaming not supported by provider", http.StatusBadGateway)
		return
	}

	req.ProviderTimeout = ph.providerDeadline
	prevState := breaker.State()
	tracked := &trackedResponseWriter{ResponseWriter: w}
	err := streamer.StreamHTTP(ctx, req, tracked)
	breaker.Record(err)
	if newState := breaker.State(); newState == "open" && prevState != "open" {
		breakerTripsTotal.WithLabelValues(providerName).Inc()
	}
	ph.updateBreakerGauge(providerName, breaker)

	duration := time.Since(start)
	if err != nil {
		pipelineRequestsTotal.WithLabelValues(providerName, "stream_error").Inc()
		pipelineLatency.WithLabelValues(providerName).Observe(duration.Seconds())
		ph.logger.Error("provider stream failed",
			"provider", providerName,
			"request_id", requestID,
			"request_class", req.RequestClass,
			"effective_model", effectiveModelForLog(req, ""),
			"policy_source", req.PolicySource,
			"agent_id", req.Metadata["agent_id"],
			"agent_name", req.Metadata["agent_name"],
			"duration", duration,
			"error", err,
		)
		if !tracked.wroteHeader {
			statusCode := http.StatusBadGateway
			errMsg := "provider stream failed"
			var provErr *ProviderError
			if errors.As(err, &provErr) {
				if provErr.StatusCode > 0 {
					statusCode = provErr.StatusCode
				}
				errMsg = provErr.Message
			}
			ph.writeRequestError(tracked, req, errMsg, statusCode)
		}
		return
	}

	pipelineRequestsTotal.WithLabelValues(providerName, "stream").Inc()
	pipelineLatency.WithLabelValues(providerName).Observe(duration.Seconds())
	ph.logger.Info("pipeline stream completed",
		"provider", providerName,
		"request_id", requestID,
		"request_class", req.RequestClass,
		"effective_model", effectiveModelForLog(req, ""),
		"policy_source", req.PolicySource,
		"duration", duration,
		"agent_id", req.Metadata["agent_id"],
		"agent_name", req.Metadata["agent_name"],
	)
}

// modelKey mappt Provider-Namen auf Compiler-Config-Keys.
func (ph *PipelineHandler) modelKey(providerName string) string {
	switch providerName {
	case "claude":
		return "claude"
	case "anthropic-direct":
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

func cloneRegenRequest(base *LLMRequest, messages []Message, temperature float64) *LLMRequest {
	if base == nil {
		return &LLMRequest{
			Messages:    messages,
			Temperature: temperature,
		}
	}

	return &LLMRequest{
		Messages:           messages,
		SystemBlocks:       cloneSystemBlocks(base),
		Temperature:        temperature,
		MaxTokens:          base.MaxTokens,
		Model:              base.Model,
		Metadata:           base.Metadata,
		Format:             base.Format,
		PreferredProvider:  base.PreferredProvider,
		PassthroughHeaders: clonePassthroughHeaders(base.PassthroughHeaders),
		RequestClass:       base.RequestClass,
		EffectiveModel:     base.EffectiveModel,
		PolicySource:       base.PolicySource,
		HierarchyTier:      base.HierarchyTier,
		ProviderTimeout:    base.ProviderTimeout,
	}
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

func parseAgentID(metadata map[string]string) int {
	if v, ok := metadata["agent_id"]; ok {
		if id, err := strconv.Atoi(v); err == nil {
			return id
		}
	}
	return 0
}

func parsePriority(metadata map[string]string) int {
	switch strings.ToUpper(strings.TrimSpace(metadata["max_priority"])) {
	case "P0":
		return 0
	case "P1":
		return 1
	case "P2":
		return 2
	case "P3":
		return 3
	default:
		return 3
	}
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
