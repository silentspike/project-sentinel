package compiler

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/capability"
)

func setupAssembler(t *testing.T) *Assembler {
	t.Helper()
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)
	caps := capability.New()
	return NewAssembler(loader, caps)
}

// AC-1: Three sources produce three blocks
func TestAssemble_ThreeSources(t *testing.T) {
	asm := setupAssembler(t)

	evo := EvolutionData{
		VoiceStyle:      "kurz und direkt",
		BehavioralNotes: "delegiert gerne",
	}

	blocks, err := asm.Assemble(1, "claude", evo, "CIRCADIAN: Morgen\nKOERPER: Wach")
	if err != nil {
		t.Fatalf("Assemble() error: %v", err)
	}

	if len(blocks) != 3 {
		t.Fatalf("expected 3 blocks, got %d", len(blocks))
	}

	if blocks[0].Label != "dna" {
		t.Errorf("block 0 label = %q, want dna", blocks[0].Label)
	}
	if blocks[1].Label != "evolution" {
		t.Errorf("block 1 label = %q, want evolution", blocks[1].Label)
	}
	if blocks[2].Label != "perception" {
		t.Errorf("block 2 label = %q, want perception", blocks[2].Label)
	}

	// DNA block should contain agent identity
	if !strings.Contains(blocks[0].Content, "Thomas Mueller") {
		t.Error("DNA block should contain agent name")
	}
	if !strings.Contains(blocks[0].Content, "CEO") {
		t.Error("DNA block should contain agent role")
	}

	// Evolution block should contain voice style
	if !strings.Contains(blocks[1].Content, "kurz und direkt") {
		t.Error("evolution block should contain voice style")
	}

	// Perception block should contain SYSTEM_INJECTION
	if !strings.Contains(blocks[2].Content, "[SYSTEM_INJECTION]") {
		t.Error("perception block should contain SYSTEM_INJECTION")
	}
}

func TestAssemble_DNAOnly(t *testing.T) {
	asm := setupAssembler(t)

	blocks, err := asm.Assemble(1, "claude", EvolutionData{}, "")
	if err != nil {
		t.Fatalf("Assemble() error: %v", err)
	}

	if len(blocks) != 1 {
		t.Fatalf("expected 1 block (DNA only), got %d", len(blocks))
	}
	if blocks[0].Label != "dna" {
		t.Errorf("block label = %q, want dna", blocks[0].Label)
	}
}

func TestAssemble_DNAAndEvolution(t *testing.T) {
	asm := setupAssembler(t)

	evo := EvolutionData{VoiceStyle: "freundlich"}
	blocks, err := asm.Assemble(1, "claude", evo, "")
	if err != nil {
		t.Fatalf("Assemble() error: %v", err)
	}

	if len(blocks) != 2 {
		t.Fatalf("expected 2 blocks, got %d", len(blocks))
	}
}

func TestAssemble_DNAAndPerception(t *testing.T) {
	asm := setupAssembler(t)

	blocks, err := asm.Assemble(1, "claude", EvolutionData{}, "KOERPER: Muede")
	if err != nil {
		t.Fatalf("Assemble() error: %v", err)
	}

	if len(blocks) != 2 {
		t.Fatalf("expected 2 blocks, got %d", len(blocks))
	}
	if blocks[1].Label != "perception" {
		t.Errorf("block 1 label = %q, want perception", blocks[1].Label)
	}
}

func TestAssemble_StaticFlags(t *testing.T) {
	asm := setupAssembler(t)

	evo := EvolutionData{VoiceStyle: "test"}
	blocks, err := asm.Assemble(1, "claude", evo, "perception")
	if err != nil {
		t.Fatalf("Assemble() error: %v", err)
	}

	if !blocks[0].Static {
		t.Error("DNA block should be static")
	}
	if !blocks[1].Static {
		t.Error("evolution block should be static")
	}
	if blocks[2].Static {
		t.Error("perception block should NOT be static")
	}
}

func TestAssemble_SmallModelFormat(t *testing.T) {
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)
	caps := capability.New()
	asm := NewAssembler(loader, caps)

	blocks, err := asm.Assemble(1, "ollama", EvolutionData{}, "")
	if err != nil {
		t.Fatalf("Assemble() error: %v", err)
	}

	// Ollama lacks caching capability, so formatDNA uses compact format
	if strings.Contains(blocks[0].Content, "Webdesign-Agentur in Nuernberg") {
		t.Error("small model format should NOT include full company description")
	}
}

func TestAssemble_LargeModelFormat(t *testing.T) {
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)
	caps := capability.New()
	asm := NewAssembler(loader, caps)

	blocks, err := asm.Assemble(1, "claude", EvolutionData{}, "")
	if err != nil {
		t.Fatalf("Assemble() error: %v", err)
	}

	if !strings.Contains(blocks[0].Content, "Webdesign-Agentur in Nuernberg") {
		t.Error("large model format should include full company description")
	}
	if !strings.Contains(blocks[0].Content, "Thomas leitet seit 5 Jahren") {
		t.Error("large model format should include bio")
	}
}

func TestAssemble_PersonalityTraits(t *testing.T) {
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)
	caps := capability.New()
	asm := NewAssembler(loader, caps)

	blocks, err := asm.Assemble(1, "claude", EvolutionData{}, "")
	if err != nil {
		t.Fatalf("Assemble() error: %v", err)
	}

	content := blocks[0].Content
	// Openness 0.8 > 0.7 → kreativ
	if !strings.Contains(content, "kreativ") {
		t.Error("high openness should produce 'kreativ' trait")
	}
	// Conscientiousness 0.9 > 0.7 → gewissenhaft
	if !strings.Contains(content, "gewissenhaft") {
		t.Error("high conscientiousness should produce 'gewissenhaft' trait")
	}
	// Neuroticism 0.3 < 0.3 → gelassen (not triggered, == 0.3 is not < 0.3)
}

