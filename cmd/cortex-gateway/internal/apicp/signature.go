package apicp

import (
	"strings"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/extraction"
)

// BuildResponseSignature derives a stable learning signature for API-CP.
// It prefers extracted action semantics over raw free text so routine actions
// can promote naturally even when their narrative wording drifts.
func BuildResponseSignature(actions []extraction.ExtractedAction, roomID string, responseContent string) string {
	if len(actions) == 0 {
		return normalizeTextSignature(responseContent)
	}

	normalizedRoom := normalizeActionTarget(roomID)
	parts := make([]string, 0, len(actions))
	for _, action := range actions {
		actionType := strings.ToLower(strings.TrimSpace(action.Type))
		emotion := strings.ToLower(strings.TrimSpace(action.Emotion))
		target := stableActionTarget(action, normalizedRoom)
		parts = append(parts, actionType+"|"+target+"|"+emotion)
	}
	return strings.Join(parts, "||")
}

func stableActionTarget(action extraction.ExtractedAction, normalizedRoom string) string {
	actionType := strings.ToLower(strings.TrimSpace(action.Type))
	switch actionType {
	case "work":
		// Work happens in the current room even when the model varies between
		// prose aliases, dashes, or omitted targets.
		if normalizedRoom != "" {
			return normalizedRoom
		}
	case "think", "break":
		if target := normalizeActionTarget(action.Target); target != "" {
			return target
		}
		return normalizedRoom
	}
	return normalizeActionTarget(action.Target)
}

func normalizeActionTarget(target string) string {
	target = strings.ToLower(strings.TrimSpace(target))
	switch target {
	case "", "-", "—", "–":
		return ""
	}
	target = strings.ReplaceAll(target, "_", "-")
	target = strings.ReplaceAll(target, " ", "-")
	return target
}

func normalizeTextSignature(content string) string {
	return strings.Join(strings.Fields(strings.TrimSpace(content)), " ")
}
