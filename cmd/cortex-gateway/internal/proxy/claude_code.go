package proxy

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"os/exec"
	"strings"
	"sync"
	"time"
)

const (
	// defaultClaudeCodeBinary is the default path to the claude CLI binary.
	defaultClaudeCodeBinary = "claude"

	// defaultClaudeCodeModel is the default model for the claude-code provider.
	defaultClaudeCodeModel = "claude-opus-4-6"

	// claudeCodeStartTimeout is how long we wait for the subprocess to become ready.
	claudeCodeStartTimeout = 30 * time.Second

	// claudeCodeResponseTimeout is the maximum time to wait for a response.
	// LLM responses can take minutes for complex prompts.
	claudeCodeResponseTimeout = 5 * time.Minute

	// claudeCodeHealthTimeout is the timeout for health checks.
	claudeCodeHealthTimeout = 10 * time.Second
)

// claudeCodeInput is the NDJSON input format for claude -p --output-format stream-json.
type claudeCodeInput struct {
	Type    string             `json:"type"`
	Message claudeCodeInputMsg `json:"message"`
}

// claudeCodeInputMsg is the message payload sent to claude subprocess.
type claudeCodeInputMsg struct {
	Role    string `json:"role"`
	Content string `json:"content"`
}

// claudeCodeEvent is a single NDJSON event from the claude subprocess output.
type claudeCodeEvent struct {
	Type      string `json:"type"`
	Subtype   string `json:"subtype,omitempty"`
	SessionID string `json:"session_id,omitempty"`
	// For assistant messages
	Message *claudeCodeAssistantMsg `json:"message,omitempty"`
	// For result events
	Result     string  `json:"result,omitempty"`
	CostUSD    float64 `json:"total_cost_usd,omitempty"`
	DurationMs int     `json:"duration_ms,omitempty"`
	IsError    bool    `json:"is_error,omitempty"`
}

// claudeCodeContentBlock represents a content block in a claude assistant message.
type claudeCodeContentBlock struct {
	Type string `json:"type"`
	Text string `json:"text,omitempty"`
}

// claudeCodeAssistantMsg is the assistant message in a claude event.
// Content is an array of content blocks, e.g. [{"type":"text","text":"..."}].
type claudeCodeAssistantMsg struct {
	Role    string                   `json:"role"`
	Content []claudeCodeContentBlock `json:"content"`
}

// ClaudeCodeProvider implements Provider using the claude CLI as a subprocess.
// It runs `claude -p --output-format stream-json` and communicates via NDJSON
// on stdin/stdout. Authentication is handled by the claude binary itself
// (OAuth token stored in ~/.claude/, no API key required).
type ClaudeCodeProvider struct {
	mu     sync.Mutex
	name   string
	model  string
	binary string
	logger *slog.Logger
}

// NewClaudeCodeProvider creates a new Claude Code subprocess provider.
func NewClaudeCodeProvider(cfg ProviderConfig, logger *slog.Logger) *ClaudeCodeProvider {
	binary := cfg.BaseURL // We repurpose BaseURL for the binary path
	if binary == "" {
		binary = defaultClaudeCodeBinary
	}
	model := cfg.Model
	if model == "" {
		model = defaultClaudeCodeModel
	}
	if logger == nil {
		logger = slog.Default()
	}
	return &ClaudeCodeProvider{
		name:   cfg.Name,
		model:  model,
		binary: binary,
		logger: logger,
	}
}

// Name returns the provider name.
func (p *ClaudeCodeProvider) Name() string {
	return p.name
}

