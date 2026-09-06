package proxy

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/BurntSushi/toml"
)

func TestCodexCLIProviderParsesCompletedInference(t *testing.T) {
	stream := strings.Join([]string{
		`{"type":"thread.started","thread_id":"thread-1"}`,
		fmt.Sprintf(`{"type":"item.completed","item":{"id":"item-0","type":"error","message":%q}}`, codexCLIDisabledCodeModePrelude),
		`{"type":"turn.started"}`,
		`{"type":"item.started","item":{"id":"reason-1","type":"reasoning","text":"summary"}}`,
		`{"type":"item.completed","item":{"id":"reason-1","type":"reasoning","text":"summary"}}`,
		`{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"Pong"}}`,
		`{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":20,"cache_write_input_tokens":10,"output_tokens":5,"reasoning_output_tokens":2}}`,
	}, "\n")

	provider := NewCodexCLIProvider(ProviderConfig{Name: CodexCLIProviderName}, nil)
	response, err := provider.parseOutputStream(strings.NewReader(stream), 1024)
	if err != nil {
		t.Fatal(err)
	}
	if response.Content != "Pong" || response.FinishReason != "completed" {
		t.Fatalf("response=%+v", response)
	}
	if response.InputTokens != 100 || response.CacheRead != 20 || response.CacheCreation != 10 ||
		response.OutputTokens != 5 || response.TokensUsed != 105 {
		t.Fatalf("usage=%+v", response)
	}
}

func TestCodexCLIProviderRejectsUnexpectedOrDuplicatePreTurnItems(t *testing.T) {
	tests := map[string]string{
		"unexpected error": strings.Join([]string{
			`{"type":"thread.started","thread_id":"thread-1"}`,
			`{"type":"item.completed","item":{"id":"item-0","type":"error","message":"unexpected"}}`,
		}, "\n"),
		"duplicate disabled-code-mode prelude": strings.Join([]string{
			`{"type":"thread.started","thread_id":"thread-1"}`,
			fmt.Sprintf(`{"type":"item.completed","item":{"id":"item-0","type":"error","message":%q}}`, codexCLIDisabledCodeModePrelude),
			fmt.Sprintf(`{"type":"item.completed","item":{"id":"item-1","type":"error","message":%q}}`, codexCLIDisabledCodeModePrelude),
		}, "\n"),
		"tool before turn": strings.Join([]string{
			`{"type":"thread.started","thread_id":"thread-1"}`,
			`{"type":"item.started","item":{"id":"item-0","type":"command_execution"}}`,
		}, "\n"),
	}

	for name, stream := range tests {
		t.Run(name, func(t *testing.T) {
			provider := NewCodexCLIProvider(ProviderConfig{Name: CodexCLIProviderName}, nil)
			if _, err := provider.parseOutputStream(strings.NewReader(stream), 1024); err == nil {
				t.Fatal("invalid pre-turn stream accepted")
			}
		})
	}
}

func TestCodexCLIProviderRejectsEveryToolItem(t *testing.T) {
	for _, itemType := range []string{
		"command_execution", "file_change", "mcp_tool_call", "collab_tool_call", "web_search", "todo_list",
	} {
		t.Run(itemType, func(t *testing.T) {
			stream := strings.Join([]string{
				`{"type":"thread.started","thread_id":"thread-1"}`,
				`{"type":"turn.started"}`,
				fmt.Sprintf(`{"type":"item.started","item":{"id":"tool-1","type":%q}}`, itemType),
			}, "\n")
			provider := NewCodexCLIProvider(ProviderConfig{Name: CodexCLIProviderName}, nil)
			_, err := provider.parseOutputStream(strings.NewReader(stream), 1024)
			if err == nil || !strings.Contains(err.Error(), "forbidden tool item") {
				t.Fatalf("error=%v", err)
			}
		})
	}
}

