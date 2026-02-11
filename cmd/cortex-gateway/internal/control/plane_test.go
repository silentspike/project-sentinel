package control

import (
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
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
