package analyzer

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"strings"
	"time"

	"github.com/silentspike/project-sentinel/services/sentinel-judge/internal/gateway"
	"github.com/silentspike/project-sentinel/services/sentinel-judge/internal/metrics"
	"github.com/silentspike/project-sentinel/services/sentinel-judge/internal/persistence"
)

// VoiceResult is the JSON structure returned by the LLM for voice analysis.
type VoiceResult struct {
	Phrases       []string `json:"phrases"`
	SentenceStyle string   `json:"sentence_style"`
	Formality     float64  `json:"formality"`
}

// BehavioralResult is the JSON structure returned by the LLM for behavioral analysis.
type BehavioralResult struct {
	Habits           []string `json:"habits"`
	InteractionStyle string   `json:"interaction_style"`
	DecisionStyle    string   `json:"decision_style"`
	Anomalies        []string `json:"anomalies"`
}

// NarrativeResult is the JSON structure returned by the LLM for narrative arc analysis.
type NarrativeResult struct {
	Mood          string   `json:"mood"`
	TurningPoints []string `json:"turning_points"`
	Theme         string   `json:"theme"`
	ArcSummary    string   `json:"arc_summary"`
}

// RelationshipEntry describes a single agent-to-colleague relationship.
type RelationshipEntry struct {
	Colleague string `json:"colleague"`
	Quality   string `json:"quality"`
}

// RelationshipResult is the JSON structure returned by the LLM for relationship analysis.
type RelationshipResult struct {
	Relationships          []RelationshipEntry `json:"relationships"`
	CollaborationPartners  []string            `json:"collaboration_partners"`
	Conflicts              []string            `json:"conflicts"`
	TeamRole               string              `json:"team_role"`
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

// AnalyzeBehavior sends messages to the LLM for behavioral pattern analysis and persists the result.
func (a *Analyzer) AnalyzeBehavior(ctx context.Context, agentID, agentRole string, messages []string, tick int64) (*BehavioralResult, error) {
	if len(messages) == 0 {
		return nil, fmt.Errorf("no messages to analyze")
	}

	start := time.Now()
	defer func() {
		metrics.LLMAnalysisDuration.WithLabelValues("behavioral_notes").Observe(time.Since(start).Seconds())
	}()

	joined := strings.Join(messages, "\n---\n")
	userPrompt := fmt.Sprintf(BehavioralNotesUserTemplate, len(messages), agentID, agentRole, joined)

	rawResp, err := a.client.Chat(ctx, BehavioralNotesSystemPrompt, userPrompt)
	if err != nil {
		return nil, fmt.Errorf("behavioral analysis llm call: %w", err)
	}

	var result BehavioralResult
	if err := json.Unmarshal([]byte(rawResp), &result); err != nil {
		return nil, fmt.Errorf("behavioral analysis parse: %w (raw: %s)", err, rawResp)
	}

	newValue, _ := json.Marshal(result)
	if err := a.evol.Write(persistence.EvolutionEntry{
		AgentID:    agentID,
		Tick:       tick,
		Field:      "behavioral_notes",
		ChangeType: "behavioral_notes",
		NewValue:   string(newValue),
		Reason:     fmt.Sprintf("behavioral analysis from %d messages", len(messages)),
		Source:     "batch_judge",
	}); err != nil {
		a.logger.Error("evolution write failed", "agent", agentID, "error", err)
	}

	return &result, nil
}

// AnalyzeNarrative sends messages to the LLM for narrative arc analysis and persists the result.
func (a *Analyzer) AnalyzeNarrative(ctx context.Context, agentID, agentRole string, messages []string, tick int64) (*NarrativeResult, error) {
	if len(messages) == 0 {
		return nil, fmt.Errorf("no messages to analyze")
	}

	start := time.Now()
	defer func() {
		metrics.LLMAnalysisDuration.WithLabelValues("narrative_arc").Observe(time.Since(start).Seconds())
	}()

	joined := strings.Join(messages, "\n---\n")
	userPrompt := fmt.Sprintf(NarrativeArcUserTemplate, len(messages), agentID, agentRole, joined)

	rawResp, err := a.client.Chat(ctx, NarrativeArcSystemPrompt, userPrompt)
	if err != nil {
		return nil, fmt.Errorf("narrative analysis llm call: %w", err)
	}

	var result NarrativeResult
	if err := json.Unmarshal([]byte(rawResp), &result); err != nil {
		return nil, fmt.Errorf("narrative analysis parse: %w (raw: %s)", err, rawResp)
	}

	newValue, _ := json.Marshal(result)
	if err := a.evol.Write(persistence.EvolutionEntry{
		AgentID:    agentID,
		Tick:       tick,
		Field:      "narrative_arc",
		ChangeType: "narrative_arc",
		NewValue:   string(newValue),
		Reason:     fmt.Sprintf("narrative arc analysis from %d messages", len(messages)),
		Source:     "batch_judge",
	}); err != nil {
		a.logger.Error("evolution write failed", "agent", agentID, "error", err)
	}

	return &result, nil
}

// AnalyzeRelationships sends messages to the LLM for relationship dynamics analysis and persists the result.
func (a *Analyzer) AnalyzeRelationships(ctx context.Context, agentID, agentRole string, messages []string, tick int64) (*RelationshipResult, error) {
	if len(messages) == 0 {
		return nil, fmt.Errorf("no messages to analyze")
	}

	start := time.Now()
	defer func() {
		metrics.LLMAnalysisDuration.WithLabelValues("relationship_dynamics").Observe(time.Since(start).Seconds())
	}()

	joined := strings.Join(messages, "\n---\n")
	userPrompt := fmt.Sprintf(RelationshipDynamicsUserTemplate, len(messages), agentID, agentRole, joined)

	rawResp, err := a.client.Chat(ctx, RelationshipDynamicsSystemPrompt, userPrompt)
	if err != nil {
		return nil, fmt.Errorf("relationship analysis llm call: %w", err)
	}

	var result RelationshipResult
	if err := json.Unmarshal([]byte(rawResp), &result); err != nil {
		return nil, fmt.Errorf("relationship analysis parse: %w (raw: %s)", err, rawResp)
	}

	newValue, _ := json.Marshal(result)
	if err := a.evol.Write(persistence.EvolutionEntry{
		AgentID:    agentID,
		Tick:       tick,
		Field:      "relationship_dynamics",
		ChangeType: "relationship_dynamics",
		NewValue:   string(newValue),
		Reason:     fmt.Sprintf("relationship dynamics analysis from %d messages", len(messages)),
		Source:     "batch_judge",
	}); err != nil {
		a.logger.Error("evolution write failed", "agent", agentID, "error", err)
	}

	return &result, nil
}
