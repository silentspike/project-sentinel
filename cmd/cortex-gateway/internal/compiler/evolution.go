package compiler

// EvolutionData holds redb-sourced agent evolution state.
// These values change after each Night-Run memory consolidation cycle.
type EvolutionData struct {
	VoiceStyle       string // How the agent speaks (learned patterns)
	BehavioralNotes  string // Learned behavioral patterns
	NarrativeSummary string // Summary of past experiences
	Relationships    string // Relationship descriptions with other agents
	AgentFacts       string // Trigger-based company facts (JIT from FactRetriever)
}

// IsEmpty returns true if no evolution data is present.
func (e *EvolutionData) IsEmpty() bool {
	return e.VoiceStyle == "" && e.BehavioralNotes == "" &&
		e.NarrativeSummary == "" && e.Relationships == "" &&
		e.AgentFacts == ""
}

// EvolutionFromMetadata creates EvolutionData from request metadata keys.
func EvolutionFromMetadata(meta map[string]string) EvolutionData {
	return EvolutionData{
		VoiceStyle:       meta["evolution_voice"],
		BehavioralNotes:  meta["evolution_notes"],
		NarrativeSummary: meta["evolution_narrative"],
		Relationships:    meta["evolution_relationships"],
		AgentFacts:       meta["evolution_facts"],
	}
}
