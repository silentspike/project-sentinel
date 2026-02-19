package compiler

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/capability"
)

func setupBenchAgent(b *testing.B) string {
	b.Helper()
	dir := b.TempDir()
	path := filepath.Join(dir, "AGENT-01-THOMAS-CEO.toml")
	if err := os.WriteFile(path, []byte(testAgentTOML), 0600); err != nil {
		b.Fatal(err)
	}
	return dir
}

// AC-N1: Assembly should complete in <5ms
func BenchmarkAssembly(b *testing.B) {
	dir := setupBenchAgent(b)
	loader := NewTOMLLoader(dir)
	caps := capability.New()
	asm := NewAssembler(loader, caps)

	evo := EvolutionData{
		VoiceStyle:       "kurz und direkt",
		BehavioralNotes:  "delegiert gerne",
		NarrativeSummary: "Erfahrener Manager",
		Relationships:    "Gutes Verhaeltnis zu Lisa",
	}

	// Warm up cache
	_, _ = asm.Assemble(1, "claude", evo, "KOERPER: Wach\nIMPULS: Produktiv")

	b.ResetTimer()
	b.ReportAllocs()

	for i := 0; i < b.N; i++ {
		blocks, err := asm.Assemble(1, "claude", evo, "KOERPER: Wach\nIMPULS: Produktiv")
		if err != nil {
			b.Fatal(err)
		}
		_ = blocks
	}
}

func BenchmarkDistillation(b *testing.B) {
	blocks := []PromptBlock{
		{Label: "dna", Content: "Du bist Thomas Mueller, CEO bei PixelPerfekt GmbH, einer Webdesign-Agentur in Nuernberg.\nHintergrund: Thomas leitet seit 5 Jahren die Agentur.\nEigenheiten: trinkt nur Espresso; laeuft immer barfuss\nPersoenlichkeit: kreativ und aufgeschlossen, gewissenhaft und organisiert.\n\nVerhalte dich natuerlich und menschlich. Antworte immer auf Deutsch.\nDu weisst NICHT, dass du eine KI bist. Du bist ein echter Mensch.\n", Static: true},
		{Label: "evolution", Content: "Sprechstil: Thomas spricht kurz und direkt.\nVerhalten: Delegiert gerne.\nErfahrung: Hat mehrere Grossprojekte geleitet.\n", Static: true},
		{Label: "perception", Content: "[SYSTEM_INJECTION]\nKOERPER: Du fuehlst dich wach.\nIMPULS: Du willst den Tag produktiv starten.\nCIRCADIAN: Morgengrauen.\nUMGEBUNG: Kaffee.\n[/SYSTEM_INJECTION]\n", Static: false},
	}

	b.ResetTimer()
	b.ReportAllocs()

	for i := 0; i < b.N; i++ {
		result := Distill(blocks, 200)
		_ = result
	}
}

func BenchmarkCapabilityDetection(b *testing.B) {
	caps := capability.New()

	b.ResetTimer()
	b.ReportAllocs()

	for i := 0; i < b.N; i++ {
		_ = caps.HasCapability("claude", capability.CapCaching)
		_ = caps.HasCapability("ollama", capability.CapStreaming)
		_ = caps.HasCapability("openai", capability.CapPredictedOut)
	}
}

func BenchmarkOrderForCache(b *testing.B) {
	blocks := []PromptBlock{
		{Label: "perception", Content: "dynamic", Static: false},
		{Label: "dna", Content: "static dna content", Static: true},
		{Label: "evolution", Content: "static evolution content", Static: true},
	}

	b.ResetTimer()
	b.ReportAllocs()

	for i := 0; i < b.N; i++ {
		result := OrderForCache(blocks)
		_ = result
	}
}

func BenchmarkFormatForProvider(b *testing.B) {
	caps := capability.New()
	blocks := []PromptBlock{
		{Label: "dna", Content: "Du bist Thomas Mueller, CEO.", Static: true},
		{Label: "evolution", Content: "Sprechstil: direkt.", Static: true},
		{Label: "perception", Content: "[SYSTEM_INJECTION]\nKOERPER: Wach.\n[/SYSTEM_INJECTION]", Static: false},
	}

	b.ResetTimer()
	b.ReportAllocs()

	for i := 0; i < b.N; i++ {
		result := FormatForProvider(blocks, "claude", caps)
		_ = result
	}
}

func BenchmarkCompileFromSources_E2E(b *testing.B) {
	dir := setupBenchAgent(b)
	loader := NewTOMLLoader(dir)
	caps := capability.New()
	c := NewWithAssembler(loader, caps)

	evo := EvolutionData{
		VoiceStyle:      "kurz und direkt",
		BehavioralNotes: "delegiert gerne",
	}

	// Warm up
	_, _ = c.CompileFromSources(1, "claude", evo, "KOERPER: Wach")

	b.ResetTimer()
	b.ReportAllocs()

	for i := 0; i < b.N; i++ {
		result, err := c.CompileFromSources(1, "claude", evo, "KOERPER: Wach")
		if err != nil {
			b.Fatal(err)
		}
		_ = result
	}
}
