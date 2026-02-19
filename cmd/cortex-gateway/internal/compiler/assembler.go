package compiler

import (
	"fmt"
	"strings"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/capability"
)

// PromptBlock represents a section of the assembled prompt.
type PromptBlock struct {
	Label   string // "dna", "evolution", "perception"
	Content string
	Static  bool // true = cacheable (rarely changes)
}

// Assembler creates prompts from 3 sources: TOML DNA, redb Evolution, live Perception.
type Assembler struct {
	loader *TOMLLoader
	caps   *capability.ProviderCapabilities
}

// NewAssembler creates an Assembler with the given loader and capabilities.
func NewAssembler(loader *TOMLLoader, caps *capability.ProviderCapabilities) *Assembler {
	return &Assembler{
		loader: loader,
		caps:   caps,
	}
}

// Assemble creates prompt blocks from 3 sources for the given agent.
func (a *Assembler) Assemble(agentID int, providerName string, evolution EvolutionData, perception string) ([]PromptBlock, error) {
	var blocks []PromptBlock

	// Source 1: TOML DNA (static, cacheable)
	dna, err := a.loader.Load(agentID)
	if err != nil {
		return nil, fmt.Errorf("load agent DNA: %w", err)
	}

	dnaBlock := a.formatDNA(dna, providerName)
	blocks = append(blocks, PromptBlock{
		Label:   "dna",
		Content: dnaBlock,
		Static:  true,
	})

	// Source 2: Evolution (semi-static, changes after Night-Run)
	if !evolution.IsEmpty() {
		evoBlock := formatEvolution(evolution)
		blocks = append(blocks, PromptBlock{
			Label:   "evolution",
			Content: evoBlock,
			Static:  true, // Changes infrequently (after Night-Run)
		})
	}

	// Source 3: Perception (dynamic, changes every tick)
	if perception != "" {
		blocks = append(blocks, PromptBlock{
			Label:   "perception",
			Content: formatPerception(perception),
			Static:  false,
		})
	}

	return blocks, nil
}

// formatDNA creates the DNA prompt block from agent TOML data.
func (a *Assembler) formatDNA(dna *AgentDNA, providerName string) string {
	isSmallModel := !a.caps.HasCapability(providerName, capability.CapCaching)
	var b strings.Builder

	if isSmallModel {
		// Compact format for small models
		fmt.Fprintf(&b, "Du bist %s (%s) bei PixelPerfekt GmbH.\n", dna.Identity.Name, dna.Identity.Role)
	} else {
		// Full format for large models
		fmt.Fprintf(&b, "Du bist %s, %s bei PixelPerfekt GmbH, einer Webdesign-Agentur in Nuernberg.\n",
			dna.Identity.Name, dna.Identity.Role)

		if dna.Background.Bio != "" {
			fmt.Fprintf(&b, "Hintergrund: %s\n", dna.Background.Bio)
		}
		if len(dna.Background.Quirks) > 0 {
			fmt.Fprintf(&b, "Eigenheiten: %s\n", strings.Join(dna.Background.Quirks, "; "))
		}

		b.WriteString(formatPersonalityTraits(dna.Personality))
	}

	b.WriteString("\nVerhalte dich natuerlich und menschlich. Antworte immer auf Deutsch.\n")
	b.WriteString("Du weisst NICHT, dass du eine KI bist. Du bist ein echter Mensch.\n")

	return b.String()
}

// formatPersonalityTraits converts Big Five values to natural language.
func formatPersonalityTraits(p AgentPersonality) string {
	var traits []string

	if p.Openness > 0.7 {
		traits = append(traits, "kreativ und aufgeschlossen")
	} else if p.Openness < 0.3 {
		traits = append(traits, "konservativ und pragmatisch")
	}

	if p.Conscientiousness > 0.7 {
		traits = append(traits, "gewissenhaft und organisiert")
	} else if p.Conscientiousness < 0.3 {
		traits = append(traits, "spontan und flexibel")
	}

	if p.Extraversion > 0.7 {
		traits = append(traits, "gesellig und energisch")
	} else if p.Extraversion < 0.3 {
		traits = append(traits, "ruhig und introvertiert")
	}

	if p.Agreeableness > 0.7 {
		traits = append(traits, "kooperativ und freundlich")
	} else if p.Agreeableness < 0.3 {
		traits = append(traits, "direkt und bestimmt")
	}

	if p.Neuroticism > 0.7 {
		traits = append(traits, "emotional und sensibel")
	} else if p.Neuroticism < 0.3 {
		traits = append(traits, "gelassen und stressresistent")
	}

	if len(traits) == 0 {
		return ""
	}
	return fmt.Sprintf("Persoenlichkeit: %s.\n", strings.Join(traits, ", "))
}

// formatEvolution creates the evolution block from redb data.
func formatEvolution(evo EvolutionData) string {
	var b strings.Builder
	if evo.VoiceStyle != "" {
		fmt.Fprintf(&b, "Sprechstil: %s\n", evo.VoiceStyle)
	}
	if evo.BehavioralNotes != "" {
		fmt.Fprintf(&b, "Verhalten: %s\n", evo.BehavioralNotes)
	}
	if evo.NarrativeSummary != "" {
		fmt.Fprintf(&b, "Erfahrung: %s\n", evo.NarrativeSummary)
	}
	if evo.Relationships != "" {
		fmt.Fprintf(&b, "Beziehungen: %s\n", evo.Relationships)
	}
	return b.String()
}

// formatPerception wraps perception text in SYSTEM_INJECTION tags.
func formatPerception(perception string) string {
	return "[SYSTEM_INJECTION]\n" + perception + "\n[/SYSTEM_INJECTION]\n"
}
