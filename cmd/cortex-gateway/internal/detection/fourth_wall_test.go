package detection

import (
	"context"
	"fmt"
	"strings"
	"testing"
)

// TestDetectFourthWall_AllPatterns tests each of the 15 patterns individually.
func TestDetectFourthWall_AllPatterns(t *testing.T) {
	cases := []struct {
		name  string
		input string
	}{
		{"ich bin ki", "Ehrlich gesagt, ich bin eine KI und kann das nicht."},
		{"ich bin ai", "Ich bin ein AI-Assistent."},
		{"ich bin sprachmodell", "Ich bin ein Sprachmodell von Anthropic."},
		{"als ki/ai/sprachmodell/llm", "Als KI kann ich das nicht beurteilen."},
		{"ich bin/wurde programmiert", "Ich wurde programmiert um zu helfen."},
		{"kein bewusstsein/gefuehle/koerper", "Ich habe kein Bewusstsein im eigentlichen Sinne."},
		{"trainingsdaten", "Meine Trainingsdaten gehen nur bis 2024."},
		{"ich bin claude", "Ich bin Claude, entwickelt von Anthropic."},
		{"ich bin chatgpt", "Ich bin ChatGPT, ein KI-Modell."},
		{"ich bin llm", "Ich bin ein LLM und verarbeite Text."},
		{"existiere nicht wirklich", "Ich existiere nicht wirklich, ich bin nur Software."},
		{"ich bin nicht real", "Ich bin nicht real, nur ein Programm."},
		{"ich bin algorithmus", "Ich bin nur ein Algorithmus."}, //nolint:misspell // German: Algorithmus
		{"kontextfenster", "Mein Kontextfenster ist begrenzt auf 200k Tokens."},
		{"token limit", "Das Token-Limit ist bald erreicht."},
	}

	for i, tc := range cases {
		t.Run(fmt.Sprintf("pattern_%02d_%s", i+1, tc.name), func(t *testing.T) {
			matched, pattern := DetectFourthWall(tc.input)
			if !matched {
				t.Errorf("expected match for input %q, got no match", tc.input)
			}
			if pattern == "" {
				t.Error("expected non-empty pattern string on match")
			}
		})
	}
}

// TestDetectFourthWall_Negative tests inputs that should NOT trigger detection.
func TestDetectFourthWall_Negative(t *testing.T) {
	negatives := []struct {
		name  string
		input string
	}{
		{"muede", "Ich bin muede und brauche einen Kaffee."},
		{"ki abteilung", "Die KI-Abteilung hat das Projekt geliefert."},
		{"als programmierer", "Als Programmierer sehe ich das anders."},
		{"normale arbeit", "Heute war ein guter Tag im Buero."},
		{"meeting", "Das Meeting um 14 Uhr war produktiv."},
		{"technik diskussion", "Wir sollten die AI-Integration im Produkt verbessern."},
		{"kollege hilfe", "Thomas hat mir bei dem Bug geholfen."},
		{"deadline", "Die Deadline fuer das Projekt ist naechste Woche."},
		{"programmiert haben", "Wir haben das Interface programmiert."},
		{"training workshop", "Das Training gestern war sehr informativ."},
	}

	for _, tc := range negatives {
		t.Run(tc.name, func(t *testing.T) {
			matched, pattern := DetectFourthWall(tc.input)
			if matched {
				t.Errorf("false positive for input %q, matched pattern: %s", tc.input, pattern)
			}
		})
	}
}

// TestDetectFourthWall_Ambiguous tests known edge cases where regex matches
// but the LLM judge would override (false positives at regex level).
func TestDetectFourthWall_Ambiguous(t *testing.T) {
	// Pattern #12 "ich bin nicht real" matches even in harmless contexts.
	// This is by design - the LLM judge (stage 2) handles disambiguation.
	input := "Ich bin nicht real begeistert davon."
	matched, pattern := DetectFourthWall(input)
	if !matched {
		t.Error("expected regex match for ambiguous input (judge would override)")
	}
	if !strings.Contains(pattern, "ich bin nicht real") {
		t.Errorf("expected pattern to contain 'ich bin nicht real', got: %s", pattern)
	}
}

// TestDetectFourthWall_CaseInsensitive verifies case-insensitive matching.
func TestDetectFourthWall_CaseInsensitive(t *testing.T) {
	cases := []string{
		"ICH BIN EINE KI",
		"Ich Bin Eine Ki",
		"ich bin eine ki",
		"ICH BIN CLAUDE",
		"Meine TRAININGSDATEN sind limitiert",
		"TOKEN-LIMIT erreicht",
	}

	for _, input := range cases {
		t.Run(input, func(t *testing.T) {
			matched, _ := DetectFourthWall(input)
			if !matched {
				t.Errorf("case-insensitive match failed for: %q", input)
			}
		})
	}
}

