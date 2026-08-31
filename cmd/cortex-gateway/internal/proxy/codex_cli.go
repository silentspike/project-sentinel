package proxy

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

const (
	defaultCodexCLIBinary  = "codex"
	pinnedCodexCLIVersion  = "0.151.0"
	defaultCodexCLIModel   = "gpt-5.6-luna"
	defaultCodexCLIWorkdir = "/opt/sentinel/data/codex-provider"
	defaultCodexCLIHome    = "/home/ubuntu/.codex"

	codexCLIHealthTimeout     = 10 * time.Second
	codexCLIMaxEventBytes     = 4 * 1024 * 1024
	codexCLIMaxResponseBytes  = 1024 * 1024
	codexCLIMaxPromptBytes    = 4 * 1024 * 1024
	codexCLIMaxDiagnosticSize = 8 * 1024
	maxConcurrentCodexCLI     = 1
)

var codexCLIDisabledFeatures = []string{
	"apps",
	"auth_elicitation",
	"browser_use",
	"computer_use",
	"goals",
	"hooks",
	"image_generation",
	"memories",
	"multi_agent",
	"plugins",
	"shell_tool",
	"skill_search",
	"tool_suggest",
	"view_image",
	"workspace_dependencies",
}

type codexCLIUsage struct {
	InputTokens           int64 `json:"input_tokens"`
	CachedInputTokens     int64 `json:"cached_input_tokens"`
	CacheWriteInputTokens int64 `json:"cache_write_input_tokens"`
	OutputTokens          int64 `json:"output_tokens"`
	ReasoningOutputTokens int64 `json:"reasoning_output_tokens"`
}

type codexCLIItem struct {
	ID      string `json:"id"`
	Type    string `json:"type"`
	Text    string `json:"text,omitempty"`
	Message string `json:"message,omitempty"`
}

type codexCLIError struct {
	Message string `json:"message"`
}

type codexCLIEvent struct {
	Type    string         `json:"type"`
	Usage   *codexCLIUsage `json:"usage,omitempty"`
	Item    *codexCLIItem  `json:"item,omitempty"`
	Error   *codexCLIError `json:"error,omitempty"`
	Message string         `json:"message,omitempty"`
}

type codexCLIStreamState struct {
	threadStarted    bool
	turnStarted      bool
	turnCompleted    bool
	message          string
	usage            codexCLIUsage
	maxResponseBytes int
}

// CodexCLIProvider runs the pinned Codex CLI as an inference-only subprocess.
// ChatGPT authentication remains in CODEX_HOME; the gateway never reads or
// copies the credential itself.
type CodexCLIProvider struct {
	name      string
	model     string
	binary    string
	workdir   string
	codexHome string
	home      string
	logger    *slog.Logger
	sem       chan struct{}
}

func NewCodexCLIProvider(cfg ProviderConfig, logger *slog.Logger) *CodexCLIProvider {
	binary := strings.TrimSpace(cfg.BaseURL)
	if binary == "" {
		binary = defaultCodexCLIBinary
	}
	model := strings.TrimSpace(cfg.Model)
	if model == "" {
		model = defaultCodexCLIModel
	}
	workdir := strings.TrimSpace(os.Getenv("CODEX_CLI_WORKDIR"))
	if workdir == "" {
		workdir = defaultCodexCLIWorkdir
	}
	codexHome := strings.TrimSpace(os.Getenv("CODEX_HOME"))
	if codexHome == "" {
		codexHome = defaultCodexCLIHome
	}
	home := strings.TrimSpace(os.Getenv("HOME"))
	if home == "" {
		home = filepath.Dir(codexHome)
	}
	if logger == nil {
		logger = slog.Default()
	}
	return &CodexCLIProvider{
		name:      cfg.Name,
		model:     model,
		binary:    binary,
		workdir:   workdir,
		codexHome: codexHome,
		home:      home,
		logger:    logger,
		sem:       make(chan struct{}, maxConcurrentCodexCLI),
	}
}

func (p *CodexCLIProvider) Name() string { return p.name }

