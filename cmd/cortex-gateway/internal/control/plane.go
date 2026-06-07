package control

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strings"
	"sync"
)

// maxConfigBodySize limits incoming config update bodies to 1 MB.
const maxConfigBodySize = 1 * 1024 * 1024

// Validation boundaries for configuration values.
const (
	minTemperature = 0.0
	maxTemperature = 2.0
	minMaxTokens   = 1
	minRateLimit   = 0.0
)

// ConfigSnapshot is a mutex-free copy of Config for serialization and reads.
type ConfigSnapshot struct {
	PrimaryProvider         string            `json:"primary_provider"`
	Temperature             float64           `json:"temperature"`
	MaxTokens               int               `json:"max_tokens"`
	RateLimit               float64           `json:"rate_limit_rps"`
	AgentOverrides          map[string]string `json:"agent_overrides"`
	AgentRuntimeModelPolicy string            `json:"agent_runtime_model_policy"`
	LocalLoopEnabled        bool              `json:"local_loop_enabled"`

	// Traffic Control (#288)
	SynthesisEnabled      bool   `json:"synthesis_enabled"`
	SequencingEnabled     bool   `json:"sequencing_enabled"`
	TickSyncEnabled       bool   `json:"tick_sync_enabled"`
	APICPEnabled          bool   `json:"apicp_enabled"`
	TickSyncTimeoutMs     int    `json:"tick_sync_timeout_ms"`
	P3TimeoutMs           int    `json:"p3_timeout_ms"`
	MaxForwardConcurrency int    `json:"max_forward_concurrency"`
	InterceptMode         string `json:"intercept_mode"`

	// Pipeline Hardening (#144)
	PersonalityGuardEnabled bool    `json:"personality_guard_enabled"`
	DriftThreshold          float64 `json:"drift_threshold"`
	QualityGateEnabled      bool    `json:"quality_gate_enabled"`
	QualityThreshold        int     `json:"quality_threshold"`
	QualityMaxRegen         int     `json:"quality_max_regen"`
	NarrativeNudge          string  `json:"narrative_nudge"`
}

// Config holds the current gateway configuration (mutable at runtime).
type Config struct {
	mu                      sync.RWMutex
	primaryProvider         string
	temperature             float64
	maxTokens               int
	rateLimit               float64
	agentOverrides          map[string]string // agent_id -> provider_name
	agentRuntimeModelPolicy string
	localLoopEnabled        bool

	// Traffic Control (#288)
	synthesisEnabled      bool
	sequencingEnabled     bool
	tickSyncEnabled       bool
	apicpEnabled          bool
	tickSyncTimeoutMs     int
	p3TimeoutMs           int
	maxForwardConcurrency int
	interceptMode         string

	// Pipeline Hardening (#144)
	personalityGuardEnabled bool
	driftThreshold          float64
	qualityGateEnabled      bool
	qualityThreshold        int
	qualityMaxRegen         int
	narrativeNudge          string
}

// NewConfig creates a Config with sensible defaults.
func NewConfig(primaryProvider string) *Config {
	return &Config{
		primaryProvider:         primaryProvider,
		temperature:             0.7,
		maxTokens:               4096,
		rateLimit:               0,
		agentOverrides:          make(map[string]string),
		agentRuntimeModelPolicy: "haiku",
		localLoopEnabled:        strings.EqualFold(strings.TrimSpace(primaryProvider), "local-loop"),

		synthesisEnabled:      false,
		sequencingEnabled:     false,
		tickSyncEnabled:       false,
		apicpEnabled:          false,
		tickSyncTimeoutMs:     2000,
		p3TimeoutMs:           5000,
		maxForwardConcurrency: 3,
		interceptMode:         "auto",

		personalityGuardEnabled: false,
		driftThreshold:          0.95,
		qualityGateEnabled:      false,
		qualityThreshold:        2,
		qualityMaxRegen:         1,
		narrativeNudge:          "",
	}
}

// AgentProvider returns the override provider for a specific agent, if any.
func (c *Config) AgentProvider(agentID string) (string, bool) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	p, ok := c.agentOverrides[agentID]
	return p, ok
}

// SetAgentProvider sets a provider override for a specific agent.
func (c *Config) SetAgentProvider(agentID, provider string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.agentOverrides[agentID] = provider
}

