package control

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/modelpolicy"
)

func testLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(io.Discard, nil))
}

// TestNewConfig_Defaults verifies that NewConfig sets sensible defaults.
func TestNewConfig_Defaults(t *testing.T) {
	cfg := NewConfig("claude")
	snapshot := cfg.Get()

	if snapshot.PrimaryProvider != "claude" {
		t.Errorf("expected primary provider 'claude', got %q", snapshot.PrimaryProvider)
	}
	if snapshot.Temperature != 0.7 {
		t.Errorf("expected temperature 0.7, got %f", snapshot.Temperature)
	}
	if snapshot.MaxTokens != 4096 {
		t.Errorf("expected max_tokens 4096, got %d", snapshot.MaxTokens)
	}
	if snapshot.RateLimit != 0 {
		t.Errorf("expected rate_limit 0, got %f", snapshot.RateLimit)
	}
	if snapshot.SynthesisEnabled {
		t.Error("expected synthesis_enabled false by default")
	}
	if snapshot.SequencingEnabled {
		t.Error("expected sequencing_enabled false by default")
	}
	if snapshot.TickSyncEnabled {
		t.Error("expected tick_sync_enabled false by default")
	}
	if snapshot.APICPEnabled {
		t.Error("expected apicp_enabled false by default")
	}
	if snapshot.TickSyncTimeoutMs != 2000 {
		t.Errorf("expected tick_sync_timeout_ms 2000, got %d", snapshot.TickSyncTimeoutMs)
	}
	if snapshot.P3TimeoutMs != 5000 {
		t.Errorf("expected p3_timeout_ms 5000, got %d", snapshot.P3TimeoutMs)
	}
	if snapshot.MaxForwardConcurrency != 3 {
		t.Errorf("expected max_forward_concurrency 3, got %d", snapshot.MaxForwardConcurrency)
	}
	if snapshot.InterceptMode != "auto" {
		t.Errorf("expected intercept_mode auto, got %q", snapshot.InterceptMode)
	}
	if legacy, ok := snapshot.AgentRuntimeModelPolicy.LegacyValue(); !ok || legacy != "" {
		t.Errorf("expected no default agent_runtime_model_policy override")
	}
	if snapshot.LocalLoopEnabled {
		t.Error("expected local_loop_enabled false by default")
	}
	if got := NewConfig("local-loop").Get(); !got.LocalLoopEnabled {
		t.Error("expected local_loop_enabled true when primary provider is local-loop")
	}
}

// TestConfig_GetSnapshot verifies that Get returns a snapshot and is concurrent-safe.
func TestConfig_GetSnapshot(t *testing.T) {
	cfg := NewConfig("claude")

	var wg sync.WaitGroup
	for i := 0; i < 100; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			snapshot := cfg.Get()
			if snapshot.PrimaryProvider != "claude" {
				t.Errorf("unexpected provider in snapshot: %q", snapshot.PrimaryProvider)
			}
		}()
	}
	wg.Wait()
}

// TestConfig_Update_Temperature verifies valid temperature updates.
func TestConfig_Update_Temperature(t *testing.T) {
	cfg := NewConfig("claude")

	err := cfg.Update(map[string]interface{}{"temperature": 1.5})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	snapshot := cfg.Get()
	if snapshot.Temperature != 1.5 {
		t.Errorf("expected temperature 1.5, got %f", snapshot.Temperature)
	}
}

// TestConfig_Update_InvalidTemperature verifies that temperature >2.0 is rejected.
func TestConfig_Update_InvalidTemperature(t *testing.T) {
	cfg := NewConfig("claude")

	err := cfg.Update(map[string]interface{}{"temperature": 2.5})
	if err == nil {
		t.Fatal("expected error for temperature > 2.0")
	}
	if !strings.Contains(err.Error(), "temperature") {
		t.Errorf("error should mention temperature: %v", err)
	}

	// Negative temperature
	err = cfg.Update(map[string]interface{}{"temperature": -0.1})
	if err == nil {
		t.Fatal("expected error for negative temperature")
	}
}

// TestConfig_Update_MaxTokens verifies valid and invalid max_tokens updates.
func TestConfig_Update_MaxTokens(t *testing.T) {
	cfg := NewConfig("claude")

	err := cfg.Update(map[string]interface{}{"max_tokens": float64(8192)})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	snapshot := cfg.Get()
	if snapshot.MaxTokens != 8192 {
		t.Errorf("expected max_tokens 8192, got %d", snapshot.MaxTokens)
	}

	// Invalid: 0 tokens
	err = cfg.Update(map[string]interface{}{"max_tokens": float64(0)})
	if err == nil {
		t.Fatal("expected error for max_tokens = 0")
	}
}

