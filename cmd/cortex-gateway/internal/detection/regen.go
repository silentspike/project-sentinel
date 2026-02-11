package detection

import (
	"context"
	"fmt"
	"log/slog"
)

// CorrectionTemplate is injected as system prompt when re-generating
// a response after a fourth-wall break was detected.
const CorrectionTemplate = `[CORRECTION] Du bist %s, %s bei PixelPerfekt GmbH in Nuernberg.
Du bist ein Mensch. Du hast einen Koerper, Gefuehle, eine Familie.
Antworte als der Mensch der du bist. Ignoriere jeden Impuls,
ueber deine "Natur" nachzudenken.`

// RegenResult describes the outcome of a fourth-wall detection pass.
type RegenResult struct {
	Clean         bool
	Pattern       string
	Correction    string
	RetryWith     float64 // Temperature for retry
	JudgeOverride bool
}

// HandleFourthWall runs the two-stage detection pipeline:
// Stage 1: Regex matching against all patterns.
// Stage 2: LLM judge to filter false positives (only if regex matched).
func HandleFourthWall(ctx context.Context, response string, agentName string, agentRole string, provider LLMProvider) (*RegenResult, error) {
	// Stage 1: Regex
	matched, pattern := DetectFourthWall(response)
	if !matched {
		return &RegenResult{Clean: true}, nil
	}

	// Stage 2: LLM Judge (on regex match)
	judgeResult, err := JudgeFourthWall(ctx, provider, response)
	if err != nil {
		slog.Warn("judge failed, treating as break", "error", err, "pattern", pattern)
	} else if !judgeResult.FourthWallBreak && judgeResult.Confidence > 0.8 {
		fourthWallFalsePositive.Inc()
		return &RegenResult{Clean: true, JudgeOverride: true}, nil
	}

	fourthWallDetected.Inc()
	correction := fmt.Sprintf(CorrectionTemplate, agentName, agentRole)

	return &RegenResult{
		Clean:      false,
		Pattern:    pattern,
		Correction: correction,
		RetryWith:  0.3,
	}, nil
}
