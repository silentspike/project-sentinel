package main

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"syscall"
	"time"

	"github.com/prometheus/client_golang/prometheus/promhttp"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/apicp"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/capability"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/compiler"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/extraction"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/forwardqueue"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/guardrails"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/intercept"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/normalizer"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/observatory"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/proxy"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/resilience"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/sequencing"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/synthesis"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/ticksync"
	"github.com/silentspike/project-sentinel/pkg/sentinel-go/eventstore"
	"github.com/silentspike/project-sentinel/pkg/sentinel-go/judge"
)

// version is set at build time via ldflags.
var version = "0.1.0"

// Server timeouts for both proxy and control plane.
const (
	readTimeout     = 30 * time.Second
	writeTimeout    = 60 * time.Second
	idleTimeout     = 120 * time.Second
	shutdownTimeout = 10 * time.Second
)

//nolint:gocyclo // composition root wires many runtime subsystems in one place
func main() {
	// 1. Structured logging via slog
	logger := slog.New(slog.NewJSONHandler(os.Stdout, &slog.HandlerOptions{Level: slog.LevelInfo}))
	slog.SetDefault(logger)

	logger.Info("cortex-gateway starting", "version", version)

	// 2. Configuration from environment
	port := envOrDefault("CORTEX_PORT", "8080")
	controlPort := envOrDefault("CORTEX_CONTROL_PORT", "8081")

	// 3. Provider registry
	registry := proxy.NewRegistry()
	forwardQueue := forwardqueue.NewManager(envIntOrDefault("SENTINEL_MAX_FORWARD_CONCURRENCY", 3))

	// Optional provider: direct Anthropic Messages API with structured system[]
	anthropicModel := envOrDefault("ANTHROPIC_MODEL", "claude-opus-4-6")
	anthropicDirectProvider := proxy.NewQueuedProvider(proxy.NewAnthropicDirectProvider(proxy.ProviderConfig{
		Name:      "anthropic-direct",
		Type:      "anthropic-direct",
		BaseURL:   envOrDefault("ANTHROPIC_BASE_URL", "https://api.anthropic.com"),
		APIKey:    os.Getenv("ANTHROPIC_API_KEY"),
		Model:     anthropicModel,
		MaxTokens: 4096,
		Priority:  1,
	}), forwardQueue)
	registry.Register("anthropic-direct", anthropicDirectProvider)
	logger.Info("registered provider", "name", "anthropic-direct", "model", anthropicModel)

	// Optional legacy/debug provider: Claude Code subprocess
	claudeCodeProvider := proxy.NewQueuedProvider(proxy.NewClaudeCodeProvider(proxy.ProviderConfig{
		Name:    "claude-code",
		Type:    "claude-code",
		BaseURL: envOrDefault("CLAUDE_CODE_BINARY", "claude"), // binary path
		Model:   envOrDefault("CLAUDE_CODE_MODEL", "claude-opus-4-6"),
	}, logger), forwardQueue)
	registry.Register("claude-code", claudeCodeProvider)
	logger.Info("registered provider", "name", "claude-code", "model", envOrDefault("CLAUDE_CODE_MODEL", "claude-opus-4-6"))

	localLoopProvider, err := proxy.NewLocalLoopProvider(proxy.LocalLoopConfig{
		Name:         proxy.LocalLoopProviderName,
		Model:        envOrDefault("CORTEX_LOCAL_LOOP_MODEL", "local-loop"),
		ScenarioPath: os.Getenv("CORTEX_LOCAL_LOOP_SCENARIO"),
	})
	if err != nil {
		logger.Error("failed to configure local-loop provider", "error", err)
		os.Exit(1)
	}
	registry.Register(proxy.LocalLoopProviderName, localLoopProvider)
	logger.Info("registered provider", "name", proxy.LocalLoopProviderName, "model", envOrDefault("CORTEX_LOCAL_LOOP_MODEL", "local-loop"))

	// 4. Control config (shared between pipeline + control plane)
	controlConfig := control.NewConfig(defaultPrimaryProvider())
	applyTrafficControlDefaults(controlConfig, logger)
	applyHardeningDefaults(controlConfig, logger)

	// 4b. Event Store (optional, enabled via SENTINEL_CORTEX_EVENT_STORE_PATH)
	var evStore *eventstore.Store
	if esPath := os.Getenv("SENTINEL_CORTEX_EVENT_STORE_PATH"); esPath != "" {
		var err error
		evStore, err = eventstore.Open(esPath)
		if err != nil {
			logger.Error("failed to open event store", "path", esPath, "error", err)
			os.Exit(1)
		}
		defer func() { _ = evStore.Close() }()
		logger.Info("event store opened", "path", esPath)
	} else {
		logger.Info("event store disabled (SENTINEL_CORTEX_EVENT_STORE_PATH not set)")
	}

	// 4c. Guardrails (optional, enabled via SENTINEL_GUARDRAILS_ENABLED)
	var guardrailsEnforcer *guardrails.Enforcer
	guardrailsCfg := guardrails.ConfigFromEnv()
	if guardrailsCfg.Enabled {
		guardrailsEnforcer = guardrails.New(guardrailsCfg)
		logger.Info("guardrails enabled",
			"rate_agent_rpm", guardrailsCfg.RateLimitPerAgent,
			"rate_global_rpm", guardrailsCfg.RateLimitGlobal,
			"budget_hourly", guardrailsCfg.BudgetHourlyTokens,
			"budget_daily", guardrailsCfg.BudgetDailyTokens,
		)
	} else {
		logger.Info("guardrails disabled (SENTINEL_GUARDRAILS_ENABLED not set)")
	}

	// 4d. Observatory (optional, enabled via config or SENTINEL_OBSERVATORY env)
	var obsHandler *observatory.Handler
	obsConfigPath := envOrDefault("SENTINEL_OBSERVATORY_CONFIG", "config/observatory.toml")
	obsCfg, obsErr := observatory.LoadConfig(obsConfigPath)
	if obsErr != nil {
		logger.Info("observatory disabled", "reason", obsErr)
	} else if obsCfg.IsEnabled() {
		obsDBPath := envOrDefault("SENTINEL_OBSERVATORY_DB", "data/observatory.db")
		obsStore, err := observatory.OpenSqliteStore(obsDBPath)
		if err != nil {
			logger.Error("failed to open observatory store", "path", obsDBPath, "error", err)
			os.Exit(1)
		}
		defer func() { _ = obsStore.Close() }()
		obsHandler = observatory.NewHandler(obsStore, obsCfg, logger)
		logger.Info("observatory enabled", "db", obsDBPath)
	}

	// 5. Processing pipeline (fully wired)
	providerDeadline := proxy.ProviderDeadlineFromEnv()
	inflightDeadline := proxy.InflightDeadlineFromEnv()
	logger.Info("provider timeout configured", "timeout", providerDeadline)
	logger.Info("inflight deadline configured", "deadline", inflightDeadline)

	// 5a. Capabilities + TOML Loader + Compiler with 3-source Assembly
	caps := capability.New()
	agentsDir := envOrDefault("SENTINEL_AGENTS_DIR", "config/agents")
	actionPolicy, err := capability.LoadAgentActionPolicy(agentsDir)
	if err != nil {
		logger.Warn("agent action capability policy unavailable; tool_use actions will be denied",
			"agents_dir", agentsDir,
			"error", err,
		)
		actionPolicy = capability.NewAgentActionPolicy(nil)
	} else {
		logger.Info("agent action capability policy loaded",
			"agents", len(actionPolicy.Definitions()),
			"agents_dir", agentsDir,
		)
	}
	tomlLoader := compiler.NewTOMLLoader(agentsDir)
	promptCompiler := compiler.NewWithAssembler(tomlLoader, caps)
	logger.Info("4-source assembly enabled (DNA + Company + Evolution + Perception)", "agents_dir", agentsDir)

	// 5b. DriftDetector + QualityScorer for Pipeline Hardening (#144)
	driftDetector := judge.NewDriftDetector()
	loadAgentProfiles(tomlLoader, driftDetector, agentsDir, logger)
	qualityScorer := judge.NewQualityScorer(driftDetector)

	// 5c. InFlightMap for query lifecycle tracking
	inflightMap := resilience.NewInFlightMap(inflightDeadline)
	go func() {
		ticker := time.NewTicker(5 * time.Second)
		defer ticker.Stop()
		for range ticker.C {
			if n := inflightMap.Prune(); n > 0 {
				logger.Debug("inflight prune", "pruned", n)
			}
		}
	}()
	logger.Info("inflight map enabled", "deadline", inflightDeadline)

	// 5d. Traffic Control: Synthesis, Sequencing, Tick-Sync, API-CP
	trafficSnap := controlConfig.Get()

	synthEngine := synthesis.NewEngine(trafficSnap.SynthesisEnabled, logger)
	if trafficSnap.SynthesisEnabled {
		logger.Info("synthesis engine enabled", "rules", 10)
	}

	chatSequencer := sequencing.NewSequencer(time.Duration(trafficSnap.P3TimeoutMs)*time.Millisecond, trafficSnap.SequencingEnabled, logger)
	if trafficSnap.SequencingEnabled {
		logger.Info("chat sequencing enabled", "timeout_ms", trafficSnap.P3TimeoutMs)
	}
	go func() {
		ticker := time.NewTicker(30 * time.Second)
		defer ticker.Stop()
		for range ticker.C {
			chatSequencer.Cleanup()
		}
	}()

	tickSync := ticksync.NewBuffer(time.Duration(trafficSnap.TickSyncTimeoutMs)*time.Millisecond, trafficSnap.TickSyncEnabled, logger)
	if trafficSnap.TickSyncEnabled {
		logger.Info("tick sync enabled", "timeout_ms", trafficSnap.TickSyncTimeoutMs)
	}
	responseLogs := proxy.NewResponseLogBuffer(200)

	operatorAPIURL := envOrDefault("SENTINEL_OPERATOR_API_URL", "http://127.0.0.1:8084")
	apicpObserver := apicp.NewObserver(apicp.Config{
		SyncURL:      operatorAPIURL + "/operator/apicp/snapshot",
		SyncInterval: 5 * time.Minute,
		SharedSecret: os.Getenv("SENTINEL_OPERATOR_API_KEY"),
	}, logger)
	if trafficSnap.APICPEnabled {
		logger.Info("api-cp learning agent enabled", "sync_url", operatorAPIURL+"/operator/apicp/snapshot")
	}
	requestInterceptor := intercept.NewManager()
	responseInterceptor := intercept.NewResponseManager()

	applyTrafficRuntimeConfig(controlConfig.Get(), synthEngine, chatSequencer, tickSync, forwardQueue)
	go func() {
		ticker := time.NewTicker(500 * time.Millisecond)
		defer ticker.Stop()
		for range ticker.C {
			applyTrafficRuntimeConfig(controlConfig.Get(), synthEngine, chatSequencer, tickSync, forwardQueue)
		}
	}()

	// Room aliases for move target resolution (rooms.toml → room_id mapping)
	roomsPath := envOrDefault("SENTINEL_ROOMS_CONFIG", "config/rooms.toml")
	if roomDefs, err := loadRoomDefs(roomsPath); err != nil {
		logger.Warn("rooms.toml not loaded, move targets will not be resolved", "error", err)
	} else {
		extraction.SetRoomAliases(roomDefs)
		logger.Info("room aliases loaded", "rooms", len(roomDefs), "path", roomsPath)
	}

	pipelineHandler := proxy.NewPipelineHandler(proxy.PipelineConfig{
		Registry:            registry,
		Config:              controlConfig,
		Compiler:            promptCompiler,
		Normalizer:          normalizer.New(),
		Extractor:           extraction.New(),
		Capabilities:        caps,
		ActionPolicy:        actionPolicy,
		Logger:              logger,
		BreakerCfg:          proxy.BreakerConfigFromEnv(),
		EventStore:          evStore,
		Guardrails:          guardrailsEnforcer,
		InFlight:            inflightMap,
		ProviderDeadline:    providerDeadline,
		Drift:               driftDetector,
		Quality:             qualityScorer,
		Synthesis:           synthEngine,
		Sequencer:           chatSequencer,
		Observer:            apicpObserver,
		Interceptor:         requestInterceptor,
		ResponseInterceptor: responseInterceptor,
		TickSync:            tickSync,
		ResponseLogs:        responseLogs,
	})

	// 6. HTTP proxy server
	proxyMux := http.NewServeMux()
	proxyMux.Handle("POST /v1/messages", pipelineHandler)
	// Legacy/internal JSON compatibility path. Prefer /internal/llm for service-to-service calls.
	proxyMux.Handle("POST /v1/chat/completions", pipelineHandler)
	// Canonical internal gateway contract for daemon/judge traffic.
	proxyMux.Handle("POST /internal/llm", pipelineHandler)
	proxyMux.HandleFunc("GET /health", handleHealth(pipelineHandler, guardrailsEnforcer != nil))
	proxyMux.HandleFunc("GET /ready", handleReady)
	proxyMux.Handle("GET /metrics", promhttp.Handler())

	proxyServer := &http.Server{
		Addr:         ":" + port,
		Handler:      proxyMux,
		ReadTimeout:  readTimeout,
		WriteTimeout: writeTimeout,
		IdleTimeout:  idleTimeout,
	}

	// 7. Control plane server (shared controlConfig → aenderungen wirken sofort)
	controlPlane := control.NewPlane(controlConfig, logger)
	controlMux := controlPlane.Handler()

	// 7b. Register guardrails routes on control plane (if enabled)
	if guardrailsEnforcer != nil {
		guardrailsHandler := guardrails.NewHandler(guardrailsEnforcer)
		guardrailsHandler.RegisterRoutes(controlMux)
	}

	// 7c. Register observatory routes on control plane (if enabled)
	if obsHandler != nil {
		obsHandler.RegisterRoutes(controlMux)
	}

	// 7d. Traffic Control stats on control plane (AC-19)
	controlMux.HandleFunc("GET /control/traffic-stats", func(w http.ResponseWriter, r *http.Request) {
		costStats := guardrails.RuntimeCostSnapshot()
		pendingIntercepts := requestInterceptor.Pending()
		pendingResponseIntercepts := responseInterceptor.Pending()
		trafficConfig := controlConfig.Get()
		lastAgentRuntime, hasLastAgentRuntime := responseLogs.LastByClass(proxy.RequestClassAgentRuntime)
		localLoopActive := trafficConfig.LocalLoopEnabled || trafficConfig.PrimaryProvider == proxy.LocalLoopProviderName
		externalMITMProvider := "anthropic-direct"
		if localLoopActive {
			externalMITMProvider = proxy.LocalLoopProviderName
		}
		stats := map[string]interface{}{
			"synthesis_enabled":           trafficConfig.SynthesisEnabled,
			"sequencing_enabled":          trafficConfig.SequencingEnabled,
			"tick_sync_enabled":           trafficConfig.TickSyncEnabled,
			"tick_sync_runtime_enabled":   tickSync.Enabled(),
			"apicp_enabled":               trafficConfig.APICPEnabled,
			"current_cost_usd":            costStats.TotalCostUSD,
			"estimated_savings_usd":       costStats.TotalSavingsUSD,
			"projected_daily_cost_usd":    costStats.ProjectedDailyCostUSD,
			"projected_daily_savings_usd": costStats.ProjectedDailySavingsUSD,
			"avg_forward_cost_usd":        costStats.AverageForwardCostUSD,
			"forward_calls":               costStats.ForwardCalls,
			"synthesis_count":             costStats.SynthesisCount,
			"synthesis_rate":              costStats.SynthesisRate,
			"cost_by_provider":            costStats.ByProvider,
			"local_loop_enabled":          localLoopActive,
			"primary_provider":            trafficConfig.PrimaryProvider,
			"internal_primary_provider":   trafficConfig.PrimaryProvider,
			"external_mitm_provider":      externalMITMProvider,
			"agent_runtime_model_policy":  trafficConfig.AgentRuntimeModelPolicy,
			"intercept_mode":              trafficConfig.InterceptMode,
			"max_forward_concurrency":     trafficConfig.MaxForwardConcurrency,
			"tick_sync_timeout_ms":        trafficConfig.TickSyncTimeoutMs,
			"p3_timeout_ms":               trafficConfig.P3TimeoutMs,
			"queue_depth":                 forwardQueue.Stats().Depth,
			"active_forward_calls":        forwardQueue.Stats().Active,
			"pending_intercepts":          len(pendingIntercepts),
			"pending_response_intercepts": len(pendingResponseIntercepts),
			"tick_sync_pending":           tickSync.Stats().Pending,
			"response_log_entries":        responseLogs.Len(),
		}
		if hasLastAgentRuntime {
			stats["last_agent_runtime_effective_model"] = lastAgentRuntime.Model
			stats["last_agent_runtime_policy_source"] = lastAgentRuntime.PolicySource
			stats["last_agent_runtime_provider"] = lastAgentRuntime.Provider
		}
		if apicpObserver != nil {
			apicpStats := apicpObserver.Stats()
			stats["apicp"] = apicpStats
			if patternsTotal, ok := apicpStats["patterns_total"]; ok {
				stats["active_patterns"] = patternsTotal
			}
			if suggestionCount, ok := apicpStats["suggestions"]; ok {
				stats["apicp_suggestion_count"] = suggestionCount
			}
			stats["apicp_suggestions"] = apicpObserver.Suggestions()
		}
		w.Header().Set("Content-Type", "application/json")
		if err := json.NewEncoder(w).Encode(stats); err != nil {
			logger.Warn("encode traffic stats failed", "error", err)
		}
	})
	controlMux.HandleFunc("GET /control/intercepts/pending", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(requestInterceptor.Pending())
	})
	controlMux.HandleFunc("GET /control/traffic-responses", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(responseLogs.Entries())
	})
	// #429: per-rule synthesis visibility + live toggle (process-memory only, like synthesis_enabled).
	controlMux.HandleFunc("GET /control/synthesis/rules", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(synthEngine.RuleStates())
	})
	controlMux.HandleFunc("POST /control/synthesis/rules/{name}", func(w http.ResponseWriter, r *http.Request) {
		name := r.PathValue("name")
		var body struct {
			Enabled bool `json:"enabled"`
		}
		if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
			http.Error(w, `{"error":"invalid body"}`, http.StatusBadRequest)
			return
		}
		if !synthEngine.SetRuleEnabled(name, body.Enabled) {
			http.Error(w, `{"error":"unknown rule"}`, http.StatusNotFound)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(synthesis.RuleState{Name: name, Enabled: body.Enabled})
	})
	controlMux.HandleFunc("GET /control/intercepts/responses/pending", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(responseInterceptor.Pending())
	})
	controlMux.HandleFunc("POST /control/intercepts/{id}/decision", func(w http.ResponseWriter, r *http.Request) {
		id := r.PathValue("id")
		if id == "" {
			http.Error(w, "missing intercept id", http.StatusBadRequest)
			return
		}

		var payload interceptDecisionPayload
		if err := json.NewDecoder(r.Body).Decode(&payload); err != nil {
			http.Error(w, "invalid request body", http.StatusBadRequest)
			return
		}

		var decision intercept.RequestDecision
		switch payload.Action {
		case string(intercept.RequestForward), "":
			decision = intercept.Forward(payload.Reason)
		case string(intercept.RequestModify):
			decision = intercept.Modify(payload.Reason, payload.ContextSuffix)
		case string(intercept.RequestDrop):
			decision = intercept.Drop(payload.Reason)
		default:
			http.Error(w, "invalid action", http.StatusBadRequest)
			return
		}

		if ok := requestInterceptor.ResolveRequest(id, decision); !ok {
			http.Error(w, "intercept not found", http.StatusNotFound)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{
			"ok":     true,
			"id":     id,
			"action": decision.Action,
		})
	})
	controlMux.HandleFunc("POST /control/intercepts/responses/{id}/decision", func(w http.ResponseWriter, r *http.Request) {
		id := r.PathValue("id")
		if id == "" {
			http.Error(w, "missing intercept id", http.StatusBadRequest)
			return
		}

		var payload interceptDecisionPayload
		if err := json.NewDecoder(r.Body).Decode(&payload); err != nil {
			http.Error(w, "invalid request body", http.StatusBadRequest)
			return
		}

		var decision intercept.ResponseDecision
		switch payload.Action {
		case string(intercept.ResponseForward), "":
			decision = intercept.ResponseDecision{Action: intercept.ResponseForward, Reason: payload.Reason}
		case string(intercept.ResponseModify):
			decision = intercept.ResponseDecision{Action: intercept.ResponseModify, Reason: payload.Reason, Content: payload.ContextSuffix}
		case string(intercept.ResponseReplace):
			decision = intercept.ResponseDecision{Action: intercept.ResponseReplace, Reason: payload.Reason, Content: payload.ContextSuffix}
		case string(intercept.ResponseDrop):
			decision = intercept.ResponseDecision{Action: intercept.ResponseDrop, Reason: payload.Reason}
		default:
			http.Error(w, "invalid action", http.StatusBadRequest)
			return
		}

		if ok := responseInterceptor.Resolve(id, decision); !ok {
			http.Error(w, "intercept not found", http.StatusNotFound)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{
			"ok":     true,
			"id":     id,
			"action": decision.Action,
		})
	})

	// #440: Hot-Reload company-context.md + invalidate all agent-DNA caches at runtime (no restart,
	// no LLM/provider call — token-safe). Triggered by sentinel-ctl / the daemon config-apply path.
	controlMux.HandleFunc("POST /control/reload", func(w http.ResponseWriter, _ *http.Request) {
		bytesLoaded := promptCompiler.ReloadCompanyContext()
		tomlLoader.InvalidateAll()
		logger.Info("hot-reload via control plane",
			"company_context_bytes", bytesLoaded, "dna_invalidated", "all")
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{
			"reloaded":              true,
			"company_context_bytes": bytesLoaded,
			"dna_invalidated":       "all",
		})
	})

	// #440: targeted agent-DNA cache invalidation (for #425 live single-agent edits). Empty body or
	// empty agent_ids => invalidate all. No LLM call.
	controlMux.HandleFunc("POST /control/dna/invalidate", func(w http.ResponseWriter, r *http.Request) {
		var req struct {
			AgentIDs []int `json:"agent_ids"`
		}
		// An empty/absent body is valid and means "invalidate all".
		_ = json.NewDecoder(r.Body).Decode(&req)
		var invalidated any
		if len(req.AgentIDs) == 0 {
			tomlLoader.InvalidateAll()
			invalidated = "all"
		} else {
			for _, id := range req.AgentIDs {
				tomlLoader.Invalidate(id)
			}
			invalidated = req.AgentIDs
		}
		logger.Info("agent DNA cache invalidated via control plane", "agents", invalidated)
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{
			"reloaded":        true,
			"dna_invalidated": invalidated,
		})
	})

	controlServer := &http.Server{
		Addr:         ":" + controlPort,
		Handler:      controlMux,
		ReadTimeout:  readTimeout,
		WriteTimeout: writeTimeout,
		IdleTimeout:  idleTimeout,
	}

	// 8. Start servers
	go func() {
		logger.Info("proxy server starting", "addr", proxyServer.Addr)
		if err := proxyServer.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			logger.Error("proxy server failed", "error", err)
			os.Exit(1)
		}
	}()

	go func() {
		logger.Info("control plane starting", "addr", controlServer.Addr)
		if err := controlServer.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			logger.Error("control server failed", "error", err)
			os.Exit(1)
		}
	}()

	// 9. Graceful shutdown on signal
	quit := make(chan os.Signal, 1)
	signal.Notify(quit, syscall.SIGINT, syscall.SIGTERM)
	sig := <-quit

	logger.Info("shutdown signal received", "signal", sig.String())

	ctx, cancel := context.WithTimeout(context.Background(), shutdownTimeout)
	defer cancel()

	if err := proxyServer.Shutdown(ctx); err != nil {
		logger.Error("proxy server shutdown error", "error", err)
	}
	if err := controlServer.Shutdown(ctx); err != nil {
		logger.Error("control server shutdown error", "error", err)
	}
	if apicpObserver != nil {
		apicpObserver.Stop()
	}

	logger.Info("cortex-gateway stopped")
}

