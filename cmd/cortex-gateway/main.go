package main

import (
	"context"
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
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/eventstore"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/extraction"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/normalizer"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/observatory"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/proxy"
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

	// 4. Control config (shared between pipeline + control plane)
	controlConfig := control.NewConfig("claude")

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

	// 4c. Observatory (optional, enabled via config or SENTINEL_OBSERVATORY env)
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

	pipelineHandler := proxy.NewPipelineHandler(proxy.PipelineConfig{
		Registry:         registry,
		Config:           controlConfig,
		Compiler:         compiler.New(),
		Normalizer:       normalizer.New(),
		Extractor:        extraction.New(),
		Capabilities:     capability.New(),
		Logger:           logger,
		BreakerCfg:       proxy.BreakerConfigFromEnv(),
		EventStore:       evStore,
		ProviderDeadline: providerDeadline,
	})

	// 6. HTTP proxy server
	proxyMux := http.NewServeMux()
	proxyMux.Handle("POST /v1/chat/completions", pipelineHandler)
	proxyMux.HandleFunc("GET /health", handleHealth)
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

	// 7b. Register observatory routes on control plane (if enabled)
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

func handleHealth(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	_, _ = fmt.Fprintf(w, `{"status":"ok","version":%q}`, version)
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
