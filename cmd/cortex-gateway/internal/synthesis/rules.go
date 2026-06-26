package synthesis

// Action represents a pre-built action for synthesis responses.
type Action struct {
	Type    string `json:"type"`
	Content string `json:"content"`
	Target  string `json:"target,omitempty"`
	Emotion string `json:"emotion,omitempty"`
}

// Rule defines a deterministic synthesis rule.
// Each rule matches a fingerprint condition and produces a templated response.
type Rule struct {
	Name      string
	Match     func(fp Fingerprint, ctx Context) bool
	Templates map[string]string // personality_type ("I"/"E") → response template
	Actions   []Action
	Build     func(fp Fingerprint, ctx Context) []Action
}

// RuleState is the per-rule enable state exposed via the control plane (#429).
type RuleState struct {
	Name    string `json:"name"`
	Enabled bool   `json:"enabled"`
}

// DefaultRules returns the initial set of deterministic synthesis rules.
// CRITICAL (AC-6): ALL rules require !HasHeard AND !HasImpulse AND !isAddressed.
// Chat/social interactions and Operator-Impulses (Gaia/Broadcast) ALWAYS go to the real LLM.
func DefaultRules() []Rule {
	return []Rule{
		{
			Name: "bio_bladder",
			Match: func(fp Fingerprint, ctx Context) bool {
				return fp.Bladder >= 9 && baseGate(fp, ctx)
			},
			Templates: map[string]string{
				"I": "*steht leise auf und geht schnellen Schrittes zur Tuer*",
				"E": "*steht hastig auf* Entschuldigung, ich muss dringend... *eilt zur Tuer*",
			},
			Build: bladderActions,
		},
		{
			Name: "bio_hunger",
			Match: func(fp Fingerprint, ctx Context) bool {
				return fp.Hunger >= 9 && baseGate(fp, ctx)
			},
			Templates: map[string]string{
				"I": "*haelt sich den Magen und steht leise auf* *geht in Richtung Kueche*",
				"E": "*klopft auf den Tisch* So, ich brauch jetzt dringend was zu essen! *steht auf und geht zur Kueche*",
			},
			Actions: []Action{
				{Type: "emote", Content: "*steht auf*", Emotion: "hungry"},
				{Type: "move", Content: "Zur Kueche", Target: "kueche"},
			},
		},
		{
			Name: "bio_energy",
			Match: func(fp Fingerprint, ctx Context) bool {
				return fp.Energy <= 1 && baseGate(fp, ctx)
			},
			Templates: map[string]string{
				"I": "*lehnt sich zurueck und schliesst kurz die Augen* *atmet tief durch*",
				"E": "*reibt sich die Augen* Boah, ich brauch dringend ne Pause! *steht auf und streckt sich*",
			},
			Actions: []Action{
				{Type: "emote", Content: "*macht eine Pause*", Emotion: "tired"},
			},
		},
		{
			Name: "bio_caffeine_low",
			Match: func(fp Fingerprint, ctx Context) bool {
				return fp.Caffeine <= 1 && fp.Energy <= 5 && baseGate(fp, ctx)
			},
			Templates: map[string]string{
				"I": "*steht auf und geht leise Kaffee holen*",
				"E": "*steht auf* Kaffee! Braucht noch jemand einen? *geht in die Kueche*",
			},
			Actions: []Action{
				{Type: "emote", Content: "*steht auf*", Emotion: "neutral"},
				{Type: "move", Content: "Kaffee holen", Target: "kueche"},
			},
		},
		{
			Name: "circadian_morning",
			Match: func(fp Fingerprint, ctx Context) bool {
				return fp.SimHour >= 6 && fp.SimHour <= 7 && fp.Energy > 5 && baseGate(fp, ctx)
			},
			Templates: map[string]string{
				"I": "*faehrt leise den Rechner hoch und oeffnet die Mails*",
				"E": "Guten Morgen! *faehrt den Rechner hoch und prueft die Mails* Mal schauen was heute ansteht...",
			},
			Actions: []Action{
				{Type: "emote", Content: "*faehrt den Rechner hoch*", Emotion: "neutral"},
			},
		},
		{
			Name: "circadian_lunch",
			Match: func(fp Fingerprint, ctx Context) bool {
				return fp.SimHour >= 12 && fp.SimHour <= 13 && fp.Hunger > 5 && baseGate(fp, ctx)
			},
			Templates: map[string]string{
				"I": "*nimmt die Sachen zusammen und geht in die Kueche*",
				"E": "Mittagspause! Wer kommt mit? *steht auf und geht zur Kueche*",
			},
			Actions: []Action{
				{Type: "emote", Content: "*steht auf*", Emotion: "happy"},
				{Type: "move", Content: "Mittagspause", Target: "kueche"},
			},
		},
		{
			Name: "physics_temp_high",
			Match: func(fp Fingerprint, ctx Context) bool {
				return fp.TempHigh && baseGate(fp, ctx)
			},
			Templates: map[string]string{
				"I": "*steht auf und oeffnet leise das Fenster*",
				"E": "*steht auf* Boah, ist das warm hier! *oeffnet das Fenster weit* So, besser!",
			},
			Actions: []Action{
				{Type: "tool_use", Content: "Fenster oeffnen", Target: "open_window", Emotion: "uncomfortable"},
			},
		},
		{
			Name: "physics_noise_high",
			Match: func(fp Fingerprint, ctx Context) bool {
				return ctx.NoiseHigh && baseGate(fp, ctx)
			},
			Templates: map[string]string{
				"I": "*zieht kurz die Schultern hoch und setzt die Kopfhoerer auf*",
				"E": "*verzieht das Gesicht* Puh, ist das laut hier. *setzt Kopfhoerer auf und arbeitet weiter*",
			},
			Actions: []Action{
				{Type: "tool_use", Content: "Kopfhoerer aufsetzen", Target: "headphones_on", Emotion: "frustrated"},
			},
		},
		{
			Name: "routine_idle_alone",
			Match: func(fp Fingerprint, ctx Context) bool {
				return fp.PresenceCount == 0 && baseGate(fp, ctx)
			},
			Templates: map[string]string{
				"I": "*arbeitet still und konzentriert am aktuellen Projekt*",
				"E": "*tippt energisch auf der Tastatur und murmelt leise vor sich hin*",
			},
			Actions: []Action{
				{Type: "emote", Content: "*arbeitet konzentriert*", Emotion: "focused"},
			},
		},
		{
			Name: "routine_idle_with_presence",
			Match: func(fp Fingerprint, ctx Context) bool {
				return fp.PresenceCount > 0 && baseGate(fp, ctx)
			},
			Templates: map[string]string{
				"I": "*arbeitet still weiter und nimmt die Anwesenheit der anderen wahr*",
				"E": "*arbeitet weiter, schaut kurz zu den anderen und bleibt im Flow*",
			},
			Actions: []Action{
				{Type: "emote", Content: "*arbeitet ruhig im Teamkontext weiter*", Emotion: "focused"},
			},
		},
	}
}

func baseGate(fp Fingerprint, ctx Context) bool {
	return !fp.HasHeard && !ctx.IsAddressed && !fp.HasChaos && !fp.HasImpulse
}

func bladderActions(_ Fingerprint, ctx Context) []Action {
	return []Action{
		{Type: "emote", Content: "*steht auf*", Emotion: "neutral"},
		{Type: "move", Content: "Zur Toilette", Target: toiletTarget(ctx.AgentID)},
	}
}

func toiletTarget(agentID int) string {
	if agentID%2 == 0 {
		return "toilette-eg-damen"
	}
	return "toilette-eg-herren"
}
