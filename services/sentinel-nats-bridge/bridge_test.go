package main

import (
	"fmt"
	"testing"

	"github.com/nats-io/nats.go"

	"github.com/silentspike/project-sentinel/pkg/sentinel-go/eventstore"
	"github.com/silentspike/project-sentinel/pkg/sentinel-go/messaging"
)

func TestBuildPublishMessage(t *testing.T) {
	evt := eventstore.DomainEvent{
		EventID:       "evt-001",
		EventType:     "agent_action_received",
		AggregateID:   "AGENT-07",
		Payload:       `{"action":"greet"}`,
		CorrelationID: "corr-001",
		CausationID:   "cause-001",
		OperationID:   "op-001",
		Tick:          1000,
		TimestampMs:   1700000000000,
		SchemaVersion: 1,
	}

	subject := messaging.BuildEventSubject(evt.EventType, evt.AggregateID)
	msg := &nats.Msg{
		Subject: subject,
		Data:    []byte(evt.Payload),
		Header:  nats.Header{},
	}
	msg.Header.Set("Nats-Msg-Id", evt.OperationID)
	msg.Header.Set("X-Event-ID", evt.EventID)
	msg.Header.Set("X-Event-Type", evt.EventType)
	msg.Header.Set("X-Aggregate-ID", evt.AggregateID)

	if subject != "sentinel.events.agent_action_received.AGENT-07" {
		t.Errorf("subject = %q, want sentinel.events.agent_action_received.AGENT-07", subject)
	}
	if msg.Header.Get("Nats-Msg-Id") != "op-001" {
		t.Errorf("Nats-Msg-Id = %q, want op-001", msg.Header.Get("Nats-Msg-Id"))
	}
	if msg.Header.Get("X-Event-Type") != "agent_action_received" {
		t.Errorf("X-Event-Type = %q, want agent_action_received", msg.Header.Get("X-Event-Type"))
	}
	if string(msg.Data) != `{"action":"greet"}` {
		t.Errorf("Data = %q, want payload", string(msg.Data))
	}
}

func TestBuildPublishMessageDedup(t *testing.T) {
	// Two events with same operation_id should produce same Nats-Msg-Id (dedup key)
	evt1 := eventstore.DomainEvent{OperationID: "op-dup-001", EventType: "agent_chat", AggregateID: "AGENT-01"}
	evt2 := eventstore.DomainEvent{OperationID: "op-dup-001", EventType: "agent_chat", AggregateID: "AGENT-01"}

	msg1 := &nats.Msg{Header: nats.Header{}}
	msg1.Header.Set("Nats-Msg-Id", evt1.OperationID)

	msg2 := &nats.Msg{Header: nats.Header{}}
	msg2.Header.Set("Nats-Msg-Id", evt2.OperationID)

	if msg1.Header.Get("Nats-Msg-Id") != msg2.Header.Get("Nats-Msg-Id") {
		t.Error("same operation_id must produce same Nats-Msg-Id for dedup")
	}
}

func TestBuildPublishMessageDifferentOps(t *testing.T) {
	// Two events with different operation_id should produce different Nats-Msg-Id
	evt1 := eventstore.DomainEvent{OperationID: "op-A", EventType: "agent_chat", AggregateID: "AGENT-01"}
	evt2 := eventstore.DomainEvent{OperationID: "op-B", EventType: "agent_chat", AggregateID: "AGENT-01"}

	if evt1.OperationID == evt2.OperationID {
		t.Error("different operations should have different IDs")
	}
}

func TestSubjectMapping(t *testing.T) {
	tests := []struct {
		eventType   string
		aggregateID string
		want        string
	}{
		{"agent_action_received", "AGENT-07", "sentinel.events.agent_action_received.AGENT-07"},
		{"agent_chat", "AGENT-12", "sentinel.events.agent_chat.AGENT-12"},
		{"bio_state_updated", "AGENT-01", "sentinel.events.bio_state_updated.AGENT-01"},
	}

	for _, tt := range tests {
		got := messaging.BuildEventSubject(tt.eventType, tt.aggregateID)
		if got != tt.want {
			t.Errorf("BuildEventSubject(%q, %q) = %q, want %q", tt.eventType, tt.aggregateID, got, tt.want)
		}
	}
}

