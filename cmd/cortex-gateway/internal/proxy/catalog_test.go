package proxy

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/modelpolicy"
)

func TestIssue395GatewayResponseGolden(t *testing.T) {
	path := filepath.Join("..", "..", "..", "..", "tests", "contracts", "issue-395-agent-runtime-response-v2.json")
	golden, err := os.ReadFile(path) //nolint:gosec // fixed repository fixture path
	if err != nil {
		t.Fatal(err)
	}
	var response PipelineResponse
	if err := json.Unmarshal(golden, &response); err != nil {
		t.Fatal(err)
	}
	if response.HierarchyTier != 2 || response.CostSource != "provider_reported" || response.EffectiveModel != "claude-sonnet-5" {
		t.Fatalf("unexpected golden response: %+v", response)
	}
	encoded, err := json.Marshal(response)
	if err != nil {
		t.Fatal(err)
	}
	var want, got map[string]any
	if err := json.Unmarshal(golden, &want); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(encoded, &got); err != nil {
		t.Fatal(err)
	}
	for _, field := range []string{"hierarchy_tier", "cost_source", "effective_model", "tier", "cost_usd"} {
		if got[field] != want[field] {
			t.Errorf("field %s: got=%v want=%v", field, got[field], want[field])
		}
	}
}

func TestProviderCatalogRealConfigAndTierMatrix(t *testing.T) {
	catalog, err := LoadProviderCatalog(filepath.Join("..", "..", "..", "..", "config", "cortex-gateway.toml"))
	if err != nil {
		t.Fatal(err)
	}
	tests := []struct {
		provider string
		tier     int
		want     string
	}{
		{"anthropic-direct", 1, "claude-opus-4-8"},
		{"anthropic-direct", 2, "claude-sonnet-5"},
		{"anthropic-direct", 3, "claude-haiku-4-5-20251001"},
		{"claude-code", 1, "claude-opus-4-8"},
		{"ollama", 2, "qwen3:8b"},
		{LocalLoopProviderName, 3, "local-loop-tier3"},
	}
	for _, test := range tests {
		got, err := catalog.Resolve(test.provider, test.tier, "")
		if err != nil || got.Model != test.want {
			t.Fatalf("%s tier %d: got %+v err=%v", test.provider, test.tier, got, err)
		}
	}
	if _, err := catalog.Resolve("ollama", 1, "claude-opus-4-8"); err == nil {
		t.Fatal("cross-provider override accepted")
	}
	if _, err := catalog.Resolve("unknown", 1, ""); err == nil {
		t.Fatal("unknown provider accepted")
	}
	if catalog.Digest() != "10ed8408bd69c9b10acda44f4cebc889680435945b08a5c3ef2cf068a58680aa" {
		t.Fatalf("semantic digest drifted: %s", catalog.Digest())
	}
	entry, _ := catalog.Entry("ollama")
	entry.AllowedModels[0] = "mutated-by-caller"
	entryAgain, _ := catalog.Entry("ollama")
	if entryAgain.AllowedModels[0] == "mutated-by-caller" {
		t.Fatal("catalog allowlist was mutable through Entry")
	}
	required := map[string]string{
		"anthropic-direct":    "anthropic-direct",
		"claude-code":         "claude-code",
		"ollama":              "ollama",
		LocalLoopProviderName: "local-loop",
	}
	if err := catalog.RequireProviders(required); err != nil {
		t.Fatal(err)
	}
	if err := catalog.RequireProviders(map[string]string{"missing": "missing"}); err == nil {
		t.Fatal("missing deploy provider accepted")
	}
	if err := catalog.ValidateInventory("ollama", []string{"qwen3:14b", "qwen3:8b"}); err == nil {
		t.Fatal("incomplete provider inventory accepted")
	}
	if err := catalog.ValidateInventory("ollama", []string{"qwen3:4b-instruct", "qwen3:8b", "qwen3:14b"}); err != nil {
		t.Fatal(err)
	}
	if err := catalog.ValidateInventory("ollama", []string{"qwen3:4b-instruct", "qwen3:8b", "qwen3:14b", "qwen3:32b"}); err == nil {
		t.Fatal("uncataloged provider inventory model accepted")
	}
	if err := catalog.ValidateInventory("ollama", []string{"qwen3:4b-instruct", "qwen3:8b", "qwen3:8b"}); err == nil {
		t.Fatal("duplicate provider inventory model accepted")
	}
	if err := catalog.ValidateInventory("ollama", []string{"qwen3:4b-instruct", "qwen3:8b", " qwen3:14b"}); err == nil {
		t.Fatal("whitespace-normalized provider inventory model accepted")
	}
}

