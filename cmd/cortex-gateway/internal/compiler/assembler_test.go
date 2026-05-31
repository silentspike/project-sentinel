package compiler

import (
	"os"
	"path/filepath"
	"strings"
	"sync"
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

func TestLoadCompanyContextReadsFromConfigDirectory(t *testing.T) {
	// Mirrors the production layout: <configDir>/agents/AGENT-*.toml + <configDir>/company-context.md
	// (agentsDir = ".../config/agents"; the context sits in its PARENT, the config dir).
	baseDir := t.TempDir()
	configDir := filepath.Join(baseDir, "config")
	agentsDir := filepath.Join(configDir, "agents")
	if err := os.MkdirAll(agentsDir, 0o750); err != nil {
		t.Fatalf("MkdirAll(agents): %v", err)
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

// setupAssemblerWithConfig builds a <root>/agents + <root>/config layout (matching
// LoadCompanyContext's filepath.Dir(agentDir)/config) so hot-reload (#440) can be exercised.
func setupAssemblerWithConfig(t *testing.T, contextContent string) (*Assembler, string) {
	t.Helper()
	root := t.TempDir()
	// Production layout: agents under <configDir>/agents, company-context.md in <configDir>.
	configDir := filepath.Join(root, "config")
	agentDir := filepath.Join(configDir, "agents")
	if err := os.MkdirAll(agentDir, 0750); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(agentDir, "AGENT-01-THOMAS-CEO.toml"), []byte(testAgentTOML), 0600); err != nil {
		t.Fatal(err)
	}
	ctxPath := filepath.Join(configDir, "company-context.md")
	if contextContent != "" {
		if err := os.WriteFile(ctxPath, []byte(contextContent), 0600); err != nil {
			t.Fatal(err)
		}
	}
	loader := NewTOMLLoader(agentDir)
	return NewAssembler(loader, capability.New()), ctxPath
}

// #440 AC-1/AC-2: reload re-reads company-context.md and the swap is atomic + visible to Assemble.
func TestReloadCompanyContextSwapsAtomically(t *testing.T) {
	asm, ctxPath := setupAssemblerWithConfig(t, "VERSION-A firmenweiter Kontext")
	if got := asm.CompanyContext(); !strings.Contains(got, "VERSION-A") {
		t.Fatalf("initial context = %q, want VERSION-A", got)
	}

	if err := os.WriteFile(ctxPath, []byte("VERSION-B firmenweiter Kontext"), 0600); err != nil {
		t.Fatal(err)
	}
	if n := asm.ReloadCompanyContext(); n == 0 {
		t.Fatal("reload returned 0 bytes for a non-empty file")
	}
	if got := asm.CompanyContext(); !strings.Contains(got, "VERSION-B") {
		t.Errorf("after reload context = %q, want VERSION-B", got)
	}

	blocks, err := asm.Assemble(1, "claude", EvolutionData{}, "")
	if err != nil {
		t.Fatalf("Assemble() error: %v", err)
	}
	foundNew := false
	for _, b := range blocks {
		if b.Label == "company" && strings.Contains(b.Content, "VERSION-B") {
			foundNew = true
		}
	}
	if !foundNew {
		t.Error("Assemble did not pick up the reloaded company context")
	}
}

// #440 AC-2: concurrent reloads + reads never expose a half-swapped value (run with -race).
func TestReloadCompanyContextConcurrent(t *testing.T) {
	asm, ctxPath := setupAssemblerWithConfig(t, "initialer Kontext")
	var wg sync.WaitGroup
	for i := 0; i < 8; i++ {
		wg.Add(1)
		go func(n int) {
			defer wg.Done()
			for j := 0; j < 50; j++ {
				_ = os.WriteFile(ctxPath, []byte(strings.Repeat("X", n*10+j+1)), 0600)
				asm.ReloadCompanyContext()
			}
		}(i)
	}
	for i := 0; i < 8; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := 0; j < 50; j++ {
				_, _ = asm.Assemble(1, "claude", EvolutionData{}, "")
				_ = asm.CompanyContext()
			}
		}()
	}
	wg.Wait()
}
