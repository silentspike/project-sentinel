package proxy

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"sort"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/control"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/forwardqueue"
)

func TestLeadershipBoundaryIdentity(t *testing.T) {
	a, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), "http://127.0.0.1:1", "credential")
	if err != nil {
		t.Fatal(err)
	}
	check := func(t *testing.T, req *LLMRequest) {
		t.Helper()
		if _, err := a.dispatchRequest(&subscriptionTestProvider{}, req); err == nil {
			t.Fatal("invalid identity dispatched")
		}
	}
	for _, key := range []string{"tenant_id", "project_id", "work_item_id", "assignment_id", "assignment_version", "leadership_review_id", "reservation_id", "request_id", "subscription_allowance_id", "agent_id", "reserved_provider", "subscription_catalog_digest", "company_execution_context_digest", "company_execution_subject", "company_execution_output_kind", "company_execution_schema"} {
		for _, mode := range []string{"missing", "empty", "whitespace"} {
			t.Run(key+"/"+mode, func(t *testing.T) {
				r := leadershipReviewTestRequest()
				delete(r.Metadata, key)
				if mode == "empty" {
					r.Metadata[key] = ""
				}
				if mode == "whitespace" {
					r.Metadata[key] = " "
				}
				check(t, r)
			})
		}
	}
	for _, key := range []string{"customer_request_id", "customer_request_version", "project_version", "adaptive_session_id", "adaptive_effect_id", "adaptive_session_version"} {
		t.Run("mixed/"+key, func(t *testing.T) { r := leadershipReviewTestRequest(); r.Metadata[key] = ""; check(t, r) })
	}
	for _, schema := range []string{"", "1", "2", "3", "4", "6"} {
		t.Run("downgrade/"+schema, func(t *testing.T) {
			r := leadershipReviewTestRequest()
			r.Metadata["company_execution_schema"] = schema
			check(t, r)
			if ok, err := classifyModelWorkRequest(r, r.Metadata["request_id"]); ok || err == nil {
				t.Fatal("downgraded review classified")
			}
			if _, err := (&CodexCLIProvider{workdir: t.TempDir()}).outputSchemaPath(r); err == nil {
				t.Fatal("downgraded review selected output schema")
			}
		})
	}
	for name, mutate := range map[string]func(*LLMRequest){
		"foreign_review":      func(r *LLMRequest) { r.Metadata["leadership_review_id"] = "01991c34-e03c-70c2-b97e-0591f4be2312" },
		"foreign_reservation": func(r *LLMRequest) { r.Metadata["reservation_id"] = "01991c34-e03c-70c2-b97e-0591f4be2312" },
		"foreign_request": func(r *LLMRequest) {
			r.Metadata["request_id"] = "company-leadership-01991c34-e03c-70c2-b97e-0591f4be2312"
		},
		"invalid_uuid": func(r *LLMRequest) {
			r.Metadata["leadership_review_id"] = "not-a-uuid"
			r.Metadata["reservation_id"] = "not-a-uuid"
			r.Metadata["request_id"] = "company-leadership-not-a-uuid"
		},
		"agent_range": func(r *LLMRequest) { r.Metadata["agent_id"] = "65536" },
		"stream":      func(r *LLMRequest) { r.Stream = true },
		"tokens":      func(r *LLMRequest) { r.MaxTokens = 0 },
		"digest":      func(r *LLMRequest) { r.AuthorityRequestDigest = "" },
		"model":       func(r *LLMRequest) { r.EffectiveModel = "foreign" },
	} {
		t.Run(name, func(t *testing.T) { r := leadershipReviewTestRequest(); mutate(r); check(t, r) })
	}
}

type leadershipDeadlineProvider struct {
	subscriptionTestProvider
	deadline time.Time
	timeout  time.Duration
}

func (p *leadershipDeadlineProvider) Send(ctx context.Context, req *LLMRequest) (*LLMResponse, error) {
	p.deadline, _ = ctx.Deadline()
	p.timeout = req.ProviderTimeout
	return p.subscriptionTestProvider.Send(ctx, req)
}

