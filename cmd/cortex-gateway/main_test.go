package main

import (
	"context"
	"encoding/json"
	"net"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/forwardqueue"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/proxy"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/sequencing"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/synthesis"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/ticksync"
)

func TestListenTCP4UsesAnIPv4Socket(t *testing.T) {
	listener, err := listenTCP4("0.0.0.0:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })

	address, ok := listener.Addr().(*net.TCPAddr)
	if !ok || address.IP.To4() == nil {
		t.Fatalf("listener address = %#v, want IPv4", listener.Addr())
	}
}

type inventoryTestProvider struct {
	name   string
	models []string
	err    error
}

type noInventoryTestProvider struct{ name string }

func (p *noInventoryTestProvider) Name() string { return p.name }
func (p *noInventoryTestProvider) Send(context.Context, *proxy.LLMRequest) (*proxy.LLMResponse, error) {
	return nil, nil
}
func (p *noInventoryTestProvider) HealthCheck(context.Context) error { return nil }

func (p *inventoryTestProvider) Name() string { return p.name }

func (p *inventoryTestProvider) Send(context.Context, *proxy.LLMRequest) (*proxy.LLMResponse, error) {
	return nil, nil
}

func (p *inventoryTestProvider) HealthCheck(context.Context) error { return p.err }

func (p *inventoryTestProvider) ModelInventory(context.Context) ([]string, error) {
	return append([]string(nil), p.models...), p.err
}

func TestDefaultPrimaryProviderFallsBackToClaudeCodeWithoutAPIKey(t *testing.T) {
	t.Setenv("CORTEX_PRIMARY_PROVIDER", "")
	t.Setenv("ANTHROPIC_API_KEY", "")

	if got := defaultPrimaryProvider(); got != "claude-code" {
		t.Fatalf("defaultPrimaryProvider() = %q, want %q", got, "claude-code")
	}
}

func TestDefaultPrimaryProviderUsesLocalLoopFlag(t *testing.T) {
	t.Setenv("CORTEX_PRIMARY_PROVIDER", "")
	t.Setenv("ANTHROPIC_API_KEY", "test-key")
	t.Setenv("CORTEX_LOCAL_LOOP", "1")

	if got := defaultPrimaryProvider(); got != proxy.LocalLoopProviderName {
		t.Fatalf("defaultPrimaryProvider() = %q, want %q", got, proxy.LocalLoopProviderName)
	}
}

func TestReadyValidatesActiveProviderInventoryWithoutExposingModels(t *testing.T) {
	catalog, err := proxy.LoadProviderCatalog(filepath.Join("..", "..", "config", "cortex-gateway.toml"))
	if err != nil {
		t.Fatal(err)
	}
	cfg := control.NewConfig("ollama")
	registry := proxy.NewRegistry()
	registry.Register("ollama", &inventoryTestProvider{
		name:   "ollama",
		models: []string{"qwen3:14b", "qwen3:8b", "qwen3:4b-instruct"},
	})

	recorder := httptest.NewRecorder()
	handleReady(catalog, cfg, registry, "").ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/ready", nil))
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d, body = %s", recorder.Code, recorder.Body.String())
	}
	var response map[string]any
	if err := json.Unmarshal(recorder.Body.Bytes(), &response); err != nil {
		t.Fatal(err)
	}
	if response["ready"] != true || response["model_inventory_status"] != "validated" {
		t.Fatalf("unexpected readiness response: %#v", response)
	}
	providerIDs, ok := response["catalog_provider_ids"].([]any)
	if !ok || len(providerIDs) != 4 {
		t.Fatalf("catalog provider IDs missing: %#v", response)
	}
	for _, model := range []string{"qwen3:14b", "qwen3:8b", "qwen3:4b-instruct"} {
		if responseBody := recorder.Body.String(); strings.Contains(responseBody, model) {
			t.Fatalf("readiness response exposed provider inventory model %q: %s", model, responseBody)
		}
	}
}

func TestReadyFailsClosedOnActiveProviderInventoryDrift(t *testing.T) {
	catalog, err := proxy.LoadProviderCatalog(filepath.Join("..", "..", "config", "cortex-gateway.toml"))
	if err != nil {
		t.Fatal(err)
	}
	cfg := control.NewConfig("ollama")
	registry := proxy.NewRegistry()
	registry.Register("ollama", &inventoryTestProvider{name: "ollama", models: []string{"qwen3:8b"}})

	recorder := httptest.NewRecorder()
	handleReady(catalog, cfg, registry, "").ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/ready", nil))
	if recorder.Code != http.StatusServiceUnavailable {
		t.Fatalf("status = %d, body = %s", recorder.Code, recorder.Body.String())
	}
	var response map[string]any
	if err := json.Unmarshal(recorder.Body.Bytes(), &response); err != nil {
		t.Fatal(err)
	}
	if response["ready"] != false || response["model_inventory_status"] != "drift" {
		t.Fatalf("unexpected readiness response: %#v", response)
	}
}

