package analyzer

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"strings"
	"time"

	"github.com/obtFusi/project-sentinel/services/sentinel-judge/internal/gateway"
	"github.com/obtFusi/project-sentinel/services/sentinel-judge/internal/metrics"
	"github.com/obtFusi/project-sentinel/services/sentinel-judge/internal/persistence"
)

// VoiceResult is the JSON structure returned by the LLM for voice analysis.
type VoiceResult struct {
	Phrases       []string `json:"phrases"`
	SentenceStyle string   `json:"sentence_style"`
	Formality     float64  `json:"formality"`
}

// Analyzer orchestrates LLM-based analysis via the Cortex Gateway.
type Analyzer struct {
	client *gateway.Client
	evol   *persistence.EvolutionStore
	logger *slog.Logger
}

// New creates an Analyzer with the given gateway client and evolution store.
func New(client *gateway.Client, evol *persistence.EvolutionStore, logger *slog.Logger) *Analyzer {
	return &Analyzer{
		client: client,
		evol:   evol,
		logger: logger,
	}
}

// AnalyzeVoice sends messages to the LLM for voice pattern analysis and persists the result.
func (a *Analyzer) AnalyzeVoice(ctx context.Context, agentID, agentRole string, messages []string, tick int64) (*VoiceResult, error) {
	if len(messages) == 0 {
		return nil, fmt.Errorf("no messages to analyze")
	}

	start := time.Now()
	defer func() {
		metrics.LLMAnalysisDuration.WithLabelValues("voice_style").Observe(time.Since(start).Seconds())
	}()

	// Build user prompt
	joined := strings.Join(messages, "\n---\n")
	userPrompt := fmt.Sprintf(VoiceAnalysisUserTemplate, len(messages), agentID, agentRole, joined)

	// Call LLM via gateway
	rawResp, err := a.client.Chat(ctx, VoiceAnalysisSystemPrompt, userPrompt)
	if err != nil {
		return nil, fmt.Errorf("voice analysis llm call: %w", err)
	}

	// Parse JSON response
	var result VoiceResult
	if err := json.Unmarshal([]byte(rawResp), &result); err != nil {
		return nil, fmt.Errorf("voice analysis parse: %w (raw: %s)", err, rawResp)
	}

	// Persist to evolution store
	newValue, _ := json.Marshal(result)
	if err := a.evol.Write(persistence.EvolutionEntry{
		AgentID:    agentID,
		Tick:       tick,
		Field:      "voice_style",
		ChangeType: "voice_style",
		NewValue:   string(newValue),
		Reason:     fmt.Sprintf("voice pattern analysis from %d messages", len(messages)),
		Source:     "batch_judge",
	}); err != nil {
		a.logger.Error("evolution write failed", "agent", agentID, "error", err)
	}

	return &result, nil
}
