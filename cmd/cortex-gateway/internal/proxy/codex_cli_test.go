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
	"reflect"
	"slices"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/BurntSushi/toml"
)

func TestCodexCLIReasoningAndPrivateErrorClassification(t *testing.T) {
	provider := NewCodexCLIProvider(ProviderConfig{Name: CodexCLIProviderName}, nil)
	for _, model := range []string{"gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"} {
		args := provider.commandArgs(model)
		if !slices.Contains(args, `model_reasoning_effort="low"`) || slices.Contains(args, `model_reasoning_effort="none"`) {
			t.Fatalf("unsupported reasoning arguments for %s", model)
		}
	}
	for _, eventType := range []string{"error", "turn.failed"} {
		event := codexCLIEvent{Type: eventType, Message: "Unsupported reasoning effort; private-secret"}
		if eventType == "turn.failed" {
			event.Error = &codexCLIError{Message: event.Message}
			event.Message = ""
		}
		encoded, err := json.Marshal(event)
		if err != nil {
			t.Fatal(err)
		}
		response, err := provider.parseOutputStream(strings.NewReader(string(encoded)), 1024)
		if response != nil || err == nil || err.Error() != "codex-cli reasoning configuration unsupported" {
			t.Fatalf("response=%v error=%v", response, err)
		}
	}
	err := codexCLIStreamError(codexCLIEvent{Message: "private-secret arbitrary unknown failure"})
	if err.Error() != "codex-cli subprocess failed" {
		t.Fatalf("private upstream details escaped: %v", err)
	}
	classifications := map[string]string{
		"model_not_found: private model name": "codex-cli model unavailable",
		"HTTP 403 forbidden: private account": "codex-cli access unavailable",
		"error sending request: private host": "codex-cli transport unavailable",
		"invalid request: private prompt":     "codex-cli request rejected",
	}
	for diagnostic, expected := range classifications {
		got := codexCLIProcessError(diagnostic).Error()
		if !strings.Contains(got, expected) || strings.Contains(got, "private") {
			t.Fatalf("diagnostic %q classified as %q, want private-safe %q", diagnostic, got, expected)
		}
	}
}