// Send executes a single LLM request by spawning a claude subprocess per request.
// Each call runs: claude -p --output-format stream-json --model <model>
// The prompt is piped via stdin, and the response is read from stdout as NDJSON.
func (p *ClaudeCodeProvider) Send(ctx context.Context, req *LLMRequest) (*LLMResponse, error) {
	p.mu.Lock()
	defer p.mu.Unlock()

	model := p.model
	if req.Model != "" {
		model = req.Model
	}

	// Build the prompt from messages
	prompt := buildPrompt(req.Messages)

	// Spawn subprocess per request
	args := []string{
		"-p", prompt,
		"--output-format", "stream-json",
		"--verbose",
		"--model", model,
	}
	if req.MaxTokens > 0 {
		args = append(args, "--max-turns", "1")
	}

	p.logger.Debug("spawning claude subprocess", "model", model, "prompt_len", len(prompt))

	cmd := exec.CommandContext(ctx, p.binary, args...) //nolint:gosec // binary from trusted config
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, fmt.Errorf("claude-code stdout pipe: %w", err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return nil, fmt.Errorf("claude-code stderr pipe: %w", err)
	}

	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("claude-code start: %w", err)
	}

	// Read stderr in background for error reporting
	var stderrBuf strings.Builder
	stderrDone := make(chan struct{})
	go func() {
		defer close(stderrDone)
		_, _ = io.Copy(&stderrBuf, stderr)
	}()

	// Parse NDJSON output
	response, err := p.parseOutputStream(stdout)

	// Wait for process to finish
	waitErr := cmd.Wait()
	<-stderrDone

	if err != nil {
		return nil, fmt.Errorf("claude-code parse response: %w (stderr: %s)", err, stderrBuf.String())
	}
	if waitErr != nil {
		// If we already got a response, the exit code might be non-zero but the response is valid
		if response != nil && response.Content != "" {
			p.logger.Warn("claude-code exited with error but response received", "error", waitErr)
		} else {
			return nil, fmt.Errorf("claude-code process: %w (stderr: %s)", waitErr, stderrBuf.String())
		}
	}

	if response == nil {
		return nil, fmt.Errorf("claude-code: no response received (stderr: %s)", stderrBuf.String())
	}

	response.Model = model
	p.logger.Debug("claude-code response received",
		"model", model,
		"content_len", len(response.Content),
		"finish_reason", response.FinishReason,
	)

	return response, nil
}

// parseOutputStream reads NDJSON events from the claude subprocess stdout
// and assembles them into an LLMResponse.
func (p *ClaudeCodeProvider) parseOutputStream(r io.Reader) (*LLMResponse, error) {
	scanner := bufio.NewScanner(r)
	// Allow large lines (LLM output can be long)
	scanner.Buffer(make([]byte, 0, 64*1024), 1024*1024)

	var contentParts []string
	var finishReason string

	for scanner.Scan() {
		line := scanner.Text()
		if line == "" {
			continue
		}

		var event claudeCodeEvent
		if err := json.Unmarshal([]byte(line), &event); err != nil {
			p.logger.Debug("claude-code: skipping unparseable line", "line", line[:min(len(line), 100)])
			continue
		}

		switch event.Type {
		case "system":
			// Init event, session started
			p.logger.Debug("claude-code session init", "session_id", event.SessionID)

		case "assistant":
			if event.Message != nil {
				for _, block := range event.Message.Content {
					if block.Type == "text" && block.Text != "" {
						contentParts = append(contentParts, block.Text)
					}
				}
			}

		case "result":
			finishReason = event.Subtype // "success" or "error"
			// Only use result text if no assistant content was collected,
			// because the result event typically duplicates the assistant text.
			if event.Result != "" && len(contentParts) == 0 {
				contentParts = append(contentParts, event.Result)
			}
			if event.IsError {
				return nil, fmt.Errorf("claude-code result error: %s", event.Result)
			}
			// Result is the final event, stop reading
			return &LLMResponse{
				Content:      strings.Join(contentParts, ""),
				FinishReason: finishReason,
			}, nil
		}
	}

	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("claude-code read stdout: %w", err)
	}

	// If we got content but no result event, still return what we have
	if len(contentParts) > 0 {
		return &LLMResponse{
			Content:      strings.Join(contentParts, ""),
			FinishReason: "eof",
		}, nil
	}

	return nil, nil
}

// HealthCheck verifies that the claude binary is available and can run.
func (p *ClaudeCodeProvider) HealthCheck(ctx context.Context) error {
	ctx, cancel := context.WithTimeout(ctx, claudeCodeHealthTimeout)
	defer cancel()

	cmd := exec.CommandContext(ctx, p.binary, "--version") //nolint:gosec // binary from trusted config
	output, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("claude-code health check: %w (output: %s)", err, string(output))
	}
	return nil
}

// buildPrompt concatenates messages into a single prompt string.
// For claude -p, we join all messages with role prefixes for context.
func buildPrompt(messages []Message) string {
	if len(messages) == 1 {
		return messages[0].Content
	}

	var sb strings.Builder
	for _, m := range messages {
		switch m.Role {
		case "system":
			sb.WriteString(m.Content)
			sb.WriteString("\n\n")
		case "user":
			sb.WriteString(m.Content)
			sb.WriteString("\n\n")
		case "assistant":
			sb.WriteString("[Previous response: ")
			sb.WriteString(m.Content)
			sb.WriteString("]\n\n")
		}
	}
	return strings.TrimSpace(sb.String())
}