func TestProviderActivationRequiresExactGateBAttestationWithoutInventory(t *testing.T) {
	catalog, err := LoadProviderCatalog(filepath.Join("..", "..", "..", "..", "config", "cortex-gateway.toml"))
	if err != nil {
		t.Fatal(err)
	}
	if err := catalog.ValidateProviderActivation("claude-code", false, ""); err == nil {
		t.Fatal("claude-code activated without Gate B attestation")
	}
	if err := catalog.ValidateProviderActivation("claude-code", false, "gate-b:claude-code:stale"); err == nil {
		t.Fatal("claude-code accepted stale catalog attestation")
	}
	if err := catalog.ValidateProviderActivation(
		"claude-code",
		false,
		catalog.ExpectedGateBAttestation("claude-code"),
	); err != nil {
		t.Fatalf("exact Gate B attestation rejected: %v", err)
	}
	if err := catalog.ValidateProviderActivation(LocalLoopProviderName, false, ""); err != nil {
		t.Fatalf("token-free local-loop blocked: %v", err)
	}
}

func TestCatalogDigestAndGateBAttestationBindHierarchyMapping(t *testing.T) {
	catalog := &ProviderCatalog{providers: map[string]ProviderCatalogEntry{
		"provider": {
			Type:          "mock",
			DefaultModel:  "model-2",
			AllowedModels: []string{"model-3", "model-1", "model-2"},
			HierarchyModels: HierarchyModelMap{
				Tier1: "model-1", Tier2: "model-2", Tier3: "model-3",
			},
		},
	}}
	if err := catalog.Validate(); err != nil {
		t.Fatal(err)
	}
	firstBytes, err := catalog.semanticBytes()
	if err != nil {
		t.Fatal(err)
	}
	want := "{\"algorithm\":\"cortex-catalog-v1\",\"providers\":[{\"allowed_models\":[\"model-1\",\"model-2\",\"model-3\"],\"default_model\":\"model-2\",\"hierarchy_models\":{\"tier_1\":\"model-1\",\"tier_2\":\"model-2\",\"tier_3\":\"model-3\"},\"id\":\"provider\",\"type\":\"mock\"}]}\n"
	if string(firstBytes) != want {
		t.Fatalf("canonical bytes:\n%s\nwant:\n%s", firstBytes, want)
	}
	firstDigest, err := catalog.SemanticDigest()
	if err != nil {
		t.Fatal(err)
	}
	catalog.digest = firstDigest
	staleAttestation := catalog.ExpectedGateBAttestation("provider")

	entry := catalog.providers["provider"]
	entry.HierarchyModels.Tier1, entry.HierarchyModels.Tier3 = entry.HierarchyModels.Tier3, entry.HierarchyModels.Tier1
	catalog.providers["provider"] = entry
	secondDigest, err := catalog.SemanticDigest()
	if err != nil {
		t.Fatal(err)
	}
	if secondDigest == firstDigest {
		t.Fatal("hierarchy remapping did not change semantic digest")
	}
	catalog.digest = secondDigest
	if err := catalog.ValidateProviderActivation("provider", false, staleAttestation); err == nil {
		t.Fatal("stale Gate B attestation survived hierarchy remapping")
	}
}

func TestProviderCatalogRejectsIncompleteHierarchyMap(t *testing.T) {
	catalog := &ProviderCatalog{providers: map[string]ProviderCatalogEntry{
		"broken": {
			Type:          "broken",
			DefaultModel:  "model-2",
			AllowedModels: []string{"model-1", "model-2", "model-3"},
			HierarchyModels: HierarchyModelMap{
				Tier1: "model-1", Tier2: "model-2",
			},
		},
	}}
	if err := catalog.Validate(); err == nil {
		t.Fatal("incomplete hierarchy map accepted")
	}
}

