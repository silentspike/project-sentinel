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

	"github.com/obtFusi/project-sentinel/pkg/sentinel-go/eventstore"
	"github.com/obtFusi/project-sentinel/pkg/sentinel-go/messaging"
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
		HealthPort int `toml:"health_port"`
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
	healthServer := &http.Server{
		Addr:         ":" + strconv.Itoa(cfg.Server.HealthPort),
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

// pollLoop continuously polls Limbo for new events and publishes them to NATS.
func pollLoop(ctx context.Context, logger *slog.Logger, store *eventstore.Store, nc *nats.Conn, cfg Config) {
	ticker := time.NewTicker(time.Duration(cfg.EventStore.PollIntervalMs) * time.Millisecond)
	defer ticker.Stop()

	var lastID int64
	totalPublished := int64(0)

	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			events, maxID, err := store.GetEventsSince(lastID, cfg.EventStore.BatchSize)
			if err != nil {
				logger.Error("poll failed", "error", err, "last_id", lastID)
				continue
			}
			if len(events) == 0 {
				continue
			}

			published := 0
			for _, evt := range events {
				subject := messaging.BuildEventSubject(evt.EventType, evt.AggregateID)

				// Nats-Msg-Id header for exactly-once dedup in JetStream
				msg := &nats.Msg{
					Subject: subject,
					Data:    []byte(evt.Payload),
					Header:  nats.Header{},
				}
				msg.Header.Set("Nats-Msg-Id", evt.OperationID)
				msg.Header.Set("X-Event-ID", evt.EventID)
				msg.Header.Set("X-Event-Type", evt.EventType)
				msg.Header.Set("X-Aggregate-ID", evt.AggregateID)
				msg.Header.Set("X-Tick", strconv.FormatInt(evt.Tick, 10))
				msg.Header.Set("X-Correlation-ID", evt.CorrelationID)

				if err := nc.PublishMsg(msg); err != nil {
					logger.Error("publish failed", "subject", subject, "error", err)
					continue
				}
				published++
			}

			lastID = maxID
			totalPublished += int64(published)

			if published > 0 {
				logger.Info("events published",
					"count", published,
					"last_id", lastID,
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