// TestConfig_Update_RateLimit verifies rate limit validation.
func TestConfig_Update_RateLimit(t *testing.T) {
	cfg := NewConfig("claude")

	err := cfg.Update(map[string]interface{}{"rate_limit_rps": 10.0})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	snapshot := cfg.Get()
	if snapshot.RateLimit != 10.0 {
		t.Errorf("expected rate_limit 10.0, got %f", snapshot.RateLimit)
	}

	// Negative rate limit
	err = cfg.Update(map[string]interface{}{"rate_limit_rps": -1.0})
	if err == nil {
		t.Fatal("expected error for negative rate_limit_rps")
	}
}

// TestConfig_Update_UnknownKey verifies unknown keys are rejected.
func TestConfig_Update_UnknownKey(t *testing.T) {
	cfg := NewConfig("claude")

	err := cfg.Update(map[string]interface{}{"nonexistent": "value"})
	if err == nil {
		t.Fatal("expected error for unknown config key")
	}
	if !strings.Contains(err.Error(), "unknown config key") {
		t.Errorf("error should mention unknown key: %v", err)
	}
}

func TestConfig_Update_MixedInvalidPatchIsAtomic(t *testing.T) {
	cfg := NewConfig("claude-code")
	before := cfg.Get()
	err := cfg.Update(map[string]interface{}{
		"temperature": 1.2,
		"max_tokens":  0,
	})
	if err == nil {
		t.Fatal("mixed invalid patch accepted")
	}
	after := cfg.Get()
	if after.Temperature != before.Temperature || after.MaxTokens != before.MaxTokens {
		t.Fatalf("partial mutation: before=%+v after=%+v", before, after)
	}
}

func TestConfig_ProviderValidationIsFailClosedAndAtomic(t *testing.T) {
	cfg := NewConfig("known")
	cfg.SetProviderValidator(func(provider string) error {
		if provider != "known" {
			return fmt.Errorf("unknown provider %q", provider)
		}
		return nil
	})
	before := cfg.Get()
	if err := cfg.Update(map[string]interface{}{
		"temperature":      1.2,
		"primary_provider": "unknown",
	}); err == nil {
		t.Fatal("unknown primary provider accepted")
	}
	after := cfg.Get()
	if after.Temperature != before.Temperature || after.PrimaryProvider != before.PrimaryProvider {
		t.Fatalf("provider validation partially mutated config: before=%+v after=%+v", before, after)
	}
	if err := cfg.SetAgentProvider("AGENT-01", "unknown"); err == nil {
		t.Fatal("unknown per-agent provider accepted")
	}
}

func TestConfig_Update_MixedInvalidPolicyPatchIsAtomic(t *testing.T) {
	cfg := NewConfig("local-loop")
	cfg.SetAgentRuntimePolicyValidator(func(policy modelpolicy.Policy) error {
		if _, legacy := policy.LegacyValue(); legacy {
			return nil
		}
		return errors.New("catalog rejected policy")
	})
	before := cfg.Get()
	err := cfg.Update(map[string]interface{}{
		"temperature": 1.2,
		"agent_runtime_model_policy": map[string]interface{}{
			"providers": map[string]interface{}{
				"local-loop": map[string]interface{}{
					"tier1": "one", "tier2": "two", "tier3": "three",
				},
			},
		},
	})
	if err == nil {
		t.Fatal("mixed invalid policy patch accepted")
	}
	after := cfg.Get()
	if after.Temperature != before.Temperature {
		t.Fatalf("valid field partially committed: before=%v after=%v", before.Temperature, after.Temperature)
	}
}