func TestReadyBlocksClaudeCodeWithoutExactGateBAttestation(t *testing.T) {
	catalog, err := proxy.LoadProviderCatalog(filepath.Join("..", "..", "config", "cortex-gateway.toml"))
	if err != nil {
		t.Fatal(err)
	}
	cfg := control.NewConfig("claude-code")
	registry := proxy.NewRegistry()
	registry.Register("claude-code", &noInventoryTestProvider{name: "claude-code"})

	for _, test := range []struct {
		name        string
		attestation string
		wantStatus  int
		wantGate    string
	}{
		{name: "missing", wantStatus: http.StatusServiceUnavailable, wantGate: "gate_b_attestation_required"},
		{name: "wrong catalog", attestation: "gate-b:claude-code:stale", wantStatus: http.StatusServiceUnavailable, wantGate: "gate_b_attestation_required"},
		{name: "exact", attestation: catalog.ExpectedGateBAttestation("claude-code"), wantStatus: http.StatusOK, wantGate: "gate_b_attested"},
	} {
		t.Run(test.name, func(t *testing.T) {
			recorder := httptest.NewRecorder()
			handleReady(catalog, cfg, registry, test.attestation).ServeHTTP(
				recorder,
				httptest.NewRequest(http.MethodGet, "/ready", nil),
			)
			if recorder.Code != test.wantStatus {
				t.Fatalf("status=%d body=%s", recorder.Code, recorder.Body.String())
			}
			var response map[string]any
			if err := json.Unmarshal(recorder.Body.Bytes(), &response); err != nil {
				t.Fatal(err)
			}
			if response["model_inventory_status"] != test.wantGate {
				t.Fatalf("response=%#v", response)
			}
		})
	}
}

func TestReadyAllowsTokenFreeLocalLoopWithoutGateBAttestation(t *testing.T) {
	catalog, err := proxy.LoadProviderCatalog(filepath.Join("..", "..", "config", "cortex-gateway.toml"))
	if err != nil {
		t.Fatal(err)
	}
	cfg := control.NewConfig(proxy.LocalLoopProviderName)
	registry := proxy.NewRegistry()
	registry.Register(proxy.LocalLoopProviderName, &noInventoryTestProvider{name: proxy.LocalLoopProviderName})
	recorder := httptest.NewRecorder()
	handleReady(catalog, cfg, registry, "").ServeHTTP(
		recorder,
		httptest.NewRequest(http.MethodGet, "/ready", nil),
	)
	if recorder.Code != http.StatusOK || !strings.Contains(recorder.Body.String(), `"model_inventory_status":"token_free_local"`) {
		t.Fatalf("status=%d body=%s", recorder.Code, recorder.Body.String())
	}
}

func TestApplyTrafficRuntimeConfigPropagatesRuntimeValues(t *testing.T) {
	cfg := control.NewConfig("claude-code")
	if err := cfg.Update(map[string]any{
		"synthesis_enabled":       true,
		"sequencing_enabled":      true,
		"tick_sync_enabled":       true,
		"tick_sync_timeout_ms":    25,
		"p3_timeout_ms":           30,
		"max_forward_concurrency": 2,
	}); err != nil {
		t.Fatalf("config update: %v", err)
	}

	synth := synthesis.NewEngine(false, nil)
	sequencer := sequencing.NewSequencer(time.Second, false, nil)
	tickSync := ticksync.NewBuffer(time.Second, false, nil)
	defer tickSync.Stop()
	queue := forwardqueue.NewManager(1)

	applyTrafficRuntimeConfig(cfg.Get(), synth, sequencer, tickSync, queue)

	if !synth.Enabled() {
		t.Fatal("synthesis engine should be enabled after runtime apply")
	}
	if !sequencer.Enabled() {
		t.Fatal("sequencer should be enabled after runtime apply")
	}
	if !tickSync.Enabled() {
		t.Fatal("tick sync should be enabled after runtime apply")
	}

	release1, err := queue.Acquire(context.Background())
	if err != nil {
		t.Fatalf("acquire 1: %v", err)
	}
	defer release1()
	release2, err := queue.Acquire(context.Background())
	if err != nil {
		t.Fatalf("acquire 2: %v", err)
	}
	defer release2()

	if stats := queue.Stats(); stats.Active != 2 {
		t.Fatalf("queue active = %d, want 2", stats.Active)
	}

	sequencer.MarkP1Active("room-1", "req-1", "AGENT-01")
	start := time.Now()
	_, _, ok := sequencer.WaitForP1("room-1")
	if ok {
		t.Fatal("expected runtime-updated sequencer timeout to fire")
	}
	if elapsed := time.Since(start); elapsed > 250*time.Millisecond {
		t.Fatalf("sequencer timeout not applied, waited %v", elapsed)
	}
}
