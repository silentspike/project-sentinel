package normalizer

import (
	"testing"
)

func TestNormalizeClaude(t *testing.T) {
	raw := []byte(`{
		"content": [
			{"type": "text", "text": "Hallo, ich bin Thomas."},
			{"type": "text", "text": " Wie kann ich helfen?"}
		],
		"model": "claude-sonnet-4-5-20250929",
		"role": "assistant",
		"stop_reason": "end_turn",
		"usage": {"input_tokens": 100, "output_tokens": 25}
	}`)

	n := New()
	resp, err := n.NormalizeClaude(raw)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if resp.Content != "Hallo, ich bin Thomas. Wie kann ich helfen?" {
		t.Errorf("content = %q, want %q", resp.Content, "Hallo, ich bin Thomas. Wie kann ich helfen?")
	}
	if resp.Role != "assistant" {
		t.Errorf("role = %q, want %q", resp.Role, "assistant")
	}
	if resp.Model != "claude-sonnet-4-5-20250929" {
		t.Errorf("model = %q, want %q", resp.Model, "claude-sonnet-4-5-20250929")
	}
	if resp.Provider != "claude" {
		t.Errorf("provider = %q, want %q", resp.Provider, "claude")
	}
	if resp.TokensUsed != 125 {
		t.Errorf("tokens_used = %d, want %d", resp.TokensUsed, 125)
	}
	if resp.FinishReason != "end_turn" {
		t.Errorf("finish_reason = %q, want %q", resp.FinishReason, "end_turn")
	}
}

func TestNormalizeClaude_DefaultRole(t *testing.T) {
	raw := []byte(`{
		"content": [{"type": "text", "text": "Test"}],
		"model": "claude-sonnet-4-5-20250929",
		"stop_reason": "end_turn",
		"usage": {"input_tokens": 10, "output_tokens": 5}
	}`)

	n := New()
	resp, err := n.NormalizeClaude(raw)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if resp.Role != "assistant" {
		t.Errorf("role = %q, want %q (default)", resp.Role, "assistant")
	}
}

func TestNormalizeOllama(t *testing.T) {
	raw := []byte(`{
		"model": "qwen3:7b",
		"message": {"role": "assistant", "content": "Guten Tag, ich bin hier um zu helfen."},
		"done": true,
		"total_duration": 5000000000,
		"eval_count": 42
	}`)

	n := New()
	resp, err := n.NormalizeOllama(raw)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if resp.Content != "Guten Tag, ich bin hier um zu helfen." {
		t.Errorf("content = %q, want %q", resp.Content, "Guten Tag, ich bin hier um zu helfen.")
	}
	if resp.Role != "assistant" {
		t.Errorf("role = %q, want %q", resp.Role, "assistant")
	}
	if resp.Model != "qwen3:7b" {
		t.Errorf("model = %q, want %q", resp.Model, "qwen3:7b")
	}
	if resp.Provider != "ollama" {
		t.Errorf("provider = %q, want %q", resp.Provider, "ollama")
	}
	if resp.TokensUsed != 42 {
		t.Errorf("tokens_used = %d, want %d", resp.TokensUsed, 42)
	}
	if resp.FinishReason != "stop" {
		t.Errorf("finish_reason = %q, want %q", resp.FinishReason, "stop")
	}
}

func TestNormalizeOllama_NotDone(t *testing.T) {
	raw := []byte(`{
		"model": "qwen3:7b",
		"message": {"role": "assistant", "content": "partial"},
		"done": false,
		"eval_count": 10
	}`)

	n := New()
	resp, err := n.NormalizeOllama(raw)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if resp.FinishReason != "" {
		t.Errorf("finish_reason = %q, want empty for incomplete response", resp.FinishReason)
	}
}

func TestNormalizeOllama_EmptyContent(t *testing.T) {
	raw := []byte(`{
		"model": "qwen3:7b",
		"message": {"role": "assistant", "content": ""},
		"done": true,
		"eval_count": 0
	}`)

	n := New()
	resp, err := n.NormalizeOllama(raw)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if resp.Content != "" {
		t.Errorf("content = %q, want empty", resp.Content)
	}
}

func TestNormalize_UnknownProvider(t *testing.T) {
	n := New()
	_, err := n.Normalize([]byte(`{}`), "openai")
	if err == nil {
		t.Fatal("expected error for unknown provider, got nil")
	}

	want := "unknown provider: openai"
	if err.Error() != want {
		t.Errorf("error = %q, want %q", err.Error(), want)
	}
}

func TestNormalizeClaude_MalformedJSON(t *testing.T) {
	n := New()
	_, err := n.NormalizeClaude([]byte(`{invalid json`))
	if err == nil {
		t.Fatal("expected error for malformed JSON, got nil")
	}
}

func TestNormalizeOllama_MalformedJSON(t *testing.T) {
	n := New()
	_, err := n.NormalizeOllama([]byte(`not json at all`))
	if err == nil {
		t.Fatal("expected error for malformed JSON, got nil")
	}
}

func TestNormalize_Dispatch(t *testing.T) {
	claudeRaw := []byte(`{
		"content": [{"type": "text", "text": "via dispatch"}],
		"model": "claude-sonnet-4-5-20250929",
		"role": "assistant",
		"stop_reason": "end_turn",
		"usage": {"input_tokens": 5, "output_tokens": 3}
	}`)

	n := New()
	resp, err := n.Normalize(claudeRaw, "claude")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if resp.Provider != "claude" {
		t.Errorf("provider = %q, want %q", resp.Provider, "claude")
	}
	if resp.Content != "via dispatch" {
		t.Errorf("content = %q, want %q", resp.Content, "via dispatch")
	}
}