func TestCodexCLIProviderRejectsIncompleteMalformedAndInconsistentStreams(t *testing.T) {
	tests := map[string]string{
		"malformed": `not-json`,
		"missing completion": strings.Join([]string{
			`{"type":"thread.started","thread_id":"thread-1"}`,
			`{"type":"turn.started"}`,
			`{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"partial"}}`,
		}, "\n"),
		"failed turn": strings.Join([]string{
			`{"type":"thread.started","thread_id":"thread-1"}`,
			`{"type":"turn.started"}`,
			`{"type":"turn.failed","error":{"message":"private upstream detail"}}`,
		}, "\n"),
		"inconsistent cache": strings.Join([]string{
			`{"type":"thread.started","thread_id":"thread-1"}`,
			`{"type":"turn.started"}`,
			`{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"answer"}}`,
			`{"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":8,"cache_write_input_tokens":8,"output_tokens":1,"reasoning_output_tokens":0}}`,
		}, "\n"),
		"event after completion": strings.Join([]string{
			`{"type":"thread.started","thread_id":"thread-1"}`,
			`{"type":"turn.started"}`,
			`{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"answer"}}`,
			`{"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":1,"reasoning_output_tokens":0}}`,
			`{"type":"item.completed","item":{"id":"reason-1","type":"reasoning","text":"late"}}`,
		}, "\n"),
	}
	for name, stream := range tests {
		t.Run(name, func(t *testing.T) {
			provider := NewCodexCLIProvider(ProviderConfig{Name: CodexCLIProviderName}, nil)
			if _, err := provider.parseOutputStream(strings.NewReader(stream), 1024); err == nil {
				t.Fatal("invalid stream accepted")
			}
		})
	}
}

func TestCodexCLIProviderSendUsesIsolatedInferenceOnlyProcess(t *testing.T) {
	workdir := t.TempDir()
	if err := os.Chmod(workdir, 0o700); err != nil { //nolint:gosec // test models the private production workdir
		t.Fatal(err)
	}
	codexHome := t.TempDir()
	artifacts := t.TempDir()
	argsPath := filepath.Join(artifacts, "args")
	envPath := filepath.Join(artifacts, "env")
	promptPath := filepath.Join(artifacts, "prompt")
	pwdPath := filepath.Join(artifacts, "pwd")
	scriptPath := filepath.Join(artifacts, "codex")
	script := fmt.Sprintf(`#!/bin/sh
set -eu
case "$1" in
  --version)
    printf 'codex-cli 0.151.0\n'
    ;;
  login)
    [ "$2" = status ]
    printf 'Logged in using ChatGPT\n'
    ;;
  exec)
    printf '%%s\n' "$@" > %q
    env > %q
    pwd > %q
    cat > %q
    printf '%%s\n' '{"type":"thread.started","thread_id":"thread-1"}'
    printf '%%s\n' '{"type":"turn.started"}'
    printf '%%s\n' '{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"Pong"}}'
    printf '%%s\n' '{"type":"turn.completed","usage":{"input_tokens":40,"cached_input_tokens":10,"cache_write_input_tokens":0,"output_tokens":2,"reasoning_output_tokens":0}}'
    ;;
  *) exit 64 ;;
esac
`, argsPath, envPath, pwdPath, promptPath)
	if err := os.WriteFile(scriptPath, []byte(script), 0o700); err != nil { //nolint:gosec // executable test fixture
		t.Fatal(err)
	}
	t.Setenv("CODEX_CLI_WORKDIR", workdir)
	t.Setenv("CODEX_HOME", codexHome)
	t.Setenv("HOME", filepath.Dir(codexHome))
	t.Setenv("TOP_SECRET_FOR_TEST", "must-not-be-inherited")

	provider := NewCodexCLIProvider(ProviderConfig{
		Name: CodexCLIProviderName, BaseURL: scriptPath, Model: "gpt-5.6-luna",
	}, nil)
	response, err := provider.Send(context.Background(), &LLMRequest{
		Messages:  []Message{{Role: "user", Content: "Reply with Pong."}},
		MaxTokens: 64,
	})
	if err != nil {
		t.Fatal(err)
	}
	if response.Content != "Pong" || response.Model != "gpt-5.6-luna" {
		t.Fatalf("response=%+v", response)
	}

	args := strings.Fields(readTestFile(t, argsPath))
	for _, required := range []string{
		"exec", "--json", "--ephemeral", "--strict-config", "--ignore-user-config", "--ignore-rules",
		"--skip-git-repo-check", "read-only", "gpt-5.6-luna", "code_mode", "code_mode_host",
		"shell_tool", "multi_agent", "-",
		"unbounded_connection_retries", "shell_snapshot", "shell_snapshot_v2",
	} {
		if !slices.Contains(args, required) {
			t.Fatalf("missing argument %q in %#v", required, args)
		}
	}
	if got := strings.TrimSpace(readTestFile(t, pwdPath)); got != workdir {
		t.Fatalf("workdir=%q want=%q", got, workdir)
	}
	environment := readTestFile(t, envPath)
	if strings.Contains(environment, "TOP_SECRET_FOR_TEST") || strings.Contains(environment, "must-not-be-inherited") {
		t.Fatalf("unrelated parent environment leaked: %s", environment)
	}
	for _, required := range []string{"CODEX_HOME=" + codexHome, "HOME=" + filepath.Dir(codexHome)} {
		if !strings.Contains(environment, required) {
			t.Fatalf("missing environment %q: %s", required, environment)
		}
	}
	prompt := readTestFile(t, promptPath)
	if !strings.Contains(prompt, "Project Sentinel inference request") || !strings.Contains(prompt, "Reply with Pong.") {
		t.Fatalf("prompt=%q", prompt)
	}
	if err := provider.HealthCheck(context.Background()); err != nil {
		t.Fatalf("health check: %v", err)
	}
}

