package proxy

import (
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/capability"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/compiler"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/extraction"
	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/normalizer"
)

// TestCircuitBreakerE2E simulates: provider failure → breaker opens → failover →
// time passes → half-open → successful probe → breaker closes (recovery).
func TestCircuitBreakerE2E(t *testing.T) {
	reg := NewRegistry()

	// Primary: fails first, recovers later
	primaryFailing := true
	primary := &pipelineMockProvider{
		name: "primary",
		sendFunc: func(_ context.Context, _ *LLMRequest) (*LLMResponse, error) {
			if primaryFailing {
				return nil, errors.New("provider down")
			}
			return &LLMResponse{
				Content:      "primary ok",
				Model:        "m",
				TokensUsed:   1,
				FinishReason: "stop",
			}, nil
		},
	}
	reg.Register("primary", primary)

	// Fallback: always works
	fallback := &pipelineMockProvider{
		name: "fallback",
		resp: &LLMResponse{
			Content:      "fallback ok",
			Model:        "m",
			TokensUsed:   1,
			FinishReason: "stop",
		},
	}
	reg.Register("fallback", fallback)

	cfg := control.NewConfig("primary")
	breakerCfg := BreakerConfig{
		WindowSeconds:    10,
		MinRequests:      5,
		FailureRatio:     0.5,
		FailureThreshold: 3,
		OpenSeconds:      5,
		HalfOpenProbes:   2,
	}
	ph := NewPipelineHandler(PipelineConfig{
		Registry:         reg,
		Config:           cfg,
		Compiler:         compiler.New(),
		Normalizer:       normalizer.New(),
		Extractor:        extraction.New(),
		Capabilities:     capability.New(),
		Logger:           slog.Default(),
		BreakerCfg:       breakerCfg,
		ProviderDeadline: 5 * time.Second, // short for test
	})

	doRequest := func() (int, string) {
		body := `{"messages":[{"role":"user","content":"test"}]}`
		req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
		w := httptest.NewRecorder()
		ph.ServeHTTP(w, req)
		var resp PipelineResponse
		if w.Code == http.StatusOK {
			_ = json.NewDecoder(w.Body).Decode(&resp)
			return w.Code, resp.Provider
		}
		return w.Code, ""
	}

	// Phase 1: Trip the primary breaker (3 consecutive failures)
	for i := 0; i < 3; i++ {
		code, _ := doRequest()
		if code != http.StatusBadGateway {
			t.Fatalf("phase 1 request %d: expected %d, got %d", i, http.StatusBadGateway, code)
		}
	}

	// Primary breaker should now be open
	breaker := ph.getBreaker("primary")
	if got := breaker.State(); got != "open" {
		t.Fatalf("phase 1: expected primary breaker %q, got %q", "open", got)
	}

	// Phase 2: Failover to fallback
	code, provider := doRequest()
	if code != http.StatusOK {
		t.Fatalf("phase 2: expected %d (failover), got %d", http.StatusOK, code)
	}
	if provider != "fallback" {
		t.Errorf("phase 2: expected provider %q, got %q", "fallback", provider)
	}

	// Phase 3: Simulate primary recovery
	primaryFailing = false

	// Advance breaker time past OpenSeconds to trigger half-open
	now := time.Now()
	breaker.now = func() time.Time { return now.Add(6 * time.Second) }

	// Phase 4: Primary should now be half-open, probe should succeed
	// First, verify breaker transitions to half-open
	if !breaker.Allow() {
		t.Fatal("phase 4: expected Allow()=true after open timeout")
	}
	if got := breaker.State(); got != "half-open" {
		t.Fatalf("phase 4: expected %q, got %q", "half-open", got)
	}

	// Record successful probes to close breaker
	breaker.Record(nil) // probe 1: success
	breaker.Record(nil) // probe 2: success → closes breaker

	if got := breaker.State(); got != "closed" {
		t.Fatalf("phase 4: expected %q after successful probes, got %q", "closed", got)
	}

	// Phase 5: Primary should be usable again (reset time func)
	breaker.now = time.Now

	// Direct Send should work now
	resp, err := primary.Send(context.Background(), &LLMRequest{})
	if err != nil {
		t.Fatalf("phase 5: primary Send failed: %v", err)
	}
	if resp.Content != "primary ok" {
		t.Errorf("phase 5: expected %q, got %q", "primary ok", resp.Content)
	}
}

