package proxy

import (
	"context"
	"fmt"
	"sync"
)

// LLMRequest represents a request to an LLM provider.
type LLMRequest struct {
	Messages    []Message `json:"messages"`
	Temperature float64   `json:"temperature"`
	MaxTokens   int       `json:"max_tokens"`
	Model       string    `json:"model,omitempty"`
}

// Message represents a single message in an LLM conversation.
type Message struct {
	Role    string `json:"role"`
	Content string `json:"content"`
}

// LLMResponse represents a response from an LLM provider.
type LLMResponse struct {
	Content      string `json:"content"`
	Model        string `json:"model"`
	TokensUsed   int    `json:"tokens_used"`
	FinishReason string `json:"finish_reason"`
}

// Provider interface for LLM backends.
type Provider interface {
	Name() string
	Send(ctx context.Context, req *LLMRequest) (*LLMResponse, error)
	HealthCheck(ctx context.Context) error
}

// ProviderConfig holds configuration for a provider.
type ProviderConfig struct {
	Name      string `toml:"name"`
	Type      string `toml:"type"`      // "claude" or "ollama"
	BaseURL   string `toml:"base_url"`
	APIKey    string `toml:"api_key"`   // From env var, not config file
	Model     string `toml:"model"`
	MaxTokens int    `toml:"max_tokens"`
	Priority  int    `toml:"priority"`  // Lower = higher priority
}

// Registry manages available LLM providers.
type Registry struct {
	mu        sync.RWMutex
	providers map[string]Provider
	primary   string // Name of the primary provider
}

// NewRegistry creates a new provider registry.
func NewRegistry() *Registry {
	return &Registry{
		providers: make(map[string]Provider),
	}
}

// Register adds a provider to the registry.
func (r *Registry) Register(name string, provider Provider) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.providers[name] = provider
	if r.primary == "" {
		r.primary = name
	}
}

// Get returns a provider by name.
func (r *Registry) Get(name string) (Provider, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	p, ok := r.providers[name]
	return p, ok
}

// Primary returns the primary provider.
func (r *Registry) Primary() (Provider, error) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	if r.primary == "" {
		return nil, fmt.Errorf("no primary provider configured")
	}
	p, ok := r.providers[r.primary]
	if !ok {
		return nil, fmt.Errorf("primary provider %q not found", r.primary)
	}
	return p, nil
}

// SetPrimary changes the primary provider.
func (r *Registry) SetPrimary(name string) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if _, ok := r.providers[name]; !ok {
		return fmt.Errorf("provider %q not registered", name)
	}
	r.primary = name
	return nil
}

// List returns all registered provider names.
func (r *Registry) List() []string {
	r.mu.RLock()
	defer r.mu.RUnlock()
	names := make([]string, 0, len(r.providers))
	for name := range r.providers {
		names = append(names, name)
	}
	return names
}

// NewProviderFromConfig creates a provider from configuration.
func NewProviderFromConfig(cfg ProviderConfig) (Provider, error) {
	switch cfg.Type {
	case "claude":
		return NewClaudeProvider(cfg), nil
	case "ollama":
		return NewOllamaProvider(cfg), nil
	default:
		return nil, fmt.Errorf("unknown provider type: %q", cfg.Type)
	}
}
