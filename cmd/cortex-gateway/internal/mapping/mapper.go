// Package mapping transforms extracted LLM actions into domain events
// for the sentinel event store (Command→Event Mapping, Issue #13 AC-5).
package mapping

import (
	"encoding/json"
	"fmt"
	"time"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/extraction"
	"github.com/silentspike/project-sentinel/pkg/sentinel-go/eventstore"
)

// ActionMeta holds contextual metadata for mapping actions to events.
type ActionMeta struct {
	AgentID   uint16 // server-authenticated numeric agent identity
	RequestID string // X-Request-ID or generated UUID
	Tick      int64  // simulation tick from metadata, 0 if unknown
}

type agentActionReceivedPayload struct {
	Type       string  `json:"type"`
	AgentID    uint16  `json:"agent_id"`
	ActionType string  `json:"action_type"`
	TargetRoom *string `json:"target_room"`
	Content    *string `json:"content"`
	Source     string  `json:"source"`
}

// allActionsEventType is the unified event type for all LLM-generated agent actions.
// The dashboard activity feed filters for this type; the specific action kind
// (chat, move, emote, work, break, think) is stored in the payload's "action_type" field.
const allActionsEventType = "agent_action_received"
const maxShippedAgentID = 60

// MapActions converts extracted actions into domain events ready for persistence.
// Each action becomes one DomainEvent with a deterministic operation_id
// ({RequestID}-{index}) for retry idempotency.
func MapActions(actions []extraction.ExtractedAction, meta ActionMeta) ([]eventstore.DomainEvent, error) {
	if len(actions) == 0 {
		return nil, nil
	}
	if meta.AgentID == 0 || meta.AgentID > maxShippedAgentID {
		return nil, fmt.Errorf("agent action mapping requires an authenticated agent id")
	}

	now := time.Now().UnixMilli()
	events := make([]eventstore.DomainEvent, 0, len(actions))
	aggregateID := fmt.Sprintf("AGENT-%02d", meta.AgentID)

	for i, action := range actions {
		payload, err := buildPayload(action, meta.AgentID)
		if err != nil {
			return nil, fmt.Errorf("encode agent action %d: %w", i, err)
		}

		events = append(events, eventstore.DomainEvent{
			EventID:          eventstore.GenerateUUID(),
			EventType:        allActionsEventType,
			AggregateID:      aggregateID,
			Payload:          payload,
			CorrelationID:    meta.RequestID,
			OperationID:      fmt.Sprintf("%s-%d", meta.RequestID, i),
			Tick:             meta.Tick,
			TimestampMs:      now,
			SchemaVersion:    1,
			CompensationType: "none",
		})
	}

	return events, nil
}

// buildPayload emits the exact sentinel-common DomainEventPayload wire shape.
func buildPayload(action extraction.ExtractedAction, agentID uint16) (string, error) {
	payload := agentActionReceivedPayload{
		Type:       "AgentActionReceived",
		AgentID:    agentID,
		ActionType: action.Type,
		TargetRoom: optionalString(action.Target),
		Content:    optionalString(action.Content),
		Source:     "external",
	}
	b, err := json.Marshal(payload)
	if err != nil {
		return "", err
	}
	return string(b), nil
}

func optionalString(value string) *string {
	if value == "" {
		return nil
	}
	return &value
}