func handleHealth(pipeline *proxy.PipelineHandler, guardrailsEnabled bool) http.HandlerFunc {
	type healthResponse struct {
		Status            string            `json:"status"`
		Version           string            `json:"version"`
		CircuitBreakers   map[string]string `json:"circuit_breakers"`
		GuardrailsEnabled bool              `json:"guardrails_enabled"`
	}

	return func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		resp := healthResponse{
			Status:            "ok",
			Version:           version,
			CircuitBreakers:   pipeline.BreakerStates(),
			GuardrailsEnabled: guardrailsEnabled,
		}
		_ = json.NewEncoder(w).Encode(resp)
	}
}

func handleReady(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	_, _ = fmt.Fprint(w, `{"ready":true}`)
}

type interceptDecisionPayload struct {
	Action        string `json:"action"`
	ContextSuffix string `json:"context_suffix"`
	Reason        string `json:"reason"`
}

func envOrDefault(key, defaultVal string) string {
	if val := os.Getenv(key); val != "" {
		return val
	}
	return defaultVal
}

func envIntOrDefault(key string, defaultVal int) int {
	if val := os.Getenv(key); val != "" {
		if parsed, err := strconv.Atoi(val); err == nil && parsed > 0 {
			return parsed
		}
	}
	return defaultVal
}

