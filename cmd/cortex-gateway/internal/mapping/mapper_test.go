package mapping

import (
	"encoding/json"
	"fmt"
	"testing"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/extraction"
)

func TestMapActionsMoveUsesCanonicalDomainPayload(t *testing.T) {
	actions := []extraction.ExtractedAction{
		{Type: "move", Content: "Ich gehe in die Kueche", Target: "kueche", Emotion: "happy"},
	}
	events, err := MapActions(actions, ActionMeta{AgentID: 1, RequestID: "req-001", Tick: 42})
	if err != nil {
		t.Fatalf("MapActions: %v", err)
	}
	if len(events) != 1 {
		t.Fatalf("expected 1 event, got %d", len(events))
	}

	event := events[0]
	if event.EventType != "agent_action_received" || event.AggregateID != "AGENT-01" {
		t.Errorf("event identity mismatch: %+v", event)
	}
	if event.CorrelationID != "req-001" || event.OperationID != "req-001-0" {
		t.Errorf("request binding mismatch: %+v", event)
	}
	if event.Tick != 42 {
		t.Errorf("tick = %d, want 42", event.Tick)
	}

	var payload map[string]any
	if err := json.Unmarshal([]byte(event.Payload), &payload); err != nil {
		t.Fatalf("payload unmarshal: %v", err)
	}
	if payload["type"] != "AgentActionReceived" || payload["agent_id"] != float64(1) {
		t.Errorf("canonical Rust tag/identity missing: %#v", payload)
	}
	if payload["action_type"] != "move" || payload["target_room"] != "kueche" {
		t.Errorf("canonical action fields mismatch: %#v", payload)
	}
	if payload["content"] != "Ich gehe in die Kueche" || payload["source"] != "external" {
		t.Errorf("canonical content/source mismatch: %#v", payload)
	}
	if _, exists := payload["target"]; exists {
		t.Errorf("legacy target field retained: %#v", payload)
	}
	if _, exists := payload["emotion"]; exists {
		t.Errorf("non-domain emotion field retained: %#v", payload)
	}
}

func TestMapActionsMultipleActionsShareAuthenticatedAggregate(t *testing.T) {
	actions := []extraction.ExtractedAction{
		{Type: "emote", Content: "*lacht*"},
		{Type: "move", Content: "gehe in den Flur", Target: "flur"},
		{Type: "chat", Content: "Hallo zusammen!"},
	}
	events, err := MapActions(actions, ActionMeta{AgentID: 5, RequestID: "req-multi", Tick: 100})
	if err != nil {
		t.Fatalf("MapActions: %v", err)
	}
	if len(events) != 3 {
		t.Fatalf("expected 3 events, got %d", len(events))
	}
	for i, event := range events {
		if event.EventType != "agent_action_received" || event.AggregateID != "AGENT-05" {
			t.Errorf("event[%d] identity mismatch: %+v", i, event)
		}
		wantOperation := fmt.Sprintf("req-multi-%d", i)
		if event.OperationID != wantOperation {
			t.Errorf("event[%d] operation_id = %q, want %q", i, event.OperationID, wantOperation)
		}
	}
}

func TestMapActionsEmptyActions(t *testing.T) {
	for _, actions := range [][]extraction.ExtractedAction{nil, {}} {
		events, err := MapActions(actions, ActionMeta{RequestID: "req-empty"})
		if err != nil {
			t.Fatalf("empty MapActions: %v", err)
		}
		if events != nil {
			t.Errorf("expected nil for empty actions, got %d events", len(events))
		}
	}
}

func TestMapActionsRejectsInvalidAuthenticatedIdentity(t *testing.T) {
	for _, agentID := range []uint16{0, maxShippedAgentID + 1} {
		events, err := MapActions(
			[]extraction.ExtractedAction{{Type: "chat", Content: "hallo"}},
			ActionMeta{AgentID: agentID, RequestID: "req-invalid-agent"},
		)
		if err == nil {
			t.Fatalf("invalid authenticated agent id %d was accepted", agentID)
		}
		if events != nil {
			t.Fatalf("failed mapping emitted events: %+v", events)
		}
	}
}

func TestOperationIDDeterministic(t *testing.T) {
	actions := []extraction.ExtractedAction{
		{Type: "chat", Content: "hallo"},
		{Type: "move", Content: "gehe in Kueche", Target: "kueche"},
	}
	meta := ActionMeta{AgentID: 1, RequestID: "fixed-req-id", Tick: 10}

	events1, err := MapActions(actions, meta)
	if err != nil {
		t.Fatal(err)
	}
	events2, err := MapActions(actions, meta)
	if err != nil {
		t.Fatal(err)
	}
	for i := range events1 {
		if events1[i].OperationID != events2[i].OperationID {
			t.Errorf("event[%d] operation_id not deterministic: %q != %q",
				i, events1[i].OperationID, events2[i].OperationID)
		}
	}
}

func TestMapActionsUnknownTypeRemainsExplicit(t *testing.T) {
	events, err := MapActions(
		[]extraction.ExtractedAction{{Type: "unknown_action", Content: "something weird"}},
		ActionMeta{AgentID: 1, RequestID: "req-unknown"},
	)
	if err != nil {
		t.Fatalf("MapActions: %v", err)
	}
	if len(events) != 1 || events[0].EventType != "agent_action_received" {
		t.Fatalf("unexpected events: %+v", events)
	}
	var payload map[string]any
	if err := json.Unmarshal([]byte(events[0].Payload), &payload); err != nil {
		t.Fatal(err)
	}
	if payload["action_type"] != "unknown_action" {
		t.Fatalf("action type was hidden: %#v", payload)
	}
}
