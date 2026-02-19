package messaging

import (
	"context"
	"fmt"
	"time"

	"github.com/nats-io/nats.go/jetstream"
)

// Stream names (SSOT — all Go services reference these constants).
const (
	StreamEvents = "SENTINEL_EVENTS"
	StreamJudge  = "SENTINEL_JUDGE"
)

// EventsStreamConfig returns the JetStream config for the main event stream.
// Retention: 7 days, 1GB max. Subjects: sentinel.events.>
func EventsStreamConfig() jetstream.StreamConfig {
	return jetstream.StreamConfig{
		Name:        StreamEvents,
		Description: "Sentinel domain events (Limbo mirror)",
		Subjects:    []string{"sentinel.events.>"},
		Storage:     jetstream.FileStorage,
		Retention:   jetstream.LimitsPolicy,
		MaxAge:      7 * 24 * time.Hour, // 7 days
		MaxBytes:    1 << 30,            // 1 GB
		Replicas:    1,
		Duplicates:  10 * time.Minute, // dedup window for Nats-Msg-Id
	}
}

// JudgeStreamConfig returns the JetStream config for judge alerts/results.
// Retention: 30 days, 100MB max. Subjects: sentinel.judge.>
func JudgeStreamConfig() jetstream.StreamConfig {
	return jetstream.StreamConfig{
		Name:        StreamJudge,
		Description: "Sentinel judge alerts and results",
		Subjects:    []string{"sentinel.judge.>"},
		Storage:     jetstream.FileStorage,
		Retention:   jetstream.LimitsPolicy,
		MaxAge:      30 * 24 * time.Hour, // 30 days
		MaxBytes:    100 << 20,           // 100 MB
		Replicas:    1,
		Duplicates:  10 * time.Minute,
	}
}

// EnsureStreams idempotently creates or updates all required JetStream streams.
func EnsureStreams(ctx context.Context, js jetstream.JetStream) error {
	configs := []jetstream.StreamConfig{
		EventsStreamConfig(),
		JudgeStreamConfig(),
	}
	for _, cfg := range configs {
		if _, err := js.CreateOrUpdateStream(ctx, cfg); err != nil {
			return fmt.Errorf("ensure stream %s: %w", cfg.Name, err)
		}
	}
	return nil
}
