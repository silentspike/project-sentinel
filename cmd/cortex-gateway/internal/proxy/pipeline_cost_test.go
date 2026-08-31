package proxy

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// TestPipelineThreadsCacheTierCost proves #427 end-to-end threading: a forwarded
// response carries the cache breakdown, the gateway-resolved tier and the
// gateway-computed cost in the PipelineResponse the daemon parses (the daemon
// does not know the EffectiveModel/cost itself).
func TestPipelineThreadsCacheTierCost(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "claude-code",
		resp: &LLMResponse{
			Content:       "Ich denke nach. *nickt*",
			Model:         "claude-opus-4-6",
			InputTokens:   1300, // folded total (fresh 1000 + cache 200 + 100)
			OutputTokens:  500,
			CacheRead:     200,
			CacheCreation: 100,
			TokensUsed:    1800,
			FinishReason:  "end_turn",
		},
	}
	reg.Register("claude-code", mock)

	ph := newTestPipelineHandler(reg, nil)

	body := `{"messages":[{"role":"user","content":"Was machst du?"}],"metadata":{"agent_id":"42"}}`
	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	w := httptest.NewRecorder()

	ph.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("status %d: %s", w.Code, w.Body.String())
	}

	var resp PipelineResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("decode response: %v", err)
	}

	if resp.Decision != "forward" {
		t.Fatalf("decision = %q, want forward", resp.Decision)
	}
	if resp.CacheRead != 200 {
		t.Errorf("cache_read = %d, want 200", resp.CacheRead)
	}
	if resp.CacheCreation != 100 {
		t.Errorf("cache_creation = %d, want 100", resp.CacheCreation)
	}
	if resp.Tier == "" {
		t.Error("tier not set on forward response")
	}
	// claude-code provider => non-zero gateway-resolved cost (1300 in / 500 out, opus).
	if resp.CostUsd <= 0 {
		t.Errorf("cost_usd = %f, want > 0 (gateway-resolved)", resp.CostUsd)
	}
}

func TestResolveTier(t *testing.T) {
	cases := map[string]string{
		"claude-haiku-4-5":     "low",
		"claude-sonnet-4-6":    "mid",
		"claude-opus-4-8":      "high",
		"gpt-5.6-luna":         "low",
		"gpt-5.6-terra":        "mid",
		"gpt-5.6-sol":          "high",
		"":                     "unknown",
		"some-other-model-xyz": "unknown",
	}
	for model, want := range cases {
		if got := resolveTier(model); got != want {
			t.Errorf("resolveTier(%q) = %q, want %q", model, got, want)
		}
	}
}

func TestCanonicalAgentID(t *testing.T) {
	if got := canonicalAgentID(map[string]string{"agent_id": "8"}); got != "AGENT-08" {
		t.Errorf("agent_id 8 -> %q, want AGENT-08", got)
	}
	if got := canonicalAgentID(map[string]string{"agent_name": "AGENT-12"}); got != "AGENT-12" {
		t.Errorf("agent_name fallback -> %q, want AGENT-12", got)
	}
	if got := canonicalAgentID(map[string]string{}); got != "unknown" {
		t.Errorf("empty metadata -> %q, want unknown", got)
	}
}