// applyHardeningDefaults reads pipeline hardening settings from environment
// variables and applies them to the control config so they survive restarts.
func applyHardeningDefaults(cfg *control.Config, logger *slog.Logger) {
	updates := make(map[string]interface{})

	if v := os.Getenv("SENTINEL_PERSONALITY_GUARD_ENABLED"); v == "true" || v == "1" {
		updates["personality_guard_enabled"] = true
	}
	if v := os.Getenv("SENTINEL_DRIFT_THRESHOLD"); v != "" {
		if f, err := strconv.ParseFloat(v, 64); err == nil {
			updates["drift_threshold"] = f
		}
	}
	if v := os.Getenv("SENTINEL_QUALITY_GATE_ENABLED"); v == "true" || v == "1" {
		updates["quality_gate_enabled"] = true
	}
	if v := os.Getenv("SENTINEL_QUALITY_THRESHOLD"); v != "" {
		if i, err := strconv.Atoi(v); err == nil {
			updates["quality_threshold"] = float64(i)
		}
	}
	if v := os.Getenv("SENTINEL_QUALITY_MAX_REGEN"); v != "" {
		if i, err := strconv.Atoi(v); err == nil {
			updates["quality_max_regen"] = float64(i)
		}
	}
	if v := os.Getenv("SENTINEL_NARRATIVE_NUDGE"); v != "" {
		updates["narrative_nudge"] = v
	}

	if len(updates) > 0 {
		if err := cfg.Update(updates); err != nil {
			logger.Error("failed to apply hardening defaults", "error", err)
		} else {
			logger.Info("pipeline hardening defaults applied", "updates", updates)
		}
	}
}

