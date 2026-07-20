// sentinel-judge is the quality analysis service for Project Sentinel.
// It consumes events via NATS JetStream, runs heuristic + LLM analysis,
// and provides a batch API for the Night-Run pipeline.
package main

import (
	"context"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/silentspike/project-sentinel/pkg/sentinel-go/messaging"
	"github.com/silentspike/project-sentinel/services/sentinel-judge/api"
	"github.com/silentspike/project-sentinel/services/sentinel-judge/internal/alerter"
	"github.com/silentspike/project-sentinel/services/sentinel-judge/internal/analyzer"
	"github.com/silentspike/project-sentinel/services/sentinel-judge/internal/config"
	"github.com/silentspike/project-sentinel/services/sentinel-judge/internal/gateway"
	"github.com/silentspike/project-sentinel/services/sentinel-judge/internal/persistence"
	"github.com/silentspike/project-sentinel/services/sentinel-judge/internal/service"
)

func main() {
	logger := slog.New(slog.NewJSONHandler(os.Stdout, &slog.HandlerOptions{Level: slog.LevelInfo}))
	slog.SetDefault(logger)

	// Load config
	configPath := envOrDefault("SENTINEL_JUDGE_CONFIG", "config/judge.toml")
	cfg, err := config.Load(configPath)
	if err != nil {
		logger.Error("failed to load config", "path", configPath, "error", err)
		os.Exit(1)
	}

	// Override from env
	if v := os.Getenv("SENTINEL_JUDGE_NATS_URL"); v != "" {
		cfg.NATS.URL = v
	}
	if v := os.Getenv("SENTINEL_JUDGE_EVOLUTION_PATH"); v != "" {
		cfg.Evolution.Path = v
	}
	if v := os.Getenv("SENTINEL_JUDGE_GATEWAY_URL"); v != "" {
		cfg.Gateway.URL = v
	}

	logger.Info("sentinel-judge starting",
		"port", cfg.Server.Port,
		"nats", cfg.NATS.URL,
		"evolution_db", cfg.Evolution.Path,
		"gateway", cfg.Gateway.URL,
	)

	// Open evolution store
	evol, err := persistence.OpenEvolution(cfg.Evolution.Path)
	if err != nil {
		logger.Error("failed to open evolution store", "error", err)
		os.Exit(1)
	}
	defer func() { _ = evol.Close() }()

	// Connect to NATS
	nc, err := messaging.Connect(messaging.ConnectOpts{
		URL:    cfg.NATS.URL,
		Name:   "sentinel-judge",
		Logger: logger,
	})
	if err != nil {
		logger.Error("failed to connect to nats", "error", err)
		os.Exit(1)
	}
	defer nc.Close()

	js, err := messaging.JetStream(nc)
	if err != nil {
		logger.Error("failed to init jetstream", "error", err)
		os.Exit(1)
	}

	// Ensure streams exist
	if err := messaging.EnsureStreams(context.Background(), js); err != nil {
		logger.Error("failed to ensure streams", "error", err)
		os.Exit(1)
	}

	// Create alerter
	alert := alerter.New(nc, logger)

	// Create gateway client for LLM analysis
	gwClient := gateway.NewClient(gateway.ClientConfig{
		URL:         cfg.Gateway.URL,
		Model:       cfg.Gateway.Model,
		Temperature: cfg.Gateway.Temperature,
		MaxTokens:   cfg.Gateway.MaxTokens,
		Timeout:     time.Duration(cfg.Gateway.TimeoutSeconds) * time.Second,
		Credential:  mustReadCredential("SENTINEL_JUDGE_CREDENTIAL_FILE", logger),
	})

	// Create analyzer (LLM-based)
	llmAnalyzer := analyzer.New(gwClient, evol, logger)

	// Create batch handler (for Night-Run HTTP API)
	batchHandler := service.NewBatchHandler(llmAnalyzer, cfg, logger)

	// Create HTTP handler
	httpHandler := api.NewHandler(batchHandler, logger)

	// Create shared eBPF state store (ADR-001: daemon bridges eBPF→NATS)
	ebpfStore := service.NewEBPFStore()

	// Create streaming consumer (NATS realtime heuristic)
	streamConsumer := service.NewStreamConsumer(js, cfg, evol, alert, ebpfStore, logger)

	// Load agent personality profiles for drift detection
	if cfg.Agents.ConfigDir != "" {
		n, err := service.LoadProfiles(cfg.Agents.ConfigDir, streamConsumer.DriftDetector(), logger)
		if err != nil {
			logger.Error("failed to load agent profiles", "dir", cfg.Agents.ConfigDir, "error", err)
			os.Exit(1)
		}
		logger.Info("agent profiles loaded", "count", n, "dir", cfg.Agents.ConfigDir)
	} else {
		logger.Warn("no agents.config_dir set, drift detection will report 0 for all agents")
	}

	// HTTP server
	mux := http.NewServeMux()
	httpHandler.RegisterRoutes(mux)

	httpServer := &http.Server{
		Addr:         ":" + strconv.Itoa(cfg.Server.Port),
		Handler:      mux,
		ReadTimeout:  30 * time.Second,
		WriteTimeout: 60 * time.Second,
		IdleTimeout:  120 * time.Second,
	}

	// Start HTTP server
	go func() {
		logger.Info("http server starting", "port", cfg.Server.Port)
		if err := httpServer.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			logger.Error("http server failed", "error", err)
			os.Exit(1)
		}
	}()

	// Start NATS streaming consumer
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	go func() {
		httpHandler.SetReady(true)
		if err := streamConsumer.Run(ctx); err != nil && ctx.Err() == nil {
			logger.Error("stream consumer failed", "error", err)
		}
	}()

	// Start eBPF consumer if enabled (ADR-001)
	if cfg.EBPF.Enabled {
		ebpfConsumer := service.NewEBPFConsumer(js, cfg, ebpfStore, logger)
		go func() {
			if err := ebpfConsumer.Run(ctx); err != nil && ctx.Err() == nil {
				logger.Error("ebpf consumer failed", "error", err)
			}
		}()
		logger.Info("ebpf consumer enabled",
			"consumer", cfg.EBPF.ConsumerName,
			"stall_threshold_ms", cfg.EBPF.StallThresholdMs,
		)
	}

	logger.Info("sentinel-judge ready",
		"http", fmt.Sprintf(":%d", cfg.Server.Port),
		"nats_consumer", cfg.NATS.ConsumerName,
		"ebpf_enabled", cfg.EBPF.Enabled,
	)

	// Wait for shutdown signal
	quit := make(chan os.Signal, 1)
	signal.Notify(quit, syscall.SIGINT, syscall.SIGTERM)
	sig := <-quit
	logger.Info("shutdown signal received", "signal", sig.String())

	// Graceful shutdown
	cancel() // stop NATS consumer

	shutCtx, shutCancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer shutCancel()
	if err := httpServer.Shutdown(shutCtx); err != nil {
		logger.Error("http server shutdown error", "error", err)
	}

	// Drain NATS
	if err := nc.Drain(); err != nil {
		logger.Warn("nats drain error", "error", err)
	}

	logger.Info("sentinel-judge stopped")
}