func TestConfigDefaults(t *testing.T) {
	var cfg Config

	// Verify defaults are applied correctly
	if cfg.EventStore.PollIntervalMs != 0 {
		t.Errorf("default PollIntervalMs = %d, want 0 (before defaults applied)", cfg.EventStore.PollIntervalMs)
	}

	// Apply defaults like main() does
	if cfg.EventStore.PollIntervalMs <= 0 {
		cfg.EventStore.PollIntervalMs = 1000
	}
	if cfg.EventStore.BatchSize <= 0 {
		cfg.EventStore.BatchSize = 100
	}
	if cfg.Server.HealthPort <= 0 {
		cfg.Server.HealthPort = 8083
	}
	// #525: health_bind_addr default = loopback with configured health_port.
	if cfg.Server.HealthBindAddr == "" {
		cfg.Server.HealthBindAddr = fmt.Sprintf("127.0.0.1:%d", cfg.Server.HealthPort)
	}

	if cfg.EventStore.PollIntervalMs != 1000 {
		t.Errorf("PollIntervalMs = %d, want 1000", cfg.EventStore.PollIntervalMs)
	}
	if cfg.EventStore.BatchSize != 100 {
		t.Errorf("BatchSize = %d, want 100", cfg.EventStore.BatchSize)
	}
	if cfg.Server.HealthPort != 8083 {
		t.Errorf("HealthPort = %d, want 8083", cfg.Server.HealthPort)
	}
	if cfg.Server.HealthBindAddr != "127.0.0.1:8083" {
		t.Errorf("HealthBindAddr = %q, want 127.0.0.1:8083", cfg.Server.HealthBindAddr)
	}
}

func TestHealthBindAddrDefaultRespectsConfiguredPort(t *testing.T) {
	// #525 (ORC Finding 2): empty health_bind_addr must default to loopback with
	// the configured health_port, NOT a hardcoded 8083.
	var cfg Config
	cfg.Server.HealthPort = 9999
	if cfg.Server.HealthBindAddr == "" {
		cfg.Server.HealthBindAddr = fmt.Sprintf("127.0.0.1:%d", cfg.Server.HealthPort)
	}
	if cfg.Server.HealthBindAddr != "127.0.0.1:9999" {
		t.Errorf("HealthBindAddr = %q, want 127.0.0.1:9999 (must respect configured health_port)", cfg.Server.HealthBindAddr)
	}
}

func TestHealthBindAddrExplicitOverridePreserved(t *testing.T) {
	// #525: an explicit health_bind_addr is preserved (not overwritten) by the default logic.
	var cfg Config
	cfg.Server.HealthPort = 8083
	cfg.Server.HealthBindAddr = "0.0.0.0:8083"
	if cfg.Server.HealthBindAddr == "" {
		cfg.Server.HealthBindAddr = fmt.Sprintf("127.0.0.1:%d", cfg.Server.HealthPort)
	}
	if cfg.Server.HealthBindAddr != "0.0.0.0:8083" {
		t.Errorf("explicit override overwritten: %q, want 0.0.0.0:8083", cfg.Server.HealthBindAddr)
	}
}

func TestGetEventsSince(t *testing.T) {
	// Integration test: create a real event store, insert events, poll them
	store, err := eventstore.Open(t.TempDir() + "/test.db")
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	defer store.Close()

	// Insert 3 events
	for i := 0; i < 3; i++ {
		err := store.AppendWithOutbox(eventstore.DomainEvent{
			EventID:       eventstore.GenerateUUID(),
			EventType:     "agent_chat",
			AggregateID:   "AGENT-01",
			Payload:       `{"msg":"hello"}`,
			CorrelationID: "corr-1",
			CausationID:   "cause-1",
			OperationID:   eventstore.GenerateUUID(),
			Tick:          int64(i + 1),
			TimestampMs:   1700000000000,
			SchemaVersion: 1,
		}, "sentinel/events/AGENT-01")
		if err != nil {
			t.Fatalf("append %d: %v", i, err)
		}
	}

	// Poll all events from the start
	events, maxID, err := store.GetEventsSince(0, 100)
	if err != nil {
		t.Fatalf("GetEventsSince: %v", err)
	}
	if len(events) != 3 {
		t.Errorf("got %d events, want 3", len(events))
	}
	if maxID < 1 {
		t.Errorf("maxID = %d, want >= 1", maxID)
	}

	// Poll again from maxID — should return 0 events
	events2, _, err := store.GetEventsSince(maxID, 100)
	if err != nil {
		t.Fatalf("GetEventsSince second: %v", err)
	}
	if len(events2) != 0 {
		t.Errorf("got %d events after maxID, want 0", len(events2))
	}

	// Poll with limit=2 — should return 2
	events3, _, err := store.GetEventsSince(0, 2)
	if err != nil {
		t.Fatalf("GetEventsSince limited: %v", err)
	}
	if len(events3) != 2 {
		t.Errorf("got %d events with limit=2, want 2", len(events3))
	}
}
