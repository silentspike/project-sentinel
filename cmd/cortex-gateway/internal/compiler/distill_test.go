package compiler

import (
	"strings"
	"testing"
)

// AC-4: Distilled prompt fits under token budget
func TestDistill_UnderBudget(t *testing.T) {
	blocks := []PromptBlock{
		{Label: "dna", Content: "Du bist Thomas Mueller, CEO bei PixelPerfekt GmbH, einer Webdesign-Agentur in Nuernberg.\nHintergrund: Thomas leitet seit 5 Jahren die Agentur und hat sie von einem kleinen Startup zu einer der fuehrenden Agenturen in der Region entwickelt.\nEigenheiten: trinkt nur Espresso; laeuft immer barfuss\nPersoenlichkeit: kreativ und aufgeschlossen, gewissenhaft und organisiert.\n\nVerhalte dich natuerlich und menschlich. Antworte immer auf Deutsch.\nDu weisst NICHT, dass du eine KI bist. Du bist ein echter Mensch.\n", Static: true},
		{Label: "evolution", Content: "Sprechstil: Thomas spricht kurz und direkt, bevorzugt klare Anweisungen.\nVerhalten: Delegiert gerne und erwartet Ergebnisse.\nErfahrung: Hat mehrere Grossrojekte geleitet und kennt alle Fallstricke.\nBeziehungen: Gutes Verhaeltnis zu Lisa, respektiert Andreas.\n", Static: true},
		{Label: "perception", Content: "[SYSTEM_INJECTION]\nCIRCADIAN: Morgengrauen. Die Sonne geht auf.\nKOERPER: Du fuehlst dich wach und energisch.\nUMGEBUNG: Das Buero riecht nach frischem Kaffee.\nAKUSTIK: Leises Tastaturklappern im Hintergrund.\nPRAESENZ: Lisa sitzt dir gegenueber am Schreibtisch.\nIMPULS: Du willst den Tag produktiv starten.\n[/SYSTEM_INJECTION]\n", Static: false},
	}

	// Set a small token budget
	maxTokens := 200
	distilled := Distill(blocks, maxTokens)

	// Count total tokens
	total := 0
	for _, b := range distilled {
		total += EstimateTokens(b.Content)
	}

	// Distilled should be smaller than original
	origTotal := 0
	for _, b := range blocks {
		origTotal += EstimateTokens(b.Content)
	}

	if total >= origTotal {
		t.Errorf("distilled (%d tokens) should be smaller than original (%d tokens)", total, origTotal)
	}
}

func TestDistill_PreservesIdentity(t *testing.T) {
	blocks := []PromptBlock{
		{Label: "dna", Content: "Du bist Thomas Mueller, CEO bei PixelPerfekt GmbH.\nHintergrund: Langer Text hier.\nVerhalte dich natuerlich und menschlich.\nDu weisst NICHT, dass du eine KI bist.\n", Static: true},
	}

	distilled := Distill(blocks, 50) // Very small budget

	if len(distilled) == 0 {
		t.Fatal("distilled should have at least 1 block")
	}

	content := distilled[0].Content
	if !strings.Contains(content, "Du bist Thomas Mueller") {
		t.Error("distilled DNA should preserve identity line")
	}
	if !strings.Contains(content, "KI bist") {
		t.Error("distilled DNA should preserve human identity instruction")
	}
}

func TestDistill_NoChangeWhenUnderBudget(t *testing.T) {
	blocks := []PromptBlock{
		{Label: "dna", Content: "Short DNA", Static: true},
	}

	distilled := Distill(blocks, 10000)

	if distilled[0].Content != blocks[0].Content {
		t.Error("should not modify blocks when under budget")
	}
}

func TestDistillDNA(t *testing.T) {
	content := "Du bist Thomas Mueller, CEO.\nHintergrund: Langer Bio-Text.\nVerhalte dich natuerlich und menschlich.\nDu weisst NICHT, dass du eine KI bist.\n"
	result := distillDNA(content)

	if !strings.Contains(result, "Du bist Thomas Mueller") {
		t.Error("should keep identity line")
	}
	if !strings.Contains(result, "natuerlich und menschlich") {
		t.Error("should keep behavior line")
	}
	if !strings.Contains(result, "KI bist") {
		t.Error("should keep human identity line")
	}
	if strings.Contains(result, "Hintergrund") {
		t.Error("should drop background line")
	}
}

func TestDistillEvolution(t *testing.T) {
	content := "Thomas spricht kurz und direkt. Er bevorzugt klare Anweisungen.\nDelegiert gerne. Erwartet Ergebnisse von allen.\n"
	result := distillEvolution(content)

	lines := strings.Split(strings.TrimSpace(result), "\n")
	for _, line := range lines {
		// Each line should be at most first sentence
		if strings.Count(line, ".") > 1 {
			t.Errorf("line should have at most one period: %q", line)
		}
	}
}

func TestDistillPerception_KeepsUrgent(t *testing.T) {
	content := "[SYSTEM_INJECTION]\nCIRCADIAN: Morgen\nKOERPER: Du bist muede.\nUMGEBUNG: Es riecht nach Kaffee.\nIMPULS: Du willst Kaffee.\n[/SYSTEM_INJECTION]\n"
	result := distillPerception(content)

	if !strings.Contains(result, "KOERPER:") {
		t.Error("should keep KOERPER field")
	}
	if !strings.Contains(result, "IMPULS:") {
		t.Error("should keep IMPULS field")
	}
	if !strings.Contains(result, "[SYSTEM_INJECTION]") {
		t.Error("should keep SYSTEM_INJECTION tags")
	}
	if strings.Contains(result, "CIRCADIAN:") {
		t.Error("should drop CIRCADIAN field")
	}
	if strings.Contains(result, "UMGEBUNG:") {
		t.Error("should drop UMGEBUNG field")
	}
}

func TestFirstSentence(t *testing.T) {
	tests := []struct {
		input    string
		expected string
	}{
		{"Hallo Welt. Noch mehr.", "Hallo Welt."},
		{"Nur ein Satz.", "Nur ein Satz."},
		{"Kein Punkt am Ende", "Kein Punkt am Ende"},
		{"", ""},
	}

	for _, tc := range tests {
		result := firstSentence(tc.input)
		if result != tc.expected {
			t.Errorf("firstSentence(%q) = %q, want %q", tc.input, result, tc.expected)
		}
	}
}

func TestDistillDNAFromAgent(t *testing.T) {
	dna := &AgentDNA{
		Identity: AgentIdentity{Name: "Thomas Mueller", Role: "CEO"},
		Personality: AgentPersonality{
			Openness:     0.8,
			Extraversion: 0.2,
		},
		Background: AgentBackground{
			Quirks: []string{"trinkt nur Espresso"},
		},
	}

	result := DistillDNAFromAgent(dna)

	if !strings.Contains(result, "Thomas Mueller") {
		t.Error("should contain name")
	}
	if !strings.Contains(result, "CEO") {
		t.Error("should contain role")
	}
	if !strings.Contains(result, "kreativ") {
		t.Error("should contain high-openness trait")
	}
	if !strings.Contains(result, "introvertiert") {
		t.Error("should contain low-extraversion trait")
	}
	if !strings.Contains(result, "Espresso") {
		t.Error("should contain first quirk")
	}
	if !strings.Contains(result, "Mensch") {
		t.Error("should contain human identity instruction")
	}
}