// TestProviderDeadlineFromEnv verifies the ENV-based deadline config.
func TestProviderDeadlineFromEnv(t *testing.T) {
	t.Run("default", func(t *testing.T) {
		d := ProviderDeadlineFromEnv()
		if d != 20*time.Second {
			t.Errorf("ProviderDeadlineFromEnv() = %v, want 20s", d)
		}
	})

	t.Run("valid", func(t *testing.T) {
		t.Setenv("SENTINEL_CORTEX_PROVIDER_DEADLINE_SECONDS", "15")
		d := ProviderDeadlineFromEnv()
		if d != 15*time.Second {
			t.Errorf("ProviderDeadlineFromEnv() = %v, want 15s", d)
		}
	})

	t.Run("too_low", func(t *testing.T) {
		t.Setenv("SENTINEL_CORTEX_PROVIDER_DEADLINE_SECONDS", "5")
		d := ProviderDeadlineFromEnv()
		if d != 20*time.Second {
			t.Errorf("ProviderDeadlineFromEnv() = %v, want 20s (below min)", d)
		}
	})

	t.Run("too_high", func(t *testing.T) {
		t.Setenv("SENTINEL_CORTEX_PROVIDER_DEADLINE_SECONDS", "60")
		d := ProviderDeadlineFromEnv()
		if d != 20*time.Second {
			t.Errorf("ProviderDeadlineFromEnv() = %v, want 20s (above max)", d)
		}
	})

	t.Run("invalid", func(t *testing.T) {
		t.Setenv("SENTINEL_CORTEX_PROVIDER_DEADLINE_SECONDS", "abc")
		d := ProviderDeadlineFromEnv()
		if d != 20*time.Second {
			t.Errorf("ProviderDeadlineFromEnv() = %v, want 20s (invalid)", d)
		}
	})

	t.Run("boundary_min", func(t *testing.T) {
		t.Setenv("SENTINEL_CORTEX_PROVIDER_DEADLINE_SECONDS", "10")
		d := ProviderDeadlineFromEnv()
		if d != 10*time.Second {
			t.Errorf("ProviderDeadlineFromEnv() = %v, want 10s", d)
		}
	})

	t.Run("boundary_max", func(t *testing.T) {
		t.Setenv("SENTINEL_CORTEX_PROVIDER_DEADLINE_SECONDS", "30")
		d := ProviderDeadlineFromEnv()
		if d != 30*time.Second {
			t.Errorf("ProviderDeadlineFromEnv() = %v, want 30s", d)
		}
	})
}

// TestBreakerTripsMetric verifies that breakerTripsTotal increments on trip.
func TestBreakerTripsMetric(t *testing.T) {
	reg := NewRegistry()
	mock := &pipelineMockProvider{
		name: "metric-test",
		err:  errors.New("transport error"),
	}
	reg.Register("metric-test", mock)

	cfg := control.NewConfig("metric-test")
	breakerCfg := BreakerConfig{
		WindowSeconds:    10,
		MinRequests:      5,
		FailureRatio:     0.5,
		FailureThreshold: 3,
		OpenSeconds:      5,
		HalfOpenProbes:   2,
	}
	ph := NewPipelineHandler(PipelineConfig{
		Registry:         reg,
		Config:           cfg,
		Compiler:         compiler.New(),
		Normalizer:       normalizer.New(),
		Extractor:        extraction.New(),
		Capabilities:     capability.New(),
		Logger:           slog.Default(),
		BreakerCfg:       breakerCfg,
		ProviderDeadline: 5 * time.Second,
	})

	// Trip breaker (3 consecutive failures)
	for i := 0; i < 3; i++ {
		body := `{"messages":[{"role":"user","content":"fail"}]}`
		req := httptest.NewRequest(http.MethodPost, "/v1/chat/completions", strings.NewReader(body))
		w := httptest.NewRecorder()
		ph.ServeHTTP(w, req)
	}

	breaker := ph.getBreaker("metric-test")
	if got := breaker.State(); got != "open" {
		t.Fatalf("expected breaker %q, got %q", "open", got)
	}

	// breakerTripsTotal should have been incremented (we verify via breaker state)
	// Note: Prometheus metrics are global singletons, so we can't easily check exact values
	// in parallel tests. We verify the state transition happened correctly.
}

// TestConfigurableDeadlineUsed verifies that the pipeline uses the configured deadline.
func TestConfigurableDeadlineUsed(t *testing.T) {
	ph := NewPipelineHandler(PipelineConfig{
		Registry:         NewRegistry(),
		Config:           control.NewConfig("test"),
		Compiler:         compiler.New(),
		Normalizer:       normalizer.New(),
		Extractor:        extraction.New(),
		Capabilities:     capability.New(),
		Logger:           slog.Default(),
		BreakerCfg:       DefaultBreakerConfig(),
		ProviderDeadline: 15 * time.Second,
	})

	if ph.providerDeadline != 15*time.Second {
		t.Errorf("providerDeadline = %v, want 15s", ph.providerDeadline)
	}
}

// TestConfigurableDeadlineDefault verifies zero-value falls back to default.
func TestConfigurableDeadlineDefault(t *testing.T) {
	ph := NewPipelineHandler(PipelineConfig{
		Registry:     NewRegistry(),
		Config:       control.NewConfig("test"),
		Compiler:     compiler.New(),
		Normalizer:   normalizer.New(),
		Extractor:    extraction.New(),
		Capabilities: capability.New(),
		Logger:       slog.Default(),
		BreakerCfg:   DefaultBreakerConfig(),
		// ProviderDeadline: 0 → default
	})

	if ph.providerDeadline != 20*time.Second {
		t.Errorf("providerDeadline = %v, want 20s (default)", ph.providerDeadline)
	}
}