func TestLeadershipBoundaryDeadline(t *testing.T) {
	for _, mode := range []string{"authority", "caller", "local_cap", "cancelled"} {
		t.Run(mode, func(t *testing.T) {
			deadline := time.Now().Add(time.Minute).Truncate(time.Millisecond)
			if mode == "local_cap" {
				deadline = deadline.Add(time.Hour)
			}
			var calls atomic.Int32
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				calls.Add(1)
				var claim subscriptionDispatch
				if err := json.NewDecoder(r.Body).Decode(&claim); err != nil {
					t.Error(err)
					return
				}
				_ = json.NewEncoder(w).Encode(subscriptionDispatchReceipt{SchemaVersion: 5, AllowanceID: claim.AllowanceID, RequestID: claim.RequestID, RequestDigest: claim.RequestDigest, DeadlineUnixMS: deadline.UnixMilli()})
			}))
			defer server.Close()
			a, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), server.URL, "credential")
			if err != nil {
				t.Fatal(err)
			}
			ctx := context.Background()
			if mode == "caller" {
				var cancel context.CancelFunc
				deadline = time.Now().Add(30 * time.Second).Truncate(time.Millisecond)
				ctx, cancel = context.WithDeadline(ctx, deadline.Add(-time.Second))
				defer cancel()
			}
			if mode == "cancelled" {
				cancelled, cancel := context.WithCancel(ctx)
				cancel()
				ctx = cancelled
			}
			provider := &leadershipDeadlineProvider{}
			r := leadershipReviewTestRequest()
			r.ProviderTimeout = time.Hour
			before := time.Now()
			_, err = a.send(ctx, provider, r)
			if mode == "cancelled" {
				if err == nil || calls.Load() != 0 || provider.calls.Load() != 0 {
					t.Fatal("cancelled request dispatched")
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			want := deadline
			if mode == "caller" {
				want, _ = ctx.Deadline()
			}
			if mode == "local_cap" {
				if provider.deadline.Before(before.Add(maxModelWorkDuration)) || provider.deadline.After(time.Now().Add(maxModelWorkDuration)) {
					t.Fatal("local duration cap not applied")
				}
			} else if !provider.deadline.Equal(want) {
				t.Fatalf("deadline=%v want=%v", provider.deadline, want)
			}
			if provider.timeout != maxModelWorkDuration || calls.Load() != 1 || provider.calls.Load() != 1 {
				t.Fatal("wrong timeout or dispatch count")
			}
		})
	}
}

func TestLeadershipBoundaryMetadataBoundByRawRequestDigest(t *testing.T) {
	r := leadershipReviewTestRequest()
	body, err := json.Marshal(r)
	if err != nil {
		t.Fatal(err)
	}
	digest := fmt.Sprintf("%x", sha256.Sum256(body))
	ph := &PipelineHandler{logger: slog.Default()}
	for _, key := range []string{"project_id", "work_item_id", "assignment_id", "assignment_version"} {
		t.Run(key, func(t *testing.T) {
			foreign := leadershipReviewTestRequest()
			foreign.Metadata[key] = "foreign"
			if key == "assignment_version" {
				foreign.Metadata[key] = "2"
			}
			changed, err := json.Marshal(foreign)
			if err != nil {
				t.Fatal(err)
			}
			for _, payload := range [][]byte{body, changed} {
				httpReq := httptest.NewRequest(http.MethodPost, "/llm/request", bytes.NewReader(payload))
				httpReq.Header.Set("X-Request-Digest", digest)
				parsed, _, ok := ph.parseRequest(httptest.NewRecorder(), httpReq)
				if !ok {
					t.Fatal("request parse failed")
				}
				if (parsed.AuthorityRequestDigest == digest) != bytes.Equal(payload, body) {
					t.Fatal("foreign metadata retained original authority digest")
				}
			}
		})
	}
}

