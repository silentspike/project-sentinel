package proxy

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"os/exec"
	"regexp"
	"strings"
	"sync"
	"time"
)

const (
	// defaultClaudeCodeBinary is the default path to the claude CLI binary.
	defaultClaudeCodeBinary = "claude"

	// defaultClaudeCodeModel is the default model for the claude-code provider.
	defaultClaudeCodeModel = "claude-opus-4-6"

	// claudeCodeHealthTimeout is the timeout for health checks.
	claudeCodeHealthTimeout = 10 * time.Second

	defaultClaudeCodeLimitCooldown = 15 * time.Minute
)

var claudeCodeResetTimeRE = regexp.MustCompile(`(?i)resets\s+([0-9]{1,2})(?::([0-9]{2}))?\s*(am|pm)\s+\(utc\)`)

// claudeCodeUsage holds token usage from claude CLI result events.
type claudeCodeUsage struct {
	InputTokens              int `json:"input_tokens"`
	OutputTokens             int `json:"output_tokens"`
	CacheReadInputTokens     int `json:"cache_read_input_tokens,omitempty"`
	CacheCreationInputTokens int `json:"cache_creation_input_tokens,omitempty"`
}

// claudeCodeEvent is a single NDJSON event from the claude subprocess output.
type claudeCodeEvent struct {
	Type      string `json:"type"`
	Subtype   string `json:"subtype,omitempty"`
	SessionID string `json:"session_id,omitempty"`
	// For assistant messages
	Message *claudeCodeAssistantMsg `json:"message,omitempty"`
	// For result events
	Result     string           `json:"result,omitempty"`
	CostUSD    float64          `json:"total_cost_usd,omitempty"`
	DurationMs int              `json:"duration_ms,omitempty"`
	IsError    bool             `json:"is_error,omitempty"`
	Usage      *claudeCodeUsage `json:"usage,omitempty"`
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
// maxConcurrentClaude limits parallel claude subprocess spawns to prevent OOM.
// Each subprocess uses ~60-100 MB, systemd MemoryMax is 1 GB.
const maxConcurrentClaude = 3

type ClaudeCodeProvider struct {
	name   string
	model  string
	binary string
	logger *slog.Logger
	sem    chan struct{} // counting semaphore (buffered channel)

	cooldownMu    sync.RWMutex
	cooldownUntil time.Time
	cooldownMsg   string
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
		sem:    make(chan struct{}, maxConcurrentClaude),
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
	if err := p.cooldownError(); err != nil {
		return nil, err
	}

	if req != nil && req.ProviderTimeout > 0 {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, req.ProviderTimeout)
		defer cancel()
	}

	// Acquire semaphore slot (respects context deadline)
	select {
	case p.sem <- struct{}{}:
		defer func() { <-p.sem }()
	case <-ctx.Done():
		return nil, fmt.Errorf("claude-code semaphore wait: %w", ctx.Err())
	}

	model := p.model
	if req.Model != "" {
		model = req.Model
	}

	// Claude Code only accepts a single --system-prompt string, so any structured
	// blocks have to be flattened deterministically for the legacy subprocess path.
	systemPrompt, userPrompt := splitRequest(req)

	// Spawn subprocess per request
	args := []string{
		"-p", userPrompt,
		"--output-format", "stream-json",
		"--verbose",
		"--model", model,
	}
	if systemPrompt != "" {
		args = append(args, "--system-prompt", systemPrompt)
	}
	if req.MaxTokens > 0 {
		args = append(args, "--max-turns", "1")
	}

	p.logger.Debug("spawning claude subprocess",
		"model", model,
		"system_prompt_len", len(systemPrompt),
		"user_prompt_len", len(userPrompt),
	)

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
		if limitErr := p.limitCooldownError(err.Error(), stderrBuf.String()); limitErr != nil {
			return nil, limitErr
		}
		return nil, fmt.Errorf("claude-code parse response: %w (stderr: %s)", err, stderrBuf.String())
	}
	if waitErr != nil {
		if limitErr := p.limitCooldownError(waitErr.Error(), stderrBuf.String()); limitErr != nil {
			return nil, limitErr
		}
		// If we already got a response, the exit code might be non-zero but the response is valid
		if response != nil && response.Content != "" {
			p.logger.Warn("claude-code exited with error but response received", "error", waitErr)
		} else {
			return nil, fmt.Errorf("claude-code process: %w (stderr: %s)", waitErr, stderrBuf.String())
		}
	}

	if response == nil {
		if limitErr := p.limitCooldownError(stderrBuf.String()); limitErr != nil {
			return nil, limitErr
		}
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
			// Extract token usage from the result event
			var inputTokens, outputTokens int
			if event.Usage != nil {
				inputTokens = event.Usage.InputTokens + event.Usage.CacheReadInputTokens + event.Usage.CacheCreationInputTokens
				outputTokens = event.Usage.OutputTokens
			}
			// Result is the final event, stop reading
			return &LLMResponse{
				Content:      strings.Join(contentParts, ""),
				FinishReason: finishReason,
				InputTokens:  inputTokens,
				OutputTokens: outputTokens,
				TokensUsed:   inputTokens + outputTokens,
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

// CurrentProviderError exposes an active provider-side cooldown without
// spawning a new claude subprocess.
func (p *ClaudeCodeProvider) CurrentProviderError() error {
	return p.cooldownError()
}

func (p *ClaudeCodeProvider) cooldownError() error {
	now := time.Now().UTC()

	p.cooldownMu.Lock()
	defer p.cooldownMu.Unlock()

	if p.cooldownUntil.IsZero() {
		return nil
	}
	if !now.Before(p.cooldownUntil) {
		p.cooldownUntil = time.Time{}
		p.cooldownMsg = ""
		return nil
	}

	msg := strings.TrimSpace(p.cooldownMsg)
	if msg == "" {
		msg = "claude-code subscription limit active"
	}
	return &ProviderError{
		StatusCode: http.StatusTooManyRequests,
		Message:    fmt.Sprintf("%s until %s", msg, p.cooldownUntil.Format(time.RFC3339)),
	}
}

func (p *ClaudeCodeProvider) limitCooldownError(parts ...string) error {
	text := strings.TrimSpace(strings.Join(parts, "\n"))
	if text == "" {
		return nil
	}

	msg, until, ok := detectClaudeCodeLimit(text, time.Now().UTC())
	if !ok {
		return nil
	}

	p.cooldownMu.Lock()
	if until.After(p.cooldownUntil) {
		p.cooldownUntil = until
		p.cooldownMsg = msg
	}
	activeUntil := p.cooldownUntil
	activeMsg := p.cooldownMsg
	p.cooldownMu.Unlock()

	p.logger.Warn("claude-code subscription limit detected",
		"until", activeUntil.Format(time.RFC3339),
		"message", activeMsg,
	)

	return &ProviderError{
		StatusCode: http.StatusTooManyRequests,
		Message:    fmt.Sprintf("%s until %s", activeMsg, activeUntil.Format(time.RFC3339)),
	}
}

func detectClaudeCodeLimit(text string, now time.Time) (string, time.Time, bool) {
	if !strings.Contains(strings.ToLower(text), "hit your limit") {
		return "", time.Time{}, false
	}

	msg := "claude-code subscription limit active"
	lines := strings.Split(text, "\n")
	for _, line := range lines {
		line = strings.TrimSpace(line)
		if strings.Contains(strings.ToLower(line), "hit your limit") {
			msg = line
			break
		}
	}

	if matches := claudeCodeResetTimeRE.FindStringSubmatch(text); len(matches) == 4 {
		hour := 0
		minute := 0
		fmt.Sscanf(matches[1], "%d", &hour)
		if matches[2] != "" {
			fmt.Sscanf(matches[2], "%d", &minute)
		}
		meridiem := strings.ToLower(matches[3])
		if meridiem == "pm" && hour != 12 {
			hour += 12
		}
		if meridiem == "am" && hour == 12 {
			hour = 0
		}

		reset := time.Date(now.Year(), now.Month(), now.Day(), hour, minute, 0, 0, time.UTC)
		if !reset.After(now) {
			reset = reset.Add(24 * time.Hour)
		}
		return msg, reset, true
	}

	return msg, now.Add(defaultClaudeCodeLimitCooldown), true
}

// splitMessages separates messages into a system prompt (passed via --system-prompt
// to override Claude Code's default coding assistant persona) and a user prompt
// (passed via -p). System messages become the system prompt; user/assistant
// messages become the user prompt.
func splitMessages(messages []Message) (systemPrompt, userPrompt string) {
	var sysParts []string
	var userParts []string

	for _, m := range messages {
		switch m.Role {
		case "system":
			sysParts = append(sysParts, m.Content)
		case "user":
			userParts = append(userParts, m.Content)
		case "assistant":
			userParts = append(userParts, "[Previous response: "+m.Content+"]")
		}
	}

	systemPrompt = strings.Join(sysParts, "\n\n")
	userPrompt = strings.Join(userParts, "\n\n")

	// If no user messages, use system prompt as user prompt (single-message case)
	if userPrompt == "" && systemPrompt != "" {
		userPrompt = systemPrompt
		systemPrompt = ""
	}

	return systemPrompt, userPrompt
}

func splitRequest(req *LLMRequest) (systemPrompt, userPrompt string) {
	legacySystemPrompt, userPrompt := splitMessages(req.Messages)

	var systemParts []string
	for _, block := range req.SystemBlocks {
		if strings.TrimSpace(block.Text) == "" {
			continue
		}
		systemParts = append(systemParts, block.Text)
	}
	if legacySystemPrompt != "" {
		systemParts = append(systemParts, legacySystemPrompt)
	}

	systemPrompt = strings.Join(systemParts, "\n\n")

	// Claude Code requires a -p payload. Preserve the previous single-message
	// fallback semantics when we only have system-level instructions.
	if userPrompt == "" && systemPrompt != "" {
		userPrompt = systemPrompt
		systemPrompt = ""
	}

	return systemPrompt, userPrompt
}

// buildPrompt concatenates messages into a single prompt string.
// For claude -p, we join all messages with role prefixes for context.
// Deprecated: Use splitMessages instead for proper --system-prompt support.
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