func TestCodexCLIProviderPreservesDeadlineAfterIncompleteStream(t *testing.T) {
	artifacts := t.TempDir()
	countPath := filepath.Join(artifacts, "starts")
	scriptPath := filepath.Join(artifacts, "codex")
	script := fmt.Sprintf(`#!/bin/sh
set -eu
printf 'started\n' >> %q
printf '%%s\n' '{"type":"thread.started","thread_id":"thread-timeout"}'
printf '%%s\n' '{"type":"turn.started"}'
exec sleep 30
`, countPath)
	if err := os.WriteFile(scriptPath, []byte(script), 0o700); err != nil { //nolint:gosec // executable local fixture; no real provider
		t.Fatal(err)
	}
	workdir := t.TempDir()
	if err := os.Chmod(workdir, 0o700); err != nil { //nolint:gosec // private production workdir contract
		t.Fatal(err)
	}
	t.Setenv("CODEX_CLI_WORKDIR", workdir)
	t.Setenv("CODEX_HOME", t.TempDir())
	provider := NewCodexCLIProvider(ProviderConfig{Name: CodexCLIProviderName, BaseURL: scriptPath}, nil)
	response, err := provider.Send(context.Background(), &LLMRequest{
		Messages:        []Message{{Role: "user", Content: "Local timeout fixture."}},
		MaxTokens:       64,
		ProviderTimeout: 250 * time.Millisecond,
	})
	if response != nil || !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("deadline was hidden by incomplete-stream error: response=%+v error=%v", response, err)
	}
	if starts := readTestFile(t, countPath); starts != "started\n" {
		t.Fatalf("unexpected provider process count: %q", starts)
	}
}

