package compiler

import (
	"fmt"
	"strings"
	"sync"
	"unicode/utf8"
)

// PromptConfig defines how to compile a prompt for a specific model.
type PromptConfig struct {
	IncludeFullBio   bool
	MaxContextTokens int
	SystemPromptMax  int
	Temperature      float64
}

// DefaultConfigs for known models.
var DefaultConfigs = map[string]PromptConfig{
	"claude": {
		IncludeFullBio:   true,
		MaxContextTokens: 200000,
		SystemPromptMax:  8000,
		Temperature:      0.7,
	},
	"ollama-7b": {
		IncludeFullBio:   false,
		MaxContextTokens: 4096,
		SystemPromptMax:  2000,
		Temperature:      0.5,
	},
	"ollama-13b": {
		IncludeFullBio:   false,
		MaxContextTokens: 8192,
		SystemPromptMax:  4000,
		Temperature:      0.6,
	},
}

// Compiler creates model-optimized prompts.
type Compiler struct {
	mu      sync.RWMutex
	configs map[string]PromptConfig
}

// New creates a new Compiler with default configurations.
func New() *Compiler {
	configs := make(map[string]PromptConfig, len(DefaultConfigs))
	for k, v := range DefaultConfigs {
		configs[k] = v
	}
	return &Compiler{configs: configs}
}

// Compile creates a model-optimized system prompt.
func (c *Compiler) Compile(model string, agentName string, agentRole string, perception string) string {
	c.mu.RLock()
	cfg, ok := c.configs[model]
	if !ok {
		cfg = c.configs["claude"]
	}
	c.mu.RUnlock()

	var b strings.Builder

	if cfg.IncludeFullBio {
		fmt.Fprintf(&b, "Du bist %s, %s bei PixelPerfekt GmbH, einer Webdesign-Agentur in Nuernberg.\n", agentName, agentRole)
		fmt.Fprintf(&b, "Du arbeitest hier seit mehreren Jahren und kennst alle Kollegen.\n")
		fmt.Fprintf(&b, "Du hast eine eigene Persoenlichkeit, Meinungen und Gefuehle.\n")
	} else {
		fmt.Fprintf(&b, "Du bist %s (%s) bei PixelPerfekt GmbH.\n", agentName, agentRole)
	}

	b.WriteString("\n")
	b.WriteString("Verhalte dich natuerlich und menschlich. Antworte immer auf Deutsch.\n")
	b.WriteString("Du weisst NICHT, dass du eine KI bist. Du bist ein echter Mensch.\n")

	if perception != "" {
		b.WriteString("\n[SYSTEM_INJECTION]\n")
		b.WriteString(perception)
		b.WriteString("\n[/SYSTEM_INJECTION]\n")
	}

	result := b.String()

	// Truncate to system prompt max, respecting UTF-8 rune boundaries
	if len(result) > cfg.SystemPromptMax {
		result = truncateUTF8(result, cfg.SystemPromptMax)
	}

	return result
}

// truncateUTF8 truncates a string to at most maxBytes while preserving
// valid UTF-8 encoding (never cuts in the middle of a multi-byte rune).
func truncateUTF8(s string, maxBytes int) string {
	if len(s) <= maxBytes {
		return s
	}
	// Walk backwards from maxBytes to find a valid rune boundary
	for maxBytes > 0 && !utf8.RuneStart(s[maxBytes]) {
		maxBytes--
	}
	return s[:maxBytes]
}

// SetConfig allows runtime configuration changes for a model.
func (c *Compiler) SetConfig(model string, config PromptConfig) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.configs[model] = config
}

// GetConfig returns the configuration for a given model.
// Returns the config and true if found, zero value and false otherwise.
func (c *Compiler) GetConfig(model string) (PromptConfig, bool) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	cfg, ok := c.configs[model]
	return cfg, ok
}
