package compiler

import (
	"strings"
	"testing"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/capability"
)

func TestCompileStructured_FallbackProducesEightTaggedBlocks(t *testing.T) {
	c := New()

	prompt := c.CompileStructured("claude", "Thomas Mueller", "CEO", StructuredPerception{
		BodyText:        "Hunger: 45%, Energy: 62%",
		EnvironmentText: "Buero der Geschaeftsfuehrung (OG)",
		ImpulseText:     "Du musst jetzt in die Kueche gehen.",
		RoomID:          "buero-ceo",
	})

	if len(prompt.SystemBlocks) != 8 {
		t.Fatalf("expected 8 system blocks, got %d", len(prompt.SystemBlocks))
	}

	expectedTags := []string{
		"agent-identity",
		"company-context",
		"personality",
		"experience",
		"body-state",
		"environment",
		"inner-voice",
		"action-format",
	}

	for i, tag := range expectedTags {
		if prompt.SystemBlocks[i].Tag != tag {
			t.Fatalf("block %d tag = %q, want %q", i, prompt.SystemBlocks[i].Tag, tag)
		}
		if !strings.Contains(prompt.SystemBlocks[i].Text, "<"+tag+">") {
			t.Errorf("block %q missing opening tag in text: %q", tag, prompt.SystemBlocks[i].Text)
		}
		if !strings.Contains(prompt.SystemBlocks[i].Text, "</"+tag+">") {
			t.Errorf("block %q missing closing tag in text: %q", tag, prompt.SystemBlocks[i].Text)
		}
	}

	if prompt.SystemBlocks[0].CacheControl == nil || prompt.SystemBlocks[0].CacheControl.Type != "ephemeral" {
		t.Errorf("expected cache control on %q block", prompt.SystemBlocks[0].Tag)
	}
	if prompt.SystemBlocks[1].CacheControl == nil || prompt.SystemBlocks[1].CacheControl.Type != "ephemeral" {
		t.Errorf("expected cache control on %q block", prompt.SystemBlocks[1].Tag)
	}
	if prompt.SystemBlocks[2].CacheControl == nil || prompt.SystemBlocks[2].CacheControl.Type != "ephemeral" {
		t.Errorf("expected cache control on %q block", prompt.SystemBlocks[2].Tag)
	}

	if !strings.Contains(prompt.LegacySystemPrompt, "<agent-identity>") {
		t.Error("expected legacy system prompt to contain flattened structured blocks")
	}
	if !strings.Contains(prompt.LegacySystemPrompt, "<action-format>") {
		t.Error("expected legacy system prompt to contain action-format block")
	}
}

func TestCompileStructuredFromSources_ProducesTaggedBlocksAndSemantics(t *testing.T) {
	dir := setupTestAgent(t)
	loader := NewTOMLLoader(dir)
	caps := capability.New()
	c := NewWithAssembler(loader, caps)

	prompt, err := c.CompileStructuredFromSources(1, "anthropic-direct", EvolutionData{
		VoiceStyle:       "direkt, selbstbewusst",
		BehavioralNotes:  "delegiert gerne",
		NarrativeSummary: "Hat die Firma vor 5 Jahren gegruendet.",
	}, StructuredPerception{
		BodyText:        "Hunger: 45%, Energy: 62%, Bladder: 34%",
		EnvironmentText: "Buero der Geschaeftsfuehrung (OG)",
		ImpulseText:     "Du hast grossen Hunger und musst in die Kueche gehen.",
		RoomID:          "buero-ceo",
	})
	if err != nil {
		t.Fatalf("CompileStructuredFromSources() error: %v", err)
	}

	if len(prompt.SystemBlocks) != 8 {
		t.Fatalf("expected 8 system blocks, got %d", len(prompt.SystemBlocks))
	}
	if prompt.SystemBlocks[0].Tag != "agent-identity" {
		t.Fatalf("expected first block to be agent-identity, got %q", prompt.SystemBlocks[0].Tag)
	}
	if !strings.Contains(prompt.SystemBlocks[0].Text, "UNVERAENDERLICH") {
		t.Errorf("expected identity semantics in first block, got %q", prompt.SystemBlocks[0].Text)
	}
	if !strings.Contains(prompt.SystemBlocks[1].Text, "Diese Informationen sind Fakten ueber deine Firma") {
		t.Errorf("expected company-context semantics, got %q", prompt.SystemBlocks[1].Text)
	}
	if !strings.Contains(prompt.SystemBlocks[2].Text, "Diese Werte definieren DEIN Verhalten") {
		t.Errorf("expected personality semantics, got %q", prompt.SystemBlocks[2].Text)
	}
	if !strings.Contains(prompt.SystemBlocks[6].Text, "Du kannst ihn NICHT ignorieren") {
		t.Errorf("expected hard inner-voice semantics, got %q", prompt.SystemBlocks[6].Text)
	}
	if !strings.Contains(prompt.SystemBlocks[7].Text, "Antworte mit JSON") {
		t.Errorf("expected action-format instructions, got %q", prompt.SystemBlocks[7].Text)
	}
}

func TestAppendNarrativeNudge_AppendsToExperienceBlock(t *testing.T) {
	c := New()
	prompt := c.CompileStructured("claude", "Thomas Mueller", "CEO", StructuredPerception{})

	updated := AppendNarrativeNudge(prompt, "Bleib heute besonders wachsam.")

	if !strings.Contains(updated.SystemBlocks[3].Text, "NARRATIVE_NUDGE: Bleib heute besonders wachsam.") {
		t.Errorf("expected narrative nudge in experience block, got %q", updated.SystemBlocks[3].Text)
	}
	if !strings.Contains(updated.LegacySystemPrompt, "NARRATIVE_NUDGE: Bleib heute besonders wachsam.") {
		t.Error("expected flattened legacy prompt to include narrative nudge")
	}
}