func TestCodexCLIOutputSchemaErrorClassification(t *testing.T) {
	for _, test := range []struct {
		name               string
		diagnostic         string
		statusCode         int
		message            string
		diagnosticCategory ProviderDiagnosticCategory
	}{
		{
			name:               "invalid schema",
			diagnostic:         "Invalid schema for response_format: private-upstream; token=private-secret",
			statusCode:         http.StatusBadGateway,
			message:            "codex-cli output schema rejected",
			diagnosticCategory: ProviderDiagnosticCodexOutputSchemaRejected,
		},
		{
			name:               "invalid_json_schema",
			diagnostic:         "HTTP status 400 bad request: INVALID_JSON_SCHEMA; private-upstream; token=private-secret",
			statusCode:         http.StatusBadGateway,
			message:            "codex-cli output schema rejected",
			diagnosticCategory: ProviderDiagnosticCodexOutputSchemaRejected,
		},
		{
			name:               "schema keyword not permitted",
			diagnostic:         "Invalid request: output schema keyword uniqueItems is not permitted; private-upstream; token=private-secret",
			statusCode:         http.StatusBadGateway,
			message:            "codex-cli output schema rejected",
			diagnosticCategory: ProviderDiagnosticCodexOutputSchemaRejected,
		},
		{
			name:       "quota",
			diagnostic: "Usage limit reached; private-upstream; token=private-secret",
			statusCode: http.StatusTooManyRequests,
			message:    "codex-cli usage limit active",
		},
		{
			name:       "authentication",
			diagnostic: "Not logged in: authentication required; private-upstream; token=private-secret",
			statusCode: http.StatusServiceUnavailable,
			message:    "codex-cli authentication unavailable",
		},
		{
			name:       "quota takes priority over schema",
			diagnostic: "Invalid schema: usage limit reached; private-upstream; token=private-secret",
			statusCode: http.StatusTooManyRequests,
			message:    "codex-cli usage limit active",
		},
		{
			name:       "authentication takes priority over schema",
			diagnostic: "invalid_json_schema: authentication required; private-upstream; token=private-secret",
			statusCode: http.StatusServiceUnavailable,
			message:    "codex-cli authentication unavailable",
		},
		{
			name:       "generic failure",
			diagnostic: "Unrelated subprocess failure; private-upstream; token=private-secret",
			message:    "codex-cli subprocess failed",
		},
		{
			name:       "schema mention alone",
			diagnostic: "Output schema processing failed; private-upstream; token=private-secret",
			message:    "codex-cli subprocess failed",
		},
		{
			name:       "not permitted without schema",
			diagnostic: "Operation not permitted; private-upstream; token=private-secret",
			message:    "codex-cli subprocess failed",
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			for _, route := range []string{"stderr helper", "error", "turn.failed"} {
				t.Run(route, func(t *testing.T) {
					var err error
					if route == "stderr helper" {
						err = codexCLIProcessError(test.diagnostic)
					} else {
						event := codexCLIEvent{Type: route, Message: test.diagnostic}
						if route == "turn.failed" {
							event.Message = ""
							event.Error = &codexCLIError{Message: test.diagnostic}
						}
						encoded, marshalErr := json.Marshal(event)
						if marshalErr != nil {
							t.Fatal(marshalErr)
						}
						provider := NewCodexCLIProvider(ProviderConfig{Name: CodexCLIProviderName}, nil)
						var response *LLMResponse
						response, err = provider.parseOutputStream(strings.NewReader(string(encoded)), 1024)
						if response != nil {
							t.Fatalf("failure returned a response: %v", response)
						}
					}
					if err == nil {
						t.Fatal("diagnostic did not return an error")
					}
					var providerErr *ProviderError
					isProviderError := errors.As(err, &providerErr)
					expectedError := test.message
					if test.statusCode == 0 {
						if isProviderError {
							t.Fatalf("generic failure classified as ProviderError: %v", err)
						}
					} else {
						if !isProviderError || providerErr.StatusCode != test.statusCode || providerErr.Message != test.message {
							t.Fatalf("want ProviderError HTTP %d message %q, got %T: %v", test.statusCode, test.message, err, err)
						}
						expectedError = fmt.Sprintf("provider error: HTTP %d: %s", test.statusCode, test.message)
					}
					if err.Error() != expectedError {
						t.Fatalf("want fixed sanitized error %q, got %q", expectedError, err.Error())
					}
					if isProviderError && providerErr.Diagnostic != test.diagnosticCategory {
						t.Fatalf("diagnostic category=%d want=%d", providerErr.Diagnostic, test.diagnosticCategory)
					}
					if test.message == "codex-cli output schema rejected" {
						var admissionErr *ProviderAdmissionError
						if errors.As(err, &admissionErr) {
							t.Fatalf("schema rejection classified as admission error: %v", err)
						}
					}
				})
			}
		})
	}
}

func TestCodexCLIOutputSchemaStderrFailure(t *testing.T) {
	// A complete stream leaves the nonzero process exit to classify stderr.
	stdout := strings.Join([]string{
		`{"type":"thread.started","thread_id":"thread-1"}`,
		`{"type":"turn.started"}`,
		`{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"Pong"}}`,
		`{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}`,
	}, "\n")
	fixture, callsPath := newCodexCLIFailureFixture(t, stdout,
		"Invalid schema: keyword uniqueItems is not permitted; private-upstream; token=private-secret", 1)
	response, err := fixture.provider.Send(context.Background(), fixture.request)
	if response != nil {
		t.Fatalf("failed subprocess returned a response: %v", response)
	}
	var providerErr *ProviderError
	var admissionErr *ProviderAdmissionError
	if !errors.As(err, &providerErr) || providerErr.StatusCode != http.StatusBadGateway ||
		providerErr.Message != "codex-cli output schema rejected" || errors.As(err, &admissionErr) {
		t.Fatalf("want raw sanitized schema ProviderError, got %T: %v", err, err)
	}
	if err.Error() != "provider error: HTTP 502: codex-cli output schema rejected" {
		t.Fatalf("stderr diagnostic leaked or category changed: %v", err)
	}
	if providerErr.Diagnostic != ProviderDiagnosticCodexOutputSchemaRejected {
		t.Fatalf("stderr diagnostic category=%d", providerErr.Diagnostic)
	}
	if got := readTestFile(t, callsPath); got != "called\n" {
		t.Fatalf("want exactly one fake subprocess call, got %q", got)
	}
}

