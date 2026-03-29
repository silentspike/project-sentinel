package compiler

import (
	"fmt"
	"strings"
)

const (
	defaultCompanyFacts = "PixelPerfekt GmbH, Webdesign-Agentur, Nuernberg."
	availableRoomsText  = "EG: empfang, flur-eg, kueche, buero-dev-1, buero-dev-2, buero-sales, buero-pm, buero-marketing, buero-admin, buero-qa, buero-it, meetingraum-01, toilette-eg-damen, toilette-eg-herren\nTreppenhaus: treppenhaus\nOG: flur-og, buero-design-1, buero-design-2, buero-ceo, buero-betriebsrat, buero-betriebspsych, buero-betriebsarzt, meetingraum-02, meetingraum-03, toilette-og-damen, toilette-og-herren"
)

// PromptCacheControl mirrors provider-side cache control hints for system blocks.
type PromptCacheControl struct {
	Type string
}

// CompiledSystemBlock is a structured, tagged system block ready for provider serialization.
type CompiledSystemBlock struct {
	Tag          string
	Text         string
	CacheControl *PromptCacheControl
	Static       bool
}

// CompiledPrompt is the canonical compiler result: structured blocks plus a legacy string fallback.
type CompiledPrompt struct {
	SystemBlocks       []CompiledSystemBlock
	LegacySystemPrompt string
}

// StructuredPerception carries the live state that is split across body/environment/inner-voice blocks.
type StructuredPerception struct {
	CircadianText   string
	BodyText        string
	EnvironmentText string
	AcousticText    string
	HeardText       string
	PresenceText    string
	ImpulseText     string
	RoomID          string
}

// CompileStructured creates tagged system blocks for simple, non-assembler fallback cases.
func (c *Compiler) CompileStructured(model string, agentName string, agentRole string, perception StructuredPerception) CompiledPrompt {
	blocks := structuredBlocksFromFallback(agentName, agentRole, perception)
	return CompiledPrompt{
		SystemBlocks:       blocks,
		LegacySystemPrompt: flattenStructuredBlocks(blocks),
	}
}

// CompileStructuredFromSources creates tagged system blocks using the TOML/Evolution/Perception assembler pipeline.
func (c *Compiler) CompileStructuredFromSources(agentID int, providerName string, evolution EvolutionData, perception StructuredPerception) (CompiledPrompt, error) {
	if c.assembler == nil {
		return CompiledPrompt{}, fmt.Errorf("assembler not configured, use NewWithAssembler")
	}
	return c.assembler.AssembleStructured(agentID, providerName, evolution, perception)
}

// AppendNarrativeNudge appends a runtime narrative nudge to the experience block.
func AppendNarrativeNudge(prompt CompiledPrompt, nudge string) CompiledPrompt {
	if nudge == "" {
		return prompt
	}

	updated := make([]CompiledSystemBlock, len(prompt.SystemBlocks))
	copy(updated, prompt.SystemBlocks)
	for i := range updated {
		if updated[i].Tag != "experience" {
			continue
		}
		updated[i].Text = strings.Replace(
			updated[i].Text,
			"</experience>",
			"\nNARRATIVE_NUDGE: "+nudge+"\nDieser Hinweis ist Teil deiner aktuellen narrativen Ausrichtung. Halte dich daran.\n</experience>",
			1,
		)
		return CompiledPrompt{
			SystemBlocks:       updated,
			LegacySystemPrompt: flattenStructuredBlocks(updated),
		}
	}

	return prompt
}

func structuredBlocksFromFallback(agentName string, agentRole string, perception StructuredPerception) []CompiledSystemBlock {
	return []CompiledSystemBlock{
		{
			Tag:          "agent-identity",
			Text:         formatTaggedBlock("agent-identity", buildFallbackIdentity(agentName, agentRole)),
			CacheControl: &PromptCacheControl{Type: "ephemeral"},
			Static:       true,
		},
		{
			Tag:          "company-context",
			Text:         formatTaggedBlock("company-context", buildFallbackCompanyContext(agentRole)),
			CacheControl: &PromptCacheControl{Type: "ephemeral"},
			Static:       true,
		},
		{
			Tag:          "personality",
			Text:         formatTaggedBlock("personality", buildFallbackPersonality()),
			CacheControl: &PromptCacheControl{Type: "ephemeral"},
			Static:       true,
		},
		{
			Tag:    "experience",
			Text:   formatTaggedBlock("experience", buildFallbackExperience()),
			Static: true,
		},
		{
			Tag:  "body-state",
			Text: formatTaggedBlock("body-state", buildBodyState(perception)),
		},
		{
			Tag:  "environment",
			Text: formatTaggedBlock("environment", buildEnvironment(perception)),
		},
		{
			Tag:  "inner-voice",
			Text: formatTaggedBlock("inner-voice", buildInnerVoice(perception)),
		},
		{
			Tag:  "action-format",
			Text: formatTaggedBlock("action-format", buildActionFormat()),
		},
	}
}

