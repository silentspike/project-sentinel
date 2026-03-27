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
	Match     func(fp Fingerprint, isAddressed bool) bool
	Templates map[string]string // personality_type ("I"/"E") → response template
	Actions   []Action
}

// DefaultRules returns the initial set of deterministic synthesis rules.
// CRITICAL (AC-6): ALL rules require !HasHeard AND !HasImpulse AND !isAddressed.
// Chat/social interactions and Operator-Impulses (Gaia/Broadcast) ALWAYS go to the real LLM.
func DefaultRules() []Rule {
	return []Rule{
		{
			Name: "bio_bladder_p0",
			Match: func(fp Fingerprint, isAddressed bool) bool {
				return fp.Bladder >= 9 && !fp.HasHeard && !isAddressed && !fp.HasChaos && !fp.HasImpulse
			},
			Templates: map[string]string{
				"I": "*steht leise auf und geht schnellen Schrittes zur Tuer*",
				"E": "*steht hastig auf* Entschuldigung, ich muss dringend... *eilt zur Tuer*",
			},
			Actions: []Action{
				{Type: "emote", Content: "*steht auf*", Emotion: "neutral"},
				{Type: "move", Content: "Zur Toilette", Target: "toilette-eg"},
			},
		},
		{
			Name: "bio_hunger_p0",
			Match: func(fp Fingerprint, isAddressed bool) bool {
				return fp.Hunger >= 9 && !fp.HasHeard && !isAddressed && !fp.HasChaos && !fp.HasImpulse
			},
			Templates: map[string]string{
				"I": "*haelt sich den Magen und steht leise auf* *geht in Richtung Kueche*",
				"E": "*klopft auf den Tisch* So, ich brauch jetzt dringend was zu essen! *steht auf und geht zur Kueche*",
			},
			Actions: []Action{
				{Type: "emote", Content: "*steht auf*", Emotion: "hungry"},
				{Type: "move", Content: "Zur Kueche", Target: "kueche-eg"},
			},
		},
		{
			Name: "bio_energy_p0",
			Match: func(fp Fingerprint, isAddressed bool) bool {
				return fp.Energy <= 1 && !fp.HasHeard && !isAddressed && !fp.HasChaos && !fp.HasImpulse
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
			Match: func(fp Fingerprint, isAddressed bool) bool {
				return fp.Caffeine <= 1 && fp.Energy <= 5 && !fp.HasHeard && !isAddressed && !fp.HasChaos && !fp.HasImpulse
			},
			Templates: map[string]string{
				"I": "*steht auf und geht leise Kaffee holen*",
				"E": "*steht auf* Kaffee! Braucht noch jemand einen? *geht in die Kueche*",
			},
			Actions: []Action{
				{Type: "emote", Content: "*steht auf*", Emotion: "neutral"},
				{Type: "move", Content: "Kaffee holen", Target: "kueche-eg"},
			},
		},
		{
			Name: "circadian_morning",
			Match: func(fp Fingerprint, isAddressed bool) bool {
				return fp.SimHour >= 6 && fp.SimHour <= 7 && fp.Energy > 5 &&
					!fp.HasHeard && !isAddressed && !fp.HasChaos && !fp.HasImpulse
			},
			Templates: map[string]string{
				"I": "*faehrt leise den Rechner hoch und oeffnet die Mails*",
				"E": "Guten Morgen! *faehrt den Rechner hoch und checkt die Mails* Mal schauen was heute ansteht...",
			},
			Actions: []Action{
				{Type: "emote", Content: "*faehrt den Rechner hoch*", Emotion: "neutral"},
			},
		},
		{
			Name: "circadian_lunch",
			Match: func(fp Fingerprint, isAddressed bool) bool {
				return fp.SimHour >= 12 && fp.SimHour <= 13 && fp.Hunger > 5 &&
					!fp.HasHeard && !isAddressed && !fp.HasChaos && !fp.HasImpulse
			},
			Templates: map[string]string{
				"I": "*packt Sachen zusammen und geht in die Kueche*",
				"E": "Mittagspause! Wer kommt mit? *steht auf und geht zur Kueche*",
			},
			Actions: []Action{
				{Type: "emote", Content: "*steht auf*", Emotion: "happy"},
				{Type: "move", Content: "Mittagspause", Target: "kueche-eg"},
			},
		},
		{
			Name: "physics_temp_high",
			Match: func(fp Fingerprint, isAddressed bool) bool {
				return fp.TempHigh && !fp.HasHeard && !isAddressed && !fp.HasChaos && !fp.HasImpulse
			},
			Templates: map[string]string{
				"I": "*steht auf und oeffnet leise das Fenster*",
				"E": "*steht auf* Boah, ist das warm hier! *oeffnet das Fenster weit* So, besser!",
			},
			Actions: []Action{
				{Type: "emote", Content: "*oeffnet das Fenster*", Emotion: "uncomfortable"},
			},
		},
		// heartbeat_idle — catch-all for agents without stimuli.
		// Matches regardless of PresenceCount (agents in shared offices work silently).
		// MUST be last rule (most general).
		{
			Name: "heartbeat_idle",
			Match: func(fp Fingerprint, isAddressed bool) bool {
				return !fp.HasHeard && !isAddressed && !fp.HasChaos && !fp.HasImpulse
			},
			Templates: map[string]string{
				"I": "*arbeitet still und konzentriert am aktuellen Projekt*",
				"E": "*tippt energisch auf der Tastatur und murmelt leise vor sich hin*",
			},
			Actions: []Action{
				{Type: "emote", Content: "*arbeitet konzentriert*", Emotion: "focused"},
			},
		},
	}
}
