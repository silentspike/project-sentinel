package proxy

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/capability"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/compiler"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/control"
)

func responseFormatMetadata() map[string]string {
	return map[string]string{
		"agent_id": "5", "agent_role": "Developer", "hierarchy_tier": "2",
		"body": "BODY-SENTINEL", "circadian": "CIRCADIAN-SENTINEL",
		"environment": "ENV-SENTINEL", "acoustic": "ACOUSTIC-SENTINEL",
		"heard": "HEARD-SENTINEL", "presence": "PRESENCE-SENTINEL",
		"impulse": "IMPULSE-SENTINEL", "room_id": "buero-dev-1",
		"perception":      "KOERPER: BODY-SENTINEL\nENVIRONMENT: ENV-SENTINEL\nIMPULS: IMPULSE-SENTINEL",
		"evolution_voice": "VOICE-SENTINEL", "evolution_notes": "NOTES-SENTINEL",
	}
}

func responseFormatPipeline(t *testing.T, structured, sources bool) (*PipelineHandler, *pipelineMockProvider) {
	t.Helper()
	provider := &pipelineMockProvider{name: "mock", resp: &LLMResponse{
		Content: `{"schema_version":1,"decision":{"kind":"blocked","reason_code":"independent_choice"}}`,
		Model:   "mock-tier2", InputTokens: 10, OutputTokens: 10, TokensUsed: 20,
	}}
	registry := NewRegistry()
	registry.Register("mock", provider)
	ph := newTestPipelineHandler(registry, control.NewConfig("mock"))
	ph.caps.SetCapability("mock", capability.CapStructuredSystem, structured)
	if sources {
		dir := t.TempDir()
		agents := filepath.Join(dir, "agents")
		if err := os.Mkdir(agents, 0700); err != nil {
			t.Fatal(err)
		}
		dna := `[identity]
id = 5
name = "Format Employee"
role = "Developer"
department = "Engineering"
[personality]
openness = 0.8
conscientiousness = 0.9
extraversion = 0.2
[background]
bio = "BIO-SENTINEL"
quirks = ["QUIRK-SENTINEL"]
`
		if err := os.WriteFile(filepath.Join(agents, "AGENT-05-FORMAT.toml"), []byte(dna), 0600); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(dir, "company-context.md"), []byte("COMPANY-SENTINEL"), 0600); err != nil {
			t.Fatal(err)
		}
		ph.compiler = compiler.NewWithAssembler(compiler.NewTOMLLoader(agents), ph.caps)
	}
	return ph, provider
}

func sendResponseFormatRequest(t *testing.T, ph *PipelineHandler, metadata map[string]string) *httptest.ResponseRecorder {
	t.Helper()
	body, err := json.Marshal(map[string]any{
		"max_tokens": 128, "messages": []Message{{Role: "user", Content: "TASK-CONTRACT-SENTINEL"}},
		"metadata": metadata,
	})
	if err != nil {
		t.Fatal(err)
	}
	req := newAgentRuntimeTestRequest(t, string(body))
	if id := metadata["request_id"]; id != "" {
		req.Header.Set("X-Request-ID", id)
	}
	w := httptest.NewRecorder()
	ph.ServeHTTP(w, req)
	return w
}

func TestGovernedResponseFormatProviderPayloads(t *testing.T) {
	contracts := []struct {
		name     string
		metadata func() map[string]string
	}{
		{"project", modelWorkMetadata}, {"sales", salesRequestMetadata},
		{"adaptive", adaptiveRequestMetadata}, {"planning", projectPlanningMetadata},
		{"leadership", func() map[string]string { return leadershipReviewTestRequest().Metadata }},
	}
	for _, structured := range []bool{false, true} {
		for _, sources := range []bool{false, true} {
			mode := "flat"
			if structured {
				mode = "structured"
			}
			if sources {
				mode += "_sources"
			} else {
				mode += "_fallback"
			}
			t.Run(mode, func(t *testing.T) {
				ph, provider := responseFormatPipeline(t, structured, sources)
				ambientMetadata := responseFormatMetadata()
				w := sendResponseFormatRequest(t, ph, ambientMetadata)
				if w.Code != http.StatusOK {
					t.Fatalf("ambient: %d %s", w.Code, w.Body.String())
				}
				ambient := provider.lastReq
				ambientBytes, err := json.Marshal(ambient)
				if err != nil {
					t.Fatal(err)
				}
				for _, contract := range contracts {
					t.Run(contract.name, func(t *testing.T) {
						metadata := contract.metadata()
						// Subscription admission is tested independently; this fixture
						// captures the exact request at the provider boundary.
						delete(metadata, "subscription_allowance_id")
						delete(metadata, "subscription_catalog_digest")
						for key, value := range ambientMetadata {
							metadata[key] = value
						}
						w := sendResponseFormatRequest(t, ph, metadata)
						if w.Code != http.StatusOK {
							t.Fatalf("governed: %d %s", w.Code, w.Body.String())
						}
						governed := provider.lastReq
						assertGovernedResponseFormat(t, governed, sources)
						assertNonFormatContentUnchanged(t, ambient, governed, structured, sources)
						assertGovernedProviderSerialization(t, governed)
						var result PipelineResponse
						if err := json.Unmarshal(w.Body.Bytes(), &result); err != nil {
							t.Fatal(err)
						}
						if result.Content != provider.resp.Content || len(result.Actions) != 0 {
							t.Fatal("employee decision changed")
						}
					})
				}
				w = sendResponseFormatRequest(t, ph, ambientMetadata)
				if w.Code != http.StatusOK {
					t.Fatal(w.Body.String())
				}
				after, err := json.Marshal(provider.lastReq)
				if err != nil {
					t.Fatal(err)
				}
				// Request IDs are generated per call; compare only prompt bytes.
				var first, last LLMRequest
				if err := json.Unmarshal(ambientBytes, &first); err != nil {
					t.Fatal(err)
				}
				if err := json.Unmarshal(after, &last); err != nil {
					t.Fatal(err)
				}
				if !reflect.DeepEqual(first.Messages, last.Messages) || !reflect.DeepEqual(first.SystemBlocks, last.SystemBlocks) {
					t.Fatal("governed compilation contaminated ambient prompt")
				}
			})
		}
	}
}

