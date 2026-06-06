package proxy

import (
	"context"
	"encoding/json"
	"math"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/extraction"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/guardrails"
)

func TestLocalLoopProviderStreamingSSESequenceAndUsage(t *testing.T) {
	provider, err := NewLocalLoopProvider(LocalLoopConfig{Name: LocalLoopProviderName})
	if err != nil {
		t.Fatalf("new local-loop provider: %v", err)
	}

	req := &LLMRequest{
		Model:  "claude-opus-4-6",
		Stream: true,
		Messages: []Message{{
			Role:    "user",
			Content: "Was machst du?",
		}},
		Metadata: map[string]string{
			"agent_id":         "7",
			"tick":             "123",
			"room_id":          "engineering",
			"heard":            "Lisa fragt nach dem Build",
			"personality_type": "focused",
		},
	}
	rec := httptest.NewRecorder()

	if err := provider.StreamHTTP(context.Background(), req, rec); err != nil {
		t.Fatalf("StreamHTTP: %v", err)
	}

	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200", rec.Code)
	}
	if got := rec.Header().Get("Content-Type"); got != "text/event-stream; charset=utf-8" {
		t.Fatalf("content-type = %q", got)
	}

	events := parseSSEEvents(t, rec.Body.String())
	wantOrder := []string{
		"message_start",
		"content_block_start",
		"ping",
		"content_block_delta",
		"content_block_stop",
		"message_delta",
		"message_stop",
	}
	if len(events) < len(wantOrder) {
		t.Fatalf("events = %#v, want at least %d", events, len(wantOrder))
	}
	gotOrder := compactDeltaEvents(events)
	for i, want := range wantOrder {
		if gotOrder[i].name != want {
			t.Fatalf("event[%d] = %q, want %q; all=%#v", i, gotOrder[i].name, want, gotOrder)
		}
	}

	start := events[0].data
	if start["type"] != "message_start" {
		t.Fatalf("message_start type = %#v", start["type"])
	}
	message, ok := start["message"].(map[string]any)
	if !ok {
		t.Fatalf("message_start message = %#v", start["message"])
	}
	if message["model"] != "claude-opus-4-6" {
		t.Fatalf("model = %#v", message["model"])
	}
	if !strings.HasPrefix(message["id"].(string), "msg_") {
		t.Fatalf("message id = %#v", message["id"])
	}
	usage := message["usage"].(map[string]any)
	if usage["service_tier"] != "standard" {
		t.Fatalf("service_tier = %#v", usage["service_tier"])
	}
	if usage["input_tokens"].(float64) <= 0 {
		t.Fatalf("input_tokens = %#v", usage["input_tokens"])
	}

	var text strings.Builder
	for _, event := range events {
		if event.name != "content_block_delta" {
			continue
		}
		delta := event.data["delta"].(map[string]any)
		text.WriteString(delta["text"].(string))
	}
	if !strings.Contains(text.String(), "AKTION:") {
		t.Fatalf("streamed text does not contain action format: %q", text.String())
	}
}

func TestLocalLoopProviderSendSyntheticCostSemantics(t *testing.T) {
	provider, err := NewLocalLoopProvider(LocalLoopConfig{Name: LocalLoopProviderName})
	if err != nil {
		t.Fatalf("new local-loop provider: %v", err)
	}

	before := guardrails.RuntimeCostSnapshot()
	resp, err := provider.Send(context.Background(), &LLMRequest{
		Messages: []Message{{Role: "user", Content: "Bitte reagiere."}},
		Metadata: map[string]string{"agent_id": "1", "tick": "42", "heard": "Hallo"},
	})
	if err != nil {
		t.Fatalf("Send: %v", err)
	}
	after := guardrails.RuntimeCostSnapshot()

	if resp.TokensUsed <= 0 || resp.InputTokens <= 0 || resp.OutputTokens <= 0 {
		t.Fatalf("expected synthetic token usage, got %+v", resp)
	}
	if !strings.Contains(resp.Content, "AKTION:") {
		t.Fatalf("content = %q", resp.Content)
	}
	if after.ForwardCalls != before.ForwardCalls {
		t.Fatalf("forward calls changed from %d to %d", before.ForwardCalls, after.ForwardCalls)
	}
	if math.Abs(after.TotalCostUSD-before.TotalCostUSD) > 1e-12 {
		t.Fatalf("total cost changed from %.12f to %.12f", before.TotalCostUSD, after.TotalCostUSD)
	}
	if after.SynthesisCount <= before.SynthesisCount {
		t.Fatalf("synthesis count did not increase: before=%d after=%d", before.SynthesisCount, after.SynthesisCount)
	}
}