func TestProviderCatalogResolvesTieredControlPolicy(t *testing.T) {
	catalog, err := LoadProviderCatalog(filepath.Join("..", "..", "..", "..", "config", "cortex-gateway.toml"))
	if err != nil {
		t.Fatal(err)
	}
	policy, err := modelpolicy.ParseValue(map[string]any{
		"providers": map[string]any{
			LocalLoopProviderName: map[string]any{
				"tier1": "local-loop-tier3",
				"tier2": "local-loop-tier2",
				"tier3": "local-loop-tier1",
			},
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := catalog.ValidatePolicy(policy); err != nil {
		t.Fatal(err)
	}
	resolved, err := catalog.ResolvePolicy(LocalLoopProviderName, RequestClassAgentRuntime, 1, "", policy)
	if err != nil || resolved.Model != "local-loop-tier3" || resolved.Source != PolicySourceAgentRuntime {
		t.Fatalf("resolution=%+v err=%v", resolved, err)
	}
	if _, err := catalog.ResolvePolicy("ollama", RequestClassAgentRuntime, 1, "", policy); err == nil {
		t.Fatal("missing provider policy map accepted")
	}
	resolved, err = catalog.ResolvePolicy(LocalLoopProviderName, RequestClassAgentRuntime, 1, "local-loop-tier1", policy)
	if err != nil || resolved.Model != "local-loop-tier1" || resolved.Source != PolicySourceRequestOverride {
		t.Fatalf("request override=%+v err=%v", resolved, err)
	}
}

func TestCallerCredentialsPathRoleMatrix(t *testing.T) {
	credentials := CallerCredentials{AgentRuntime: "agent", PlatformControlplane: "platform", Evolution: "evolution", Judge: "judge"}
	if err := credentials.Validate(); err != nil {
		t.Fatal(err)
	}
	next := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		role, ok := callerRoleFromContext(r.Context())
		if !ok {
			t.Fatal("caller role missing")
		}
		_, _ = w.Write([]byte(role))
	})
	tests := []struct {
		path, token string
		status      int
		body        string
	}{
		{"/internal/agent-runtime", "agent", 200, "agent_runtime"},
		{"/internal/agent-runtime", "judge", 403, ""},
		{"/internal/llm", "platform", 200, "platform_controlplane"},
		{"/internal/llm", "evolution", 200, "evolution"},
		{"/internal/llm", "judge", 200, "judge"},
		{"/internal/llm", "agent", 403, ""},
		{"/internal/llm", "wrong", 401, ""},
		{"/internal/llm", "judge ", 401, ""},
	}
	for _, test := range tests {
		req := httptest.NewRequest(http.MethodPost, test.path, nil)
		req.Header.Set("Authorization", "Bearer "+test.token)
		rec := httptest.NewRecorder()
		credentials.Middleware(next).ServeHTTP(rec, req)
		if rec.Code != test.status {
			t.Errorf("%s/%s status=%d want=%d", test.path, test.token, rec.Code, test.status)
		}
		if test.body != "" && strings.TrimSpace(rec.Body.String()) != test.body {
			t.Errorf("body=%q want=%q", rec.Body.String(), test.body)
		}
	}
}

func TestCallerCredentialsStripInternalSecretsBeforePipeline(t *testing.T) {
	credentials := CallerCredentials{AgentRuntime: "agent", PlatformControlplane: "platform", Evolution: "evolution", Judge: "judge"}
	next := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		for _, name := range []string{"Authorization", "Proxy-Authorization", "X-Sentinel-Internal-Role", "X-Sentinel-Caller-Role", "X-Internal-Caller"} {
			if value := r.Header.Get(name); value != "" {
				t.Errorf("internal header %s reached the pipeline: %q", name, value)
			}
		}
		w.WriteHeader(http.StatusNoContent)
	})
	req := httptest.NewRequest(http.MethodPost, "/internal/agent-runtime", nil)
	req.Header.Set("Authorization", "Bearer agent")
	req.Header.Set("Proxy-Authorization", "proxy-secret")
	req.Header.Set("X-Sentinel-Internal-Role", "forged")
	req.Header.Set("X-Sentinel-Caller-Role", "forged")
	req.Header.Set("X-Internal-Caller", "forged")
	rec := httptest.NewRecorder()
	credentials.Middleware(next).ServeHTTP(rec, req)
	if rec.Code != http.StatusNoContent {
		t.Fatalf("status=%d", rec.Code)
	}
}

func TestCallerCredentialsPreservePublicProviderAuthorization(t *testing.T) {
	credentials := CallerCredentials{AgentRuntime: "agent", PlatformControlplane: "platform", Evolution: "evolution", Judge: "judge"}
	next := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if got := r.Header.Get("Authorization"); got != "Bearer provider-token" {
			t.Fatalf("public provider authorization=%q", got)
		}
		w.WriteHeader(http.StatusNoContent)
	})
	req := httptest.NewRequest(http.MethodPost, "/v1/messages", nil)
	req.Header.Set("Authorization", "Bearer provider-token")
	rec := httptest.NewRecorder()
	credentials.Middleware(next).ServeHTTP(rec, req)
	if rec.Code != http.StatusNoContent {
		t.Fatalf("status=%d", rec.Code)
	}
}

