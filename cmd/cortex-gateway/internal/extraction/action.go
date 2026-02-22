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
			{"tool_use", regexp.MustCompile(`(?i)(oeffne?|starte?|benutze?|schreibe?)`)},
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

	return &ExtractedAction{
		Type:    normalizeActionType(ja.ActionType),
		Content: ja.Content,
		Target:  ja.Target,
	}
}

// Extract parses an LLM response for actions and emotions.
func (e *Extractor) Extract(response string) []ExtractedAction {
	// Try structured JSON first (preferred)
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

	// If no specific action detected, it is a chat message
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

// extractMoveTarget tries to extract the destination from a move statement.
func extractMoveTarget(text string) string {
	for _, pat := range moveTargetPatterns {
		matches := pat.FindStringSubmatch(text)
		if len(matches) >= 2 {
			target := matches[1]
			// Trim trailing asterisks and whitespace
			target = regexp.MustCompile(`[\s*]+$`).ReplaceAllString(target, "")
			if target != "" {
				return target
			}
		}
	}
	return ""
}