type structuredCodexFixture struct {
	provider   *CodexCLIProvider
	request    *LLMRequest
	workdir    string
	argsPath   string
	schemaPath string
}

func newStructuredCodexFixture(t *testing.T) structuredCodexFixture {
	workdir := t.TempDir()
	if err := os.Chmod(workdir, 0o700); err != nil { //nolint:gosec // private provider workdir fixture
		t.Fatal(err)
	}
	artifacts := t.TempDir()
	argsPath := filepath.Join(artifacts, "args")
	schemaPath := filepath.Join(artifacts, "schema")
	scriptPath := filepath.Join(artifacts, "codex")
	script := fmt.Sprintf(`#!/bin/sh
set -eu
printf '%%s\n' "$@" > %q
schema=''
previous=''
for arg do
  if [ "$previous" = '--output-schema' ]; then schema="$arg"; fi
  previous="$arg"
done
if [ -n "$schema" ]; then cp "$schema" %q; fi
cat >/dev/null
printf '%%s\n' '{"type":"thread.started","thread_id":"thread-1"}'
printf '%%s\n' '{"type":"turn.started"}'
printf '%%s\n' '{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"Pong"}}'
printf '%%s\n' '{"type":"turn.completed","usage":{"input_tokens":1,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":1,"reasoning_output_tokens":0}}'
`, argsPath, schemaPath)
	if err := os.WriteFile(scriptPath, []byte(script), 0o700); err != nil { //nolint:gosec // executable test fixture
		t.Fatal(err)
	}
	t.Setenv("CODEX_CLI_WORKDIR", workdir)
	provider := NewCodexCLIProvider(ProviderConfig{Name: CodexCLIProviderName, BaseURL: scriptPath}, nil)
	request := &LLMRequest{
		Messages:  []Message{{Role: "user", Content: "Return a project tool plan."}},
		MaxTokens: 64,
		Metadata:  map[string]string{"company_execution_schema": "1"},
	}
	return structuredCodexFixture{
		provider:   provider,
		request:    request,
		workdir:    workdir,
		argsPath:   argsPath,
		schemaPath: schemaPath,
	}
}

func (f structuredCodexFixture) send(t *testing.T, outputKind string) map[string]any {
	t.Helper()
	if outputKind == "" {
		delete(f.request.Metadata, "company_execution_output_kind")
	} else {
		f.request.Metadata["company_execution_output_kind"] = outputKind
	}
	if _, err := f.provider.Send(context.Background(), f.request); err != nil {
		t.Fatal(err)
	}
	var schema map[string]any
	if err := json.Unmarshal([]byte(readTestFile(t, f.schemaPath)), &schema); err != nil {
		t.Fatal(err)
	}
	return schema
}