func TestLeadershipBoundaryDecisionSchemaContract(t *testing.T) {
	// Lock the embedded provider contract without moving daemon validation here.
	var schema map[string]any
	if err := json.Unmarshal(codexCLILeadershipSchema, &schema); err != nil {
		t.Fatal(err)
	}
	properties := schema["properties"].(map[string]any)
	decision := properties["decision"].(map[string]any)
	fields := decision["properties"].(map[string]any)
	assertJSON := func(got any, want string) {
		t.Helper()
		encoded, err := json.Marshal(got)
		if err != nil || string(encoded) != want {
			t.Fatalf("got %s want %s", encoded, want)
		}
	}
	assertJSON(schema["type"], `"object"`)
	assertJSON(schema["additionalProperties"], `false`)
	assertJSON(schema["required"], `["schema_version","decision"]`)
	assertJSON(properties["schema_version"], `{"enum":[1],"type":"integer"}`)
	assertJSON(decision["type"], `"object"`)
	assertJSON(decision["additionalProperties"], `false`)
	assertJSON(decision["required"], `["kind","rationale","evidence_refs"]`)
	assertJSON(fields["kind"], `{"enum":["resolve_blocked","keep_blocked"],"type":"string"}`)
	rationale := fields["rationale"].(map[string]any)
	assertJSON(rationale, `{"maxLength":2048,"minLength":1,"pattern":"\\S","type":"string"}`)
	refs := fields["evidence_refs"].(map[string]any)
	assertJSON(refs["type"], `"array"`)
	assertJSON(refs["items"], `{"type":"string"}`)
	assertJSON(refs["maxItems"], `8`)
	if _, present := refs["uniqueItems"]; present {
		t.Fatal("generation schema must not use uniqueItems")
	}
	if description, _ := refs["description"].(string); !strings.Contains(strings.ToLower(description), "unique") {
		t.Fatal("generation schema must describe evidence reference uniqueness")
	}
	if len(properties) != 2 || len(fields) != 3 {
		t.Fatal("unexpected decision fields")
	}
}

// Bounded fixture checks, not a vendor-certified validator or response validator.
// String lengths remain unchanged for the non-fine-tuned route; their mention in
// fine-tuned exclusions alone is not an explicit general support guarantee.
// Canonical Rust decision.validate/validate_refs remains authoritative.
func nativeGenerationSchemaIssues(value any, path string) []string {
	schema, ok := value.(map[string]any)
	if !ok {
		return []string{path + ": schema must be an object"}
	}
	var issues []string
	if _, present := schema["uniqueItems"]; present {
		issues = append(issues, path+".uniqueItems: unsupported generation keyword")
	}
	if path == "$" {
		if schema["type"] != "object" {
			issues = append(issues, path+".type: root must be an object")
		}
		if _, present := schema["anyOf"]; present {
			issues = append(issues, path+".anyOf: root alternatives are unsupported")
		}
	}
	if alternatives, present := schema["anyOf"]; present {
		branches, ok := alternatives.([]any)
		if !ok || len(branches) == 0 {
			issues = append(issues, path+".anyOf: expected nonempty alternatives")
		}
		for i, branch := range branches {
			issues = append(issues, nativeGenerationSchemaIssues(branch, fmt.Sprintf("%s.anyOf[%d]", path, i))...)
		}
	}
	if enum, present := schema["enum"]; present {
		values, ok := enum.([]any)
		if !ok || len(values) == 0 {
			issues = append(issues, path+".enum: expected nonempty values")
		}
	}
	_, hasProperties := schema["properties"]
	if schema["type"] == "object" || hasProperties {
		issues = append(issues, nativeGenerationObjectIssues(schema, path)...)
	}
	if items, present := schema["items"]; present {
		issues = append(issues, nativeGenerationSchemaIssues(items, path+".items")...)
	} else if schema["type"] == "array" {
		issues = append(issues, path+".items: expected an item schema")
	}
	return issues
}

