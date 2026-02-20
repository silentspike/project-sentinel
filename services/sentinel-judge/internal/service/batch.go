package service

import (
	"context"
	"log/slog"

	"github.com/obtFusi/project-sentinel/pkg/sentinel-go/judge"
	"github.com/obtFusi/project-sentinel/services/sentinel-judge/internal/analyzer"
	"github.com/obtFusi/project-sentinel/services/sentinel-judge/internal/config"
)

// BatchRequest is the input for a batch analysis (called by Night-Run via HTTP).
type BatchRequest struct {
	AgentID        string   `json:"agent_id"`
	AgentRole      string   `json:"agent_role"`
	ShiftStartTick int64    `json:"shift_start_tick"`
	ShiftEndTick   int64    `json:"shift_end_tick"`
	Messages       []string `json:"messages"`
	AnalysisTypes  []string `json:"analysis_types"` // ["voice_style", "behavioral_notes", "narrative_arc", "relationship_dynamics", "drift", "quality", "fatigue"]
}

// BatchResponse is the result of a batch analysis.
type BatchResponse struct {
	AgentID         string                  `json:"agent_id"`
	VoiceStyle      *analyzer.VoiceResult        `json:"voice_style,omitempty"`
	BehavioralNotes *analyzer.BehavioralResult   `json:"behavioral_notes,omitempty"`
	NarrativeArc    *analyzer.NarrativeResult    `json:"narrative_arc,omitempty"`
	Relationships   *analyzer.RelationshipResult `json:"relationship_dynamics,omitempty"`
	Drift           *judge.DriftResult           `json:"drift,omitempty"`
	Quality         *judge.QualityResult         `json:"quality,omitempty"`
	Fatigue         *judge.FatigueResult         `json:"fatigue,omitempty"`
	EvolutionEvents int                     `json:"evolution_events"`
	Alerts          []string                `json:"alerts"`
}

// BatchHandler processes batch analysis requests from the Night-Run pipeline.
type BatchHandler struct {
	analyzer *analyzer.Analyzer
	drift    *judge.DriftDetector
	quality  *judge.QualityScorer
	fatigue  *judge.FatigueDetector
	cfg      *config.Config
	logger   *slog.Logger
}

// NewBatchHandler creates a batch handler with the given dependencies.
func NewBatchHandler(
	analyzer *analyzer.Analyzer,
	cfg *config.Config,
	logger *slog.Logger,
) *BatchHandler {
	drift := judge.NewDriftDetector()
	return &BatchHandler{
		analyzer: analyzer,
		drift:    drift,
		quality:  judge.NewQualityScorer(drift),
		fatigue:  judge.NewFatigueDetector(),
		cfg:      cfg,
		logger:   logger,
	}
}

// Analyze runs the requested analysis types on the provided messages.
func (bh *BatchHandler) Analyze(ctx context.Context, req BatchRequest) (*BatchResponse, error) {
	resp := &BatchResponse{
		AgentID: req.AgentID,
	}

	typesSet := make(map[string]bool)
	for _, t := range req.AnalysisTypes {
		typesSet[t] = true
	}

	// Heuristic analyses (fast, no LLM needed)
	if typesSet["drift"] {
		result := bh.drift.CheckDrift(req.AgentID, req.Messages)
		resp.Drift = &result
		if severityAtLeast(result.Severity, bh.cfg.Thresholds.DriftAlertSeverity) {
			resp.Alerts = append(resp.Alerts, "drift: "+result.Severity)
		}
	}

	if typesSet["quality"] && len(req.Messages) > 0 {
		latest := req.Messages[len(req.Messages)-1]
		history := req.Messages[:len(req.Messages)-1]
		result := bh.quality.ScoreMessage(req.AgentID, latest, history)
		resp.Quality = &result
		if result.Score <= bh.cfg.Thresholds.QualityAlertMinScore {
			resp.Alerts = append(resp.Alerts, "quality: low score")
		}
	}

	if typesSet["fatigue"] {
		result := bh.fatigue.CheckFatigue(req.AgentID, req.Messages)
		resp.Fatigue = &result
		if result.FatigueScore >= bh.cfg.Thresholds.FatigueAlertMinScore {
			resp.Alerts = append(resp.Alerts, "fatigue: "+fatigueLevel(result.FatigueScore))
		}
	}

	// LLM analysis (slow, requires gateway)
	if typesSet["voice_style"] && bh.analyzer != nil {
		result, err := bh.analyzer.AnalyzeVoice(ctx, req.AgentID, req.AgentRole, req.Messages, req.ShiftEndTick)
		if err != nil {
			bh.logger.Error("voice analysis failed", "agent", req.AgentID, "error", err)
		} else {
			resp.VoiceStyle = result
			resp.EvolutionEvents++
		}
	}

	if typesSet["behavioral_notes"] && bh.analyzer != nil {
		result, err := bh.analyzer.AnalyzeBehavior(ctx, req.AgentID, req.AgentRole, req.Messages, req.ShiftEndTick)
		if err != nil {
			bh.logger.Error("behavioral analysis failed", "agent", req.AgentID, "error", err)
		} else {
			resp.BehavioralNotes = result
			resp.EvolutionEvents++
		}
	}

	if typesSet["narrative_arc"] && bh.analyzer != nil {
		result, err := bh.analyzer.AnalyzeNarrative(ctx, req.AgentID, req.AgentRole, req.Messages, req.ShiftEndTick)
		if err != nil {
			bh.logger.Error("narrative analysis failed", "agent", req.AgentID, "error", err)
		} else {
			resp.NarrativeArc = result
			resp.EvolutionEvents++
		}
	}

	if typesSet["relationship_dynamics"] && bh.analyzer != nil {
		result, err := bh.analyzer.AnalyzeRelationships(ctx, req.AgentID, req.AgentRole, req.Messages, req.ShiftEndTick)
		if err != nil {
			bh.logger.Error("relationship analysis failed", "agent", req.AgentID, "error", err)
		} else {
			resp.Relationships = result
			resp.EvolutionEvents++
		}
	}

	return resp, nil
}