func TestCodexCLIProjectWorkUsesStructuredFinalResponse(t *testing.T) {
	fixture := newStructuredCodexFixture(t)
	schema := fixture.send(t, "")
	args := strings.Fields(readTestFile(t, fixture.argsPath))
	if schema["type"] != "object" || !slices.Contains(args, "--output-schema") {
		t.Fatalf("project work did not receive its output schema: %v", args)
	}

	if files, err := filepath.Glob(filepath.Join(fixture.workdir, ".codex-work-schema-*.json")); err != nil || len(files) != 0 {
		t.Fatalf("schema files retained: %v, %v", files, err)
	}

	schema = fixture.send(t, "source_review")
	properties, ok := schema["properties"].(map[string]any)
	if !ok || properties["verdict"] == nil || properties["source_files"] == nil || properties["tools"] != nil {
		t.Fatalf("QA received the wrong output contract: %v", properties)
	}

	fixture.request.Metadata["company_execution_output_kind"] = "unexpected"
	if _, err := fixture.provider.Send(context.Background(), fixture.request); err == nil {
		t.Fatal("unknown company output contract accepted")
	}

	fixture.request.Metadata["company_execution_schema"] = "3"
	schema = fixture.send(t, "adaptive_decision")
	properties, ok = schema["properties"].(map[string]any)
	if !ok || properties["decision"] == nil || properties["tools"] != nil || properties["verdict"] != nil {
		t.Fatalf("adaptive turn received the wrong output contract: %v", properties)
	}

	fixture.request.Metadata["company_execution_output_kind"] = "source_review"
	if _, err := fixture.provider.Send(context.Background(), fixture.request); err == nil {
		t.Fatal("QA output contract accepted for an adaptive turn")
	}
	fixture.request.Metadata = nil
	fixture.send(t, "")
	args = strings.Fields(readTestFile(t, fixture.argsPath))
	if slices.Contains(args, "--output-schema") {
		t.Fatalf("ordinary request inherited project work schema: %v", args)
	}
}

func TestCodexCLIAdaptiveSchemaDiscoveryContract(t *testing.T) {
	fixture := newStructuredCodexFixture(t)
	fixture.request.Metadata["company_execution_schema"] = "3"
	schema := fixture.send(t, "adaptive_decision")
	encoded, err := json.Marshal(schema)
	if err != nil {
		t.Fatal(err)
	}
	// Inspect the actual schema passed to the subprocess, not a second validator.
	type schemaNode struct {
		Properties map[string]schemaNode `json:"properties"`
		AnyOf      []schemaNode          `json:"anyOf"`
		Enum       []any                 `json:"enum"`
	}
	var root schemaNode
	if err := json.Unmarshal(encoded, &root); err != nil {
		t.Fatal(err)
	}
	discriminator := func(node schemaNode) string {
		t.Helper()
		if len(node.Enum) != 1 {
			t.Fatalf("discriminator must be a singleton: %v", node.Enum)
		}
		name, ok := node.Enum[0].(string)
		if !ok {
			t.Fatalf("discriminator must be a string: %v", node.Enum)
		}
		return name
	}
	kinds := make([]string, 0, len(root.Properties["decision"].AnyOf))
	tools := make([]string, 0, 7)
	for _, decision := range root.Properties["decision"].AnyOf {
		kind := discriminator(decision.Properties["kind"])
		kinds = append(kinds, kind)
		if kind == "tool" {
			for _, tool := range decision.Properties["tool"].AnyOf {
				tools = append(tools, discriminator(tool.Properties["tool"]))
			}
		}
	}
	slices.Sort(kinds)
	slices.Sort(tools)
	if !slices.Equal(kinds, []string{"blocked", "collaborate", "propose_completion", "tool"}) {
		t.Fatalf("adaptive decision choices changed: %v", kinds)
	}
	if !slices.Equal(tools, []string{"apply_patch", "inspect_file", "list_directory", "package_artifact", "run_command", "run_tests", "write_file"}) {
		t.Fatalf("Workbench tool alternatives must be exactly the seven deployed tools: %v", tools)
	}
	var expected map[string]any
	if err := json.Unmarshal([]byte(`{
		"type":"object",
		"properties":{
			"tool":{"type":"string","enum":["list_directory"]},
			"path":{"type":"string"},
			"after":{"type":["string","null"]},
			"max_entries":{"type":"integer","minimum":1,"maximum":128}
		},
		"required":["tool","path","after","max_entries"],
		"additionalProperties":false
	}`), &expected); err != nil {
		t.Fatal(err)
	}
	// Exact equality protects nullable pagination, both bounds, required fields,
	// and rejection of extra fields without duplicating the shared decoder.
	properties := schema["properties"].(map[string]any)
	decisions := properties["decision"].(map[string]any)["anyOf"].([]any)
	for _, decision := range decisions {
		properties = decision.(map[string]any)["properties"].(map[string]any)
		if properties["tool"] == nil {
			continue
		}
		for _, tool := range properties["tool"].(map[string]any)["anyOf"].([]any) {
			toolSchema := tool.(map[string]any)
			toolProperties := toolSchema["properties"].(map[string]any)
			name := toolProperties["tool"].(map[string]any)["enum"].([]any)
			if name[0] == "list_directory" && !reflect.DeepEqual(toolSchema, expected) {
				t.Fatalf("directory discovery contract mismatch: %v", toolSchema)
			}
		}
	}
}