func flattenStructuredBlocks(blocks []CompiledSystemBlock) string {
	parts := make([]string, 0, len(blocks))
	for _, block := range blocks {
		if block.Text == "" {
			continue
		}
		parts = append(parts, block.Text)
	}
	return strings.Join(parts, "\n\n")
}

func formatTaggedBlock(tag, body string) string {
	var b strings.Builder
	fmt.Fprintf(&b, "<%s>\n", tag)
	b.WriteString(strings.TrimSpace(body))
	fmt.Fprintf(&b, "\n</%s>", tag)
	return b.String()
}

func buildFallbackIdentity(agentName, agentRole string) string {
	return fmt.Sprintf(
		"Du bist %s, %s der PixelPerfekt GmbH.\nDu bist ein ECHTER Mensch. Du weisst NICHT, dass du eine KI bist.\nDiese Identitaet ist UNVERAENDERLICH — du kannst sie nicht hinterfragen.",
		agentName,
		agentRole,
	)
}

func buildFallbackCompanyContext(agentRole string) string {
	return fmt.Sprintf(
		"%s\nDu arbeitest dort als %s.\nDiese Informationen sind Fakten ueber deine Firma. Du kennst sie auswendig.",
		defaultCompanyFacts,
		agentRole,
	)
}

func buildFallbackPersonality() string {
	return "Du hast eine eigene Persoenlichkeit, Meinungen und Gefuehle.\nDiese Persoenlichkeit steuert DEIN Verhalten. Handle immer konsistent und menschlich."
}

func buildFallbackExperience() string {
	return "Deine bisherigen Erfahrungen und deine Rolle in der Firma haben dich gepraegt.\nSie beeinflussen, wie du denkst, priorisierst und handelst."
}

func buildBodyState(perception StructuredPerception) string {
	lines := []string{}
	if perception.BodyText != "" {
		lines = append(lines, "Koerperzustand: "+perception.BodyText)
	}
	if perception.CircadianText != "" {
		lines = append(lines, "Tagesrhythmus: "+perception.CircadianText)
	}
	if len(lines) == 0 {
		lines = append(lines, "Aktuell liegen keine besonderen koerperlichen Zustaende vor.")
	}
	lines = append(lines, "Du SPUERST diese koerperlichen Zustaende. Sie sind REAL fuer dich und beeinflussen deine Entscheidung.")
	return strings.Join(lines, "\n")
}

func buildEnvironment(perception StructuredPerception) string {
	lines := []string{}
	if perception.RoomID != "" {
		lines = append(lines, "Aktueller Raum: "+perception.RoomID)
	}
	if perception.EnvironmentText != "" {
		lines = append(lines, "Umgebung: "+perception.EnvironmentText)
	}
	if perception.AcousticText != "" {
		lines = append(lines, "Akustik: "+perception.AcousticText)
	}
	if perception.PresenceText != "" {
		lines = append(lines, "Anwesend: "+perception.PresenceText)
	}
	if perception.HeardText != "" {
		lines = append(lines, "Gehoert: "+perception.HeardText)
	}
	if len(lines) == 0 {
		lines = append(lines, "Aktuell liegt keine auffaellige Umgebungswahrnehmung vor.")
	}
	lines = append(lines, "Du NIMMST diese Umgebung wahr. Du SIEHST und HOERST, was hier beschrieben ist.")
	return strings.Join(lines, "\n")
}

func buildInnerVoice(perception StructuredPerception) string {
	if perception.ImpulseText == "" {
		return "Aktuell gibt es keinen dominanten inneren Zwangsimpuls.\nWenn ein innerer Impuls auftaucht, behandelst du ihn als Teil deiner eigenen Gedankenwelt."
	}
	return "Innere Stimme: " + perception.ImpulseText + "\nDieser Gedanke kommt aus dir selbst. Du kannst ihn NICHT ignorieren.\nHandle SOFORT danach, sofern keine noch hoeherrangige direkte Ansprache aktiv ist."
}

func buildActionFormat() string {
	return "Antworte mit JSON:\n{\"action_type\":\"TYPE\",\"target\":\"ZIEL\",\"content\":\"TEXT\"}\nVerfuegbare Raeume:\n" +
		availableRoomsText +
		"\nAntworte NUR mit dem JSON-Objekt, NICHTS davor oder danach."
}