func nativeGenerationObjectIssues(schema map[string]any, path string) []string {
	var issues []string
	properties, ok := schema["properties"].(map[string]any)
	if !ok {
		issues = append(issues, path+".properties: expected an object")
	}
	if schema["additionalProperties"] != false {
		issues = append(issues, path+".additionalProperties: must be false")
	}
	required, ok := schema["required"].([]any)
	if !ok {
		issues = append(issues, path+".required: expected all property names")
	}
	seen := make(map[string]bool)
	for i, value := range required {
		name, ok := value.(string)
		if !ok {
			issues = append(issues, fmt.Sprintf("%s.required[%d]: expected a property name", path, i))
			continue
		}
		if _, exists := properties[name]; !exists || seen[name] {
			issues = append(issues, path+".required: unknown or duplicate property "+name)
		}
		seen[name] = true
	}
	names := make([]string, 0, len(properties))
	for name := range properties {
		names = append(names, name)
	}
	sort.Strings(names)
	for _, name := range names {
		if !seen[name] {
			issues = append(issues, path+".required: missing property "+name)
		}
		issues = append(issues, nativeGenerationSchemaIssues(properties[name], path+".properties."+name)...)
	}
	return issues
}

func TestNativeGenerationSchemaConformance(t *testing.T) {
	for _, fixture := range []struct {
		name string
		body []byte
	}{
		{"work", codexCLIWorkSchema},
		{"review", codexCLIReviewSchema},
		{"adaptive", codexCLIAdaptiveSchema},
		{"leadership", codexCLILeadershipSchema},
		{"unknown_leadership", codexCLIUnknownLeadershipSchema},
		{"continuation_leadership", codexCLIContinuationLeadershipSchema},
		{"budget_leadership", codexCLIBudgetLeadershipSchema},
	} {
		t.Run(fixture.name, func(t *testing.T) {
			var schema any
			if err := json.Unmarshal(fixture.body, &schema); err != nil {
				t.Fatal(err)
			}
			for _, issue := range nativeGenerationSchemaIssues(schema, "$") {
				t.Error(issue)
			}
		})
	}
}

func TestNativeGenerationSchemaConformanceChecker(t *testing.T) {
	for _, tc := range []struct {
		name, schema, want string
	}{
		{"supported_constraints", `{"type":"object","properties":{"value":{"type":"number","minimum":-1,"maximum":2,"enum":[-1,2]},"refs":{"type":"array","minItems":1,"maxItems":8,"description":"Unique references","items":{"type":"string","pattern":"\\S"}},"nullable":{"type":["string","null"]}},"required":["value","refs","nullable"],"additionalProperties":false}`, ""},
		{"unique_false", `{"type":"array","uniqueItems":false,"items":{"type":"string"}}`, "$.uniqueItems: unsupported generation keyword"},
		{"nested_unique", `{"anyOf":[{"type":"string","enum":["keep"]},{"type":"array","items":{"type":"array","uniqueItems":true,"items":{"type":"string"}}}]}`, "$.anyOf[1].items.uniqueItems: unsupported generation keyword"},
		{"missing_required", `{"type":"object","properties":{"a":{"type":"string"}},"required":[],"additionalProperties":false}`, "$.required: missing property a"},
		{"extra_required", `{"type":"object","properties":{},"required":["a"],"additionalProperties":false}`, "$.required: unknown or duplicate property a"},
		{"duplicate_required", `{"type":"object","properties":{"a":{"type":"string"}},"required":["a","a"],"additionalProperties":false}`, "$.required: unknown or duplicate property a"},
		{"open_object", `{"type":"object","properties":{},"required":[],"additionalProperties":true}`, "$.additionalProperties: must be false"},
		{"empty_anyof", `{"anyOf":[]}`, "$.anyOf: expected nonempty alternatives"},
		{"invalid_alternative", `{"anyOf":[{"type":"string"},false]}`, "$.anyOf[1]: schema must be an object"},
		{"empty_enum", `{"type":"string","enum":[]}`, "$.enum: expected nonempty values"},
		{"missing_items", `{"type":"array"}`, "$.items: expected an item schema"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var schema any
			if err := json.Unmarshal([]byte(tc.schema), &schema); err != nil {
				t.Fatal(err)
			}
			// Nested schemas may be arrays or anyOf; only fixture roots require objects.
			path := "$.nested"
			want := strings.Replace(tc.want, "$", path, 1)
			for attempt := 0; attempt < 3; attempt++ {
				if got := strings.Join(nativeGenerationSchemaIssues(schema, path), "\n"); got != want {
					t.Fatalf("got %q want %q", got, want)
				}
			}
		})
	}
}