func envOrDefault(key, defaultVal string) string {
	if val := os.Getenv(key); val != "" {
		return val
	}
	return defaultVal
}

func mustReadCredential(envName string, logger *slog.Logger) string {
	path := os.Getenv(envName)
	credential, err := readCredentialFile(path)
	if err != nil || path == "" {
		logger.Error("required caller credential unavailable", "credential_env", envName)
		os.Exit(1)
	}
	return credential
}

func readCredentialFile(path string) (string, error) {
	info, err := os.Stat(path) //nolint:gosec // operator-provided systemd credential path
	if err != nil {
		return "", err
	}
	if !info.Mode().IsRegular() || !secureCredentialFile(path, info) {
		return "", fmt.Errorf("credential must be an owner-only regular file")
	}
	value, err := os.ReadFile(path) //nolint:gosec // operator-provided systemd credential path
	if err != nil {
		return "", err
	}
	credential := string(value)
	if strings.HasSuffix(credential, "\r\n") {
		credential = strings.TrimSuffix(credential, "\r\n")
	} else {
		credential = strings.TrimSuffix(credential, "\n")
	}
	if credential == "" || strings.TrimSpace(credential) != credential {
		return "", fmt.Errorf("credential is empty or contains surrounding whitespace")
	}
	return credential, nil
}

func secureCredentialFile(path string, info os.FileInfo) bool {
	if info.Mode().Perm()&0o077 == 0 {
		return true
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok {
		return false
	}
	return secureCredentialMode(
		info.Mode().Perm(), stat.Uid, stat.Gid, path, os.Getenv("CREDENTIALS_DIRECTORY"),
	)
}

func secureCredentialMode(mode os.FileMode, uid, gid uint32, path, credentialsDirectory string) bool {
	return mode == 0o440 && uid == 0 && gid == 0 && credentialsDirectory != "" &&
		filepath.Clean(filepath.Dir(path)) == filepath.Clean(credentialsDirectory)
}
