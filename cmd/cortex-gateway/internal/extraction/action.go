package extraction

import (
	"encoding/json"
	"regexp"
	"strings"
)

// ExtractedAction represents a parsed agent intention from an LLM response.
type ExtractedAction struct {
	Type    string `json:"type"` // "chat", "move", "emote", "tool_use"
	Content string `json:"content"`
	Target  string `json:"target,omitempty"`
	Emotion string `json:"emotion,omitempty"`
	Intent  string `json:"intent,omitempty"`
}

// emotionPattern pairs an emotion label with its detection regex.
type emotionPattern struct {
	label   string
	pattern *regexp.Regexp
}

// actionPattern pairs an action type with its detection regex.
type actionPattern struct {
	actionType string
	pattern    *regexp.Regexp
}

// Extractor parses LLM responses for actions and emotions.
type Extractor struct {
	emotionPatterns []emotionPattern
	actionPatterns  []actionPattern
	emotePattern    *regexp.Regexp
}

// New creates a new Extractor with default patterns.
func New() *Extractor {
	return &Extractor{
		emotionPatterns: []emotionPattern{
			{"happy", regexp.MustCompile(`(?i)(lach|freudig|gluecklich|freu mich)`)},
			{"frustrated", regexp.MustCompile(`(?i)(frustriert|genervt|aergerlich)`)},
			{"stressed", regexp.MustCompile(`(?i)(gestresst|unter druck|nervoes)`)},
			{"tired", regexp.MustCompile(`(?i)(muede|erschoepft|schlapp)`)},
			{"excited", regexp.MustCompile(`(?i)(aufgeregt|begeistert|gespannt)`)},
		},
		actionPatterns: []actionPattern{
			{"move", regexp.MustCompile(`(?i)(geh[te]?\s+(?:\w+\s+)*(?:zu|in|nach|Richtung)|lauf[te]?\s+(?:\w+\s+)*(?:zu|in|nach|Richtung)|verlasst|verlaesst|verlasse|betritt|betrete|komm[te]?\s+(?:\w+\s+)*(?:in|zu|an)|unterwegs\s+(?:\w+\s+)*(?:nach|zu|Richtung))`)},
			{"tool_use", regexp.MustCompile(`(?i)(oeffne?\s+\w+|starte?\s+\w+|benutze?\s+\w+|schreibe?\s+(?:eine?\s+)?(?:Datei|File|Code|Script|Programm))`)},
		},
		emotePattern: regexp.MustCompile(`\*[^*]+\*`),
	}
}

// jsonAction is the structured JSON format the LLM is instructed to produce.
type jsonAction struct {
	ActionType string `json:"action_type"`
	Target     string `json:"target"`
	Content    string `json:"content"`
}

// normalizeActionType maps LLM action_type values to internal types.
func normalizeActionType(t string) string {
	switch strings.ToLower(strings.TrimSpace(t)) {
	case "chat":
		return "chat"
	case "move":
		return "move"
	case "emote":
		return "emote"
	case "work":
		return "work"
	case "break":
		return "break"
	case "think":
		return "think"
	case "tool_use":
		return "tool_use"
	default:
		return "chat"
	}
}

// tryParseJSON attempts to parse a structured JSON action from the LLM response.
func tryParseJSON(response string) *ExtractedAction {
	trimmed := strings.TrimSpace(response)
	// Find JSON object in response (LLM might add text around it)
	start := strings.Index(trimmed, "{")
	end := strings.LastIndex(trimmed, "}")
	if start < 0 || end < 0 || end <= start {
		return nil
	}
	jsonStr := trimmed[start : end+1]

	var ja jsonAction
	if err := json.Unmarshal([]byte(jsonStr), &ja); err != nil {
		return nil
	}
	if ja.ActionType == "" {
		return nil
	}

	target := ja.Target
	if normalizeActionType(ja.ActionType) == "move" && target != "" {
		target = resolveRoomID(target)
	}

	return &ExtractedAction{
		Type:    normalizeActionType(ja.ActionType),
		Content: ja.Content,
		Target:  target,
	}
}

// aktionPattern parses the German "AKTION: X\nZIEL: Y\nINHALT: Z" format
// that the LLM produces when responding to structured prompts.
var aktionPattern = regexp.MustCompile(`(?i)AKTION:\s*(\w+)\s*\nZIEL:\s*(.*?)\s*\nINHALT:\s*(.*)`)

// tryParseAktion attempts to parse the German structured action format.
func tryParseAktion(response string) *ExtractedAction {
	matches := aktionPattern.FindStringSubmatch(response)
	if len(matches) < 4 {
		return nil
	}
	actionType := normalizeActionType(matches[1])
	target := strings.TrimSpace(matches[2])
	content := strings.TrimSpace(matches[3])

	// Resolve room target for move actions
	if actionType == "move" && target != "" && target != "-" {
		target = resolveRoomID(target)
	}

	return &ExtractedAction{
		Type:    actionType,
		Content: content,
		Target:  target,
	}
}

