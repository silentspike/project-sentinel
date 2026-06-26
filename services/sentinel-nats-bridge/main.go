// sentinel-nats-bridge polls the Limbo event store and publishes events to
// NATS JetStream. Temporary service — will be replaced by a Rust daemon
// Zenoh→NATS bridge (async-nats) in Phase 2.
package main

import (
	"context"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"syscall"
	"time"

	"github.com/BurntSushi/toml"
	"github.com/nats-io/nats.go"

	"github.com/silentspike/project-sentinel/pkg/sentinel-go/eventstore"
	"github.com/silentspike/project-sentinel/pkg/sentinel-go/messaging"
)

// Config holds bridge configuration (loaded from TOML).
type Config struct {
	EventStore struct {
		Path           string `toml:"path"`
		PollIntervalMs int    `toml:"poll_interval_ms"`
		BatchSize      int    `toml:"batch_size"`
	} `toml:"eventstore"`
	NATS struct {
		URL string `toml:"url"`
	} `toml:"nats"`
	Server struct {
		HealthPort     int    `toml:"health_port"`
		HealthBindAddr string `toml:"health_bind_addr"`
	} `toml:"server"`
}

func main() {
	logger := slog.New(slog.NewJSONHandler(os.Stdout, &slog.HandlerOptions{Level: slog.LevelInfo}))
	slog.SetDefault(logger)

	// Load config
	configPath := envOrDefault("SENTINEL_BRIDGE_CONFIG", "config/nats-bridge.toml")
	var cfg Config
	if _, err := toml.DecodeFile(configPath, &cfg); err != nil {
		logger.Error("failed to load config", "path", configPath, "error", err)
		os.Exit(1)
	}

	// Override from env
	if v := os.Getenv("SENTINEL_BRIDGE_EVENT_STORE_PATH"); v != "" {
		cfg.EventStore.Path = v
	}
	if v := os.Getenv("SENTINEL_BRIDGE_NATS_URL"); v != "" {
		cfg.NATS.URL = v
	}

	if cfg.EventStore.PollIntervalMs <= 0 {
		cfg.EventStore.PollIntervalMs = 1000
	}
	if cfg.EventStore.BatchSize <= 0 {
		cfg.EventStore.BatchSize = 100
	}
	if cfg.Server.HealthPort <= 0 {
		cfg.Server.HealthPort = 8083
	}
	// #525: loopback secure default for the health endpoint. An explicit
	// health_bind_addr overrides; empty -> loopback with the configured
	// health_port (no hardcoded 8083 — ORC Finding 2).
	if cfg.Server.HealthBindAddr == "" {
		cfg.Server.HealthBindAddr = fmt.Sprintf("127.0.0.1:%d", cfg.Server.HealthPort)
	}

	logger.Info("sentinel-nats-bridge starting",
		"event_store", cfg.EventStore.Path,
		"nats_url", cfg.NATS.URL,
		"poll_interval_ms", cfg.EventStore.PollIntervalMs,
		"batch_size", cfg.EventStore.BatchSize,
	)

	// Open event store (read-only consumer)
	store, err := eventstore.Open(cfg.EventStore.Path)
	if err != nil {
		logger.Error("failed to open event store", "error", err)
		os.Exit(1)
	}
	defer func() { _ = store.Close() }()

	// Ensure outbox migration (adds retry_count, last_error columns if missing)
	if err := store.EnsureOutboxMigration(); err != nil {
		logger.Error("failed to migrate outbox schema", "error", err)
		os.Exit(1)
	}

	// Connect to NATS
	nc, err := messaging.Connect(messaging.ConnectOpts{
		URL:    cfg.NATS.URL,
		Name:   "sentinel-nats-bridge",
		Logger: logger,
	})
	if err != nil {
		logger.Error("failed to connect to nats", "error", err)
		os.Exit(1)
	}
	defer nc.Close()

	// Ensure streams exist
	js, err := messaging.JetStream(nc)
	if err != nil {
		logger.Error("failed to init jetstream", "error", err)
		os.Exit(1)
	}
	if err := messaging.EnsureStreams(context.Background(), js); err != nil {
		logger.Error("failed to ensure streams", "error", err)
		os.Exit(1)
	}
	logger.Info("nats streams ensured")

	// Health endpoint
	healthMux := http.NewServeMux()
	healthMux.HandleFunc("GET /health", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		fmt.Fprint(w, `{"status":"ok","service":"sentinel-nats-bridge"}`)
	})
	healthMux.HandleFunc("GET /ready", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		if !nc.IsConnected() {
			w.WriteHeader(http.StatusServiceUnavailable)
			fmt.Fprint(w, `{"status":"not_ready","reason":"nats disconnected"}`)
			return
		}
		fmt.Fprint(w, `{"status":"ok","service":"sentinel-nats-bridge"}`)
	})
	healthServer := &http.Server{
		Addr:         cfg.Server.HealthBindAddr,
		Handler:      healthMux,
		ReadTimeout:  5 * time.Second,
		WriteTimeout: 5 * time.Second,
	}
	go func() {
		logger.Info("health server starting", "port", cfg.Server.HealthPort)
		if err := healthServer.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			logger.Error("health server failed", "error", err)
		}
	}()

	// Poll loop
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	go pollLoop(ctx, logger, store, nc, cfg)

	// Wait for shutdown signal
	quit := make(chan os.Signal, 1)
	signal.Notify(quit, syscall.SIGINT, syscall.SIGTERM)
	sig := <-quit
	logger.Info("shutdown signal received", "signal", sig.String())

	cancel()
	shutCtx, shutCancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer shutCancel()
	_ = healthServer.Shutdown(shutCtx)

	// Drain NATS connection (flush pending publishes)
	if err := nc.Drain(); err != nil {
		logger.Warn("nats drain error", "error", err)
	}

	logger.Info("sentinel-nats-bridge stopped")
}

