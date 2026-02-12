package extraction

import "regexp"

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
			{"move", regexp.MustCompile(`(?i)(gehe? (zu|in|nach)|laufe? (zu|in))`)},
			{"tool_use", regexp.MustCompile(`(?i)(oeffne?|starte?|benutze?|schreibe?)`)},
		},
		emotePattern: regexp.MustCompile(`\*[^*]+\*`),
	}
}

// Extract parses an LLM response for actions and emotions.
func (e *Extractor) Extract(response string) []ExtractedAction {
	var actions []ExtractedAction
	emotion := e.DetectEmotion(response)

	// Check for emotes (text within *asterisks*)
	emotes := e.emotePattern.FindAllString(response, -1)
	for _, emote := range emotes {
		actions = append(actions, ExtractedAction{
			Type:    "emote",
			Content: emote,
			Emotion: emotion,
		})
	}

	// Check for move actions
	for _, ap := range e.actionPatterns {
		if ap.pattern.MatchString(response) {
			target := ""
			if ap.actionType == "move" {
				target = extractMoveTarget(response)
			}
			actions = append(actions, ExtractedAction{
				Type:    ap.actionType,
				Content: response,
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

// moveTargetPattern extracts the destination from movement phrases.
var moveTargetPattern = regexp.MustCompile(`(?i)(?:gehe?|laufe?) (?:zu|in|nach) (?:der |die |das |dem |den )?(.+?)(?:\.|,|!|\s*$)`)

// extractMoveTarget tries to extract the destination from a move statement.
func extractMoveTarget(text string) string {
	matches := moveTargetPattern.FindStringSubmatch(text)
	if len(matches) >= 2 {
		return matches[1]
	}
	return ""
}