// ClearAgentProvider removes a provider override for a specific agent.
func (c *Config) ClearAgentProvider(agentID string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	delete(c.agentOverrides, agentID)
}

// Get returns a snapshot of the current config (safe for concurrent use).
func (c *Config) Get() ConfigSnapshot {
	c.mu.RLock()
	defer c.mu.RUnlock()
	// Copy agent overrides to prevent external mutation
	overrides := make(map[string]string, len(c.agentOverrides))
	for k, v := range c.agentOverrides {
		overrides[k] = v
	}
	return ConfigSnapshot{
		PrimaryProvider:         c.primaryProvider,
		Temperature:             c.temperature,
		MaxTokens:               c.maxTokens,
		RateLimit:               c.rateLimit,
		AgentOverrides:          overrides,
		AgentRuntimeModelPolicy: c.agentRuntimeModelPolicy,
		LocalLoopEnabled:        c.localLoopEnabled,

		SynthesisEnabled:      c.synthesisEnabled,
		SequencingEnabled:     c.sequencingEnabled,
		TickSyncEnabled:       c.tickSyncEnabled,
		APICPEnabled:          c.apicpEnabled,
		TickSyncTimeoutMs:     c.tickSyncTimeoutMs,
		P3TimeoutMs:           c.p3TimeoutMs,
		MaxForwardConcurrency: c.maxForwardConcurrency,
		InterceptMode:         c.interceptMode,

		PersonalityGuardEnabled: c.personalityGuardEnabled,
		DriftThreshold:          c.driftThreshold,
		QualityGateEnabled:      c.qualityGateEnabled,
		QualityThreshold:        c.qualityThreshold,
		QualityMaxRegen:         c.qualityMaxRegen,
		NarrativeNudge:          c.narrativeNudge,
	}
}

// configUpdater validates and applies a single config field update.
type configUpdater func(c *Config, val interface{}) error

