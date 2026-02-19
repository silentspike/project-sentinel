// Package mapping transforms extracted LLM actions into domain events
// for the sentinel event store (Command→Event Mapping, Issue #13 AC-5).
package mapping

import (
	"encoding/json"
	"fmt"
	"time"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/extraction"
	"github.com/obtFusi/project-sentinel/pkg/sentinel-go/eventstore"
)

// ActionMeta holds contextual metadata for mapping actions to events.
type ActionMeta struct {
	AgentName string // e.g. "AGENT-01"
	RequestID string // X-Request-ID or generated UUID
	Tick      int64  // simulation tick from metadata, 0 if unknown
}

// actionTypeToEventType maps extraction action types to domain event types.
var actionTypeToEventType = map[string]string{
	"move":     "agent_move",
	"emote":    "agent_emote",
	"tool_use": "agent_tool_use",
	"chat":     "agent_chat",
}

// MapActions converts extracted actions into domain events ready for persistence.
// Each action becomes one DomainEvent with a deterministic operation_id
// ({RequestID}-{index}) for retry idempotency.
func MapActions(actions []extraction.ExtractedAction, meta ActionMeta) []eventstore.DomainEvent {
	if len(actions) == 0 {
		return nil
	}

	now := time.Now().UnixMilli()
	events := make([]eventstore.DomainEvent, 0, len(actions))

	for i, action := range actions {
		eventType, ok := actionTypeToEventType[action.Type]
		if !ok {
			eventType = "agent_action"
		}

		payload := buildPayload(action)

		events = append(events, eventstore.DomainEvent{
			EventID:          eventstore.GenerateUUID(),
			EventType:        eventType,
			AggregateID:      meta.AgentName,
			Payload:          payload,
			CorrelationID:    meta.RequestID,
			OperationID:      fmt.Sprintf("%s-%d", meta.RequestID, i),
			Tick:             meta.Tick,
			TimestampMs:      now,
			SchemaVersion:    1,
			CompensationType: "none",
		})
	}

	return events
}

// buildPayload creates a JSON payload from the extracted action fields.
func buildPayload(action extraction.ExtractedAction) string {
	m := map[string]string{
		"type": action.Type,
	}
	if action.Content != "" {
		m["content"] = action.Content
	}
	if action.Target != "" {
		m["target"] = action.Target
	}
	if action.Emotion != "" {
		m["emotion"] = action.Emotion
	}
	if action.Intent != "" {
		m["intent"] = action.Intent
	}

	b, err := json.Marshal(m)
	if err != nil {
		// Fallback: never fail on serialization.
		return fmt.Sprintf(`{"type":%q,"error":"marshal failed"}`, action.Type)
	}
	return string(b)
}