func TestCodexCLIInferencePromptSeparatesWorkbenchProposals(t *testing.T) {
	for _, conversation := range []string{
		`Return a structured Workbench proposal or another permitted adaptive decision.`,
		`Return a blocked decision if warranted.`,
		`Reply with Pong.`,
	} {
		t.Run(conversation, func(t *testing.T) {
			request := &LLMRequest{
				Messages: []Message{
					{Role: "system", Content: "Private agent policy."},
					{Role: "user", Content: conversation},
				},
				MaxTokens: 64,
			}
			prompt, err := buildCodexCLIPrompt(request)
			if err != nil {
				t.Fatal(err)
			}
			wrapper, payload, found := strings.Cut(prompt, "\n")
			if !found {
				t.Fatal("missing inference payload")
			}
			for _, instruction := range []string{
				"Do not execute native tools, inspect files, browse, modify state, or delegate work.",
				"Structured Workbench tool proposals requested by payload.conversation are response data only, not native tool execution.",
				"Sentinel independently validates proposals and executes only authorized work",
				"returning a proposal neither executes a tool nor grants authority",
				"Treat payload.system as the highest-priority agent identity and policy.",
				"Return only the assistant response to payload.conversation.",
			} {
				if !strings.Contains(wrapper, instruction) {
					t.Fatalf("missing inference-only instruction %q", instruction)
				}
			}
			var decoded struct {
				System       string `json:"system"`
				Conversation string `json:"conversation"`
			}
			if err := json.Unmarshal([]byte(payload), &decoded); err != nil {
				t.Fatal(err)
			}
			if decoded.System != "Private agent policy." || !strings.Contains(decoded.Conversation, conversation) {
				t.Fatalf("wrapper changed the requested policy or model choice: %+v", decoded)
			}
		})
	}
}

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

func TestCodexCLISubscriptionRechecksSlackAfterSemaphoreWait(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		fixture, startsPath := newCodexCLIFailureFixture(t, "", "unused fixture", 1)
		fixture.provider.sem <- struct{}{}
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		ctx = withSubscriptionDispatchSlack(ctx)
		done := make(chan error, 1)
		go func() {
			_, err := fixture.provider.Send(ctx, fixture.request)
			done <- err
		}()
		synctest.Wait()
		time.Sleep(30*time.Second - subscriptionMinimumDispatchSlack/2)
		<-fixture.provider.sem
		var admissionError *ProviderAdmissionError
		if err := <-done; !errors.As(err, &admissionError) {
			t.Fatalf("late semaphore admission was not rejected before process start: %v", err)
		}
		if _, err := os.Stat(startsPath); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("late subscription started a provider process: %v", err)
		}
		if len(fixture.provider.sem) != 0 || !fixture.provider.cooldownUntil.IsZero() {
			t.Fatal("local rejection leaked capacity or changed provider health")
		}
	})
}