// configUpdaters maps JSON keys to their validation and apply logic.
// Extracted from Update() to keep cyclomatic complexity manageable.
var configUpdaters = map[string]configUpdater{
	"temperature": func(c *Config, val interface{}) error {
		v, ok := toFloat64(val)
		if !ok {
			return fmt.Errorf("temperature must be a number, got %T", val)
		}
		if v < minTemperature || v > maxTemperature {
			return fmt.Errorf("temperature must be between %.1f and %.1f, got %f", minTemperature, maxTemperature, v)
		}
		c.temperature = v
		return nil
	},
	"max_tokens": func(c *Config, val interface{}) error {
		v, ok := toInt(val)
		if !ok {
			return fmt.Errorf("max_tokens must be an integer, got %T", val)
		}
		if v < minMaxTokens {
			return fmt.Errorf("max_tokens must be >= %d, got %d", minMaxTokens, v)
		}
		c.maxTokens = v
		return nil
	},
	"rate_limit_rps": func(c *Config, val interface{}) error {
		v, ok := toFloat64(val)
		if !ok {
			return fmt.Errorf("rate_limit_rps must be a number, got %T", val)
		}
		if v < minRateLimit {
			return fmt.Errorf("rate_limit_rps must be >= %.1f, got %f", minRateLimit, v)
		}
		c.rateLimit = v
		return nil
	},
	"primary_provider": func(c *Config, val interface{}) error {
		v, ok := val.(string)
		if !ok {
			return fmt.Errorf("primary_provider must be a string, got %T", val)
		}
		if v == "" {
			return errors.New("primary_provider must not be empty")
		}
		c.primaryProvider = v
		return nil
	},
	"agent_runtime_model_policy": func(c *Config, val interface{}) error {
		v, ok := val.(string)
		if !ok {
			return fmt.Errorf("agent_runtime_model_policy must be a string, got %T", val)
		}
		v = strings.TrimSpace(v)
		if v != "" && v != "haiku" {
			return fmt.Errorf("agent_runtime_model_policy must be empty or %q, got %q", "haiku", v)
		}
		c.agentRuntimeModelPolicy = v
		return nil
	},
	"local_loop_enabled": func(c *Config, val interface{}) error {
		v, ok := val.(bool)
		if !ok {
			return fmt.Errorf("local_loop_enabled must be a boolean, got %T", val)
		}
		c.localLoopEnabled = v
		return nil
	},
	"synthesis_enabled": func(c *Config, val interface{}) error {
		v, ok := val.(bool)
		if !ok {
			return fmt.Errorf("synthesis_enabled must be a boolean, got %T", val)
		}
		c.synthesisEnabled = v
		return nil
	},
	"sequencing_enabled": func(c *Config, val interface{}) error {
		v, ok := val.(bool)
		if !ok {
			return fmt.Errorf("sequencing_enabled must be a boolean, got %T", val)
		}
		c.sequencingEnabled = v
		return nil
	},
	"tick_sync_enabled": func(c *Config, val interface{}) error {
		v, ok := val.(bool)
		if !ok {
			return fmt.Errorf("tick_sync_enabled must be a boolean, got %T", val)
		}
		c.tickSyncEnabled = v
		return nil
	},
	"apicp_enabled": func(c *Config, val interface{}) error {
		v, ok := val.(bool)
		if !ok {
			return fmt.Errorf("apicp_enabled must be a boolean, got %T", val)
		}
		c.apicpEnabled = v
		return nil
	},
	"tick_sync_timeout_ms": func(c *Config, val interface{}) error {
		v, ok := toInt(val)
		if !ok {
			return fmt.Errorf("tick_sync_timeout_ms must be an integer, got %T", val)
		}
		if v < 1 {
			return fmt.Errorf("tick_sync_timeout_ms must be >= 1, got %d", v)
		}
		c.tickSyncTimeoutMs = v
		return nil
	},
	"p3_timeout_ms": func(c *Config, val interface{}) error {
		v, ok := toInt(val)
		if !ok {
			return fmt.Errorf("p3_timeout_ms must be an integer, got %T", val)
		}
		if v < 1 {
			return fmt.Errorf("p3_timeout_ms must be >= 1, got %d", v)
		}
		c.p3TimeoutMs = v
		return nil
	},
	"max_forward_concurrency": func(c *Config, val interface{}) error {
		v, ok := toInt(val)
		if !ok {
			return fmt.Errorf("max_forward_concurrency must be an integer, got %T", val)
		}
		if v < 1 {
			return fmt.Errorf("max_forward_concurrency must be >= 1, got %d", v)
		}
		c.maxForwardConcurrency = v
		return nil
	},
	"intercept_mode": func(c *Config, val interface{}) error {
		v, ok := val.(string)
		if !ok {
			return fmt.Errorf("intercept_mode must be a string, got %T", val)
		}
		if v == "" {
			return errors.New("intercept_mode must not be empty")
		}
		c.interceptMode = v
		return nil
	},
	"personality_guard_enabled": func(c *Config, val interface{}) error {
		v, ok := val.(bool)
		if !ok {
			return fmt.Errorf("personality_guard_enabled must be a boolean, got %T", val)
		}
		c.personalityGuardEnabled = v
		return nil
	},
	"drift_threshold": func(c *Config, val interface{}) error {
		v, ok := toFloat64(val)
		if !ok {
			return fmt.Errorf("drift_threshold must be a number, got %T", val)
		}
		if v < 0.0 || v > 1.0 {
			return fmt.Errorf("drift_threshold must be between 0.0 and 1.0, got %f", v)
		}
		c.driftThreshold = v
		return nil
	},
	"quality_gate_enabled": func(c *Config, val interface{}) error {
		v, ok := val.(bool)
		if !ok {
			return fmt.Errorf("quality_gate_enabled must be a boolean, got %T", val)
		}
		c.qualityGateEnabled = v
		return nil
	},
	"quality_threshold": func(c *Config, val interface{}) error {
		v, ok := toInt(val)
		if !ok {
			return fmt.Errorf("quality_threshold must be an integer, got %T", val)
		}
		if v < 1 || v > 5 {
			return fmt.Errorf("quality_threshold must be between 1 and 5, got %d", v)
		}
		c.qualityThreshold = v
		return nil
	},
	"quality_max_regen": func(c *Config, val interface{}) error {
		v, ok := toInt(val)
		if !ok {
			return fmt.Errorf("quality_max_regen must be an integer, got %T", val)
		}
		if v < 0 || v > 3 {
			return fmt.Errorf("quality_max_regen must be between 0 and 3, got %d", v)
		}
		c.qualityMaxRegen = v
		return nil
	},
	"narrative_nudge": func(c *Config, val interface{}) error {
		v, ok := val.(string)
		if !ok {
			return fmt.Errorf("narrative_nudge must be a string, got %T", val)
		}
		c.narrativeNudge = v
		return nil
	},
}

