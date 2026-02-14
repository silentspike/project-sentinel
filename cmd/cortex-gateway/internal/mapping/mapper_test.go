package mapping

import (
	"encoding/json"
	"testing"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/extraction"
)

func TestMapActions_Move(t *testing.T) {
	actions := []extraction.ExtractedAction{
		{Type: "move", Content: "Ich gehe in die Kueche", Target: "kueche", Emotion: "happy"},
	}
	meta := ActionMeta{AgentName: "AGENT-01", RequestID: "req-001", Tick: 42}

	events := MapActions(actions, meta)
	if len(events) != 1 {
		t.Fatalf("expected 1 event, got %d", len(events))
	}

	e := events[0]
	if e.EventType != "agent_move" {
		t.Errorf("expected event_type=agent_move, got %q", e.EventType)
	}
	if e.AggregateID != "AGENT-01" {
		t.Errorf("expected aggregate_id=AGENT-01, got %q", e.AggregateID)
	}
	if e.CorrelationID != "req-001" {
		t.Errorf("expected correlation_id=req-001, got %q", e.CorrelationID)
	}
	if e.OperationID != "req-001-0" {
		t.Errorf("expected operation_id=req-001-0, got %q", e.OperationID)
	}
	if e.Tick != 42 {
		t.Errorf("expected tick=42, got %d", e.Tick)
	}

	// Verify payload contains target
	var payload map[string]string
	if err := json.Unmarshal([]byte(e.Payload), &payload); err != nil {
		t.Fatalf("payload unmarshal: %v", err)
	}
	if payload["target"] != "kueche" {
		t.Errorf("expected target=kueche, got %q", payload["target"])
	}
	if payload["emotion"] != "happy" {
		t.Errorf("expected emotion=happy, got %q", payload["emotion"])
	}
}

func TestMapActions_MultipleActions(t *testing.T) {
	actions := []extraction.ExtractedAction{
		{Type: "emote", Content: "*lacht*", Emotion: "happy"},
		{Type: "move", Content: "gehe in den Flur", Target: "flur", Emotion: "happy"},
		{Type: "chat", Content: "Hallo zusammen!", Emotion: "neutral"},
	}
	meta := ActionMeta{AgentName: "AGENT-05", RequestID: "req-multi", Tick: 100}

	events := MapActions(actions, meta)
	if len(events) != 3 {
		t.Fatalf("expected 3 events, got %d", len(events))
	}

	expectedTypes := []string{"agent_emote", "agent_move", "agent_chat"}
	expectedOpIDs := []string{"req-multi-0", "req-multi-1", "req-multi-2"}

	for i, e := range events {
		if e.EventType != expectedTypes[i] {
			t.Errorf("event[%d]: expected type=%q, got %q", i, expectedTypes[i], e.EventType)
		}
		if e.OperationID != expectedOpIDs[i] {
			t.Errorf("event[%d]: expected op_id=%q, got %q", i, expectedOpIDs[i], e.OperationID)
		}
		if e.AggregateID != "AGENT-05" {
			t.Errorf("event[%d]: expected aggregate=AGENT-05, got %q", i, e.AggregateID)
		}
	}
}

func TestMapActions_EmptyActions(t *testing.T) {
	events := MapActions(nil, ActionMeta{AgentName: "AGENT-01", RequestID: "req-empty"})
	if events != nil {
		t.Errorf("expected nil for empty actions, got %d events", len(events))
	}

	events = MapActions([]extraction.ExtractedAction{}, ActionMeta{AgentName: "AGENT-01", RequestID: "req-empty"})
	if events != nil {
		t.Errorf("expected nil for zero-length actions, got %d events", len(events))
	}
}

func TestOperationIdDeterministic(t *testing.T) {
	actions := []extraction.ExtractedAction{
		{Type: "chat", Content: "hallo"},
		{Type: "move", Content: "gehe in Kueche", Target: "kueche"},
	}
	meta := ActionMeta{AgentName: "AGENT-01", RequestID: "fixed-req-id", Tick: 10}

	events1 := MapActions(actions, meta)
	events2 := MapActions(actions, meta)

	for i := range events1 {
		if events1[i].OperationID != events2[i].OperationID {
			t.Errorf("event[%d]: operation_id not deterministic: %q != %q",
				i, events1[i].OperationID, events2[i].OperationID)
		}
	}
}

func TestMapActions_UnknownType(t *testing.T) {
	actions := []extraction.ExtractedAction{
		{Type: "unknown_action", Content: "something weird"},
	}
	meta := ActionMeta{AgentName: "AGENT-01", RequestID: "req-unknown"}

	events := MapActions(actions, meta)
	if len(events) != 1 {
		t.Fatalf("expected 1 event, got %d", len(events))
	}
	if events[0].EventType != "agent_action" {
		t.Errorf("expected fallback event_type=agent_action, got %q", events[0].EventType)
	}
}