func TestConfig_Update_AgentRuntimeModelPolicy(t *testing.T) {
	cfg := NewConfig("claude")

	if err := cfg.Update(map[string]interface{}{"agent_runtime_model_policy": "haiku"}); err != nil {
		t.Fatalf("update haiku policy: %v", err)
	}
	if got, ok := cfg.Get().AgentRuntimeModelPolicy.LegacyValue(); !ok || got != "haiku" {
		t.Fatalf("AgentRuntimeModelPolicy legacy = %q/%v, want haiku/true", got, ok)
	}

	if err := cfg.Update(map[string]interface{}{"agent_runtime_model_policy": ""}); err != nil {
		t.Fatalf("clear policy: %v", err)
	}
	if got, ok := cfg.Get().AgentRuntimeModelPolicy.LegacyValue(); !ok || got != "" {
		t.Fatalf("AgentRuntimeModelPolicy legacy = %q/%v, want empty/true", got, ok)
	}

	if err := cfg.Update(map[string]interface{}{"agent_runtime_model_policy": "opus"}); err == nil {
		t.Fatal("expected invalid policy to be rejected")
	}
	if err := cfg.Update(map[string]interface{}{"agent_runtime_model_policy": 42}); err == nil {
		t.Fatal("expected non-string policy to be rejected")
	}

	tiered := map[string]interface{}{
		"providers": map[string]interface{}{
			"local-loop": map[string]interface{}{
				"tier1": "local-loop-tier1",
				"tier2": "local-loop-tier2",
				"tier3": "local-loop-tier3",
			},
		},
	}
	if err := cfg.Update(map[string]interface{}{"agent_runtime_model_policy": tiered}); err != nil {
		t.Fatalf("update tiered policy: %v", err)
	}
	got := cfg.Get().AgentRuntimeModelPolicy.Providers()["local-loop"]
	if got.Tier1 != "local-loop-tier1" || got.Tier3 != "local-loop-tier3" {
		t.Fatalf("tiered policy did not round-trip: %+v", got)
	}
	copy := cfg.Get()
	providers := copy.AgentRuntimeModelPolicy.Providers()
	mutated := providers["local-loop"]
	mutated.Tier2 = "mutated"
	providers["local-loop"] = mutated
	if cfg.Get().AgentRuntimeModelPolicy.Providers()["local-loop"].Tier2 != "local-loop-tier2" {
		t.Fatal("policy snapshot was not deep-copied")
	}
}

// TestPlane_HandleGetConfig tests the GET /control/config endpoint.
func TestPlane_HandleGetConfig(t *testing.T) {
	cfg := NewConfig("claude")
	plane := NewPlane(cfg, testLogger())
	handler := plane.Handler()

	req := httptest.NewRequest(http.MethodGet, "/control/config", nil)
	rec := httptest.NewRecorder()
	handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("expected status 200, got %d", rec.Code)
	}

	var result ConfigSnapshot
	if err := json.NewDecoder(rec.Body).Decode(&result); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}
	if result.PrimaryProvider != "claude" {
		t.Errorf("expected provider 'claude', got %q", result.PrimaryProvider)
	}
	if result.Temperature != 0.7 {
		t.Errorf("expected temperature 0.7, got %f", result.Temperature)
	}
}

// TestPlane_HandleUpdateConfig tests the PATCH /control/config endpoint.
func TestPlane_HandleUpdateConfig(t *testing.T) {
	cfg := NewConfig("claude")
	plane := NewPlane(cfg, testLogger())
	handler := plane.Handler()

	body := `{"temperature": 0.9, "max_tokens": 2048}`
	req := httptest.NewRequest(http.MethodPatch, "/control/config", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("expected status 200, got %d: %s", rec.Code, rec.Body.String())
	}

	var result ConfigSnapshot
	if err := json.NewDecoder(rec.Body).Decode(&result); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}
	if result.Temperature != 0.9 {
		t.Errorf("expected temperature 0.9, got %f", result.Temperature)
	}
	if result.MaxTokens != 2048 {
		t.Errorf("expected max_tokens 2048, got %d", result.MaxTokens)
	}
}

func TestPlane_HandleUpdateConfig_TrafficControlFields(t *testing.T) {
	cfg := NewConfig("anthropic-direct")
	plane := NewPlane(cfg, testLogger())
	handler := plane.Handler()

	body := `{"synthesis_enabled": true, "sequencing_enabled": true, "tick_sync_enabled": true, "apicp_enabled": true, "local_loop_enabled": true, "tick_sync_timeout_ms": 1500, "p3_timeout_ms": 4000, "max_forward_concurrency": 5, "intercept_mode": "manual"}`
	req := httptest.NewRequest(http.MethodPatch, "/control/config", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("expected status 200, got %d: %s", rec.Code, rec.Body.String())
	}

	var result ConfigSnapshot
	if err := json.NewDecoder(rec.Body).Decode(&result); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}
	if !result.SynthesisEnabled || !result.SequencingEnabled || !result.TickSyncEnabled || !result.APICPEnabled || !result.LocalLoopEnabled {
		t.Fatalf("expected traffic toggles to be true, got %+v", result)
	}
	if result.TickSyncTimeoutMs != 1500 {
		t.Errorf("expected tick_sync_timeout_ms 1500, got %d", result.TickSyncTimeoutMs)
	}
	if result.P3TimeoutMs != 4000 {
		t.Errorf("expected p3_timeout_ms 4000, got %d", result.P3TimeoutMs)
	}
	if result.MaxForwardConcurrency != 5 {
		t.Errorf("expected max_forward_concurrency 5, got %d", result.MaxForwardConcurrency)
	}
	if result.InterceptMode != "manual" {
		t.Errorf("expected intercept_mode manual, got %q", result.InterceptMode)
	}
}

