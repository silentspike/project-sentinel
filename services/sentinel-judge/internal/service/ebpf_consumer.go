// Package service — eBPF metrics consumer (ADR-001).
//
// Subscribes to the SENTINEL_EBPF JetStream stream (daemon Zenoh→NATS bridge)
// and maintains per-agent eBPF state for heuristic pipeline enrichment.
package service

import (
	"context"
	"encoding/json"
	"log/slog"
	"sync"
	"time"

	"github.com/nats-io/nats.go/jetstream"

	"github.com/obtFusi/project-sentinel/pkg/sentinel-go/messaging"
	"github.com/obtFusi/project-sentinel/services/sentinel-judge/internal/config"
	"github.com/obtFusi/project-sentinel/services/sentinel-judge/internal/metrics"
)

// EBPFState holds the latest eBPF signal for a single agent.
type EBPFState struct {
	Stalled   bool
	StallMs   uint64
	IOReadB   uint64
	IOWriteB  uint64
	UpdatedAt time.Time
}

// EBPFStore is a thread-safe store for per-agent eBPF state.
// Written by EBPFConsumer, read by StreamConsumer.runHeuristics().
type EBPFStore struct {
	mu    sync.RWMutex
	state map[string]EBPFState // agent_id -> EBPFState
}

// NewEBPFStore creates an empty eBPF state store.
func NewEBPFStore() *EBPFStore {
	return &EBPFStore{state: make(map[string]EBPFState)}
}

// Get returns the eBPF state for an agent (zero-value if unknown).
func (s *EBPFStore) Get(agentID string) EBPFState {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.state[agentID]
}

// set updates the eBPF state for an agent.
func (s *EBPFStore) set(agentID string, st EBPFState) {
	s.mu.Lock()
	s.state[agentID] = st
	s.mu.Unlock()
}

// StalledAgent mirrors the Rust StalledAgent struct published on sentinel.ebpf.agent-health.
type StalledAgent struct {
	AgentID      string `json:"agent_id"`
	StallCount   uint64 `json:"stall_count"`
	TotalStallMs uint64 `json:"total_stall_ms"`
}

// EBPFConsumer subscribes to SENTINEL_EBPF and updates the shared EBPFStore.
type EBPFConsumer struct {
	js     jetstream.JetStream
	cfg    *config.Config
	store  *EBPFStore
	logger *slog.Logger
}

// NewEBPFConsumer creates a new eBPF metrics consumer.
func NewEBPFConsumer(
	js jetstream.JetStream,
	cfg *config.Config,
	store *EBPFStore,
	logger *slog.Logger,
) *EBPFConsumer {
	return &EBPFConsumer{js: js, cfg: cfg, store: store, logger: logger}
}

// Run starts the eBPF consumer. Blocks until ctx is cancelled.
func (ec *EBPFConsumer) Run(ctx context.Context) error {
	consumer, err := ec.js.CreateOrUpdateConsumer(ctx, messaging.StreamEBPF, jetstream.ConsumerConfig{
		Durable:       ec.cfg.EBPF.ConsumerName,
		AckPolicy:     jetstream.AckExplicitPolicy,
		FilterSubject: "sentinel.ebpf.agent-health",
		MaxDeliver:    3,
	})
	if err != nil {
		return err
	}

	ec.logger.Info("ebpf consumer started",
		"stream", messaging.StreamEBPF,
		"consumer", ec.cfg.EBPF.ConsumerName,
	)

	iter, err := consumer.Messages(jetstream.PullMaxMessages(10))
	if err != nil {
		return err
	}
	defer iter.Stop()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}

		msg, err := iter.Next()
		if err != nil {
			if ctx.Err() != nil {
				return ctx.Err()
			}
			ec.logger.Error("ebpf consumer next error", "error", err)
			time.Sleep(time.Second)
			continue
		}

		ec.processAgentHealth(msg)
	}
}

// processAgentHealth parses a stalled-agents payload and updates the EBPFStore.
func (ec *EBPFConsumer) processAgentHealth(msg jetstream.Msg) {
	metrics.EBPFEventsProcessed.Inc()

	var agents []StalledAgent
	if err := json.Unmarshal(msg.Data(), &agents); err != nil {
		ec.logger.Warn("ebpf agent-health parse error", "error", err)
		_ = msg.Ack()
		return
	}

	now := time.Now()
	threshold := uint64(ec.cfg.EBPF.StallThresholdMs)

	for _, a := range agents {
		stalled := a.TotalStallMs >= threshold
		ec.store.set(a.AgentID, EBPFState{
			Stalled:   stalled,
			StallMs:   a.TotalStallMs,
			UpdatedAt: now,
		})

		metrics.EBPFStallCount.WithLabelValues(a.AgentID).Set(float64(a.StallCount))

		if stalled {
			ec.logger.Info("agent stalled (eBPF)",
				"agent", a.AgentID,
				"stall_ms", a.TotalStallMs,
				"stall_count", a.StallCount,
			)
		}
	}

	_ = msg.Ack()
}