func TestLeadershipBoundaryContinuationSchemaContracts(t *testing.T) {
	for _, tc := range []struct {
		name, keep string
		body       []byte
		version    int
	}{
		{"unknown_leadership", "keep_unknown", codexCLIUnknownLeadershipSchema, 2},
		{"continuation_leadership", "keep_blocked", codexCLIContinuationLeadershipSchema, 2},
		{"budget_leadership", "defer_budget", codexCLIBudgetLeadershipSchema, 3},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var schema map[string]any
			if err := json.Unmarshal(tc.body, &schema); err != nil {
				t.Fatal(err)
			}
			assertJSON := func(got any, want string) {
				t.Helper()
				encoded, err := json.Marshal(got)
				if err != nil || string(encoded) != want {
					t.Fatalf("got %s want %s", encoded, want)
				}
			}
			properties := schema["properties"].(map[string]any)
			assertJSON(properties["schema_version"], fmt.Sprintf(`{"enum":[%d],"type":"integer"}`, tc.version))
			decision := properties["decision"].(map[string]any)
			alternatives, ok := decision["anyOf"].([]any)
			if !ok || len(alternatives) != 2 || len(decision) != 1 {
				t.Fatal("expected exactly the keep and continue alternatives")
			}
			if len(properties) != 2 {
				t.Fatal("unexpected envelope fields")
			}
			for i, alternative := range alternatives {
				branch := alternative.(map[string]any)
				fields := branch["properties"].(map[string]any)
				kind := tc.keep
				wantFields := 3
				if i == 1 {
					kind = "continue"
					wantFields = 5
					assertJSON(fields["additional_model_calls"], `{"maximum":64,"minimum":1,"type":"integer"}`)
					assertJSON(fields["window_ms"], `{"maximum":300000,"minimum":1000,"type":"integer"}`)
				}
				assertJSON(fields["kind"], fmt.Sprintf(`{"enum":[%q],"type":"string"}`, kind))
				assertJSON(fields["rationale"], `{"maxLength":2048,"minLength":1,"pattern":"\\S","type":"string"}`)
				refs := fields["evidence_refs"].(map[string]any)
				assertJSON(refs["type"], `"array"`)
				assertJSON(refs["items"], `{"type":"string"}`)
				assertJSON(refs["minItems"], `1`)
				assertJSON(refs["maxItems"], `8`)
				if _, present := refs["uniqueItems"]; present {
					t.Errorf("alternative %d must not use uniqueItems", i)
				}
				if description, _ := refs["description"].(string); !strings.Contains(strings.ToLower(description), "unique") {
					t.Errorf("alternative %d must describe evidence reference uniqueness", i)
				}
				if len(fields) != wantFields {
					t.Errorf("alternative %d has unexpected fields", i)
				}
			}
		})
	}
}