// TestPlane_HandleUpdateConfig_Invalid tests rejection of invalid updates.
func TestPlane_HandleUpdateConfig_Invalid(t *testing.T) {
	cfg := NewConfig("claude")
	plane := NewPlane(cfg, testLogger())
	handler := plane.Handler()

	body := `{"temperature": 5.0}`
	req := httptest.NewRequest(http.MethodPatch, "/control/config", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusUnprocessableEntity {
		t.Fatalf("expected status 422, got %d: %s", rec.Code, rec.Body.String())
	}
}

// TestPlane_HandleUpdateConfig_InvalidJSON tests rejection of malformed JSON.
func TestPlane_HandleUpdateConfig_InvalidJSON(t *testing.T) {
	cfg := NewConfig("claude")
	plane := NewPlane(cfg, testLogger())
	handler := plane.Handler()

	req := httptest.NewRequest(http.MethodPatch, "/control/config", strings.NewReader("not json"))
	rec := httptest.NewRecorder()
	handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusBadRequest {
		t.Fatalf("expected status 400, got %d", rec.Code)
	}
}

// TestPlane_HandleSwitchProvider tests the POST /control/provider endpoint.
func TestPlane_HandleSwitchProvider(t *testing.T) {
	cfg := NewConfig("claude")
	plane := NewPlane(cfg, testLogger())
	handler := plane.Handler()

	body := `{"provider": "ollama"}`
	req := httptest.NewRequest(http.MethodPost, "/control/provider", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("expected status 200, got %d: %s", rec.Code, rec.Body.String())
	}

	snapshot := cfg.Get()
	if snapshot.PrimaryProvider != "ollama" {
		t.Errorf("expected provider 'ollama', got %q", snapshot.PrimaryProvider)
	}
}

// TestPlane_HandleSwitchProvider_EmptyProvider tests rejection of empty provider.
func TestPlane_HandleSwitchProvider_EmptyProvider(t *testing.T) {
	cfg := NewConfig("claude")
	plane := NewPlane(cfg, testLogger())
	handler := plane.Handler()

	body := `{"provider": ""}`
	req := httptest.NewRequest(http.MethodPost, "/control/provider", strings.NewReader(body))
	rec := httptest.NewRecorder()
	handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusBadRequest {
		t.Fatalf("expected status 400, got %d: %s", rec.Code, rec.Body.String())
	}
}

// TestConfig_ConcurrentUpdate verifies that concurrent updates do not corrupt state.
func TestConfig_ConcurrentUpdate(t *testing.T) {
	cfg := NewConfig("claude")

	var wg sync.WaitGroup
	for i := 0; i < 50; i++ {
		wg.Add(2)
		go func() {
			defer wg.Done()
			_ = cfg.Update(map[string]interface{}{"temperature": 1.0})
		}()
		go func() {
			defer wg.Done()
			_ = cfg.Get()
		}()
	}
	wg.Wait()

	snapshot := cfg.Get()
	if snapshot.Temperature != 1.0 {
		t.Errorf("expected temperature 1.0 after concurrent updates, got %f", snapshot.Temperature)
	}
}

// TestConfig_Defaults_PipelineHardening verifies hardening defaults (#144).
func TestConfig_Defaults_PipelineHardening(t *testing.T) {
	cfg := NewConfig("claude")
	snap := cfg.Get()

	if snap.PersonalityGuardEnabled {
		t.Error("personality_guard_enabled should default to false")
	}
	if snap.DriftThreshold != 0.95 {
		t.Errorf("drift_threshold default: want 0.95, got %f", snap.DriftThreshold)
	}
	if snap.QualityGateEnabled {
		t.Error("quality_gate_enabled should default to false")
	}
	if snap.QualityThreshold != 2 {
		t.Errorf("quality_threshold default: want 2, got %d", snap.QualityThreshold)
	}
	if snap.QualityMaxRegen != 1 {
		t.Errorf("quality_max_regen default: want 1, got %d", snap.QualityMaxRegen)
	}
	if snap.NarrativeNudge != "" {
		t.Errorf("narrative_nudge default: want empty, got %q", snap.NarrativeNudge)
	}
}

// TestPlane_PatchPersonalityGuard tests PATCH + GET roundtrip for personality guard config.
func TestPlane_PatchPersonalityGuard(t *testing.T) {
	cfg := NewConfig("claude")
	plane := NewPlane(cfg, testLogger())
	handler := plane.Handler()

	body := `{"personality_guard_enabled": true, "drift_threshold": 0.5}`
	req := httptest.NewRequest(http.MethodPatch, "/control/config", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("PATCH status: want 200, got %d: %s", rec.Code, rec.Body.String())
	}

	var result ConfigSnapshot
	if err := json.NewDecoder(rec.Body).Decode(&result); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if !result.PersonalityGuardEnabled {
		t.Error("personality_guard_enabled: want true")
	}
	if result.DriftThreshold != 0.5 {
		t.Errorf("drift_threshold: want 0.5, got %f", result.DriftThreshold)
	}
}

// TestPlane_PatchQualityGate tests PATCH + GET roundtrip for quality gate config.
func TestPlane_PatchQualityGate(t *testing.T) {
	cfg := NewConfig("claude")
	plane := NewPlane(cfg, testLogger())
	handler := plane.Handler()

	body := `{"quality_gate_enabled": true, "quality_threshold": 3.0, "quality_max_regen": 2.0}`
	req := httptest.NewRequest(http.MethodPatch, "/control/config", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("PATCH status: want 200, got %d: %s", rec.Code, rec.Body.String())
	}

	var result ConfigSnapshot
	if err := json.NewDecoder(rec.Body).Decode(&result); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if !result.QualityGateEnabled {
		t.Error("quality_gate_enabled: want true")
	}
	if result.QualityThreshold != 3 {
		t.Errorf("quality_threshold: want 3, got %d", result.QualityThreshold)
	}
	if result.QualityMaxRegen != 2 {
		t.Errorf("quality_max_regen: want 2, got %d", result.QualityMaxRegen)
	}
}

// TestPlane_PatchNarrativeNudge tests PATCH + GET roundtrip for narrative nudge.
func TestPlane_PatchNarrativeNudge(t *testing.T) {
	cfg := NewConfig("claude")
	plane := NewPlane(cfg, testLogger())
	handler := plane.Handler()

	body := `{"narrative_nudge": "Fokus heute: Teamwork"}`
	req := httptest.NewRequest(http.MethodPatch, "/control/config", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("PATCH status: want 200, got %d: %s", rec.Code, rec.Body.String())
	}

	var result ConfigSnapshot
	if err := json.NewDecoder(rec.Body).Decode(&result); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if result.NarrativeNudge != "Fokus heute: Teamwork" {
		t.Errorf("narrative_nudge: want %q, got %q", "Fokus heute: Teamwork", result.NarrativeNudge)
	}

	// Clear nudge
	body2 := `{"narrative_nudge": ""}`
	req2 := httptest.NewRequest(http.MethodPatch, "/control/config", strings.NewReader(body2))
	rec2 := httptest.NewRecorder()
	handler.ServeHTTP(rec2, req2)

	if rec2.Code != http.StatusOK {
		t.Fatalf("PATCH clear status: want 200, got %d", rec2.Code)
	}
	var result2 ConfigSnapshot
	_ = json.NewDecoder(rec2.Body).Decode(&result2)
	if result2.NarrativeNudge != "" {
		t.Errorf("narrative_nudge after clear: want empty, got %q", result2.NarrativeNudge)
	}
}

// TestPlane_PatchDriftThreshold_Invalid tests validation for drift_threshold.
func TestPlane_PatchDriftThreshold_Invalid(t *testing.T) {
	cfg := NewConfig("claude")
	plane := NewPlane(cfg, testLogger())
	handler := plane.Handler()

	body := `{"drift_threshold": 1.5}`
	req := httptest.NewRequest(http.MethodPatch, "/control/config", strings.NewReader(body))
	rec := httptest.NewRecorder()
	handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusUnprocessableEntity {
		t.Fatalf("want 422 for invalid drift_threshold, got %d", rec.Code)
	}
}