func applyTrafficControlDefaults(cfg *control.Config, logger *slog.Logger) {
	updates, err := control.LoadTrafficControlDefaults(control.DaemonConfigPath())
	if err != nil {
		logger.Warn("failed to load daemon traffic control defaults, using hardcoded defaults", "error", err)
		updates = control.DefaultTrafficControlUpdates()
	}

	if v, ok := envBoolValue("SENTINEL_SYNTHESIS_ENABLED"); ok {
		updates["synthesis_enabled"] = v
	}
	if v, ok := envBoolValue("SENTINEL_SEQUENCING_ENABLED"); ok {
		updates["sequencing_enabled"] = v
	}
	if v, ok := envBoolValue("SENTINEL_TICK_SYNC_ENABLED"); ok {
		updates["tick_sync_enabled"] = v
	}
	if v, ok := envBoolValue("SENTINEL_APICP_ENABLED"); ok {
		updates["apicp_enabled"] = v
	}
	if v, ok := envBoolValue("CORTEX_LOCAL_LOOP"); ok {
		updates["local_loop_enabled"] = v
	}
	if v, ok := envIntValue("SENTINEL_TICK_SYNC_TIMEOUT_MS"); ok {
		updates["tick_sync_timeout_ms"] = v
	}
	if v, ok := envIntValue("SENTINEL_P3_TIMEOUT_MS"); ok {
		updates["p3_timeout_ms"] = v
	}
	if v, ok := envIntValue("SENTINEL_MAX_FORWARD_CONCURRENCY"); ok {
		updates["max_forward_concurrency"] = v
	}
	if v, ok := envStringValue("SENTINEL_INTERCEPT_MODE"); ok {
		updates["intercept_mode"] = v
	}

	if err := cfg.Update(updates); err != nil {
		logger.Error("failed to apply traffic control defaults", "error", err)
		return
	}
	logger.Info("traffic control defaults applied", "config_path", control.DaemonConfigPath(), "updates", updates)
}