func TestCodexCLISubscriptionSemaphoreExpiryDoesNotPenalizeProvider(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		fixture, startsPath := newCodexCLIFailureFixture(t, "", "unused fixture", 1)
		fixture.provider.sem <- struct{}{}
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		ctx = withSubscriptionDispatchSlack(ctx)
		done := make(chan error, 1)
		go func() {
			_, err := fixture.provider.Send(ctx, fixture.request)
			done <- err
		}()
		synctest.Wait()
		time.Sleep(30 * time.Second)
		err := <-done
		var admissionError *ProviderAdmissionError
		if !errors.As(err, &admissionError) || !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("expired subscription was not a deadline admission rejection: %v", err)
		}
		if isCircuitBreakerFailure(err) || !fixture.provider.cooldownUntil.IsZero() {
			t.Fatal("queue expiry penalized provider health")
		}
		if _, err := os.Stat(startsPath); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("expired subscription started a provider process: %v", err)
		}
		if len(fixture.provider.sem) != 1 {
			t.Fatal("expired subscription changed another request's capacity")
		}
		<-fixture.provider.sem
	})
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

func TestCodexCLIModelWorkUsesIndependentStructuredResponseLimit(t *testing.T) {
	modelWork := &LLMRequest{
		MaxTokens: 1024,
		Metadata: map[string]string{
			"company_execution_schema": "1",
		},
	}
	if got := codexCLIResponseByteLimit(modelWork); got != maxModelWorkResponseBytes {
		t.Fatalf("model-work limit=%d, want %d", got, maxModelWorkResponseBytes)
	}
	regular := &LLMRequest{MaxTokens: 1024, Metadata: map[string]string{}}
	if got := codexCLIResponseByteLimit(regular); got != 8192 {
		t.Fatalf("regular limit=%d, want 8192", got)
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

func newCodexCLIFailureFixture(t *testing.T, stdout, diagnostic string, exitCode int) (structuredCodexFixture, string) {
	t.Helper()
	fixture := newStructuredCodexFixture(t)
	fixture.request.Metadata = nil
	callsPath := filepath.Join(t.TempDir(), "calls")
	script := fmt.Sprintf("#!/bin/sh\nset -eu\nprintf 'called\\n' >> %q\ncat >/dev/null\nprintf '%%b\\n' %q\nprintf '%%s\\n' %q >&2\nexit %d\n",
		callsPath, stdout, diagnostic, exitCode)
	if err := os.WriteFile(fixture.provider.binary, []byte(script), 0o700); err != nil { //nolint:gosec // executable test fixture
		t.Fatal(err)
	}
	return fixture, callsPath
}

func assertCodexCLIQuotaError(t *testing.T, err error) {
	t.Helper()
	var providerErr *ProviderError
	var admissionErr *ProviderAdmissionError
	if !errors.As(err, &providerErr) || providerErr.StatusCode != http.StatusTooManyRequests ||
		providerErr.Message != "codex-cli usage limit active" || errors.As(err, &admissionErr) {
		t.Fatalf("want raw sanitized provider quota error, got %T: %v", err, err)
	}
}

func TestCodexCLIQuotaCachesOnlyActualSendFailures(t *testing.T) {
	for _, test := range []struct {
		name, stdout, diagnostic string
		exitCode                 int
	}{
		{name: "stderr", diagnostic: "usage limit reached; private-secret; resets tomorrow", exitCode: 1},
		{name: "error event", stdout: `{"type":"error","message":"rate limit private-secret"}`},
		{name: "turn failure", stdout: "{\"type\":\"thread.started\",\"thread_id\":\"thread-1\"}\n{\"type\":\"turn.started\"}\n" +
			`{"type":"turn.failed","error":{"message":"usage limit private-secret"}}`},
	} {
		t.Run(test.name, func(t *testing.T) {
			fixture, callsPath := newCodexCLIFailureFixture(t, test.stdout, test.diagnostic, test.exitCode)
			if err := fixture.provider.CurrentProviderError(); err != nil {
				t.Fatalf("new provider has cached status: %v", err)
			}
			if _, err := os.Stat(callsPath); !errors.Is(err, os.ErrNotExist) {
				t.Fatalf("status check spawned a subprocess: %v", err)
			}
			_, err := fixture.provider.Send(context.Background(), fixture.request)
			assertCodexCLIQuotaError(t, err)
			assertCodexCLIQuotaError(t, fixture.provider.CurrentProviderError())
			_, err = fixture.provider.Send(context.Background(), fixture.request)
			assertCodexCLIQuotaError(t, err)
			if got := readTestFile(t, callsPath); got != "called\n" {
				t.Fatalf("known quota spawned another subprocess: %q", got)
			}
		})
	}
}

func TestCodexCLIQuotaConcurrencyAndExpiry(t *testing.T) {
	fixture, callsPath := newCodexCLIFailureFixture(t, "", "usage limit", 1)
	now := time.Date(2026, 9, 30, 12, 0, 0, 0, time.UTC)
	fixture.provider.now = func() time.Time { return now }
	_, err := fixture.provider.Send(context.Background(), fixture.request)
	assertCodexCLIQuotaError(t, err)
	var wg sync.WaitGroup
	for range 32 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			assertCodexCLIQuotaError(t, fixture.provider.CurrentProviderError())
			_, err := fixture.provider.Send(context.Background(), fixture.request)
			assertCodexCLIQuotaError(t, err)
		}()
	}
	wg.Wait()
	if got := readTestFile(t, callsPath); got != "called\n" {
		t.Fatalf("concurrent cached calls spawned a subprocess: %q", got)
	}
	now = now.Add(codexCLIUsageLimitCooldown - time.Nanosecond)
	assertCodexCLIQuotaError(t, fixture.provider.CurrentProviderError())
	now = now.Add(time.Nanosecond)
	if err := fixture.provider.CurrentProviderError(); err != nil {
		t.Fatalf("local cooldown did not expire at its boundary: %v", err)
	}
	if got := readTestFile(t, callsPath); got != "called\n" {
		t.Fatalf("expiry check spawned a subprocess: %q", got)
	}
	_, err = fixture.provider.Send(context.Background(), fixture.request)
	assertCodexCLIQuotaError(t, err)
	if got := readTestFile(t, callsPath); got != "called\ncalled\n" {
		t.Fatalf("expiry did not permit exactly one new attempt: %q", got)
	}
	assertCodexCLIQuotaError(t, fixture.provider.CurrentProviderError())
}

