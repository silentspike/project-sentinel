package main

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/prometheus/client_golang/prometheus/promhttp"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/capability"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/compiler"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/extraction"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/guardrails"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/normalizer"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/observatory"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/proxy"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/resilience"
	"github.com/obtFusi/project-sentinel/pkg/sentinel-go/eventstore"
	"github.com/obtFusi/project-sentinel/pkg/sentinel-go/judge"
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

	claudeAPIKey := os.Getenv("ANTHROPIC_API_KEY")
	if claudeAPIKey != "" {
		claudeProvider := proxy.NewClaudeProvider(proxy.ProviderConfig{
			Name:      "claude",
			Type:      "claude",
			BaseURL:   envOrDefault("CLAUDE_BASE_URL", "https://api.anthropic.com"),
			APIKey:    claudeAPIKey,
			Model:     envOrDefault("CLAUDE_MODEL", "claude-sonnet-4-5-20250929"),
			MaxTokens: 4096,
			Priority:  1,
		})
		registry.Register("claude", claudeProvider)
		logger.Info("registered provider", "name", "claude")
	} else {
		logger.Warn("ANTHROPIC_API_KEY not set, claude provider not registered")
	}

	ollamaURL := envOrDefault("OLLAMA_BASE_URL", "http://localhost:11434")
	ollamaProvider := proxy.NewOllamaProvider(proxy.ProviderConfig{
		Name:      "ollama",
		Type:      "ollama",
		BaseURL:   ollamaURL,
		Model:     envOrDefault("OLLAMA_MODEL", "qwen3:7b"),
		MaxTokens: 4096,
		Priority:  2,
	})
	registry.Register("ollama", ollamaProvider)
	logger.Info("registered provider", "name", "ollama")

	// 3b. Claude Code provider (subprocess, no API key required)
	if os.Getenv("CLAUDE_CODE_ENABLED") == "1" {
		claudeCodeProvider := proxy.NewClaudeCodeProvider(proxy.ProviderConfig{
			Name:    "claude-code",
			Type:    "claude-code",
			BaseURL: envOrDefault("CLAUDE_CODE_BINARY", "claude"), // binary path
			Model:   envOrDefault("CLAUDE_CODE_MODEL", "claude-opus-4-6"),
		}, logger)
		registry.Register("claude-code", claudeCodeProvider)
		logger.Info("registered provider", "name", "claude-code", "model", envOrDefault("CLAUDE_CODE_MODEL", "claude-opus-4-6"))
	}

	// 4. Control config (shared between pipeline + control plane)
	defaultProvider := "claude"
	if os.Getenv("CLAUDE_CODE_ENABLED") == "1" {
		defaultProvider = "claude-code"
	}
	controlConfig := control.NewConfig(defaultProvider)

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
	logger.Info("provider deadline configured", "deadline", providerDeadline)

	// 5a. Capabilities + TOML Loader + Compiler with 3-source Assembly
	caps := capability.New()
	agentsDir := envOrDefault("SENTINEL_AGENTS_DIR", "agents")
	tomlLoader := compiler.NewTOMLLoader(agentsDir)
	promptCompiler := compiler.NewWithAssembler(tomlLoader, caps)
	logger.Info("3-source assembly enabled", "agents_dir", agentsDir)

	// 5b. DriftDetector + QualityScorer for Pipeline Hardening (#144)
	driftDetector := judge.NewDriftDetector()
	loadAgentProfiles(tomlLoader, driftDetector, agentsDir, logger)
	qualityScorer := judge.NewQualityScorer(driftDetector)

	// 5c. InFlightMap for query lifecycle tracking
	inflightMap := resilience.NewInFlightMap(providerDeadline)
	go func() {
		ticker := time.NewTicker(5 * time.Second)
		defer ticker.Stop()
		for range ticker.C {
			if n := inflightMap.Prune(); n > 0 {
				logger.Debug("inflight prune", "pruned", n)
			}
		}
	}()
	logger.Info("inflight map enabled", "deadline", providerDeadline)

	pipelineHandler := proxy.NewPipelineHandler(proxy.PipelineConfig{
		Registry:         registry,
		Config:           controlConfig,
		Compiler:         promptCompiler,
		Normalizer:       normalizer.New(),
		Extractor:        extraction.New(),
		Capabilities:     caps,
		Logger:           logger,
		BreakerCfg:       proxy.BreakerConfigFromEnv(),
		EventStore:       evStore,
		Guardrails:       guardrailsEnforcer,
		InFlight:         inflightMap,
		ProviderDeadline: providerDeadline,
		Drift:            driftDetector,
		Quality:          qualityScorer,
	})

	// 6. HTTP proxy server
	proxyMux := http.NewServeMux()
	proxyMux.Handle("POST /v1/chat/completions", pipelineHandler)
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

func envOrDefault(key, defaultVal string) string {
	if val := os.Getenv(key); val != "" {
		return val
	}
	return defaultVal
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
