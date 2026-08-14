// sentinel-nats-bridge polls the Limbo event store and publishes events to
// NATS JetStream. Temporary service — will be replaced by a Rust daemon
// Zenoh→NATS bridge (async-nats) in Phase 2.
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
	"sync/atomic"
	"syscall"
	"time"

	"github.com/BurntSushi/toml"
	"github.com/nats-io/nats.go"
	"github.com/nats-io/nats.go/jetstream"

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

	readiness := &readinessState{}
	healthMux := newHealthHandler(store, nc, readiness)
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

	go pollLoop(ctx, logger, store, js, readiness, cfg)

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

type outboxStore interface {
	GetOutboxBatch(limit int) ([]eventstore.OutboxPublishEntry, error)
	MarkPublishedCAS(id int64, eventID, operationID string) error
	MarkRetryCAS(id int64, eventID, operationID, reason string) error
	MarkFailedCAS(id int64, eventID, operationID, reason string) error
	OutboxCounts() (eventstore.OutboxStatusCounts, error)
}

type jetStreamPublisher interface {
	PublishMsg(context.Context, *nats.Msg, ...jetstream.PublishOpt) (*jetstream.PubAck, error)
}

type natsConnectionState interface {
	IsConnected() bool
}

type readinessState struct {
	initialScanComplete atomic.Bool
}

type readinessResponse struct {
	Status       string `json:"status"`
	Service      string `json:"service"`
	Reason       string `json:"reason,omitempty"`
	Pending      int64  `json:"pending,omitempty"`
	Failed       int64  `json:"failed,omitempty"`
	NonPublished int64  `json:"nonpublished,omitempty"`
}

func newHealthHandler(
	store outboxStore,
	connection natsConnectionState,
	readiness *readinessState,
) http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /health", func(w http.ResponseWriter, _ *http.Request) {
		writeReadinessJSON(w, http.StatusOK, readinessResponse{
			Status:  "ok",
			Service: "sentinel-nats-bridge",
		})
	})
	mux.HandleFunc("GET /ready", func(w http.ResponseWriter, _ *http.Request) {
		status, response := currentReadiness(store, connection, readiness)
		writeReadinessJSON(w, status, response)
	})
	return mux
}

func currentReadiness(
	store outboxStore,
	connection natsConnectionState,
	readiness *readinessState,
) (int, readinessResponse) {
	response := readinessResponse{Status: "not_ready", Service: "sentinel-nats-bridge"}
	if !connection.IsConnected() {
		response.Reason = "nats_disconnected"
		return http.StatusServiceUnavailable, response
	}
	if !readiness.initialScanComplete.Load() {
		response.Reason = "initial_scan_pending"
		return http.StatusServiceUnavailable, response
	}
	counts, err := store.OutboxCounts()
	if err != nil {
		response.Reason = "outbox_status_unavailable"
		return http.StatusServiceUnavailable, response
	}
	response.Pending = counts.Pending
	response.Failed = counts.Failed
	response.NonPublished = counts.NonPublished
	switch {
	case counts.Failed != 0:
		response.Reason = "outbox_failed"
	case counts.Pending != 0:
		response.Reason = "outbox_pending"
	case counts.NonPublished != 0:
		response.Reason = "outbox_nonpublished"
	default:
		response.Status = "ok"
		return http.StatusOK, response
	}
	return http.StatusServiceUnavailable, response
}

func writeReadinessJSON(w http.ResponseWriter, status int, response readinessResponse) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	if err := json.NewEncoder(w).Encode(response); err != nil {
		slog.Error("health response encode failed", "error", err)
	}
}

// pollLoop drains immediately on startup and then retries after each poll
// interval. A sweep never continues past an uncertain store or broker effect.
func pollLoop(
	ctx context.Context,
	logger *slog.Logger,
	store outboxStore,
	publisher jetStreamPublisher,
	readiness *readinessState,
	cfg Config,
) {
	ticker := time.NewTicker(time.Duration(cfg.EventStore.PollIntervalMs) * time.Millisecond)
	defer ticker.Stop()

	totalPublished := int64(0)

	for {
		published, err := drainOutbox(ctx, store, publisher, readiness, cfg.EventStore.BatchSize)
		totalPublished += int64(published)
		if published != 0 {
			logger.Info("outbox entries published", "count", published, "total", totalPublished)
		}
		if err != nil && ctx.Err() == nil {
			logger.Error("outbox drain stopped", "error", err)
		}
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}

func drainOutbox(
	ctx context.Context,
	store outboxStore,
	publisher jetStreamPublisher,
	readiness *readinessState,
	batchSize int,
) (int, error) {
	totalPublished := 0
	for {
		if err := ctx.Err(); err != nil {
			return totalPublished, err
		}
		entries, err := store.GetOutboxBatch(batchSize)
		if err != nil {
			return totalPublished, fmt.Errorf("get outbox batch: %w", err)
		}
		if len(entries) == 0 {
			readiness.initialScanComplete.Store(true)
			return totalPublished, nil
		}
		for _, entry := range entries {
			if err := ctx.Err(); err != nil {
				return totalPublished, err
			}
			msg := buildPublishMessage(entry)
			ack, err := publisher.PublishMsg(ctx, msg)
			if err != nil {
				if transitionErr := markPublishFailure(store, entry); transitionErr != nil {
					return totalPublished, fmt.Errorf("publish failed and retry transition failed: %w", transitionErr)
				}
				return totalPublished, fmt.Errorf("JetStream publish failed: %w", err)
			}
			if ack == nil {
				return totalPublished, fmt.Errorf("JetStream publish returned no PubAck")
			}
			if err := store.MarkPublishedCAS(entry.OutboxID, entry.EventID, entry.OperationID); err != nil {
				return totalPublished, fmt.Errorf("PubAck adoption failed: %w", err)
			}
			totalPublished++
		}
	}
}

func markPublishFailure(store outboxStore, entry eventstore.OutboxPublishEntry) error {
	const reason = "jetstream_publish_failed"
	if entry.RetryCount+1 >= maxRetries {
		return store.MarkFailedCAS(entry.OutboxID, entry.EventID, entry.OperationID, reason)
	}
	return store.MarkRetryCAS(entry.OutboxID, entry.EventID, entry.OperationID, reason)
}

func buildPublishMessage(entry eventstore.OutboxPublishEntry) *nats.Msg {
	subject := messaging.BuildEventSubject(entry.EventType, entry.AggregateID)
	msg := &nats.Msg{Subject: subject, Data: []byte(entry.Payload), Header: nats.Header{}}
	msg.Header.Set("Nats-Msg-Id", entry.OperationID)
	msg.Header.Set("X-Event-ID", entry.EventID)
	msg.Header.Set("X-Event-Type", entry.EventType)
	msg.Header.Set("X-Aggregate-ID", entry.AggregateID)
	msg.Header.Set("X-Tick", strconv.FormatInt(entry.Tick, 10))
	msg.Header.Set("X-Correlation-ID", entry.CorrelationID)
	return msg
}

func envOrDefault(key, defaultVal string) string {
	if val := os.Getenv(key); val != "" {
		return val
	}
	return defaultVal
}