// Extract parses an LLM response for actions and emotions.
func (e *Extractor) Extract(response string) []ExtractedAction {
	// Try German structured AKTION format first (most reliable for sentinel agents)
	if parsed := tryParseAktion(response); parsed != nil {
		parsed.Emotion = e.DetectEmotion(parsed.Content)
		return []ExtractedAction{*parsed}
	}

	// Try structured JSON (preferred for generic responses)
	if parsed := tryParseJSON(response); parsed != nil {
		parsed.Emotion = e.DetectEmotion(parsed.Content)
		return []ExtractedAction{*parsed}
	}

	// Fallback: regex-based extraction for unstructured responses
	var actions []ExtractedAction
	emotion := e.DetectEmotion(response)

	// Check for emotes (text within *asterisks*)
	emotes := e.emotePattern.FindAllString(response, -1)
	for _, emote := range emotes {
		// If the emote contains a move pattern, classify as move instead
		classified := false
		for _, ap := range e.actionPatterns {
			if ap.pattern.MatchString(emote) {
				target := ""
				if ap.actionType == "move" {
					target = extractMoveTarget(emote)
				}
				actions = append(actions, ExtractedAction{
					Type:    ap.actionType,
					Content: emote,
					Target:  target,
					Emotion: emotion,
				})
				classified = true
				break
			}
		}
		if !classified {
			actions = append(actions, ExtractedAction{
				Type:    "emote",
				Content: emote,
				Emotion: emotion,
			})
		}
	}

	// Check remaining text (outside emotes) for action patterns
	remaining := e.emotePattern.ReplaceAllString(response, "")
	for _, ap := range e.actionPatterns {
		if ap.pattern.MatchString(remaining) {
			target := ""
			if ap.actionType == "move" {
				target = extractMoveTarget(remaining)
			}
			actions = append(actions, ExtractedAction{
				Type:    ap.actionType,
				Content: remaining,
				Target:  target,
				Emotion: emotion,
			})
		}
	}

	// Remaining text outside emotes/action-patterns = direct speech (Chat)
	remaining = strings.TrimSpace(remaining)
	if len(remaining) > 5 {
		alreadyCovered := false
		for _, a := range actions {
			if strings.Contains(a.Content, remaining) {
				alreadyCovered = true
				break
			}
		}
		if !alreadyCovered {
			actions = append(actions, ExtractedAction{
				Type:    "chat",
				Content: remaining,
				Emotion: emotion,
			})
		}
	}

	// If no specific action detected at all, entire response is chat
	if len(actions) == 0 {
		actions = append(actions, ExtractedAction{
			Type:    "chat",
			Content: response,
			Emotion: emotion,
		})
	}

	return actions
}

// DetectEmotion finds the dominant emotion in text.
// Returns the first matched emotion label, or "neutral" if none match.
func (e *Extractor) DetectEmotion(text string) string {
	for _, ep := range e.emotionPatterns {
		if ep.pattern.MatchString(text) {
			return ep.label
		}
	}
	return "neutral"
}

// moveTargetPatterns extracts the destination from German movement phrases.
var moveTargetPatterns = []*regexp.Regexp{
	// "geht zielstrebig Richtung Kueche" — allows adverbs between verb and direction
	regexp.MustCompile(`(?i)(?:geh[te]?|lauf[te]?)\s+(?:\w+\s+)*(?:Richtung|nach)\s+(?:der |die |das |dem |den )?(.+?)(?:\.|,|!|\*|\s*$)`),
	// "gehe in die Kueche" — direct verb + preposition
	regexp.MustCompile(`(?i)(?:geh[te]?|lauf[te]?)\s+(?:zu|in)\s+(?:der |die |das |dem |den )?(.+?)(?:\.|,|!|\*|\s*$)`),
	regexp.MustCompile(`(?i)(?:komm[te]?)\s+(?:in|zu|an)\s+(?:der |die |das |dem |den )?(.+?)(?:\.|,|!|\*|\s*$)`),
	regexp.MustCompile(`(?i)(?:betritt|betrete)\s+(?:die |den |das |dem )?(.+?)(?:\.|,|!|\*|\s*$)`),
	regexp.MustCompile(`(?i)unterwegs\s+(?:\w+\s+)*(?:nach|zu|Richtung)\s+(?:der |die |das |dem |den )?(.+?)(?:\.|,|!|\*|\s*$)`),
	regexp.MustCompile(`(?i)verlasst\s+(?:die |den |das |dem )?(.+?)(?:\.|,|!|\*|\s*$)`),
	regexp.MustCompile(`(?i)verlaesst\s+(?:die |den |das |dem )?(.+?)(?:\.|,|!|\*|\s*$)`),
}

// RoomAlias maps a prose name (lowercase) to a room_id.
type RoomAlias struct {
	prose  string // lowercase prose name
	roomID string // canonical room_id from rooms.toml
}

// roomAliases is populated by SetRoomAliases from rooms.toml data.
// Falls back to prose if no match found.
var roomAliases []RoomAlias