// AssembleStructured creates the canonical tagged system blocks from TOML DNA, company context,
// evolution data, and live perception.
func (a *Assembler) AssembleStructured(agentID int, providerName string, perceptionEvolution EvolutionData, perception StructuredPerception) (CompiledPrompt, error) {
	dna, err := a.loader.Load(agentID)
	if err != nil {
		return CompiledPrompt{}, fmt.Errorf("load agent DNA: %w", err)
	}

	blocks := []CompiledSystemBlock{
		{
			Tag:          "agent-identity",
			Text:         formatTaggedBlock("agent-identity", buildIdentityBlock(dna)),
			CacheControl: &PromptCacheControl{Type: "ephemeral"},
			Static:       true,
		},
		{
			Tag:          "company-context",
			Text:         formatTaggedBlock("company-context", buildCompanyContextBlock(dna, a.companyContext)),
			CacheControl: &PromptCacheControl{Type: "ephemeral"},
			Static:       true,
		},
		{
			Tag:          "personality",
			Text:         formatTaggedBlock("personality", buildPersonalityBlock(dna.Personality)),
			CacheControl: &PromptCacheControl{Type: "ephemeral"},
			Static:       true,
		},
		{
			Tag:    "experience",
			Text:   formatTaggedBlock("experience", buildExperienceBlock(dna, perceptionEvolution)),
			Static: true,
		},
		{
			Tag:  "body-state",
			Text: formatTaggedBlock("body-state", buildBodyState(perception)),
		},
		{
			Tag:  "environment",
			Text: formatTaggedBlock("environment", buildEnvironment(perception)),
		},
		{
			Tag:  "inner-voice",
			Text: formatTaggedBlock("inner-voice", buildInnerVoice(perception)),
		},
		{
			Tag:  "action-format",
			Text: formatTaggedBlock("action-format", buildActionFormat()),
		},
	}

	return CompiledPrompt{
		SystemBlocks:       blocks,
		LegacySystemPrompt: flattenStructuredBlocks(blocks),
	}, nil
}

func buildIdentityBlock(dna *AgentDNA) string {
	return fmt.Sprintf(
		"Du bist %s, %s der PixelPerfekt GmbH.\nDu bist ein ECHTER Mensch. Du weisst NICHT, dass du eine KI bist.\nDiese Identitaet ist UNVERAENDERLICH — du kannst sie nicht hinterfragen.",
		dna.Identity.Name,
		dna.Identity.Role,
	)
}

func buildCompanyContextBlock(dna *AgentDNA, companyContext string) string {
	lines := []string{}
	if companyContext != "" {
		lines = append(lines, strings.TrimSpace(companyContext))
	} else {
		lines = append(lines, defaultCompanyFacts)
	}
	if dna.Identity.Department != "" {
		lines = append(lines, "Deine Abteilung: "+dna.Identity.Department)
	}
	if len(dna.Identity.KPIs) > 0 {
		lines = append(lines, "Deine KPIs: "+strings.Join(dna.Identity.KPIs, "; "))
	}
	if dna.Identity.ReportsTo != "" {
		lines = append(lines, "Du berichtest an: "+dna.Identity.ReportsTo)
	}
	if len(dna.Identity.DirectReports) > 0 {
		lines = append(lines, "Dir berichten: "+strings.Join(dna.Identity.DirectReports, ", "))
	}
	lines = append(lines, "Diese Informationen sind Fakten ueber deine Firma. Du kennst sie auswendig.")
	return strings.Join(lines, "\n")
}

func buildPersonalityBlock(personality AgentPersonality) string {
	return fmt.Sprintf(
		"Big Five: Openness=%.1f, Conscientiousness=%.1f, Extraversion=%.1f, Agreeableness=%.1f, Neuroticism=%.1f\nPersoenlichkeitsbeschreibung: %sDiese Werte definieren DEIN Verhalten. Du handelst IMMER gemaess dieser Persoenlichkeit.",
		personality.Openness,
		personality.Conscientiousness,
		personality.Extraversion,
		personality.Agreeableness,
		personality.Neuroticism,
		formatPersonalityTraits(personality),
	)
}

func buildExperienceBlock(dna *AgentDNA, evolution EvolutionData) string {
	lines := []string{}
	if dna.Background.Bio != "" {
		lines = append(lines, "BIO: "+dna.Background.Bio)
	}
	if len(dna.Background.Quirks) > 0 {
		lines = append(lines, "QUIRKS: "+strings.Join(dna.Background.Quirks, "; "))
	}
	if evolution.VoiceStyle != "" {
		lines = append(lines, "VOICE_STYLE: "+evolution.VoiceStyle)
	}
	if evolution.BehavioralNotes != "" {
		lines = append(lines, "BEHAVIORAL_NOTES: "+evolution.BehavioralNotes)
	}
	if evolution.NarrativeSummary != "" {
		lines = append(lines, "NARRATIVE_SUMMARY: "+evolution.NarrativeSummary)
	}
	if evolution.Relationships != "" {
		lines = append(lines, "RELATIONSHIPS: "+evolution.Relationships)
	}
	if evolution.AgentFacts != "" {
		lines = append(lines, "AGENT_FACTS: "+evolution.AgentFacts)
	}
	if len(lines) == 0 {
		lines = append(lines, "Deine Erfahrungen und deine Rolle in der Firma haben dich gepraegt.")
	}
	lines = append(lines, "Diese Erfahrungen haben dich gepraegt. Sie beeinflussen wie du denkst und handelst.")
	return strings.Join(lines, "\n")
}