func TestCodexCLIProviderUsesExplicitNativeAuthWithoutTransportRetries(t *testing.T) {
	p := NewCodexCLIProvider(ProviderConfig{Name: CodexCLIProviderName}, nil)
	args := p.commandArgs("test-model")
	var config map[string]any
	for index, arg := range args {
		if arg == "-c" && index+1 < len(args) {
			if _, err := toml.Decode(args[index+1], &config); err != nil {
				t.Fatal(err)
			}
		}
	}
	if config["model_provider"] != "sentinel_chatgpt" {
		t.Fatal("retry overrides would be ignored for the built-in provider")
	}
	providers := config["model_providers"].(map[string]any)
	provider := providers["sentinel_chatgpt"].(map[string]any)
	if provider["name"] != "OpenAI" || provider["wire_api"] != "responses" ||
		provider["requires_openai_auth"] != true || provider["supports_websockets"] != false ||
		provider["request_max_retries"] != int64(0) || provider["stream_max_retries"] != int64(0) {
		t.Fatalf("invalid provider contract: %#v", provider)
	}
	for _, key := range []string{"base_url", "env_key", "experimental_bearer_token", "auth"} {
		if _, present := provider[key]; present {
			t.Fatalf("native ChatGPT authority overridden by %s", key)
		}
	}
	if !slices.Contains(args, "unbounded_connection_retries") {
		t.Fatal("unbounded connection retries remain enabled")
	}
}

// Optional binary conformance, never a real-provider test. The exact deployed
// CLI talks only to this loopback fixture and receives an empty private home.
func TestCodexCLIPinnedBinarySingleAttemptTransport(t *testing.T) {
	binary := os.Getenv("SENTINEL_TEST_CODEX_CLI_BINARY")
	if binary == "" {
		t.Skip("set SENTINEL_TEST_CODEX_CLI_BINARY to the pinned CLI for local conformance")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	version, err := exec.CommandContext(ctx, binary, "--version").Output() //nolint:gosec // explicitly selected local test binary
	if err != nil || strings.TrimSpace(string(version)) != "codex-cli "+pinnedCodexCLIVersion {
		t.Fatalf("pinned test binary unavailable: %v", err)
	}
	for _, outcome := range []string{"http_500", "stream_disconnect", "completed"} {
		t.Run(outcome, func(t *testing.T) {
			var mu sync.Mutex
			var requests []map[string]any
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if r.Header.Get("Authorization") != "" {
					t.Error("loopback fixture must never receive credentials")
				}
				if r.Method != http.MethodPost || !strings.Contains(r.URL.Path, "/responses") {
					http.Error(w, "fixture rejects unrelated endpoint", http.StatusNotFound)
					return
				}
				var body map[string]any
				if err := json.NewDecoder(io.LimitReader(r.Body, 1024*1024)).Decode(&body); err != nil {
					http.Error(w, "invalid fixture request", http.StatusBadRequest)
					return
				}
				mu.Lock()
				requests = append(requests, body)
				mu.Unlock()
				if outcome == "http_500" {
					http.Error(w, `{"error":{"message":"fixture failure","type":"server_error"}}`, http.StatusInternalServerError)
					return
				}
				w.Header().Set("Content-Type", "text/event-stream")
				_, _ = io.WriteString(w, "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"fixture-response\"}}\n\n")
				if outcome == "completed" {
					_, _ = io.WriteString(w, "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"id\":\"fixture-message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Fixture response\"}]}}\n\n")
					_, _ = io.WriteString(w, "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"fixture-response\",\"usage\":{\"input_tokens\":12,\"output_tokens\":2,\"total_tokens\":14}}}\n\n")
				}
				w.(http.Flusher).Flush()
			}))
			defer server.Close()
			workdir, home := t.TempDir(), t.TempDir()
			p := NewCodexCLIProvider(ProviderConfig{Name: CodexCLIProviderName}, nil)
			p.workdir, p.home, p.codexHome = workdir, home, home
			args := p.commandArgs(defaultCodexCLIModel)
			args = append(args[:len(args)-1],
				"-c", fmt.Sprintf("model_providers.sentinel_chatgpt.base_url=%q", server.URL+"/v1"),
				"-c", "model_providers.sentinel_chatgpt.requires_openai_auth=false", "-")
			runCtx, stop := context.WithTimeout(context.Background(), 15*time.Second)
			defer stop()
			cmd := exec.CommandContext(runCtx, binary, args...) //nolint:gosec // pinned test executable, loopback-only fixture args
			cmd.Dir, cmd.Env = workdir, p.commandEnv()
			cmd.Stdin = strings.NewReader("Return one short word. Do not use tools.")
			stdout := &cappedBuffer{limit: codexCLIMaxDiagnosticSize}
			stderr := &cappedBuffer{limit: codexCLIMaxDiagnosticSize}
			cmd.Stdout, cmd.Stderr = stdout, stderr
			runErr := cmd.Run()
			if runCtx.Err() != nil || (runErr == nil) != (outcome == "completed") {
				t.Fatalf("unexpected terminal result: %v, context %v: %s %s", runErr, runCtx.Err(), stdout.String(), stderr.String())
			}
			if outcome == "completed" {
				response, parseErr := p.parseOutputStream(strings.NewReader(stdout.String()), codexCLIMaxResponseBytes)
				if parseErr != nil || response.Content != "Fixture response" || response.InputTokens != 12 || response.OutputTokens != 2 {
					t.Fatalf("pinned response/usage not preserved: %+v, %v", response, parseErr)
				}
			}
			mu.Lock()
			defer mu.Unlock()
			if len(requests) != 1 {
				t.Fatalf("provider requests=%d, expected exactly one: %s %s", len(requests), stdout.String(), stderr.String())
			}
			assertCodexCLIInferenceOnlyRequest(t, requests[0])
		})
	}
}