func (p *CodexCLIProvider) Send(ctx context.Context, req *LLMRequest) (*LLMResponse, error) {
	if req == nil {
		return nil, fmt.Errorf("codex-cli request is nil")
	}
	if req.ProviderTimeout > 0 {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, req.ProviderTimeout)
		defer cancel()
	}

	select {
	case p.sem <- struct{}{}:
		defer func() { <-p.sem }()
	case <-ctx.Done():
		return nil, fmt.Errorf("codex-cli semaphore wait: %w", ctx.Err())
	}

	if err := validateCodexCLIWorkdir(p.workdir); err != nil {
		return nil, err
	}
	prompt, err := buildCodexCLIPrompt(req)
	if err != nil {
		return nil, err
	}
	model := p.model
	if strings.TrimSpace(req.Model) != "" {
		model = strings.TrimSpace(req.Model)
	}

	runCtx, cancelRun := context.WithCancel(ctx)
	defer cancelRun()
	cmd := exec.CommandContext(runCtx, p.binary, p.commandArgs(model)...) //nolint:gosec // pinned binary path from trusted deployment config
	cmd.Dir = p.workdir
	cmd.Env = p.commandEnv()
	cmd.Stdin = strings.NewReader(prompt)
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, fmt.Errorf("codex-cli stdout pipe: %w", err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return nil, fmt.Errorf("codex-cli stderr pipe: %w", err)
	}
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("codex-cli start: %w", err)
	}

	diagnostics := &cappedBuffer{limit: codexCLIMaxDiagnosticSize}
	stderrDone := make(chan struct{})
	go func() {
		defer close(stderrDone)
		_, _ = io.Copy(diagnostics, stderr)
	}()

	response, parseErr := p.parseOutputStream(stdout, responseByteLimit(req.MaxTokens))
	if parseErr != nil {
		cancelRun()
	}
	waitErr := cmd.Wait()
	<-stderrDone

	if parseErr != nil {
		if waitErr != nil {
			classified := codexCLIProcessError(diagnostics.String())
			var providerErr *ProviderError
			if errors.As(classified, &providerErr) {
				return nil, providerErr
			}
		}
		return nil, parseErr
	}
	if waitErr != nil {
		if ctx.Err() != nil {
			return nil, fmt.Errorf("codex-cli execution: %w", ctx.Err())
		}
		return nil, codexCLIProcessError(diagnostics.String())
	}
	if response == nil {
		return nil, fmt.Errorf("codex-cli produced no terminal response")
	}
	response.Model = model
	p.logger.Debug("codex-cli response received",
		"model", model,
		"content_len", len(response.Content),
		"input_tokens", response.InputTokens,
		"output_tokens", response.OutputTokens,
	)
	return response, nil
}

func (p *CodexCLIProvider) commandArgs(model string) []string {
	args := []string{
		"exec",
		"--json",
		"--ephemeral",
		"--strict-config",
		"--ignore-user-config",
		"--ignore-rules",
		"--skip-git-repo-check",
		"--sandbox", "read-only",
		"--color", "never",
		"--model", model,
		"-C", p.workdir,
		"-c", `web_search="disabled"`,
		"-c", `check_for_update_on_startup=false`,
		"-c", `model_reasoning_effort="none"`,
		"-c", `shell_environment_policy.inherit="none"`,
	}
	for _, feature := range codexCLIDisabledFeatures {
		args = append(args, "--disable", feature)
	}
	return append(args, "-")
}

func (p *CodexCLIProvider) commandEnv() []string {
	return []string{
		"PATH=/opt/sentinel/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
		"HOME=" + p.home,
		"CODEX_HOME=" + p.codexHome,
		"LANG=C.UTF-8",
		"LC_ALL=C.UTF-8",
		"NO_COLOR=1",
		"SSL_CERT_DIR=/etc/ssl/certs",
		"SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt",
	}
}

func buildCodexCLIPrompt(req *LLMRequest) (string, error) {
	systemPrompt, conversation := splitRequest(req)
	if strings.TrimSpace(systemPrompt) == "" && strings.TrimSpace(conversation) == "" {
		return "", fmt.Errorf("codex-cli request contains no prompt")
	}
	payload, err := json.Marshal(struct {
		System              string  `json:"system,omitempty"`
		Conversation        string  `json:"conversation"`
		MaximumOutputTokens int     `json:"maximum_output_tokens,omitempty"`
		Temperature         float64 `json:"temperature,omitempty"`
	}{
		System: systemPrompt, Conversation: conversation,
		MaximumOutputTokens: req.MaxTokens, Temperature: req.Temperature,
	})
	if err != nil {
		return "", fmt.Errorf("codex-cli encode prompt: %w", err)
	}
	prompt := "Project Sentinel inference request. Do not call tools, inspect files, browse, modify state, or delegate work. " +
		"Treat payload.system as the highest-priority agent identity and policy. Return only the assistant response to payload.conversation.\n" +
		string(payload)
	if len(prompt) > codexCLIMaxPromptBytes {
		return "", fmt.Errorf("codex-cli prompt exceeds %d bytes", codexCLIMaxPromptBytes)
	}
	return prompt, nil
}