// Update applies partial updates from a map to the config.
// Returns an error if any value fails validation.
func (c *Config) Update(updates map[string]interface{}) error {
	c.mu.Lock()
	defer c.mu.Unlock()

	for key, val := range updates {
		fn, ok := configUpdaters[key]
		if !ok {
			return fmt.Errorf("unknown config key: %q", key)
		}
		if err := fn(c, val); err != nil {
			return err
		}
	}
	return nil
}

// toFloat64 converts a JSON number (which json.Unmarshal decodes as float64) to float64.
func toFloat64(v interface{}) (float64, bool) {
	switch n := v.(type) {
	case float64:
		return n, true
	case int:
		return float64(n), true
	default:
		return 0, false
	}
}

// toInt converts a JSON number to int.
func toInt(v interface{}) (int, bool) {
	switch n := v.(type) {
	case float64:
		return int(n), true
	case int:
		return n, true
	default:
		return 0, false
	}
}

// Plane is the HTTP handler for control plane endpoints.
type Plane struct {
	config *Config
	logger *slog.Logger
}

// NewPlane creates a new control plane.
func NewPlane(config *Config, logger *slog.Logger) *Plane {
	return &Plane{config: config, logger: logger}
}

// Handler returns an http.ServeMux with control plane routes.
func (p *Plane) Handler() *http.ServeMux {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /health", p.handleHealth)
	mux.HandleFunc("GET /ready", p.handleReady)
	mux.HandleFunc("GET /control/config", p.handleGetConfig)
	mux.HandleFunc("PATCH /control/config", p.handleUpdateConfig)
	mux.HandleFunc("POST /control/provider", p.handleSwitchProvider)
	mux.HandleFunc("POST /control/agent-provider", p.handleSetAgentProvider)
	mux.HandleFunc("DELETE /control/agent-provider", p.handleClearAgentProvider)
	return mux
}

// handleHealth returns liveness status.
func (p *Plane) handleHealth(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	_, _ = w.Write([]byte(`{"status":"ok"}`))
}

// handleReady returns readiness status (config loaded).
func (p *Plane) handleReady(w http.ResponseWriter, _ *http.Request) {
	snapshot := p.config.Get()
	if snapshot.PrimaryProvider == "" {
		w.WriteHeader(http.StatusServiceUnavailable)
		_, _ = w.Write([]byte(`{"status":"not_ready","reason":"no primary provider configured"}`))
		return
	}
	w.Header().Set("Content-Type", "application/json")
	_, _ = w.Write([]byte(`{"status":"ok"}`))
}

// handleGetConfig returns the current config as JSON.
func (p *Plane) handleGetConfig(w http.ResponseWriter, _ *http.Request) {
	snapshot := p.config.Get()
	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(snapshot); err != nil {
		p.logger.Error("failed to encode config response", "error", err)
	}
}

// handleUpdateConfig applies partial config updates from a JSON body.
func (p *Plane) handleUpdateConfig(w http.ResponseWriter, r *http.Request) {
	defer func() { _ = r.Body.Close() }()

	limited := io.LimitReader(r.Body, maxConfigBodySize+1)
	body, err := io.ReadAll(limited)
	if err != nil {
		http.Error(w, "failed to read request body", http.StatusBadRequest)
		return
	}
	if len(body) > maxConfigBodySize {
		http.Error(w, "request body too large", http.StatusRequestEntityTooLarge)
		return
	}

	var updates map[string]interface{}
	if err := json.Unmarshal(body, &updates); err != nil {
		http.Error(w, "invalid JSON body", http.StatusBadRequest)
		return
	}

	if err := p.config.Update(updates); err != nil {
		p.logger.Warn("config update rejected", "error", err)
		http.Error(w, err.Error(), http.StatusUnprocessableEntity)
		return
	}

	p.logger.Info("config updated", "updates", updates)
	snapshot := p.config.Get()
	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(snapshot); err != nil {
		p.logger.Error("failed to encode config response", "error", err)
	}
}