func assertCodexCLIInferenceOnlyRequest(t *testing.T, request map[string]any) {
	t.Helper()
	if value, present := request["tools"]; present {
		if tools, ok := value.([]any); !ok || len(tools) != 0 {
			t.Fatalf("inference-only CLI advertised tools: %#v", value)
		}
	}
	if request["model"] != defaultCodexCLIModel {
		t.Fatal("pinned CLI changed the selected model")
	}
	if _, present := request["max_output_tokens"]; present {
		t.Fatal("pinned CLI transport capability changed; re-review the token-limit contract")
	}
}

func TestCodexCLIProviderRejectsNonPrivateOrNonEmptyWorkdir(t *testing.T) {
	dir := t.TempDir()
	if err := os.Chmod(dir, 0o700); err != nil { //nolint:gosec // test models the private production workdir
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "unexpected"), []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := validateCodexCLIWorkdir(dir); err == nil || !strings.Contains(err.Error(), "empty") {
		t.Fatalf("error=%v", err)
	}
	if err := os.Remove(filepath.Join(dir, "unexpected")); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(dir, 0o755); err != nil { //nolint:gosec // intentionally insecure negative fixture
		t.Fatal(err)
	}
	if err := validateCodexCLIWorkdir(dir); err == nil || !strings.Contains(err.Error(), "private") {
		t.Fatalf("error=%v", err)
	}
}

func TestCodexCLIProviderRejectsWorkdirThroughSymlink(t *testing.T) {
	root := t.TempDir()
	realDir := filepath.Join(root, "real")
	if err := os.Mkdir(realDir, 0o700); err != nil {
		t.Fatal(err)
	}
	linkDir := filepath.Join(root, "link")
	if err := os.Symlink(realDir, linkDir); err != nil {
		t.Fatal(err)
	}
	if err := validateCodexCLIWorkdir(linkDir); err == nil || !strings.Contains(err.Error(), "symlink") {
		t.Fatalf("error=%v", err)
	}
}

