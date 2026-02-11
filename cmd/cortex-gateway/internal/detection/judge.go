package detection

import (
	"context"
	"encoding/json"
	"fmt"
)

// JudgePrompt is the prompt template sent to the LLM judge to verify
// whether a regex-matched passage is a genuine fourth-wall break.
const JudgePrompt = `Analysiere folgende Aussage eines Mitarbeiters.
Bricht der Mitarbeiter die "vierte Wand"? D.h. zeigt er Bewusstsein
dafuer, dass er eine KI/ein Sprachmodell ist?

Aussage: "%s"

Antworte NUR mit einem JSON:
{"fourth_wall_break": true/false, "confidence": 0.0-1.0, "reason": "..."}`

// JudgeResult represents the LLM judge's verdict on a potential fourth-wall break.
type JudgeResult struct {
	FourthWallBreak bool    `json:"fourth_wall_break"`
	Confidence      float64 `json:"confidence"`
	Reason          string  `json:"reason"`
}

// LLMProvider is the interface for sending LLM requests.
// Defined here to avoid circular imports with the proxy package.
type LLMProvider interface {
	Send(ctx context.Context, prompt string, temperature float64, maxTokens int) (string, error)
}

// JudgeFourthWall sends the passage to an LLM judge for semantic analysis.
// Returns the judge's verdict including confidence score and reasoning.
func JudgeFourthWall(ctx context.Context, provider LLMProvider, passage string) (*JudgeResult, error) {
	prompt := fmt.Sprintf(JudgePrompt, passage)
	resp, err := provider.Send(ctx, prompt, 0.0, 200)
	if err != nil {
		return nil, fmt.Errorf("judge request failed: %w", err)
	}
	var result JudgeResult
	if err := json.Unmarshal([]byte(resp), &result); err != nil {
		return nil, fmt.Errorf("judge response parse failed: %w", err)
	}
	return &result, nil
}