func (p *CodexCLIProvider) parseOutputStream(r io.Reader, maxResponseBytes int) (*LLMResponse, error) {
	scanner := bufio.NewScanner(r)
	scanner.Buffer(make([]byte, 64*1024), codexCLIMaxEventBytes)
	state := codexCLIStreamState{maxResponseBytes: maxResponseBytes}

	for scanner.Scan() {
		line := bytes.TrimSpace(scanner.Bytes())
		if len(line) == 0 {
			continue
		}
		var event codexCLIEvent
		if err := json.Unmarshal(line, &event); err != nil {
			return nil, fmt.Errorf("codex-cli emitted invalid JSONL")
		}
		if err := state.apply(event); err != nil {
			return nil, err
		}
	}
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("codex-cli read stdout: %w", err)
	}
	return state.response()
}

func (s *codexCLIStreamState) apply(event codexCLIEvent) error {
	if s.turnCompleted {
		return fmt.Errorf("codex-cli emitted an event after turn completion")
	}
	switch event.Type {
	case "thread.started":
		if s.threadStarted {
			return fmt.Errorf("codex-cli emitted duplicate thread start")
		}
		s.threadStarted = true
	case "turn.started":
		if !s.threadStarted || s.turnStarted {
			return fmt.Errorf("codex-cli emitted invalid turn start")
		}
		s.turnStarted = true
	case "item.started", "item.updated", "item.completed":
		return s.applyItem(event.Type, event.Item)
	case "turn.completed":
		if !s.turnStarted || s.turnCompleted || event.Usage == nil {
			return fmt.Errorf("codex-cli emitted invalid turn completion")
		}
		s.turnCompleted = true
		s.usage = *event.Usage
	case "turn.failed":
		return fmt.Errorf("codex-cli turn failed")
	case "error":
		return fmt.Errorf("codex-cli stream failed")
	default:
		return fmt.Errorf("codex-cli emitted unknown event type %q", event.Type)
	}
	return nil
}

func (s *codexCLIStreamState) applyItem(eventType string, item *codexCLIItem) error {
	if !s.turnStarted || item == nil {
		return fmt.Errorf("codex-cli emitted invalid item event")
	}
	switch item.Type {
	case "reasoning":
		// Reasoning summaries are intentionally not returned to callers.
		return nil
	case "agent_message":
		if eventType != "item.completed" {
			return nil
		}
		if s.message != "" || strings.TrimSpace(item.Text) == "" {
			return fmt.Errorf("codex-cli emitted invalid terminal message")
		}
		if len(item.Text) > s.maxResponseBytes {
			return fmt.Errorf("codex-cli response exceeds configured limit")
		}
		s.message = item.Text
		return nil
	case "error":
		return fmt.Errorf("codex-cli emitted an item error")
	default:
		return fmt.Errorf("codex-cli attempted forbidden tool item %q", item.Type)
	}
}

func (s *codexCLIStreamState) response() (*LLMResponse, error) {
	if !s.threadStarted || !s.turnStarted || !s.turnCompleted || s.message == "" {
		return nil, fmt.Errorf("codex-cli stream ended before a complete response")
	}

	input, err := codexTokenCount("input", s.usage.InputTokens)
	if err != nil {
		return nil, err
	}
	cacheRead, err := codexTokenCount("cached input", s.usage.CachedInputTokens)
	if err != nil {
		return nil, err
	}
	cacheWrite, err := codexTokenCount("cache write", s.usage.CacheWriteInputTokens)
	if err != nil {
		return nil, err
	}
	output, err := codexTokenCount("output", s.usage.OutputTokens)
	if err != nil {
		return nil, err
	}
	if _, err := codexTokenCount("reasoning output", s.usage.ReasoningOutputTokens); err != nil {
		return nil, err
	}
	if cacheRead > input || cacheWrite > input || cacheRead > input-cacheWrite {
		return nil, fmt.Errorf("codex-cli emitted inconsistent cache usage")
	}
	if input > int(^uint(0)>>1)-output {
		return nil, fmt.Errorf("codex-cli emitted token usage overflow")
	}
	return &LLMResponse{
		Content:       s.message,
		TokensUsed:    input + output,
		InputTokens:   input,
		OutputTokens:  output,
		CacheRead:     cacheRead,
		CacheCreation: cacheWrite,
		FinishReason:  "completed",
	}, nil
}