func TestCallerCredentialsRejectDuplicates(t *testing.T) {
	credentials := CallerCredentials{AgentRuntime: "same", PlatformControlplane: "same", Evolution: "evolution", Judge: "judge"}
	if err := credentials.Validate(); err == nil {
		t.Fatal("duplicate caller credentials accepted")
	}
}

func TestCredentialNormalizationRemovesOnlyOneLineEnding(t *testing.T) {
	if got := trimCredentialLineEnding(" token \n"); got != " token " {
		t.Fatalf("credential whitespace changed: %q", got)
	}
	if got := trimCredentialLineEnding("token\n\n"); got != "token\n" {
		t.Fatalf("more than one line ending removed: %q", got)
	}
}

func TestCredentialFileRequiresOwnerOnlyPermissions(t *testing.T) {
	path := filepath.Join(t.TempDir(), "caller-token")
	if err := os.WriteFile(path, []byte("token\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if got, err := readCredentialFile(path, "TEST_CREDENTIAL_FILE"); err != nil || got != "token" {
		t.Fatalf("owner-only credential=%q err=%v", got, err)
	}
	if err := os.Chmod(path, 0o640); err != nil { //nolint:gosec // negative permission test
		t.Fatal(err)
	}
	if _, err := readCredentialFile(path, "TEST_CREDENTIAL_FILE"); err == nil {
		t.Fatal("group-readable credential accepted")
	}
}

func TestSystemdCredentialPermissions(t *testing.T) {
	const directory = "/run/credentials/sentinel-gateway.service"
	path := filepath.Join(directory, "caller-agent-runtime")
	if !secureCredentialMode(0o440, 0, 0, path, directory) {
		t.Fatal("systemd root:root 0440 credential rejected")
	}
	for _, test := range []struct {
		name      string
		mode      os.FileMode
		uid, gid  uint32
		path, dir string
	}{
		{"outside credential directory", 0o440, 0, 0, "/tmp/token", directory},
		{"group writable", 0o460, 0, 0, path, directory},
		{"wrong owner", 0o440, 1000, 0, path, directory},
		{"wrong group", 0o440, 0, 1000, path, directory},
		{"missing credential directory", 0o440, 0, 0, path, ""},
	} {
		t.Run(test.name, func(t *testing.T) {
			if secureCredentialMode(test.mode, test.uid, test.gid, test.path, test.dir) {
				t.Fatal("insecure systemd credential permissions accepted")
			}
		})
	}
}

func TestAuthorizedClassificationAndPublicClaimStripping(t *testing.T) {
	agent := &LLMRequest{Metadata: map[string]string{"agent_id": "7", "agent_role": "Engineer", "hierarchy_tier": "2"}}
	class, err := ClassifyRequest("/internal/agent-runtime", agent, CallerRoleAgentRuntime)
	if err != nil || class != RequestClassAgentRuntime || agent.HierarchyTier != 2 {
		t.Fatalf("agent classification: %s tier=%d err=%v", class, agent.HierarchyTier, err)
	}
	public := &LLMRequest{Metadata: map[string]string{"agent_id": "7", "agent_role": "CEO", "hierarchy_tier": "1", "safe": "yes"}}
	class, err = ClassifyRequest("/v1/chat/completions", public, CallerRoleExternalCompat)
	if err != nil || class != RequestClassExternalCompat {
		t.Fatal(err)
	}
	if _, retained := public.Metadata["agent_id"]; retained || public.Metadata["safe"] != "yes" {
		t.Fatalf("public claims not stripped safely: %#v", public.Metadata)
	}
	service := &LLMRequest{Metadata: map[string]string{"agent_role": "CEO"}}
	if _, err := ClassifyRequest("/internal/llm", service, CallerRoleJudge); err == nil {
		t.Fatal("service caller supplied agent claim")
	}
}

func assertAuthorizedAgentRuntimeResponseLog(
	t *testing.T,
	entry ResponseLogEntry,
	catalog *ProviderCatalog,
	credentials CallerCredentials,
) {
	t.Helper()
	if entry.CallerRole != CallerRoleAgentRuntime || entry.HierarchyTier != 1 ||
		entry.ModelTier != "unknown" || entry.EffectiveModel != "mock-tier1" ||
		entry.CatalogDigest != catalog.Digest() || entry.CostSource != CostSourceNonProviderZero {
		t.Fatalf("response inspector entry=%+v", entry)
	}
	encodedLog, err := json.Marshal(entry)
	if err != nil {
		t.Fatal(err)
	}
	for _, secret := range []string{
		credentials.AgentRuntime,
		credentials.PlatformControlplane,
		credentials.Evolution,
		credentials.Judge,
	} {
		if strings.Contains(string(encodedLog), secret) {
			t.Fatalf("response inspector leaked credential material: %s", encodedLog)
		}
	}
}

func TestAuthorizedAgentRuntimePipelineResolvesCatalogModelAndWireFields(t *testing.T) {
	catalog := &ProviderCatalog{providers: map[string]ProviderCatalogEntry{
		"mock": {
			Type: "mock", DefaultModel: "mock-tier2",
			AllowedModels: []string{"mock-tier1", "mock-tier2", "mock-tier3"},
			HierarchyModels: HierarchyModelMap{
				Tier1: "mock-tier1", Tier2: "mock-tier2", Tier3: "mock-tier3",
			},
		},
	}}
	if err := catalog.Validate(); err != nil {
		t.Fatal(err)
	}
	digest, err := catalog.SemanticDigest()
	if err != nil {
		t.Fatal(err)
	}
	catalog.digest = digest
	config := control.NewConfig("mock")
	config.SetAgentRuntimePolicyValidator(catalog.ValidatePolicy)
	provider := &pipelineMockProvider{name: "mock", resp: &LLMResponse{
		Content: "ok", Model: "mock-tier1", InputTokens: 10, OutputTokens: 2, TokensUsed: 12,
	}}
	registry := NewRegistry()
	registry.Register("mock", provider)
	handler := newTestPipelineHandler(registry, config)
	handler.catalog = catalog
	handler.responseLogs = NewResponseLogBuffer(10)
	credentials := CallerCredentials{
		AgentRuntime:         "agent-runtime-secret-value",
		PlatformControlplane: "platform-secret-value",
		Evolution:            "evolution-secret-value",
		Judge:                "judge-secret-value",
	}

	req := httptest.NewRequest(http.MethodPost, "/internal/agent-runtime", strings.NewReader(
		`{"messages":[{"role":"user","content":"test"}],"metadata":{"agent_id":"7","agent_role":"Engineer","hierarchy_tier":"1"}}`,
	))
	req.Header.Set("Authorization", "Bearer "+credentials.AgentRuntime)
	recorder := httptest.NewRecorder()
	credentials.Middleware(handler).ServeHTTP(recorder, req)
	if recorder.Code != http.StatusOK {
		t.Fatalf("status=%d body=%s", recorder.Code, recorder.Body.String())
	}
	if provider.lastReq == nil || provider.lastReq.Model != "mock-tier1" || provider.lastReq.HierarchyTier != 1 {
		t.Fatalf("provider request=%+v", provider.lastReq)
	}
	var response PipelineResponse
	if err := json.NewDecoder(recorder.Body).Decode(&response); err != nil {
		t.Fatal(err)
	}
	if response.HierarchyTier != 1 || response.EffectiveModel != "mock-tier1" ||
		response.CostSource != CostSourceNonProviderZero || response.Tier != "unknown" {
		t.Fatalf("wire response=%+v", response)
	}
	entries := handler.responseLogs.Entries()
	if len(entries) != 1 {
		t.Fatalf("response log entries=%d, want 1", len(entries))
	}
	assertAuthorizedAgentRuntimeResponseLog(t, entries[0], catalog, credentials)
}

func TestResponseInspectorDistinguishesEvolutionAndJudgeCallerRoles(t *testing.T) {
	provider := &pipelineMockProvider{name: "mock", resp: &LLMResponse{
		Content: "ok", Model: "mock-model", InputTokens: 1, OutputTokens: 1, TokensUsed: 2,
	}}
	registry := NewRegistry()
	registry.Register("mock", provider)
	handler := newTestPipelineHandler(registry, control.NewConfig("mock"))
	handler.responseLogs = NewResponseLogBuffer(10)

	for _, role := range []CallerRole{CallerRoleEvolution, CallerRoleJudge} {
		req := httptest.NewRequest(http.MethodPost, "/internal/llm", strings.NewReader(
			`{"messages":[{"role":"user","content":"test"}]}`,
		))
		req = req.WithContext(callerRoleContext(req.Context(), role))
		recorder := httptest.NewRecorder()
		handler.ServeHTTP(recorder, req)
		if recorder.Code != http.StatusOK {
			t.Fatalf("role=%s status=%d body=%s", role, recorder.Code, recorder.Body.String())
		}
	}

	entries := handler.responseLogs.Entries()
	if len(entries) != 2 || entries[0].CallerRole != CallerRoleEvolution ||
		entries[1].CallerRole != CallerRoleJudge {
		t.Fatalf("caller roles not preserved: %+v", entries)
	}
}

func TestAgentRuntimeStreamingFailsBeforeProviderExecution(t *testing.T) {
	provider := &pipelineMockProvider{name: "mock", resp: &LLMResponse{Content: "unexpected"}}
	registry := NewRegistry()
	registry.Register("mock", provider)
	handler := newTestPipelineHandler(registry, control.NewConfig("mock"))

	req := newAgentRuntimeTestRequest(t,
		`{"messages":[{"role":"user","content":"test"}],"stream":true,"metadata":{"agent_id":"7","hierarchy_tier":"1"}}`,
	)
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, req)
	if recorder.Code != http.StatusUnprocessableEntity {
		t.Fatalf("status=%d body=%s", recorder.Code, recorder.Body.String())
	}
	if provider.calls != 0 {
		t.Fatalf("streaming agent-runtime request reached provider %d times", provider.calls)
	}
}

