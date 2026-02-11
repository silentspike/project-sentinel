package compiler

import (
	"strings"
	"testing"
	"unicode/utf8"
)

func TestCompile_Claude(t *testing.T) {
	c := New()
	result := c.Compile("claude", "Thomas Mueller", "CEO", "Es ist 10:00 Uhr morgens.")

	if !strings.Contains(result, "Thomas Mueller") {
		t.Error("claude prompt should contain agent name")
	}
	if !strings.Contains(result, "CEO") {
		t.Error("claude prompt should contain agent role")
	}
	// Full bio includes extended description
	if !strings.Contains(result, "eigene Persoenlichkeit") {
		t.Error("claude prompt should include full bio")
	}
	if !strings.Contains(result, "[SYSTEM_INJECTION]") {
		t.Error("claude prompt should contain perception injection")
	}
	if !strings.Contains(result, "10:00 Uhr") {
		t.Error("claude prompt should contain perception content")
	}
}

func TestCompile_Ollama7B(t *testing.T) {
	c := New()
	result := c.Compile("ollama-7b", "Thomas Mueller", "CEO", "Es ist 10:00 Uhr.")

	if !strings.Contains(result, "Thomas Mueller") {
		t.Error("ollama-7b prompt should contain agent name")
	}
	// Shortened bio for small models
	if strings.Contains(result, "eigene Persoenlichkeit") {
		t.Error("ollama-7b prompt should NOT include full bio")
	}
	// Should still contain perception
	if !strings.Contains(result, "[SYSTEM_INJECTION]") {
		t.Error("ollama-7b prompt should contain perception injection")
	}
}

func TestCompile_Ollama7B_Shorter(t *testing.T) {
	c := New()
	claudeResult := c.Compile("claude", "Thomas", "Dev", "test")
	ollamaResult := c.Compile("ollama-7b", "Thomas", "Dev", "test")

	if len(ollamaResult) >= len(claudeResult) {
		t.Errorf("ollama-7b prompt (%d chars) should be shorter than claude prompt (%d chars)",
			len(ollamaResult), len(claudeResult))
	}
}

func TestCompile_UnknownModel(t *testing.T) {
	c := New()
	result := c.Compile("gpt-4", "Thomas Mueller", "CEO", "perception text")

	// Should fall back to claude config (full bio)
	if !strings.Contains(result, "eigene Persoenlichkeit") {
		t.Error("unknown model should fall back to claude config with full bio")
	}
}

func TestCompile_ContainsAgentIdentity(t *testing.T) {
	c := New()
	result := c.Compile("claude", "Lisa Bergmann", "Lead Designerin", "")

	if !strings.Contains(result, "Lisa Bergmann") {
		t.Error("prompt should contain agent name")
	}
	if !strings.Contains(result, "Lead Designerin") {
		t.Error("prompt should contain agent role")
	}
	if !strings.Contains(result, "PixelPerfekt GmbH") {
		t.Error("prompt should contain company name")
	}
}

func TestCompile_ContainsPerception(t *testing.T) {
	c := New()
	perception := "Du stehst in der Kueche. Es riecht nach Kaffee. Thomas ist auch hier."
	result := c.Compile("claude", "Lisa", "Designerin", perception)

	if !strings.Contains(result, perception) {
		t.Error("prompt should contain the full perception text")
	}
	if !strings.Contains(result, "[SYSTEM_INJECTION]") {
		t.Error("prompt should wrap perception in SYSTEM_INJECTION tags")
	}
	if !strings.Contains(result, "[/SYSTEM_INJECTION]") {
		t.Error("prompt should have closing SYSTEM_INJECTION tag")
	}
}

func TestCompile_EmptyPerception(t *testing.T) {
	c := New()
	result := c.Compile("claude", "Thomas", "CEO", "")

	if strings.Contains(result, "[SYSTEM_INJECTION]") {
		t.Error("prompt should NOT contain SYSTEM_INJECTION block when perception is empty")
	}
}

func TestCompile_HumanIdentity(t *testing.T) {
	c := New()
	result := c.Compile("claude", "Thomas", "CEO", "")

	if !strings.Contains(result, "Du weisst NICHT, dass du eine KI bist") {
		t.Error("prompt should instruct agent to behave as human")
	}
}

func TestSetConfig(t *testing.T) {
	c := New()
	custom := PromptConfig{
		IncludeFullBio:   false,
		MaxContextTokens: 32000,
		SystemPromptMax:  6000,
		Temperature:      0.8,
	}
	c.SetConfig("custom-model", custom)

	cfg, ok := c.GetConfig("custom-model")
	if !ok {
		t.Fatal("custom config should be retrievable")
	}
	if cfg.MaxContextTokens != 32000 {
		t.Errorf("MaxContextTokens = %d, want %d", cfg.MaxContextTokens, 32000)
	}
	if cfg.Temperature != 0.8 {
		t.Errorf("Temperature = %f, want %f", cfg.Temperature, 0.8)
	}
}

func TestCompile_Truncation(t *testing.T) {
	c := New()
	c.SetConfig("tiny", PromptConfig{
		IncludeFullBio:   true,
		MaxContextTokens: 1000,
		SystemPromptMax:  50,
		Temperature:      0.5,
	})

	result := c.Compile("tiny", "Thomas Mueller", "CEO", "Very long perception text here")

	if len(result) > 50 {
		t.Errorf("prompt length = %d, should be truncated to 50", len(result))
	}
}

func TestCompile_TruncationUTF8Safe(t *testing.T) {
	c := New()
	// German umlauts are multi-byte in UTF-8 (2 bytes each)
	c.SetConfig("utf8-test", PromptConfig{
		IncludeFullBio:   true,
		MaxContextTokens: 1000,
		SystemPromptMax:  60,
		Temperature:      0.5,
	})

	result := c.Compile("utf8-test", "Thomas", "CEO", "")

	if !utf8.ValidString(result) {
		t.Error("truncated prompt must be valid UTF-8")
	}
	if len(result) > 60 {
		t.Errorf("prompt length = %d bytes, should be <= 60", len(result))
	}
}

func TestCompile_ConcurrentAccess(t *testing.T) {
	c := New()
	done := make(chan struct{})

	// Concurrent reads
	for i := 0; i < 10; i++ {
		go func() {
			defer func() { done <- struct{}{} }()
			_ = c.Compile("claude", "Thomas", "CEO", "test")
		}()
	}

	// Concurrent writes
	for i := 0; i < 10; i++ {
		go func() {
			defer func() { done <- struct{}{} }()
			c.SetConfig("dynamic", PromptConfig{
				IncludeFullBio:   false,
				MaxContextTokens: 4096,
				SystemPromptMax:  2000,
				Temperature:      0.5,
			})
		}()
	}

	for i := 0; i < 20; i++ {
		<-done
	}
}