func defaultPrimaryProvider() string {
	if v := os.Getenv("CORTEX_PRIMARY_PROVIDER"); v != "" {
		return v
	}
	if enabled, ok := envBoolValue("CORTEX_LOCAL_LOOP"); ok && enabled {
		return proxy.LocalLoopProviderName
	}
	if os.Getenv("ANTHROPIC_API_KEY") != "" {
		return "anthropic-direct"
	}
	return "claude-code"
}

func applyTrafficRuntimeConfig(
	snap control.ConfigSnapshot,
	synthEngine *synthesis.Engine,
	chatSequencer *sequencing.Sequencer,
	tickSync *ticksync.Buffer,
	forwardQueue *forwardqueue.Manager,
) {
	if synthEngine != nil {
		synthEngine.SetEnabled(snap.SynthesisEnabled)
	}
	if chatSequencer != nil {
		chatSequencer.SetEnabled(snap.SequencingEnabled)
		chatSequencer.SetTimeout(time.Duration(snap.P3TimeoutMs) * time.Millisecond)
	}
	if tickSync != nil {
		tickSync.SetTimeout(time.Duration(snap.TickSyncTimeoutMs) * time.Millisecond)
		tickSync.SetEnabled(snap.TickSyncEnabled)
	}
	if forwardQueue != nil {
		forwardQueue.SetMaxConcurrent(snap.MaxForwardConcurrency)
	}
}

