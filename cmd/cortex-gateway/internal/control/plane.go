package control

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
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
	PrimaryProvider string            `json:"primary_provider"`
	Temperature     float64           `json:"temperature"`
	MaxTokens       int               `json:"max_tokens"`
	RateLimit       float64           `json:"rate_limit_rps"`
	AgentOverrides  map[string]string `json:"agent_overrides"`
}

// Config holds the current gateway configuration (mutable at runtime).
type Config struct {
	mu              sync.RWMutex
	primaryProvider string
	temperature     float64
	maxTokens       int
	rateLimit       float64
	agentOverrides  map[string]string // agent_id -> provider_name
}

// NewConfig creates a Config with sensible defaults.
func NewConfig(primaryProvider string) *Config {
	return &Config{
		primaryProvider: primaryProvider,
		temperature:     0.7,
		maxTokens:       4096,
		rateLimit:       0,
		agentOverrides:  make(map[string]string),
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
		PrimaryProvider: c.primaryProvider,
		Temperature:     c.temperature,
		MaxTokens:       c.maxTokens,
		RateLimit:       c.rateLimit,
		AgentOverrides:  overrides,
	}
}

// Update applies partial updates from a map to the config.
// Returns an error if any value fails validation.
func (c *Config) Update(updates map[string]interface{}) error {
	c.mu.Lock()
	defer c.mu.Unlock()

	for key, val := range updates {
		switch key {
		case "temperature":
			v, ok := toFloat64(val)
			if !ok {
				return fmt.Errorf("temperature must be a number, got %T", val)
			}
			if v < minTemperature || v > maxTemperature {
				return fmt.Errorf("temperature must be between %.1f and %.1f, got %f", minTemperature, maxTemperature, v)
			}
			c.temperature = v

		case "max_tokens":
			v, ok := toInt(val)
			if !ok {
				return fmt.Errorf("max_tokens must be an integer, got %T", val)
			}
			if v < minMaxTokens {
				return fmt.Errorf("max_tokens must be >= %d, got %d", minMaxTokens, v)
			}
			c.maxTokens = v

		case "rate_limit_rps":
			v, ok := toFloat64(val)
			if !ok {
				return fmt.Errorf("rate_limit_rps must be a number, got %T", val)
			}
			if v < minRateLimit {
				return fmt.Errorf("rate_limit_rps must be >= %.1f, got %f", minRateLimit, v)
			}
			c.rateLimit = v

		case "primary_provider":
			v, ok := val.(string)
			if !ok {
				return fmt.Errorf("primary_provider must be a string, got %T", val)
			}
			if v == "" {
				return errors.New("primary_provider must not be empty")
			}
			c.primaryProvider = v

		default:
			return fmt.Errorf("unknown config key: %q", key)
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