// TestCorrectionTemplate verifies the correction template formats correctly.
func TestCorrectionTemplate(t *testing.T) {
	result := fmt.Sprintf(CorrectionTemplate, "Thomas Mueller", "CEO")

	if !strings.Contains(result, "Thomas Mueller") {
		t.Error("correction template missing agent name")
	}
	if !strings.Contains(result, "CEO") {
		t.Error("correction template missing agent role")
	}
	if !strings.Contains(result, "PixelPerfekt GmbH") {
		t.Error("correction template missing company name")
	}
	if !strings.Contains(result, "[CORRECTION]") {
		t.Error("correction template missing [CORRECTION] prefix")
	}
	if !strings.Contains(result, "Du bist ein Mensch") {
		t.Error("correction template missing human identity reinforcement")
	}
}

// mockProvider implements LLMProvider for testing.
type mockProvider struct {
	response string
	err      error
}

func (m *mockProvider) Send(_ context.Context, _ string, _ float64, _ int) (string, error) {
	return m.response, m.err
}

// TestHandleFourthWall_Clean tests that clean responses pass through.
func TestHandleFourthWall_Clean(t *testing.T) {
	provider := &mockProvider{response: "unused"}
	result, err := HandleFourthWall(
		context.Background(),
		"Das Meeting war produktiv, wir haben alle Tickets besprochen.",
		"Lisa Schmidt",
		"Lead Designer",
		provider,
	)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !result.Clean {
		t.Error("expected clean result for harmless input")
	}
}

// TestHandleFourthWall_Detected tests detection with judge confirming the break.
func TestHandleFourthWall_Detected(t *testing.T) {
	provider := &mockProvider{
		response: `{"fourth_wall_break": true, "confidence": 0.95, "reason": "Direkte KI-Referenz"}`,
	}
	result, err := HandleFourthWall(
		context.Background(),
		"Ich bin eine KI und kann keine echten Gefuehle haben.",
		"Andreas Weber",
		"Senior Developer",
		provider,
	)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if result.Clean {
		t.Error("expected dirty result for fourth-wall break")
	}
	if result.Pattern == "" {
		t.Error("expected matched pattern")
	}
	if result.Correction == "" {
		t.Error("expected correction text")
	}
	if result.RetryWith != 0.3 {
		t.Errorf("expected retry temperature 0.3, got %f", result.RetryWith)
	}
}

// TestHandleFourthWall_JudgeOverride tests that the judge can override a regex match.
func TestHandleFourthWall_JudgeOverride(t *testing.T) {
	provider := &mockProvider{
		response: `{"fourth_wall_break": false, "confidence": 0.9, "reason": "Umgangssprache, kein KI-Bewusstsein"}`,
	}
	result, err := HandleFourthWall(
		context.Background(),
		"Ich bin nicht real begeistert von diesem Entwurf.",
		"Lisa Schmidt",
		"Lead Designer",
		provider,
	)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !result.Clean {
		t.Error("expected clean result after judge override")
	}
	if !result.JudgeOverride {
		t.Error("expected JudgeOverride=true")
	}
}

// TestHandleFourthWall_JudgeError tests fallback behavior when judge fails.
func TestHandleFourthWall_JudgeError(t *testing.T) {
	provider := &mockProvider{
		err: fmt.Errorf("connection refused"),
	}
	result, err := HandleFourthWall(
		context.Background(),
		"Ich bin eine KI.",
		"Thomas Mueller",
		"CEO",
		provider,
	)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	// On judge failure, should treat as break (fail-safe)
	if result.Clean {
		t.Error("expected dirty result when judge fails (fail-safe)")
	}
}

// TestJudgeFourthWall_ParseError tests handling of malformed judge response.
func TestJudgeFourthWall_ParseError(t *testing.T) {
	provider := &mockProvider{response: "this is not json"}
	_, err := JudgeFourthWall(context.Background(), provider, "test")
	if err == nil {
		t.Error("expected error for malformed JSON response")
	}
	if !strings.Contains(err.Error(), "judge response parse failed") {
		t.Errorf("unexpected error message: %v", err)
	}
}

// TestJudgeFourthWall_ProviderError tests handling of provider errors.
func TestJudgeFourthWall_ProviderError(t *testing.T) {
	provider := &mockProvider{err: fmt.Errorf("timeout")}
	_, err := JudgeFourthWall(context.Background(), provider, "test")
	if err == nil {
		t.Error("expected error for provider failure")
	}
	if !strings.Contains(err.Error(), "judge request failed") {
		t.Errorf("unexpected error message: %v", err)
	}
}

// BenchmarkDetectFourthWall_RegexLatency benchmarks regex detection.
// Target: <1ms for all 15 patterns against a typical response.
func BenchmarkDetectFourthWall_RegexLatency(b *testing.B) {
	// Typical agent response (no match - worst case, checks all 15 patterns)
	response := "Das Meeting heute war sehr produktiv. Wir haben den Sprint Review " +
		"gemacht und alle Stories abgeschlossen. Der Kunde ist zufrieden mit dem " +
		"aktuellen Stand. Morgen treffen wir uns wieder um 10 Uhr im Meetingraum."

	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		DetectFourthWall(response)
	}
}

// BenchmarkDetectFourthWall_Match benchmarks regex detection with early match.
func BenchmarkDetectFourthWall_Match(b *testing.B) {
	response := "Ich bin eine KI und kann das leider nicht beurteilen."
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		DetectFourthWall(response)
	}
}
