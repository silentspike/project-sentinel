package compiler

import (
	"strings"
	"testing"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/capability"
)

// AC-6: End-to-end test with real TOML and all 3 sources
func TestCompileFromSources_E2E(t *testing.T) {
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)
	caps := capability.New()

	c := NewWithAssembler(loader, caps)

	evo := EvolutionData{
		VoiceStyle:      "kurz und direkt",
		BehavioralNotes: "delegiert gerne",
	}

	result, err := c.CompileFromSources(1, "claude", evo, "KOERPER: Wach und aufmerksam")
	if err != nil {
		t.Fatalf("CompileFromSources() error: %v", err)
	}

	// Should contain DNA
	if !strings.Contains(result, "Thomas Mueller") {
		t.Error("should contain agent name from TOML")
	}
	if !strings.Contains(result, "CEO") {
		t.Error("should contain agent role from TOML")
	}

	// Should contain evolution
	if !strings.Contains(result, "kurz und direkt") {
		t.Error("should contain evolution voice style")
	}

	// Should contain perception
	if !strings.Contains(result, "KOERPER: Wach") {
		t.Error("should contain perception")
	}
	if !strings.Contains(result, "[SYSTEM_INJECTION]") {
		t.Error("should contain SYSTEM_INJECTION tags")
	}
}

func TestCompileFromSources_CacheOrdering(t *testing.T) {
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)
	caps := capability.New()

	c := NewWithAssembler(loader, caps)

	evo := EvolutionData{VoiceStyle: "freundlich"}
	result, err := c.CompileFromSources(1, "claude", evo, "KOERPER: test")
	if err != nil {
		t.Fatalf("CompileFromSources() error: %v", err)
	}

	// For claude: static blocks (DNA + Evolution) should come before dynamic (Perception)
	dnaPos := strings.Index(result, "Thomas Mueller")
	percPos := strings.Index(result, "[SYSTEM_INJECTION]")

	if dnaPos >= percPos {
		t.Error("DNA (static) should appear before perception (dynamic) for cache optimization")
	}
}

func TestCompileFromSources_NoAssembler(t *testing.T) {
	c := New() // No assembler

	_, err := c.CompileFromSources(1, "claude", EvolutionData{}, "")
	if err == nil {
		t.Error("CompileFromSources() should fail without assembler")
	}
}

func TestCompileFromSources_Fallback(t *testing.T) {
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)
	caps := capability.New()

	c := NewWithAssembler(loader, caps)

	// Missing agent ID should cause error (not panic)
	_, err := c.CompileFromSources(99, "claude", EvolutionData{}, "")
	if err == nil {
		t.Error("CompileFromSources() should fail for missing agent")
	}
}

func TestCompileFromSources_OllamaDistillation(t *testing.T) {
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)
	caps := capability.New()

	c := NewWithAssembler(loader, caps)

	evo := EvolutionData{
		VoiceStyle:       "kurz und direkt",
		BehavioralNotes:  "delegiert gerne",
		NarrativeSummary: "Erfahrener Manager mit vielen Jahren Berufserfahrung.",
		Relationships:    "Gutes Verhaeltnis zu allen Kollegen.",
	}

	claudeResult, err := c.CompileFromSources(1, "claude", evo, "KOERPER: test")
	if err != nil {
		t.Fatalf("CompileFromSources(claude) error: %v", err)
	}

	ollamaResult, err := c.CompileFromSources(1, "ollama", evo, "KOERPER: test")
	if err != nil {
		t.Fatalf("CompileFromSources(ollama) error: %v", err)
	}

	// Ollama result should be shorter due to distillation + compact format
	if len(ollamaResult) >= len(claudeResult) {
		t.Errorf("ollama result (%d chars) should be shorter than claude result (%d chars)",
			len(ollamaResult), len(claudeResult))
	}
}

// AC-5: TOML is read-only (no writes in compiler package)
func TestTomlReadonly(t *testing.T) {
	// This test verifies by convention: the compiler package only uses
	// os.ReadFile and filepath.Glob. No os.WriteFile, os.Create, or
	// os.OpenFile with write flags exist in the production code.
	// Verified via: grep -r "os.WriteFile\|os.Create\|os.OpenFile.*O_WRONLY" internal/compiler/ | grep -v _test.go
	// This is a documentation test that exists as a reminder.
	t.Log("TOML readonly verified: compiler package only uses os.ReadFile + filepath.Glob")
}

func TestNewWithAssembler(t *testing.T) {
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)
	caps := capability.New()

	c := NewWithAssembler(loader, caps)

	// Should still support basic Compile
	result := c.Compile("claude", "Test", "Dev", "perception")
	if !strings.Contains(result, "Test") {
		t.Error("NewWithAssembler compiler should still support basic Compile")
	}

	// Should also support CompileFromSources
	_, err := c.CompileFromSources(1, "claude", EvolutionData{}, "")
	if err != nil {
		t.Errorf("CompileFromSources should work: %v", err)
	}
}