func assertGovernedResponseFormat(t *testing.T, req *LLMRequest, sources bool) {
	t.Helper()
	system, conversation := splitRequest(req)
	if !strings.Contains(system, governedResponseFormat) || strings.Contains(system, "action_type") || strings.Contains(system, "Moegliche Aktionen:") {
		t.Fatalf("competing or missing response format: %s", system)
	}
	if !strings.Contains(conversation, "TASK-CONTRACT-SENTINEL") {
		t.Fatal("task contract lost")
	}
	markers := []string{"BODY-SENTINEL", "ENV-SENTINEL", "IMPULSE-SENTINEL"}
	if sources {
		markers = append(markers, "Format Employee", "COMPANY-SENTINEL", "VOICE-SENTINEL", "NOTES-SENTINEL")
	}
	for _, marker := range markers {
		if !strings.Contains(system, marker) {
			t.Errorf("lost %s", marker)
		}
	}
}

func assertNonFormatContentUnchanged(t *testing.T, ambient, governed *LLMRequest, structured, sources bool) {
	t.Helper()
	if structured {
		if len(governed.SystemBlocks) != len(ambient.SystemBlocks) {
			t.Fatal("block count changed")
		}
		for i := 0; i < len(ambient.SystemBlocks)-1; i++ {
			if !reflect.DeepEqual(governed.SystemBlocks[i], ambient.SystemBlocks[i]) {
				t.Errorf("non-format block %d changed", i)
			}
		}
		return
	}
	start, end := "Antworte IMMER mit genau EINER Aktion im JSON-Format:\n", "\n[SYSTEM_INJECTION]"
	if sources {
		start, end = "Moegliche Aktionen:\n", "Wenn Kollegen anwesend sind, sprich sie an!"
	}
	ambientSystem, _ := splitRequest(ambient)
	system, _ := splitRequest(governed)
	before, tail, found := strings.Cut(ambientSystem, start)
	_, after, ended := strings.Cut(tail, end)
	expected := before + governedResponseFormat + "\n" + end + after
	if sources {
		expected = governedResponseFormat + "\n" + before + end + after
	}
	if !found || !ended || system != expected {
		t.Fatal("flat non-format content changed")
	}
}

func assertGovernedProviderSerialization(t *testing.T, req *LLMRequest) {
	t.Helper()
	// Exercise the actual CLI and Anthropic serializers, not just compiler blocks.
	cli, err := buildCodexCLIPrompt(req)
	if err != nil || !strings.Contains(cli, governedResponseFormat) || strings.Contains(cli, "action_type") {
		t.Fatalf("CLI payload: %v %s", err, cli)
	}
	blocks, messages := splitAnthropicMessages(req)
	payload, err := json.Marshal(struct {
		System   []claudeSystemBlock `json:"system"`
		Messages []claudeMessage     `json:"messages"`
	}{blocks, messages})
	if err != nil || !strings.Contains(string(payload), governedResponseFormat) || strings.Contains(string(payload), "action_type") {
		t.Fatalf("Anthropic payload: %v %s", err, payload)
	}
}

func TestGovernedResponseFormatInvalidMetadataNeverReachesProvider(t *testing.T) {
	for _, structured := range []bool{false, true} {
		ph, provider := responseFormatPipeline(t, structured, false)
		for _, schema := range []string{"1", "2", "3", "4", "5", "unknown"} {
			metadata := responseFormatMetadata()
			metadata["company_execution_schema"] = schema
			w := sendResponseFormatRequest(t, ph, metadata)
			if w.Code != http.StatusUnprocessableEntity || provider.calls != 0 {
				t.Fatalf("schema=%s status=%d calls=%d", schema, w.Code, provider.calls)
			}
		}
	}
}

func TestGovernedResponseFormatAnthropicPassThroughUnchanged(t *testing.T) {
	ph, _ := responseFormatPipeline(t, true, false)
	for _, modelWork := range []bool{false, true} {
		req := LLMRequest{
			Format: RequestFormatAnthropic, Metadata: responseFormatMetadata(),
			Messages:     []Message{{Role: "user", Content: "original"}},
			SystemBlocks: []SystemBlock{{Type: "text", Text: "caller-owned-system", CacheControl: &CacheControl{Type: "ephemeral"}}},
		}
		before, err := json.Marshal(req)
		if err != nil {
			t.Fatal(err)
		}
		ph.injectPerception(&req, "Employee", "Developer", "mock", control.ConfigSnapshot{}, modelWork)
		after, err := json.Marshal(req)
		if err != nil {
			t.Fatal(err)
		}
		if string(before) != string(after) {
			t.Fatal("Anthropic system pass-through changed")
		}
	}
}