func envBoolValue(key string) (bool, bool) {
	val := os.Getenv(key)
	if val == "" {
		return false, false
	}
	switch val {
	case "1", "true", "TRUE", "True":
		return true, true
	case "0", "false", "FALSE", "False":
		return false, true
	default:
		return false, false
	}
}

func envIntValue(key string) (int, bool) {
	val := os.Getenv(key)
	if val == "" {
		return 0, false
	}
	parsed, err := strconv.Atoi(val)
	if err != nil {
		return 0, false
	}
	return parsed, true
}

func envStringValue(key string) (string, bool) {
	val := os.Getenv(key)
	if val == "" {
		return "", false
	}
	return val, true
}

// loadAgentProfiles reads all agent TOMLs and registers their Big Five profiles
// with the DriftDetector for personality guard checks.
func loadAgentProfiles(loader *compiler.TOMLLoader, detector *judge.DriftDetector, agentsDir string, logger *slog.Logger) {
	loaded := 0
	for id := 1; id <= 54; id++ {
		dna, err := loader.Load(id)
		if err != nil {
			continue // agent TOML not found, skip
		}
		agentName := fmt.Sprintf("AGENT-%02d", id)
		detector.RegisterProfile(agentName, judge.PersonalityProfile{
			Role:         dna.Identity.Role,
			Extraversion: dna.Personality.Extraversion,
			Neuroticism:  dna.Personality.Neuroticism,
		})
		loaded++
	}
	logger.Info("agent personality profiles loaded", "count", loaded, "agents_dir", agentsDir)
}

