package proxy

import "testing"

func TestResponseLogBufferRingOverwriteAndLastByClass(t *testing.T) {
	t.Parallel()

	buffer := NewResponseLogBuffer(2)
	buffer.Add(ResponseLogEntry{
		RequestID:    "external-1",
		RequestClass: RequestClassExternalCompat,
		Provider:     "anthropic-direct",
		Model:        "claude-opus-4-6",
		Content:      "external",
	})
	buffer.Add(ResponseLogEntry{
		RequestID:    "agent-1",
		RequestClass: RequestClassAgentRuntime,
		Provider:     "claude-code",
		Model:        "haiku",
		PolicySource: PolicySourceAgentRuntime,
		AgentID:      "12",
		AgentName:    "Thomas Mueller",
		Content:      "first runtime",
	})
	buffer.Add(ResponseLogEntry{
		RequestID:    "agent-2",
		RequestClass: RequestClassAgentRuntime,
		Provider:     "claude-code",
		Model:        "haiku",
		PolicySource: PolicySourceAgentRuntime,
		AgentID:      "13",
		AgentName:    "Julia Neumann",
		Content:      "second runtime",
	})

	entries := buffer.Entries()
	if len(entries) != 2 {
		t.Fatalf("Entries() len = %d, want 2", len(entries))
	}
	if entries[0].RequestID != "agent-1" || entries[1].RequestID != "agent-2" {
		t.Fatalf("Entries() order = [%s %s], want [agent-1 agent-2]", entries[0].RequestID, entries[1].RequestID)
	}
	if entries[1].LoggedAt.IsZero() {
		t.Fatal("expected Add() to populate LoggedAt")
	}

	lastRuntime, ok := buffer.LastByClass(RequestClassAgentRuntime)
	if !ok {
		t.Fatal("LastByClass(agent_runtime) not found")
	}
	if lastRuntime.RequestID != "agent-2" || lastRuntime.AgentID != "13" || lastRuntime.PolicySource != PolicySourceAgentRuntime {
		t.Fatalf("LastByClass(agent_runtime) = %+v, want latest runtime entry", lastRuntime)
	}

	if _, ok := buffer.LastByClass(RequestClassExternalCompat); ok {
		t.Fatal("LastByClass(external_compat) found overwritten entry")
	}
}