// maxRetries is the maximum number of publish attempts before marking an outbox entry as failed.
const maxRetries = 5

// pollLoop continuously polls the outbox for pending entries and publishes them to NATS.
func pollLoop(ctx context.Context, logger *slog.Logger, store *eventstore.Store, nc *nats.Conn, cfg Config) {
	ticker := time.NewTicker(time.Duration(cfg.EventStore.PollIntervalMs) * time.Millisecond)
	defer ticker.Stop()

	totalPublished := int64(0)

	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			entries, err := store.GetOutboxBatch(cfg.EventStore.BatchSize)
			if err != nil {
				logger.Error("outbox poll failed", "error", err)
				continue
			}
			if len(entries) == 0 {
				continue
			}

			var publishedIDs []int64
			for _, entry := range entries {
				subject := messaging.BuildEventSubject(entry.EventType, entry.AggregateID)

				msg := &nats.Msg{
					Subject: subject,
					Data:    []byte(entry.Payload),
					Header:  nats.Header{},
				}
				msg.Header.Set("Nats-Msg-Id", entry.OperationID)
				msg.Header.Set("X-Event-ID", entry.EventID)
				msg.Header.Set("X-Event-Type", entry.EventType)
				msg.Header.Set("X-Aggregate-ID", entry.AggregateID)
				msg.Header.Set("X-Tick", strconv.FormatInt(entry.Tick, 10))
				msg.Header.Set("X-Correlation-ID", entry.CorrelationID)

				if err := nc.PublishMsg(msg); err != nil {
					logger.Error("publish failed",
						"subject", subject,
						"outbox_id", entry.OutboxID,
						"retry", entry.RetryCount,
						"error", err,
					)
					if entry.RetryCount+1 >= maxRetries {
						if markErr := store.MarkFailed(entry.OutboxID); markErr != nil {
							logger.Error("mark failed error", "outbox_id", entry.OutboxID, "error", markErr)
						}
					} else {
						if markErr := store.MarkRetry(entry.OutboxID, err.Error()); markErr != nil {
							logger.Error("mark retry error", "outbox_id", entry.OutboxID, "error", markErr)
						}
					}
					continue
				}
				publishedIDs = append(publishedIDs, entry.OutboxID)
			}

			if len(publishedIDs) > 0 {
				if err := store.MarkPublished(publishedIDs); err != nil {
					logger.Error("mark published failed", "count", len(publishedIDs), "error", err)
				}
				totalPublished += int64(len(publishedIDs))
				logger.Info("outbox entries published",
					"count", len(publishedIDs),
					"total", totalPublished,
				)
			}
		}
	}
}

func envOrDefault(key, defaultVal string) string {
	if val := os.Getenv(key); val != "" {
		return val
	}
	return defaultVal
}