func TestLeadershipBoundaryOutputSchemaAndLegacy(t *testing.T) {
	for _, tc := range []struct {
		name string
		req  *LLMRequest
		want []byte
	}{
		{"leadership", leadershipReviewTestRequest(), codexCLILeadershipSchema},
		{"unknown_leadership", continuationReviewTestRequest("unknown_model"), codexCLIUnknownLeadershipSchema},
		{"blocked_continuation", continuationReviewTestRequest("blocked_continuation"), codexCLIContinuationLeadershipSchema},
		{"budget_window_exhausted", continuationReviewTestRequest("budget_window_exhausted"), codexCLIBudgetLeadershipSchema},
		{"legacy1", subscriptionTestRequest(), codexCLIWorkSchema},
		{"legacy2", salesSubscriptionTestRequest(), nil},
		{"legacy3", adaptiveSubscriptionTestRequest(), codexCLIAdaptiveSchema},
		{"legacy4", projectPlanningSubscriptionTestRequest(), nil},
	} {
		t.Run(tc.name, func(t *testing.T) {
			p := &CodexCLIProvider{workdir: t.TempDir()}
			path, err := p.outputSchemaPath(tc.req)
			if err != nil {
				t.Fatal(err)
			}
			if tc.want == nil {
				if path != "" {
					t.Fatal("unexpected legacy schema")
				}
				return
			}
			got, err := os.ReadFile(path) // #nosec G304 -- provider-generated fixture inside t.TempDir.
			if err != nil {
				t.Fatal(err)
			}
			if !bytes.Equal(got, tc.want) {
				t.Fatal("wrong output schema")
			}
			args := p.commandArgsWithSchema("model-a", path)
			if !strings.Contains(strings.Join(args, " "), "--output-schema "+path) {
				t.Fatal("schema not passed to CLI")
			}
			p.cleanupOutputSchema(path)
			if _, err := os.Stat(path); !os.IsNotExist(err) {
				t.Fatal("schema not removed")
			}
		})
	}
	r := subscriptionTestRequest()
	r.Metadata["company_execution_output_kind"] = "source_review"
	p := &CodexCLIProvider{workdir: t.TempDir()}
	path, err := p.outputSchemaPath(r)
	if err != nil {
		t.Fatal(err)
	}
	got, err := os.ReadFile(path) // #nosec G304 -- provider-generated fixture inside t.TempDir.
	if err != nil || !bytes.Equal(got, codexCLIReviewSchema) {
		t.Fatal("legacy source review changed")
	}
	p.cleanupOutputSchema(path)
}

func TestLeadershipBoundaryResponseBytes(t *testing.T) {
	r := leadershipReviewTestRequest()
	for _, tokens := range []int{1, 128, maxModelWorkResponseBytes} {
		r.MaxTokens = tokens
		if codexCLIResponseByteLimit(r) != maxModelWorkResponseBytes {
			t.Fatal("leadership limit depends on token hint")
		}
	}
	for _, size := range []int{maxModelWorkResponseBytes - 1, maxModelWorkResponseBytes, maxModelWorkResponseBytes + 1} {
		t.Run(fmt.Sprint(size), func(t *testing.T) {
			// Multibyte content proves the guard measures bytes rather than runes.
			content := strings.Repeat("\u00e9", size/2) + strings.Repeat("x", size%2)
			message, _ := json.Marshal(content)
			stream := `{"type":"thread.started"}` + "\n" + `{"type":"turn.started"}` + "\n" + `{"type":"item.completed","item":{"type":"agent_message","text":` + string(message) + `}}` + "\n" + `{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}`
			_, err := (&CodexCLIProvider{}).parseOutputStream(strings.NewReader(stream), codexCLIResponseByteLimit(r))
			want := size <= maxModelWorkResponseBytes
			if (err == nil) != want {
				t.Fatalf("size=%d err=%v", size, err)
			}
			if (&PipelineHandler{}).modelWorkResponseAllowed(content, "", control.ConfigSnapshot{}) != want {
				t.Fatal("pipeline byte guard differs")
			}
		})
	}
}

func leadershipReceiptFixture(mode string, receipt subscriptionDispatchReceipt) []byte {
	switch mode {
	case "schema":
		receipt.SchemaVersion = 1
	case "allowance":
		receipt.AllowanceID = "foreign"
	case "request":
		receipt.RequestID = "foreign"
	case "digest":
		receipt.RequestDigest = strings.Repeat("e", 64)
	case "expired":
		receipt.DeadlineUnixMS = time.Now().Add(-time.Second).UnixMilli()
	case "zero_deadline":
		receipt.DeadlineUnixMS = 0
	}
	body, _ := json.Marshal(receipt)
	switch mode {
	case "missing":
		body = []byte(strings.Replace(string(body), `"schema_version":5,`, "", 1))
	case "null":
		body = []byte(strings.Replace(string(body), `"schema_version":5`, `"schema_version":null`, 1))
	case "duplicate":
		body = append([]byte(`{"schema_version":1,`), body[1:]...)
	case "case":
		body = []byte(strings.Replace(string(body), "schema_version", "SCHEMA_VERSION", 1))
	case "unknown":
		body = append([]byte(`{"extra":true,`), body[1:]...)
	case "suffix":
		body = append(body, []byte(` {}`)...)
	case "oversized":
		body = append(body, bytes.Repeat([]byte(" "), 4097)...)
	case "exact_byte_bound":
		body = append(body, bytes.Repeat([]byte(" "), 4096-len(body))...)
	case "truncated":
		body = body[:len(body)-1]
	}
	return body
}