func TestLocalLoopDeterministicHashIgnoresRequestID(t *testing.T) {
	provider, err := NewLocalLoopProvider(LocalLoopConfig{Name: LocalLoopProviderName})
	if err != nil {
		t.Fatalf("new local-loop provider: %v", err)
	}

	req := &LLMRequest{
		Messages: []Message{{Role: "user", Content: "Status?"}},
		Metadata: map[string]string{
			"agent_id":         "5",
			"tick":             "99",
			"room_id":          "office",
			"heard":            "",
			"personality_type": "calm",
		},
	}
	a := provider.generate(req)
	b := provider.generate(req)
	if a.ID != b.ID || a.Content != b.Content {
		t.Fatalf("same stable fields produced different response: a=%+v b=%+v", a, b)
	}

	req.Metadata["tick"] = "100"
	c := provider.generate(req)
	if c.ID == a.ID {
		t.Fatalf("changing stable hash field did not change message id: %s", c.ID)
	}
}

func TestLocalLoopScenarioExactAgentWildcardAndInvalid(t *testing.T) {
	dir := t.TempDir()
	scenarioPath := filepath.Join(dir, "scenario.jsonl")
	scenario := strings.Join([]string{
		`{"agent_id":"AGENT-01","tick":7,"content":"AKTION: Emote\nZIEL: -\nINHALT: *exact*"}`,
		`{"agent_id":"AGENT-01","content":"AKTION: Emote\nZIEL: -\nINHALT: *agent*"}`,
		`{"agent_id":"*","content":"AKTION: Emote\nZIEL: -\nINHALT: *wildcard*"}`,
	}, "\n")
	if err := os.WriteFile(scenarioPath, []byte(scenario), 0o600); err != nil {
		t.Fatalf("write scenario: %v", err)
	}

	provider, err := NewLocalLoopProvider(LocalLoopConfig{Name: LocalLoopProviderName, ScenarioPath: scenarioPath})
	if err != nil {
		t.Fatalf("new local-loop provider: %v", err)
	}

	tests := []struct {
		name string
		meta map[string]string
		want string
	}{
		{name: "exact", meta: map[string]string{"agent_id": "1", "tick": "7"}, want: "*exact*"},
		{name: "agent", meta: map[string]string{"agent_id": "1", "tick": "8"}, want: "*agent*"},
		{name: "wildcard", meta: map[string]string{"agent_id": "2", "tick": "7"}, want: "*wildcard*"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := provider.generate(&LLMRequest{Metadata: tt.meta}).Content
			if !strings.Contains(got, tt.want) {
				t.Fatalf("content = %q, want %q", got, tt.want)
			}
		})
	}

	invalidPath := filepath.Join(dir, "invalid.jsonl")
	if err := os.WriteFile(invalidPath, []byte(`{"agent_id":"AGENT-01"}`), 0o600); err != nil {
		t.Fatalf("write invalid scenario: %v", err)
	}
	if _, err := NewLocalLoopProvider(LocalLoopConfig{Name: LocalLoopProviderName, ScenarioPath: invalidPath}); err == nil {
		t.Fatal("expected invalid scenario to fail")
	}
}

func TestLocalLoopResponseExtractsRealAction(t *testing.T) {
	provider, err := NewLocalLoopProvider(LocalLoopConfig{Name: LocalLoopProviderName})
	if err != nil {
		t.Fatalf("new local-loop provider: %v", err)
	}

	resp, err := provider.Send(context.Background(), &LLMRequest{
		Messages: []Message{{Role: "user", Content: "Kannst du antworten?"}},
		Metadata: map[string]string{"agent_id": "3", "tick": "44", "heard": "Direkte Ansprache"},
	})
	if err != nil {
		t.Fatalf("Send: %v", err)
	}
	actions := extraction.New().Extract(resp.Content)
	if len(actions) == 0 {
		t.Fatalf("expected real extractor action, content=%q", resp.Content)
	}
	if actions[0].Type == "" || actions[0].Content == "" {
		t.Fatalf("invalid action extracted: %+v", actions[0])
	}
}

type parsedSSEEvent struct {
	name string
	data map[string]any
}

func parseSSEEvents(t *testing.T, body string) []parsedSSEEvent {
	t.Helper()
	frames := strings.Split(strings.TrimSpace(body), "\n\n")
	events := make([]parsedSSEEvent, 0, len(frames))
	for _, frame := range frames {
		var event parsedSSEEvent
		for _, line := range strings.Split(frame, "\n") {
			switch {
			case strings.HasPrefix(line, "event: "):
				event.name = strings.TrimPrefix(line, "event: ")
			case strings.HasPrefix(line, "data: "):
				if err := json.Unmarshal([]byte(strings.TrimPrefix(line, "data: ")), &event.data); err != nil {
					t.Fatalf("decode data for frame %q: %v", frame, err)
				}
			}
		}
		if event.name == "" || event.data == nil {
			t.Fatalf("invalid SSE frame: %q", frame)
		}
		events = append(events, event)
	}
	return events
}

func compactDeltaEvents(events []parsedSSEEvent) []parsedSSEEvent {
	out := make([]parsedSSEEvent, 0, len(events))
	seenDelta := false
	for _, event := range events {
		if event.name == "content_block_delta" {
			if seenDelta {
				continue
			}
			seenDelta = true
		}
		out = append(out, event)
	}
	return out
}
