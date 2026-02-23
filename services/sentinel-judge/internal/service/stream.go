// Package service implements the judge's NATS streaming consumer and batch handler.
package service

import (
	"context"
	"encoding/json"
	"log/slog"
	"sync"
	"time"

	"github.com/nats-io/nats.go/jetstream"

	"github.com/obtFusi/project-sentinel/pkg/sentinel-go/judge"
	"github.com/obtFusi/project-sentinel/pkg/sentinel-go/messaging"
	"github.com/obtFusi/project-sentinel/services/sentinel-judge/internal/alerter"
	"github.com/obtFusi/project-sentinel/services/sentinel-judge/internal/config"
	"github.com/obtFusi/project-sentinel/services/sentinel-judge/internal/metrics"
	"github.com/obtFusi/project-sentinel/services/sentinel-judge/internal/persistence"
)

// StreamConsumer processes events from NATS JetStream in realtime.
type StreamConsumer struct {
	js       jetstream.JetStream
	cfg      *config.Config
	drift    *judge.DriftDetector
	quality  *judge.QualityScorer
	fatigue  *judge.FatigueDetector
	swap     *judge.SwapTrigger
	alerter  *alerter.Alerter
	evol     *persistence.EvolutionStore
	logger   *slog.Logger

	// Message buffer per agent for heuristic analysis
	mu       sync.RWMutex
	messages map[string][]string // agent_id -> recent messages
}

// NewStreamConsumer creates a new NATS streaming consumer for realtime heuristic analysis.
func NewStreamConsumer(
	js jetstream.JetStream,
	cfg *config.Config,
	evol *persistence.EvolutionStore,
	alerter *alerter.Alerter,
	logger *slog.Logger,
) *StreamConsumer {
	drift := judge.NewDriftDetector()
	return &StreamConsumer{
		js:       js,
		cfg:      cfg,
		drift:    drift,
		quality:  judge.NewQualityScorer(drift),
		fatigue:  judge.NewFatigueDetector(),
		swap:     judge.NewSwapTrigger(5, cfg.Thresholds.QualityAlertMinScore),
		alerter:  alerter,
		evol:     evol,
		logger:   logger,
		messages: make(map[string][]string),
	}
}

const maxMessagesPerAgent = 20

// Run starts the streaming consumer. Blocks until ctx is cancelled.
func (sc *StreamConsumer) Run(ctx context.Context) error {
	// Create or get durable pull consumer
	consumer, err := sc.js.CreateOrUpdateConsumer(ctx, messaging.StreamEvents, jetstream.ConsumerConfig{
		Durable:       sc.cfg.NATS.ConsumerName,
		AckPolicy:     jetstream.AckExplicitPolicy,
		MaxDeliver:    3,
		FilterSubjects: []string{
			"sentinel.events.agent_action_received.*",
			"sentinel.events.agent_chat.*",
		},
	})
	if err != nil {
		return err
	}

	sc.logger.Info("nats consumer started", "name", sc.cfg.NATS.ConsumerName)

	// Consume messages
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
			sc.logger.Error("consumer next error", "error", err)
			time.Sleep(time.Second)
			continue
		}

		sc.processMessage(msg)
	}
}

// processMessage handles a single NATS message through the heuristic pipeline.
func (sc *StreamConsumer) processMessage(msg jetstream.Msg) {
	metrics.EventsProcessed.Inc()

	// Parse event metadata from headers
	agentID := msg.Headers().Get("X-Aggregate-ID")
	if agentID == "" {
		// Try to parse from subject
		_, agentID, _ = messaging.ParseEventSubject(msg.Subject())
	}
	if agentID == "" {
		_ = msg.Ack()
		return
	}

	// Extract message content from payload (values can be strings, numbers, or null)
	var payload map[string]any
	if err := json.Unmarshal(msg.Data(), &payload); err != nil {
		sc.logger.Warn("failed to parse event payload", "error", err)
		_ = msg.Ack()
		return
	}
	content, _ := payload["content"].(string)
	if content == "" {
		content, _ = payload["msg"].(string)
	}
	if content == "" {
		_ = msg.Ack()
		return
	}

	// Buffer message for agent
	sc.mu.Lock()
	sc.messages[agentID] = append(sc.messages[agentID], content)
	if len(sc.messages[agentID]) > maxMessagesPerAgent {
		sc.messages[agentID] = sc.messages[agentID][len(sc.messages[agentID])-maxMessagesPerAgent:]
	}
	recentMessages := make([]string, len(sc.messages[agentID]))
	copy(recentMessages, sc.messages[agentID])
	sc.mu.Unlock()

	// Run heuristic pipeline
	sc.runHeuristics(agentID, content, recentMessages)

	_ = msg.Ack()
}

// runHeuristics executes the 4-algorithm heuristic pipeline on agent messages.
func (sc *StreamConsumer) runHeuristics(agentID, latestMessage string, recentMessages []string) {
	// 1. Drift Detection
	driftResult := sc.drift.CheckDrift(agentID, recentMessages)
	metrics.DriftScore.WithLabelValues(agentID).Set(driftResult.DriftScore)

	if severityAtLeast(driftResult.Severity, sc.cfg.Thresholds.DriftAlertSeverity) {
		sc.alerter.Emit(alerter.Alert{
			AgentID:  agentID,
			Type:     "drift",
			Severity: driftResult.Severity,
			Score:    driftResult.DriftScore,
			Details:  driftResult.Details,
		})
	}

	// 2. Quality Scoring
	qualityResult := sc.quality.ScoreMessage(agentID, latestMessage, recentMessages)
	metrics.QualityScore.WithLabelValues(agentID).Set(float64(qualityResult.Score))

	if qualityResult.Score <= sc.cfg.Thresholds.QualityAlertMinScore {
		sc.alerter.Emit(alerter.Alert{
			AgentID:  agentID,
			Type:     "quality",
			Severity: "mild",
			Score:    float64(qualityResult.Score),
			Details:  qualityResult.Details,
		})
	}

	// 3. Fatigue Detection
	fatigueResult := sc.fatigue.CheckFatigue(agentID, recentMessages)
	metrics.FatigueScore.WithLabelValues(agentID).Set(fatigueResult.FatigueScore)

	if fatigueResult.FatigueScore >= sc.cfg.Thresholds.FatigueAlertMinScore {
		sc.alerter.Emit(alerter.Alert{
			AgentID:  agentID,
			Type:     "fatigue",
			Severity: fatigueLevel(fatigueResult.FatigueScore),
			Score:    fatigueResult.FatigueScore,
			Details:  fatigueResult.Details,
		})
	}

	// 4. Swap Decision
	sc.swap.RecordScore(agentID, qualityResult.Score)
	swapDecision := sc.swap.ShouldSwap(agentID)
	if swapDecision.ShouldSwap {
		sc.alerter.Emit(alerter.Alert{
			AgentID:  agentID,
			Type:     "swap",
			Severity: "moderate",
			Score:    0,
			Details:  swapDecision.Reason,
		})
	}
}

// severityAtLeast checks if actual severity meets or exceeds the threshold.
func severityAtLeast(actual, threshold string) bool {
	levels := map[string]int{"none": 0, "mild": 1, "moderate": 2, "critical": 3}
	return levels[actual] >= levels[threshold]
}

// fatigueLevel maps a fatigue score to a severity level.
func fatigueLevel(score float64) string {
	if score >= 0.8 {
		return "critical"
	}
	if score >= 0.6 {
		return "moderate"
	}
	return "mild"
}