func checkLeadershipAuthorityTransport(t *testing.T, r *http.Request) {
	t.Helper()
	if r.Method != http.MethodPost || r.URL.Path != "/operator/workflow/subscription-dispatch" || r.Header.Get("Authorization") != "Bearer credential" {
		t.Error("incorrect authority transport")
	}
}

func TestLeadershipBoundaryExactReceipt(t *testing.T) {
	for _, mode := range []string{"approved", "exact_byte_bound", "schema", "allowance", "request", "digest", "expired", "zero_deadline", "missing", "null", "duplicate", "case", "unknown", "suffix", "oversized", "truncated", "rejected", "claim_lost"} {
		t.Run(mode, func(t *testing.T) {
			provider := &subscriptionTestProvider{}
			queue := forwardqueue.NewManager(1)
			var calls atomic.Int32
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				calls.Add(1)
				if provider.calls.Load() != 0 {
					t.Error("provider preceded claim")
				}
				if queue.Stats().Active != 1 {
					t.Error("claim preceded queue lease")
				}
				checkLeadershipAuthorityTransport(t, r)
				var claim subscriptionDispatch
				if err := json.NewDecoder(r.Body).Decode(&claim); err != nil {
					t.Error(err)
					return
				}
				want := subscriptionDispatch{SchemaVersion: 5, AllowanceID: "leadership-allowance-test", AgentID: 6, RequestID: leadershipReviewTestRequest().Metadata["request_id"], RequestDigest: strings.Repeat("d", 64), ContextDigest: strings.Repeat("b", 64), Provider: CodexCLIProviderName, Model: "model-a", CatalogDigest: strings.Repeat("c", 64), Subject: &customerRequestExecutionSubject{Kind: "adaptive_leadership_review", ReviewID: leadershipReviewTestRequest().Metadata["leadership_review_id"]}}
				gotJSON, _ := json.Marshal(claim)
				wantJSON, _ := json.Marshal(want)
				if !bytes.Equal(gotJSON, wantJSON) {
					t.Errorf("claim mismatch: %s", gotJSON)
				}
				receipt := subscriptionDispatchReceipt{SchemaVersion: 5, AllowanceID: claim.AllowanceID, RequestID: claim.RequestID, RequestDigest: claim.RequestDigest, DeadlineUnixMS: time.Now().Add(time.Minute).UnixMilli()}
				switch mode {
				case "rejected":
					w.WriteHeader(http.StatusForbidden)
					return
				case "claim_lost":
					conn, _, err := w.(http.Hijacker).Hijack()
					if err != nil {
						t.Error(err)
						return
					}
					_ = conn.Close()
					return
				}
				body := leadershipReceiptFixture(mode, receipt)
				_, _ = w.Write(body)
			}))
			defer server.Close()
			a, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), server.URL, "credential")
			if err != nil {
				t.Fatal(err)
			}
			_, err = NewSubscriptionQueuedProvider(provider, queue, a).Send(context.Background(), leadershipReviewTestRequest())
			approved := mode == "approved" || mode == "exact_byte_bound"
			if (err == nil) != approved {
				t.Fatalf("unexpected result: %v", err)
			}
			if err != nil {
				var admissionErr *ProviderAdmissionError
				if !errors.As(err, &admissionErr) {
					t.Fatal("not an admission error")
				}
			}
			wantCalls := int32(0)
			if approved {
				wantCalls = 1
			}
			if calls.Load() != 1 || provider.calls.Load() != wantCalls {
				t.Fatal("retry or unauthorized provider call")
			}
			if queue.Stats().Active != 0 {
				t.Fatal("queue lease leaked")
			}
		})
	}
}