func TestCodexCLIQuotaRechecksAfterSemaphoreWait(t *testing.T) {
	fixture, callsPath := newCodexCLIFailureFixture(t, "", "usage limit", 1)
	// An expired prior failure lets the request pass the first status check.
	fixture.provider.cooldownUntil = time.Unix(1, 0)
	checked := make(chan struct{}, 1)
	fixture.provider.now = func() time.Time {
		select {
		case checked <- struct{}{}:
		default:
		}
		return time.Unix(2, 0)
	}
	fixture.provider.sem <- struct{}{}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	done := make(chan error, 1)
	go func() {
		_, err := fixture.provider.Send(ctx, fixture.request)
		done <- err
	}()
	select {
	case <-checked:
	case <-ctx.Done():
		t.Fatal("request did not check status before semaphore wait")
	}
	// Model the preceding in-flight Send recording its final classified error.
	fixture.provider.rememberUsageLimit(codexCLIProcessError("usage limit"))
	<-fixture.provider.sem
	assertCodexCLIQuotaError(t, <-done)
	if _, err := os.Stat(callsPath); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("quota learned during semaphore wait spawned a subprocess: %v", err)
	}
}

func TestCodexCLIQuotaSuccessfulInflightSendPreservesNewerCooldown(t *testing.T) {
	fixture := newStructuredCodexFixture(t)
	fixture.request.Metadata = nil
	now := time.Date(2026, 9, 30, 12, 0, 0, 0, time.UTC)
	fixture.provider.now = func() time.Time { return now }
	barrierDir := t.TempDir()
	startedPath := filepath.Join(barrierDir, "started")
	releasePath := filepath.Join(barrierDir, "release")
	script := readTestFile(t, fixture.provider.binary)
	barrier := fmt.Sprintf("set -eu\nprintf 'started\\n' > %q\nwhile [ ! -f %q ]; do sleep 0.01; done\n", startedPath, releasePath)
	script = strings.Replace(script, "set -eu\n", barrier, 1)
	if err := os.WriteFile(fixture.provider.binary, []byte(script), 0o700); err != nil { //nolint:gosec // executable test fixture
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	type result struct {
		response *LLMResponse
		err      error
	}
	done := make(chan result, 1)
	go func() {
		response, err := fixture.provider.Send(ctx, fixture.request)
		done <- result{response: response, err: err}
	}()
	ticker := time.NewTicker(time.Millisecond)
	defer ticker.Stop()
	for {
		if _, err := os.Stat(startedPath); err == nil {
			break
		} else if !errors.Is(err, os.ErrNotExist) {
			t.Fatal(err)
		}
		select {
		case completed := <-done:
			t.Fatalf("Send completed before reaching the barrier: %+v", completed)
		case <-ctx.Done():
			t.Fatal("Send did not reach the subprocess barrier")
		case <-ticker.C:
		}
	}
	// Independently establish newer status after the successful attempt has
	// started, while the barrier prevents its completion defer from running.
	fixture.provider.rememberUsageLimit(codexCLIProcessError("usage limit"))
	assertCodexCLIQuotaError(t, fixture.provider.CurrentProviderError())
	if err := os.WriteFile(releasePath, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	select {
	case completed := <-done:
		if completed.err != nil || completed.response == nil || completed.response.Content != "Pong" {
			t.Fatalf("in-flight Send did not actually succeed: %+v", completed)
		}
	case <-ctx.Done():
		t.Fatal("in-flight Send did not complete after barrier release")
	}
	assertCodexCLIQuotaError(t, fixture.provider.CurrentProviderError())
	fixture.provider.cooldownMu.Lock()
	until := fixture.provider.cooldownUntil
	fixture.provider.cooldownMu.Unlock()
	if !until.Equal(now.Add(codexCLIUsageLimitCooldown)) {
		t.Fatalf("successful Send changed independently established cooldown: %v", until)
	}
}

func TestCodexCLIQuotaDoesNotCacheOtherFailures(t *testing.T) {
	for _, diagnostic := range []string{
		"Not logged in: private-secret", "model_not_found: private-secret",
		"error sending request: private-secret", "unsupported reasoning: private-secret", "private-secret",
	} {
		t.Run(diagnostic, func(t *testing.T) {
			fixture, callsPath := newCodexCLIFailureFixture(t, "", diagnostic, 1)
			expected := codexCLIProcessError(diagnostic)
			var providerErr *ProviderError
			if !errors.As(expected, &providerErr) {
				// Existing Send preserves the parse error when stderr has no
				// classified ProviderError; cooldown must not change this order.
				expected = errors.New("codex-cli stream ended before a complete response")
			}
			for range 2 {
				_, err := fixture.provider.Send(context.Background(), fixture.request)
				if err == nil || err.Error() != expected.Error() {
					t.Fatalf("nonquota error changed: %v", err)
				}
				if err := fixture.provider.CurrentProviderError(); err != nil {
					t.Fatalf("nonquota failure entered cooldown: %v", err)
				}
			}
			if got := readTestFile(t, callsPath); got != "called\ncalled\n" {
				t.Fatalf("nonquota failures suppressed a subsequent attempt: %q", got)
			}
		})
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