// loadRoomDefs loads room definitions from rooms.toml for move target resolution.
func loadRoomDefs(path string) ([]extraction.RoomDef, error) {
	data, err := os.ReadFile(path) //nolint:gosec // path comes from trusted local config
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", path, err)
	}

	// Simple TOML parsing: extract id and name from [[rooms]] entries.
	// We use a line-by-line parser since we only need id/name fields.
	var defs []extraction.RoomDef
	var currentID, currentName string
	inRoom := false

	for _, line := range splitLines(string(data)) {
		trimmed := trimLine(line)
		if trimmed == "[[rooms]]" {
			if inRoom && currentID != "" {
				defs = append(defs, extraction.RoomDef{ID: currentID, Name: currentName})
			}
			currentID = ""
			currentName = ""
			inRoom = true
			continue
		}
		if !inRoom {
			continue
		}
		if key, val, ok := parseTomlKV(trimmed); ok {
			switch key {
			case "id":
				currentID = val
			case "name":
				currentName = val
			}
		}
	}
	// Last room
	if inRoom && currentID != "" {
		defs = append(defs, extraction.RoomDef{ID: currentID, Name: currentName})
	}

	return defs, nil
}

func splitLines(s string) []string {
	var lines []string
	start := 0
	for i := 0; i < len(s); i++ {
		if s[i] == '\n' {
			lines = append(lines, s[start:i])
			start = i + 1
		}
	}
	if start < len(s) {
		lines = append(lines, s[start:])
	}
	return lines
}

func trimLine(s string) string {
	i := 0
	for i < len(s) && (s[i] == ' ' || s[i] == '\t' || s[i] == '\r') {
		i++
	}
	j := len(s)
	for j > i && (s[j-1] == ' ' || s[j-1] == '\t' || s[j-1] == '\r') {
		j--
	}
	return s[i:j]
}

func parseTomlKV(line string) (key, val string, ok bool) {
	eq := -1
	for i := 0; i < len(line); i++ {
		if line[i] == '=' {
			eq = i
			break
		}
	}
	if eq < 0 {
		return "", "", false
	}
	key = trimLine(line[:eq])
	raw := trimLine(line[eq+1:])
	// Strip quotes
	if len(raw) >= 2 && raw[0] == '"' && raw[len(raw)-1] == '"' {
		val = raw[1 : len(raw)-1]
	} else {
		val = raw
	}
	return key, val, true
}
