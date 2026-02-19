package compiler

import (
	"fmt"
	"strings"
)

// Distill reduces prompt blocks to fit within maxTokens.
// Prioritizes identity and urgent perceptions, compresses everything else.
func Distill(blocks []PromptBlock, maxTokens int) []PromptBlock {
	total := 0
	for _, b := range blocks {
		total += EstimateTokens(b.Content)
	}

	if total <= maxTokens {
		return blocks
	}

	result := make([]PromptBlock, 0, len(blocks))
	for _, b := range blocks {
		switch b.Label {
		case "dna":
			result = append(result, PromptBlock{
				Label:   b.Label,
				Content: distillDNA(b.Content),
				Static:  b.Static,
			})
		case "evolution":
			result = append(result, PromptBlock{
				Label:   b.Label,
				Content: distillEvolution(b.Content),
				Static:  b.Static,
			})
		case "perception":
			result = append(result, PromptBlock{
				Label:   b.Label,
				Content: distillPerception(b.Content),
				Static:  b.Static,
			})
		default:
			result = append(result, b)
		}
	}
	return result
}

// distillDNA compresses DNA to essential identity info.
func distillDNA(content string) string {
	lines := strings.Split(content, "\n")
	var b strings.Builder
	for _, line := range lines {
		// Keep: identity line ("Du bist ..."), human identity line, behavior instruction
		if strings.HasPrefix(line, "Du bist ") ||
			strings.Contains(line, "natuerlich und menschlich") ||
			strings.Contains(line, "KI bist") {
			b.WriteString(line)
			b.WriteByte('\n')
		}
	}
	return b.String()
}

// distillEvolution keeps only the first sentence of each evolution field.
func distillEvolution(content string) string {
	lines := strings.Split(content, "\n")
	var b strings.Builder
	for _, line := range lines {
		if line == "" {
			continue
		}
		b.WriteString(firstSentence(line))
		b.WriteByte('\n')
	}
	return b.String()
}

// distillPerception keeps only KOERPER and IMPULS fields (most urgent).
func distillPerception(content string) string {
	lines := strings.Split(content, "\n")
	var b strings.Builder
	keep := false
	for _, line := range lines {
		if strings.HasPrefix(line, "[SYSTEM_INJECTION]") || strings.HasPrefix(line, "[/SYSTEM_INJECTION]") {
			b.WriteString(line)
			b.WriteByte('\n')
			continue
		}
		if strings.HasPrefix(line, "KOERPER:") || strings.HasPrefix(line, "IMPULS:") {
			keep = true
		} else if len(line) > 0 && line[0] >= 'A' && line[0] <= 'Z' && strings.Contains(line, ":") {
			keep = false
		}
		if keep {
			b.WriteString(line)
			b.WriteByte('\n')
		}
	}
	return b.String()
}

// firstSentence returns the text up to and including the first period.
func firstSentence(s string) string {
	idx := strings.Index(s, ". ")
	if idx >= 0 {
		return s[:idx+1]
	}
	if strings.HasSuffix(s, ".") {
		return s
	}
	return s
}

// DistillDNAFromAgent creates a compact DNA string for small models.
func DistillDNAFromAgent(dna *AgentDNA) string {
	var traits []string
	p := dna.Personality

	if p.Openness > 0.7 {
		traits = append(traits, "kreativ")
	}
	if p.Conscientiousness > 0.7 {
		traits = append(traits, "gewissenhaft")
	}
	if p.Extraversion > 0.7 {
		traits = append(traits, "extrovertiert")
	} else if p.Extraversion < 0.3 {
		traits = append(traits, "introvertiert")
	}

	personality := ""
	if len(traits) > 0 {
		personality = fmt.Sprintf(" (%s)", strings.Join(traits, ", "))
	}

	quirk := ""
	if len(dna.Background.Quirks) > 0 {
		quirk = fmt.Sprintf(" Eigenheit: %s.", dna.Background.Quirks[0])
	}

	return fmt.Sprintf("Du bist %s (%s) bei PixelPerfekt GmbH.%s%s\nVerhalte dich natuerlich. Antworte auf Deutsch. Du bist ein Mensch.\n",
		dna.Identity.Name, dna.Identity.Role, personality, quirk)
}
