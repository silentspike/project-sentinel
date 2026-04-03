package proxy

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"sync"
	"time"
)

type RequestFormat string

const (
	RequestFormatInternal  RequestFormat = "internal"
	RequestFormatAnthropic RequestFormat = "anthropic"
)

// LLMRequest represents a request to an LLM provider.
type LLMRequest struct {
	Messages           []Message         `json:"messages"`
	SystemBlocks       []SystemBlock     `json:"system,omitempty"`
	Temperature        float64           `json:"temperature"`
	MaxTokens          int               `json:"max_tokens"`
	Stream             bool              `json:"stream,omitempty"`
	Model              string            `json:"model,omitempty"`
	Metadata           map[string]string `json:"metadata,omitempty"`
	Format             RequestFormat     `json:"-"`
	PreferredProvider  string            `json:"-"`
	PassthroughHeaders map[string]string `json:"-"`
	// ProviderTimeout applies only to the real provider execution, not queue wait.
	ProviderTimeout time.Duration `json:"-"`
}

// Message represents a single message in an LLM conversation.
type Message struct {
	Role          string            `json:"role"`
	Content       string            `json:"content"`
	ContentBlocks []json.RawMessage `json:"-"`
}

// CacheControl annotates provider-side content caching hints.
type CacheControl struct {
	Type string `json:"type"`
	TTL  string `json:"ttl,omitempty"`
}

// SystemBlock is a structured system-level content block.
type SystemBlock struct {
	Type         string        `json:"type"`
	Text         string        `json:"text"`
	CacheControl *CacheControl `json:"cache_control,omitempty"`
}

// LLMResponse represents a response from an LLM provider.
type LLMResponse struct {
	Content       string            `json:"content"`
	ContentBlocks []json.RawMessage `json:"-"`
	Model         string            `json:"model"`
	TokensUsed    int               `json:"tokens_used"`
	InputTokens   int               `json:"input_tokens"`
	OutputTokens  int               `json:"output_tokens"`
	FinishReason  string            `json:"finish_reason"`
}

// Provider interface for LLM backends.
type Provider interface {
	Name() string
	Send(ctx context.Context, req *LLMRequest) (*LLMResponse, error)
	HealthCheck(ctx context.Context) error
}

// StreamingProvider is an optional provider capability for relaying HTTP
// streaming responses such as Anthropic SSE.
type StreamingProvider interface {
	StreamHTTP(ctx context.Context, req *LLMRequest, w http.ResponseWriter) error
}

// ProviderStatusReporter is an optional provider capability for exposing an
// already-known runtime status without spawning a new upstream call.
type ProviderStatusReporter interface {
	CurrentProviderError() error
}

// ProviderConfig holds configuration for a provider.
type ProviderConfig struct {
	Name      string `toml:"name"`
	Type      string `toml:"type"` // "claude", "anthropic-direct", "ollama", or "claude-code"
	BaseURL   string `toml:"base_url"`
	APIKey    string `toml:"api_key"` //nolint:gosec // field name, not a credential
	Model     string `toml:"model"`
	MaxTokens int    `toml:"max_tokens"`
	Priority  int    `toml:"priority"` // Lower = higher priority
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
	case "anthropic-direct":
		return NewAnthropicDirectProvider(cfg), nil
	case "ollama":
		return NewOllamaProvider(cfg), nil
	case "claude-code":
		return NewClaudeCodeProvider(cfg, nil), nil
	default:
		return nil, fmt.Errorf("unknown provider type: %q", cfg.Type)
	}
}

func cloneRawMessages(in []json.RawMessage) []json.RawMessage {
	if len(in) == 0 {
		return nil
	}
	out := make([]json.RawMessage, len(in))
	for i, block := range in {
		if block == nil {
			continue
		}
		out[i] = append(json.RawMessage(nil), block...)
	}
	return out
}