// SetRoomAliases configures the room name resolver from rooms.toml data.
// Each entry maps: room_id, room_name, and common German aliases.
func SetRoomAliases(rooms []RoomDef) {
	roomAliases = nil
	for _, r := range rooms {
		id := r.ID
		// Exact id match
		roomAliases = append(roomAliases, RoomAlias{strings.ToLower(id), id})
		// Official name from rooms.toml
		if r.Name != "" {
			roomAliases = append(roomAliases, RoomAlias{strings.ToLower(r.Name), id})
		}
	}
	// Common German aliases that LLMs use
	staticAliases := map[string]string{
		"kueche":                "kueche",
		"kueche-eg":             "kueche",
		"küche":                 "kueche",
		"küche eg":              "kueche",
		"pausenraum":            "kueche",
		"toilette":              "toilette-eg-herren",
		"toilette-eg":           "toilette-eg-herren",
		"klo":                   "toilette-eg-herren",
		"wc":                    "toilette-eg-herren",
		"treppenhaus":           "treppenhaus",
		"flur":                  "flur-eg",
		"empfang":               "empfang",
		"rezeption":             "empfang",
		"chefbuero":             "buero-ceo",
		"chefbüro":              "buero-ceo",
		"geschaeftsfuehrung":    "buero-ceo",
		"geschäftsführung":      "buero-ceo",
		"ceo":                   "buero-ceo",
		"entwicklungsbuero":     "buero-dev-1",
		"entwicklungsbüro":      "buero-dev-1",
		"dev":                   "buero-dev-1",
		"entwicklungsbuero 1":   "buero-dev-1",
		"entwicklungsbüro 1":    "buero-dev-1",
		"entwicklungsbuero 2":   "buero-dev-2",
		"entwicklungsbüro 2":    "buero-dev-2",
		"designbuero":           "buero-design-1",
		"designbüro":            "buero-design-1",
		"designbuero 1":         "buero-design-1",
		"designbüro 1":          "buero-design-1",
		"designbuero 2":         "buero-design-2",
		"designbüro 2":          "buero-design-2",
		"meetingraum":           "meetingraum-01",
		"konferenzraum":         "meetingraum-01",
		"besprechungsraum":      "meetingraum-01",
		"meetingraum galileo":   "meetingraum-01",
		"meetingraum tesla":     "meetingraum-02",
		"meetingraum edison":    "meetingraum-03",
		"vertrieb":              "buero-sales",
		"vertriebsbuero":        "buero-sales",
		"vertriebsbüro":         "buero-sales",
		"sales":                 "buero-sales",
		"projektmanagement":     "buero-pm",
		"pm":                    "buero-pm",
		"marketing":             "buero-marketing",
		"marketingbuero":        "buero-marketing",
		"marketingbüro":         "buero-marketing",
		"verwaltung":            "buero-admin",
		"verwaltungsbuero":      "buero-admin",
		"verwaltungsbüro":       "buero-admin",
		"admin":                 "buero-admin",
		"qa":                    "buero-qa",
		"qualitaetssicherung":   "buero-qa",
		"qualitätssicherung":    "buero-qa",
		"it":                    "buero-it",
		"betriebsrat":           "buero-betriebsrat",
		"betriebsratsbuero":     "buero-betriebsrat",
		"betriebsratsbüro":      "buero-betriebsrat",
		"betriebspsychologie":   "buero-betriebspsych",
		"psychologie":           "buero-betriebspsych",
		"betriebsmedizin":       "buero-betriebsarzt",
		"betriebsarzt":          "buero-betriebsarzt",
		"arzt":                  "buero-betriebsarzt",
	}
	for prose, id := range staticAliases {
		roomAliases = append(roomAliases, RoomAlias{prose, id})
	}
}

// RoomDef is a minimal room definition for alias setup.
type RoomDef struct {
	ID   string
	Name string
}

// resolveRoomID maps a prose target to a canonical room_id.
// Returns the original string (lowercased) if no match found.
func resolveRoomID(prose string) string {
	if prose == "" {
		return ""
	}
	lower := strings.ToLower(strings.TrimSpace(prose))

	// 1. Exact match against aliases
	for _, a := range roomAliases {
		if a.prose == lower {
			return a.roomID
		}
	}

	// 2. Substring match: "Entwicklungsbuero 1" contains "entwicklungsbuero 1"
	for _, a := range roomAliases {
		if strings.Contains(lower, a.prose) && len(a.prose) >= 3 {
			return a.roomID
		}
	}

	// 3. No match — return lowercased original (will be rejected by Rust validation)
	return lower
}

// extractMoveTarget tries to extract the destination from a move statement
// and resolves it to a canonical room_id.
func extractMoveTarget(text string) string {
	for _, pat := range moveTargetPatterns {
		matches := pat.FindStringSubmatch(text)
		if len(matches) >= 2 {
			target := matches[1]
			// Trim trailing asterisks and whitespace
			target = regexp.MustCompile(`[\s*]+$`).ReplaceAllString(target, "")
			if target != "" {
				return resolveRoomID(target)
			}
		}
	}
	return ""
}