func TestRegenerationRequestPreservesResolvedHierarchyTier(t *testing.T) {
	base := &LLMRequest{HierarchyTier: 2, EffectiveModel: "mock-tier2"}
	clone := cloneRegenRequest(base, []Message{{Role: "user", Content: "retry"}}, 0.2)
	if clone.HierarchyTier != 2 || clone.EffectiveModel != "mock-tier2" {
		t.Fatalf("regeneration lost resolved routing: %+v", clone)
	}
}

func TestPublicPipelineOmitsAgentRuntimeWireFields(t *testing.T) {
	catalog := &ProviderCatalog{providers: map[string]ProviderCatalogEntry{
		"mock": {
			Type: "mock", DefaultModel: "mock-tier2",
			AllowedModels: []string{"mock-tier1", "mock-tier2", "mock-tier3"},
			HierarchyModels: HierarchyModelMap{
				Tier1: "mock-tier1", Tier2: "mock-tier2", Tier3: "mock-tier3",
			},
		},
	}}
	if err := catalog.Validate(); err != nil {
		t.Fatal(err)
	}
	config := control.NewConfig("mock")
	provider := &pipelineMockProvider{name: "mock", resp: &LLMResponse{
		Content: "ok", Model: "mock-tier2", InputTokens: 10, OutputTokens: 2, TokensUsed: 12,
	}}
	registry := NewRegistry()
	registry.Register("mock", provider)
	handler := newTestPipelineHandler(registry, config)
	handler.catalog = catalog

	req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(
		`{"messages":[{"role":"user","content":"test"}],"metadata":{"agent_id":"7","agent_role":"CEO","hierarchy_tier":"1"}}`,
	))
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, req)
	if recorder.Code != http.StatusOK {
		t.Fatalf("status=%d body=%s", recorder.Code, recorder.Body.String())
	}
	var response map[string]any
	if err := json.NewDecoder(recorder.Body).Decode(&response); err != nil {
		t.Fatal(err)
	}
	for _, field := range []string{"hierarchy_tier", "cost_source", "effective_model"} {
		if _, present := response[field]; present {
			t.Fatalf("public response exposed agent-runtime field %q: %#v", field, response)
		}
	}
	if provider.lastReq == nil || provider.lastReq.RequestClass != RequestClassExternalCompat {
		t.Fatalf("public request class = %+v", provider.lastReq)
	}
}

func TestReportedZeroCostRemainsAuthoritative(t *testing.T) {
	zero := 0.0
	cost, source := resolveResponseCost(PipelineResponse{Provider: "claude-code", ReportedCostUSD: &zero})
	if cost != 0 || source != "provider_reported" {
		t.Fatalf("cost=%f source=%s", cost, source)
	}
	cost, source = resolveResponseCost(PipelineResponse{Provider: "claude-code"})
	if cost != 0 || source != "pricing_unknown" {
		t.Fatalf("cost=%f source=%s", cost, source)
	}
}