func TestAssemble_MissingAgent(t *testing.T) {
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)
	caps := capability.New()
	asm := NewAssembler(loader, caps)

	_, err := asm.Assemble(99, "claude", EvolutionData{}, "")
	if err == nil {
		t.Error("Assemble() should fail for missing agent")
	}
}

func TestLoadCompanyContextReadsSiblingConfigDirectory(t *testing.T) {
	baseDir := t.TempDir()
	agentsDir := filepath.Join(baseDir, "agents")
	configDir := filepath.Join(baseDir, "config")
	if err := os.MkdirAll(agentsDir, 0o750); err != nil {
		t.Fatalf("MkdirAll(agents): %v", err)
	}
	if err := os.MkdirAll(configDir, 0o750); err != nil {
		t.Fatalf("MkdirAll(config): %v", err)
	}

	agentPath := filepath.Join(agentsDir, "AGENT-01-THOMAS-CEO.toml")
	if err := os.WriteFile(agentPath, []byte(testAgentTOML), 0o600); err != nil {
		t.Fatalf("WriteFile(agent): %v", err)
	}
	want := "PixelPerfekt GmbH - Kontext aus config/company-context.md"
	if err := os.WriteFile(filepath.Join(configDir, "company-context.md"), []byte(want), 0o600); err != nil {
		t.Fatalf("WriteFile(company-context): %v", err)
	}

	got := LoadCompanyContext(agentsDir)
	if got != want {
		t.Fatalf("LoadCompanyContext() = %q, want %q", got, want)
	}
}

func TestFormatPersonalityTraits_HighValues(t *testing.T) {
	p := AgentPersonality{
		Openness:          0.8,
		Conscientiousness: 0.8,
		Extraversion:      0.8,
		Agreeableness:     0.8,
		Neuroticism:       0.8,
	}
	result := formatPersonalityTraits(p)

	if !strings.Contains(result, "kreativ") {
		t.Error("expected 'kreativ' for high openness")
	}
	if !strings.Contains(result, "gewissenhaft") {
		t.Error("expected 'gewissenhaft' for high conscientiousness")
	}
	if !strings.Contains(result, "gesellig") {
		t.Error("expected 'gesellig' for high extraversion")
	}
	if !strings.Contains(result, "kooperativ") {
		t.Error("expected 'kooperativ' for high agreeableness")
	}
	if !strings.Contains(result, "emotional") {
		t.Error("expected 'emotional' for high neuroticism")
	}
}

func TestFormatPersonalityTraits_LowValues(t *testing.T) {
	p := AgentPersonality{
		Openness:          0.2,
		Conscientiousness: 0.2,
		Extraversion:      0.2,
		Agreeableness:     0.2,
		Neuroticism:       0.2,
	}
	result := formatPersonalityTraits(p)

	if !strings.Contains(result, "konservativ") {
		t.Error("expected 'konservativ' for low openness")
	}
	if !strings.Contains(result, "spontan") {
		t.Error("expected 'spontan' for low conscientiousness")
	}
	if !strings.Contains(result, "introvertiert") {
		t.Error("expected 'introvertiert' for low extraversion")
	}
	if !strings.Contains(result, "direkt") {
		t.Error("expected 'direkt' for low agreeableness")
	}
	if !strings.Contains(result, "gelassen") {
		t.Error("expected 'gelassen' for low neuroticism")
	}
}

func TestFormatPersonalityTraits_MidValues(t *testing.T) {
	p := AgentPersonality{
		Openness:          0.5,
		Conscientiousness: 0.5,
		Extraversion:      0.5,
		Agreeableness:     0.5,
		Neuroticism:       0.5,
	}
	result := formatPersonalityTraits(p)

	if result != "" {
		t.Errorf("mid-range values should produce empty string, got %q", result)
	}
}

func TestFormatEvolution(t *testing.T) {
	evo := EvolutionData{
		VoiceStyle:       "kurz und direkt",
		BehavioralNotes:  "delegiert gerne",
		NarrativeSummary: "Erfahrener Manager",
		Relationships:    "Gutes Verhaeltnis zu Lisa",
	}
	result := formatEvolution(evo)

	if !strings.Contains(result, "Sprechstil: kurz und direkt") {
		t.Error("should contain voice style")
	}
	if !strings.Contains(result, "Verhalten: delegiert gerne") {
		t.Error("should contain behavioral notes")
	}
	if !strings.Contains(result, "Erfahrung: Erfahrener Manager") {
		t.Error("should contain narrative summary")
	}
	if !strings.Contains(result, "Beziehungen: Gutes Verhaeltnis") {
		t.Error("should contain relationships")
	}
}

func TestFormatPerception(t *testing.T) {
	result := formatPerception("KOERPER: Wach und aufmerksam")

	if !strings.Contains(result, "[SYSTEM_INJECTION]") {
		t.Error("should contain opening tag")
	}
	if !strings.Contains(result, "[/SYSTEM_INJECTION]") {
		t.Error("should contain closing tag")
	}
	if !strings.Contains(result, "KOERPER: Wach und aufmerksam") {
		t.Error("should contain perception content")
	}
}