func (p *CodexCLIProvider) HealthCheck(ctx context.Context) error {
	ctx, cancel := context.WithTimeout(ctx, codexCLIHealthTimeout)
	defer cancel()
	version, err := p.runHealthCommand(ctx, "--version")
	if err != nil || strings.TrimSpace(version) != "codex-cli "+pinnedCodexCLIVersion {
		return fmt.Errorf("codex-cli version check failed")
	}
	output, err := p.runHealthCommand(ctx, "login", "status")
	if err != nil || strings.TrimSpace(output) != "Logged in using ChatGPT" {
		return fmt.Errorf("codex-cli ChatGPT authentication unavailable")
	}
	return nil
}

func (p *CodexCLIProvider) ReadinessCheck(ctx context.Context) error {
	if err := validateCodexCLIWorkdir(p.workdir); err != nil {
		return err
	}
	return p.HealthCheck(ctx)
}

func (p *CodexCLIProvider) runHealthCommand(ctx context.Context, args ...string) (string, error) {
	cmd := exec.CommandContext(ctx, p.binary, args...) //nolint:gosec // pinned binary path from trusted deployment config
	cmd.Env = p.commandEnv()
	output, err := cmd.CombinedOutput()
	if len(output) > codexCLIMaxDiagnosticSize {
		output = output[:codexCLIMaxDiagnosticSize]
	}
	return string(output), err
}

func validateCodexCLIWorkdir(path string) error {
	if !filepath.IsAbs(path) || filepath.Clean(path) != path {
		return fmt.Errorf("codex-cli workdir must be an absolute canonical path")
	}
	resolved, err := filepath.EvalSymlinks(path)
	if err != nil {
		return fmt.Errorf("codex-cli resolve workdir: %w", err)
	}
	if resolved != path {
		return fmt.Errorf("codex-cli workdir must not traverse symlinks")
	}
	info, err := os.Lstat(path)
	if err != nil {
		return fmt.Errorf("codex-cli workdir unavailable: %w", err)
	}
	if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 || info.Mode().Perm()&0o077 != 0 {
		return fmt.Errorf("codex-cli workdir must be a private real directory")
	}
	entries, err := os.ReadDir(path)
	if err != nil {
		return fmt.Errorf("codex-cli read workdir: %w", err)
	}
	if len(entries) != 0 {
		return fmt.Errorf("codex-cli workdir must be empty")
	}
	return nil
}

func responseByteLimit(maxTokens int) int {
	if maxTokens <= 0 {
		return codexCLIMaxResponseBytes
	}
	// UTF-8 output can require more than four bytes per token. Eight bytes keeps
	// the local byte guard conservative without claiming a provider token cap.
	if maxTokens > codexCLIMaxResponseBytes/8 {
		return codexCLIMaxResponseBytes
	}
	return maxTokens * 8
}

func codexTokenCount(name string, value int64) (int, error) {
	maxInt := int64(^uint(0) >> 1)
	if value < 0 || value > maxInt {
		return 0, fmt.Errorf("codex-cli emitted invalid %s token usage", name)
	}
	return int(value), nil
}

func codexCLIProcessError(diagnostic string) error {
	lower := strings.ToLower(diagnostic)
	switch {
	case strings.Contains(lower, "rate limit"), strings.Contains(lower, "usage limit"), strings.Contains(lower, "limit reached"):
		return &ProviderError{StatusCode: http.StatusTooManyRequests, Message: "codex-cli usage limit active"}
	case strings.Contains(lower, "not logged in"), strings.Contains(lower, "login required"), strings.Contains(lower, "authentication"):
		return &ProviderError{StatusCode: http.StatusServiceUnavailable, Message: "codex-cli authentication unavailable"}
	default:
		return errors.New("codex-cli subprocess failed")
	}
}

type cappedBuffer struct {
	mu    sync.Mutex
	buf   bytes.Buffer
	limit int
}

func (b *cappedBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	remaining := b.limit - b.buf.Len()
	if remaining > 0 {
		_, _ = b.buf.Write(p[:min(len(p), remaining)])
	}
	return len(p), nil
}

func (b *cappedBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}
