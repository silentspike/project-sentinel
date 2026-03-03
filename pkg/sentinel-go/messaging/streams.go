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
	StreamEBPF   = "SENTINEL_EBPF"
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

// EBPFStreamConfig returns the JetStream config for eBPF metrics (daemon bridge).
// Memory storage: eBPF metrics are ephemeral, no persistence needed.
// Retention: 1 day, 50MB max. Subjects: sentinel.ebpf.>
// ADR-001: Daemon bridges Zenoh eBPF topics to NATS for Go consumers (Judge).
func EBPFStreamConfig() jetstream.StreamConfig {
	return jetstream.StreamConfig{
		Name:        StreamEBPF,
		Description: "Sentinel eBPF metrics (daemon Zenoh→NATS bridge, ADR-001)",
		Subjects:    []string{"sentinel.ebpf.>"},
		Storage:     jetstream.MemoryStorage,
		Retention:   jetstream.LimitsPolicy,
		MaxAge:      24 * time.Hour, // 1 day
		MaxBytes:    50 << 20,       // 50 MB
		Replicas:    1,
	}
}

// EnsureStreams idempotently creates or updates all required JetStream streams.
func EnsureStreams(ctx context.Context, js jetstream.JetStream) error {
	configs := []jetstream.StreamConfig{
		EventsStreamConfig(),
		JudgeStreamConfig(),
		EBPFStreamConfig(),
	}
	for _, cfg := range configs {
		if _, err := js.CreateOrUpdateStream(ctx, cfg); err != nil {
			return fmt.Errorf("ensure stream %s: %w", cfg.Name, err)
		}
	}
	return nil
}