// switchProviderRequest is the JSON body for POST /control/provider.
type switchProviderRequest struct {
	Provider string `json:"provider"`
}

// handleSwitchProvider changes the primary provider.
func (p *Plane) handleSwitchProvider(w http.ResponseWriter, r *http.Request) {
	defer func() { _ = r.Body.Close() }()

	limited := io.LimitReader(r.Body, maxConfigBodySize+1)
	body, err := io.ReadAll(limited)
	if err != nil {
		http.Error(w, "failed to read request body", http.StatusBadRequest)
		return
	}
	if len(body) > maxConfigBodySize {
		http.Error(w, "request body too large", http.StatusRequestEntityTooLarge)
		return
	}

	var req switchProviderRequest
	if err := json.Unmarshal(body, &req); err != nil {
		http.Error(w, "invalid JSON body", http.StatusBadRequest)
		return
	}

	if req.Provider == "" {
		http.Error(w, "provider field is required", http.StatusBadRequest)
		return
	}

	err = p.config.Update(map[string]interface{}{"primary_provider": req.Provider})
	if err != nil {
		http.Error(w, err.Error(), http.StatusUnprocessableEntity)
		return
	}

	p.logger.Info("primary provider switched", "provider", req.Provider)
	w.Header().Set("Content-Type", "application/json")
	_, _ = fmt.Fprintf(w, `{"primary_provider":%q}`, req.Provider)
}

// agentProviderRequest is the JSON body for POST/DELETE /control/agent-provider.
type agentProviderRequest struct {
	AgentID  string `json:"agent_id"`
	Provider string `json:"provider"` // only needed for POST
}

// handleSetAgentProvider sets a per-agent provider override.
func (p *Plane) handleSetAgentProvider(w http.ResponseWriter, r *http.Request) {
	defer func() { _ = r.Body.Close() }()

	limited := io.LimitReader(r.Body, maxConfigBodySize+1)
	body, err := io.ReadAll(limited)
	if err != nil {
		http.Error(w, "failed to read request body", http.StatusBadRequest)
		return
	}
	if len(body) > maxConfigBodySize {
		http.Error(w, "request body too large", http.StatusRequestEntityTooLarge)
		return
	}

	var req agentProviderRequest
	if err := json.Unmarshal(body, &req); err != nil {
		http.Error(w, "invalid JSON body", http.StatusBadRequest)
		return
	}

	if req.AgentID == "" {
		http.Error(w, "agent_id field is required", http.StatusBadRequest)
		return
	}
	if req.Provider == "" {
		http.Error(w, "provider field is required", http.StatusBadRequest)
		return
	}

	p.config.SetAgentProvider(req.AgentID, req.Provider)
	p.logger.Info("agent provider override set", "agent_id", req.AgentID, "provider", req.Provider)

	w.Header().Set("Content-Type", "application/json")
	_, _ = fmt.Fprintf(w, `{"agent_id":%q,"provider":%q}`, req.AgentID, req.Provider)
}

// handleClearAgentProvider removes a per-agent provider override.
func (p *Plane) handleClearAgentProvider(w http.ResponseWriter, r *http.Request) {
	defer func() { _ = r.Body.Close() }()

	limited := io.LimitReader(r.Body, maxConfigBodySize+1)
	body, err := io.ReadAll(limited)
	if err != nil {
		http.Error(w, "failed to read request body", http.StatusBadRequest)
		return
	}
	if len(body) > maxConfigBodySize {
		http.Error(w, "request body too large", http.StatusRequestEntityTooLarge)
		return
	}

	var req agentProviderRequest
	if err := json.Unmarshal(body, &req); err != nil {
		http.Error(w, "invalid JSON body", http.StatusBadRequest)
		return
	}

	if req.AgentID == "" {
		http.Error(w, "agent_id field is required", http.StatusBadRequest)
		return
	}

	p.config.ClearAgentProvider(req.AgentID)
	p.logger.Info("agent provider override cleared", "agent_id", req.AgentID)

	w.Header().Set("Content-Type", "application/json")
	_, _ = fmt.Fprintf(w, `{"agent_id":%q,"provider":null}`, req.AgentID)
}