func TestCodexCLIReadinessRequiresChatGPTLoginAndValidWorkdir(t *testing.T) {
	workdir := t.TempDir()
	if err := os.Chmod(workdir, 0o700); err != nil { //nolint:gosec // test models the private production workdir
		t.Fatal(err)
	}
	scriptPath := filepath.Join(t.TempDir(), "codex")
	script := `#!/bin/sh
case "$1" in
  --version) printf 'codex-cli 0.151.0\n' ;;
  login) printf 'Logged in using an API key\n' ;;
  *) exit 64 ;;
esac
`
	if err := os.WriteFile(scriptPath, []byte(script), 0o700); err != nil { //nolint:gosec // executable test fixture
		t.Fatal(err)
	}
	t.Setenv("CODEX_CLI_WORKDIR", workdir)
	provider := NewCodexCLIProvider(ProviderConfig{Name: CodexCLIProviderName, BaseURL: scriptPath}, nil)
	if err := provider.ReadinessCheck(context.Background()); err == nil || !strings.Contains(err.Error(), "ChatGPT") {
		t.Fatalf("error=%v", err)
	}

	if err := os.WriteFile(filepath.Join(workdir, "unexpected"), []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := provider.ReadinessCheck(context.Background()); err == nil || !strings.Contains(err.Error(), "empty") {
		t.Fatalf("error=%v", err)
	}
}

func TestCodexCLIReadinessRejectsWrongVersionAndMisleadingLoginText(t *testing.T) {
	workdir := t.TempDir()
	if err := os.Chmod(workdir, 0o700); err != nil { //nolint:gosec // test models the private production workdir
		t.Fatal(err)
	}
	t.Setenv("CODEX_CLI_WORKDIR", workdir)

	for name, script := range map[string]string{
		"wrong version": `#!/bin/sh
case "$1" in
  --version) printf 'codex-cli 0.150.0\n' ;;
  login) printf 'Logged in using ChatGPT\n' ;;
  *) exit 64 ;;
esac
`,
		"misleading login": `#!/bin/sh
case "$1" in
  --version) printf 'codex-cli 0.151.0\n' ;;
  login) printf 'Not Logged in using ChatGPT\n' ;;
  *) exit 64 ;;
esac
`,
	} {
		t.Run(name, func(t *testing.T) {
			scriptPath := filepath.Join(t.TempDir(), "codex")
			if err := os.WriteFile(scriptPath, []byte(script), 0o700); err != nil { //nolint:gosec // executable test fixture
				t.Fatal(err)
			}
			provider := NewCodexCLIProvider(ProviderConfig{Name: CodexCLIProviderName, BaseURL: scriptPath}, nil)
			if err := provider.ReadinessCheck(context.Background()); err == nil {
				t.Fatal("invalid runtime readiness accepted")
			}
		})
	}
}

func TestCodexCLIProviderSanitizesAuthenticationFailure(t *testing.T) {
	workdir := t.TempDir()
	if err := os.Chmod(workdir, 0o700); err != nil { //nolint:gosec // test models the private production workdir
		t.Fatal(err)
	}
	scriptPath := filepath.Join(t.TempDir(), "codex")
	script := "#!/bin/sh\nprintf '%s\\n' 'Not logged in: secret diagnostic' >&2\nexit 1\n"
	if err := os.WriteFile(scriptPath, []byte(script), 0o700); err != nil { //nolint:gosec // executable test fixture
		t.Fatal(err)
	}
	t.Setenv("CODEX_CLI_WORKDIR", workdir)
	provider := NewCodexCLIProvider(ProviderConfig{Name: CodexCLIProviderName, BaseURL: scriptPath}, nil)
	_, err := provider.Send(context.Background(), &LLMRequest{Messages: []Message{{Role: "user", Content: "hello"}}})
	var providerErr *ProviderError
	if !errors.As(err, &providerErr) || providerErr.StatusCode != http.StatusServiceUnavailable {
		t.Fatalf("error=%v", err)
	}
	if strings.Contains(err.Error(), "secret diagnostic") {
		t.Fatalf("diagnostic leaked: %v", err)
	}
}

func TestNewProviderFromConfigCodexCLI(t *testing.T) {
	provider, err := NewProviderFromConfig(ProviderConfig{Name: CodexCLIProviderName, Type: CodexCLIProviderName})
	if err != nil {
		t.Fatal(err)
	}
	if provider.Name() != CodexCLIProviderName {
		t.Fatalf("name=%q", provider.Name())
	}
}

func readTestFile(t *testing.T, path string) string {
	t.Helper()
	data, err := os.ReadFile(path) //nolint:gosec // path is a test-owned temporary file
	if err != nil {
		t.Fatal(err)
	}
	return string(data)
}
