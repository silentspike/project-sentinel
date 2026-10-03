package proxy

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"sync/atomic"
	"testing"
)

func evidenceSchemaTestRefs() []string {
	return []string{
		"adaptive-budget-root:subscription-original:" + strings.Repeat("a", 64),
		"workbench-observation:original-effect:" + strings.Repeat("b", 64),
		"project-reference:\u65e5\u672c\u8a9e-\u00e9",
	}
}

func evidenceSchemaMetadata(t *testing.T, refs []string) string {
	t.Helper()
	encoded, err := json.Marshal(refs)
	if err != nil {
		t.Fatal(err)
	}
	return string(encoded)
}

func evidenceSchemaDecisionBranches(t *testing.T, schema map[string]any) []map[string]any {
	t.Helper()
	decision := schema["properties"].(map[string]any)["decision"].(map[string]any)
	if alternatives, ok := decision["anyOf"].([]any); ok {
		branches := make([]map[string]any, 0, len(alternatives))
		for _, value := range alternatives {
			branches = append(branches, value.(map[string]any))
		}
		return branches
	}
	return []map[string]any{decision}
}

func evidenceSchemaRead(t *testing.T, provider *CodexCLIProvider, req *LLMRequest) ([]byte, map[string]any) {
	t.Helper()
	before, err := json.Marshal(req)
	if err != nil {
		t.Fatal(err)
	}
	digest := req.AuthorityRequestDigest
	path, err := provider.outputSchemaPath(req)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { provider.cleanupOutputSchema(path) })
	body, err := os.ReadFile(path) // #nosec G304 -- generated schema in t.TempDir.
	if err != nil {
		t.Fatal(err)
	}
	var schema map[string]any
	if err := json.Unmarshal(body, &schema); err != nil {
		t.Fatal(err)
	}
	if issues := nativeGenerationSchemaIssues(schema, "$"); len(issues) != 0 {
		t.Fatalf("unsupported generation schema: %v", issues)
	}
	after, err := json.Marshal(req)
	if err != nil || !bytes.Equal(before, after) || req.AuthorityRequestDigest != digest {
		t.Fatal("schema generation changed request bytes or the bound request digest")
	}
	return body, schema
}

func TestLeadershipEvidenceSchemaExactEnumsAndHistoricalBytes(t *testing.T) {
	for _, tc := range []struct {
		kind string
		body []byte
	}{
		{"", codexCLILeadershipSchema},
		{"unknown_model", codexCLIUnknownLeadershipSchema},
		{"blocked_continuation", codexCLIContinuationLeadershipSchema},
		{"budget_window_exhausted", codexCLIBudgetLeadershipSchema},
		{"admission_repair", codexCLIAdmissionRepairSchema},
		{"work_funding", codexCLIWorkFundingSchema},
	} {
		t.Run(tc.kind, func(t *testing.T) {
			req := leadershipReviewTestRequest()
			if tc.kind != "" {
				req.Metadata["leadership_review_kind"] = tc.kind
			}
			provider := &CodexCLIProvider{workdir: t.TempDir()}
			legacy, _ := evidenceSchemaRead(t, provider, req)
			if !bytes.Equal(legacy, tc.body) {
				t.Fatal("metadata-absent historical schema bytes changed")
			}
			refs := evidenceSchemaTestRefs()
			req.Metadata["leadership_evidence_refs"] = evidenceSchemaMetadata(t, refs)
			metadata := req.Metadata["leadership_evidence_refs"]
			_, schema := evidenceSchemaRead(t, provider, req)
			var original map[string]any
			if err := json.Unmarshal(tc.body, &original); err != nil {
				t.Fatal(err)
			}
			branches := evidenceSchemaDecisionBranches(t, schema)
			originalBranches := evidenceSchemaDecisionBranches(t, original)
			for i, branch := range branches {
				evidence := branch["properties"].(map[string]any)["evidence_refs"].(map[string]any)
				items := evidence["items"].(map[string]any)
				if items["type"] != "string" {
					t.Fatal("evidence item type changed")
				}
				var got []string
				for _, value := range items["enum"].([]any) {
					got = append(got, value.(string))
				}
				if !reflect.DeepEqual(got, refs) || len(items) != 2 {
					t.Fatalf("exact supplied evidence changed: %q", got)
				}
				for _, foreign := range []string{"adaptive-budget-root:subscription-original", "foreign-reference"} {
					for _, value := range got {
						if value == foreign {
							t.Fatal("truncated or foreign evidence allowed by enum")
						}
					}
				}
				// Restore just the enum-bearing items and compare every other constraint.
				evidence["items"] = originalBranches[i]["properties"].(map[string]any)["evidence_refs"].(map[string]any)["items"]
			}
			if !reflect.DeepEqual(schema, original) || req.Metadata["leadership_evidence_refs"] != metadata {
				t.Fatal("generation changed another constraint or request metadata")
			}
			delete(req.Metadata, "leadership_evidence_refs")
			legacyAgain, _ := evidenceSchemaRead(t, provider, req)
			if !bytes.Equal(tc.body, legacyAgain) {
				t.Fatal("embedded schema was mutated")
			}
		})
	}
}

func TestLeadershipEvidenceSchemaEmptyLegacySource(t *testing.T) {
	req := leadershipReviewTestRequest()
	req.Metadata["leadership_evidence_refs"] = "[]"
	_, schema := evidenceSchemaRead(t, &CodexCLIProvider{workdir: t.TempDir()}, req)
	for _, branch := range evidenceSchemaDecisionBranches(t, schema) {
		evidence := branch["properties"].(map[string]any)["evidence_refs"].(map[string]any)
		items := evidence["items"].(map[string]any)
		if evidence["minItems"] != float64(0) || evidence["maxItems"] != float64(0) ||
			!reflect.DeepEqual(items, map[string]any{"type": "string"}) {
			t.Fatal("legacy empty evidence must permit only an empty array, without an empty enum")
		}
	}
	for _, kind := range []string{"unknown_model", "blocked_continuation", "budget_window_exhausted", "admission_repair", "work_funding"} {
		req.Metadata["leadership_review_kind"] = kind
		if ok, err := classifyModelWorkRequest(req, req.Metadata["request_id"]); ok || err == nil {
			t.Fatalf("empty subject evidence admitted for %s", kind)
		}
	}
}

func TestLeadershipEvidenceSchemaRustReferenceBounds(t *testing.T) {
	for name, raw := range map[string]string{
		"unicode_4096_bytes":  evidenceSchemaMetadata(t, []string{strings.Repeat("\u00e9", 2048)}),
		"paired_surrogate":    `["\ud83d\ude00"]`,
		"literal_replacement": `["\ufffd"]`,
		"escaped_backslash":   `["literal\\ud800"]`,
		"32_refs": evidenceSchemaMetadata(t, func() []string {
			var refs []string
			for i := 0; i < 32; i++ {
				refs = append(refs, fmt.Sprintf("reference-%d", i))
			}
			return refs
		}()),
		"encoded_limit": `["reference"]` + strings.Repeat(" ", maxLeadershipEvidenceMetadataBytes-len(`["reference"]`)),
	} {
		t.Run(name, func(t *testing.T) {
			req := leadershipReviewTestRequest()
			req.Metadata["leadership_evidence_refs"] = raw
			if ok, err := classifyModelWorkRequest(req, req.Metadata["request_id"]); !ok || err != nil {
				t.Fatalf("valid Rust-compatible evidence rejected: %v", err)
			}
		})
	}
}

func TestLeadershipEvidenceSchemaInvalidMetadataBeforeIO(t *testing.T) {
	var callbacks atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		callbacks.Add(1)
		w.WriteHeader(http.StatusForbidden)
	}))
	defer server.Close()
	admission, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), server.URL, "credential")
	if err != nil {
		t.Fatal(err)
	}
	for name, raw := range map[string]string{
		"empty_metadata": "", "null": "null", "object": `{}`, "string": `"reference"`,
		"malformed": `["reference"`, "trailing": `["reference"] []`, "wrong_item": `[1]`,
		"null_item": `[null]`, "empty_ref": `[""]`, "blank_ref": `["  "]`,
		"unicode_blank": `["\u00a0"]`, "control": `["reference\n"]`, "unicode_control": `["a\u0085b"]`,
		"duplicate": `["same","same"]`, "escaped_duplicate": `["same","\u0073ame"]`,
		"unpaired_high": `["\ud800"]`, "unpaired_low": `["\udc00"]`, "wrong_pair": `["\ud800\u1234"]`,
		"invalid_utf8":   "[\"" + string([]byte{0xff}) + "\"]",
		"ref_byte_limit": evidenceSchemaMetadata(t, []string{strings.Repeat("\u00e9", 2049)}),
		"encoded_limit":  strings.Repeat(" ", maxLeadershipEvidenceMetadataBytes) + "[]",
		"count_limit": evidenceSchemaMetadata(t, func() []string {
			var refs []string
			for i := 0; i < 33; i++ {
				refs = append(refs, fmt.Sprintf("reference-%d", i))
			}
			return refs
		}()),
	} {
		t.Run(name, func(t *testing.T) {
			req := leadershipReviewTestRequest()
			req.Metadata["leadership_evidence_refs"] = raw
			if ok, err := classifyModelWorkRequest(req, req.Metadata["request_id"]); ok || err == nil {
				t.Fatal("invalid metadata classified")
			}
			provider := &subscriptionTestProvider{}
			if _, err := admission.send(context.Background(), provider, req); err == nil {
				t.Fatal("invalid metadata admitted")
			}
			if callbacks.Load() != 0 || provider.calls.Load() != 0 {
				t.Fatal("invalid evidence reached authority or provider I/O")
			}
			fixture, starts := newCodexCLIFailureFixture(t, "", "unused", 1)
			req.Messages = []Message{{Role: "user", Content: "Synthetic generation guard test."}}
			if _, err := fixture.provider.Send(context.Background(), req); err == nil {
				t.Fatal("invalid evidence reached Codex")
			}
			if _, err := os.Stat(starts); !errors.Is(err, os.ErrNotExist) {
				t.Fatal("invalid evidence started a provider process")
			}
		})
	}
}

func TestLeadershipEvidenceSchemaPresenceRejectsMixedSubjects(t *testing.T) {
	for _, raw := range []string{"", "[]", `["reference"]`} {
		for _, factory := range []func() *LLMRequest{
			subscriptionTestRequest, salesSubscriptionTestRequest, adaptiveSubscriptionTestRequest, projectPlanningSubscriptionTestRequest,
			func() *LLMRequest {
				req := subscriptionTestRequest()
				delete(req.Metadata, "company_execution_schema")
				return req
			},
		} {
			req := factory()
			req.Metadata["leadership_evidence_refs"] = raw
			if !hasLeadershipReviewMetadata(req.Metadata) {
				t.Fatal("metadata presence was hidden by an empty value")
			}
			if ok, err := classifyModelWorkRequest(req, req.Metadata["request_id"]); ok || err == nil {
				t.Fatal("mixed evidence subject classified")
			}
			if _, _, err := subscriptionExecutionSubject(req); err == nil {
				t.Fatal("mixed evidence subject admitted")
			}
			if _, err := (&CodexCLIProvider{workdir: t.TempDir()}).outputSchemaPath(req); err == nil {
				t.Fatal("mixed evidence selected a schema")
			}
		}
	}
}

func freshObservationTestRequest() *LLMRequest {
	req := adaptiveSubscriptionTestRequest()
	req.Metadata["company_execution_output_kind"] = "adaptive_decision"
	req.Metadata["company_execution_fresh_observation_required"] = "true"
	return req
}

func TestFreshObservationSchemaOnlyInspectionOrBlockedAndLegacyBytes(t *testing.T) {
	req := freshObservationTestRequest()
	provider := &CodexCLIProvider{workdir: t.TempDir()}
	_, schema := evidenceSchemaRead(t, provider, req)
	branches := evidenceSchemaDecisionBranches(t, schema)
	if len(branches) != 2 {
		t.Fatal("fresh observation must contain only tool and blocked decisions")
	}
	decisions := make([]string, 0, len(branches))
	var tools []string
	for _, branch := range branches {
		fields := branch["properties"].(map[string]any)
		kind := generationSchemaKind(fields, "kind")
		decisions = append(decisions, kind)
		if kind == "tool" {
			for _, value := range fields["tool"].(map[string]any)["anyOf"].([]any) {
				tools = append(tools, generationSchemaKind(value.(map[string]any)["properties"].(map[string]any), "tool"))
			}
		}
	}
	if !reflect.DeepEqual(decisions, []string{"tool", "blocked"}) ||
		!reflect.DeepEqual(tools, []string{"list_directory", "inspect_file"}) {
		t.Fatalf("non-observation action survived: decisions=%v tools=%v", decisions, tools)
	}
	delete(req.Metadata, "company_execution_fresh_observation_required")
	legacy, _ := evidenceSchemaRead(t, provider, req)
	if !bytes.Equal(legacy, codexCLIAdaptiveSchema) {
		t.Fatal("ordinary or historically reserved adaptive schema changed")
	}
	delete(req.Metadata, "company_execution_output_kind")
	legacy, _ = evidenceSchemaRead(t, provider, req)
	if !bytes.Equal(legacy, codexCLIAdaptiveSchema) {
		t.Fatal("legacy empty adaptive output kind changed")
	}
}

func TestFreshObservationSchemaInvalidMarkerBeforeIO(t *testing.T) {
	mutations := map[string]func(*LLMRequest){
		"empty":               func(req *LLMRequest) { req.Metadata["company_execution_fresh_observation_required"] = "" },
		"false":               func(req *LLMRequest) { req.Metadata["company_execution_fresh_observation_required"] = "false" },
		"uppercase":           func(req *LLMRequest) { req.Metadata["company_execution_fresh_observation_required"] = "TRUE" },
		"padded":              func(req *LLMRequest) { req.Metadata["company_execution_fresh_observation_required"] = " true " },
		"missing_output":      func(req *LLMRequest) { delete(req.Metadata, "company_execution_output_kind") },
		"wrong_output":        func(req *LLMRequest) { req.Metadata["company_execution_output_kind"] = "tool_plan" },
		"missing_schema":      func(req *LLMRequest) { delete(req.Metadata, "company_execution_schema") },
		"wrong_identity":      func(req *LLMRequest) { req.Metadata["adaptive_effect_id"] = "foreign" },
		"leadership_evidence": func(req *LLMRequest) { req.Metadata["leadership_evidence_refs"] = "[]" },
		"stream":              func(req *LLMRequest) { req.Stream = true },
		"wrong_class":         func(req *LLMRequest) { req.RequestClass = RequestClassExternalCompat },
	}
	for _, schema := range []string{"1", "2", "4", "5", "6"} {
		mutations["schema_"+schema] = func(req *LLMRequest) { req.Metadata["company_execution_schema"] = schema }
	}
	var callbacks atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		callbacks.Add(1)
		w.WriteHeader(http.StatusForbidden)
	}))
	defer server.Close()
	admission, err := NewSubscriptionAdmission("subscription-test", strings.Repeat("c", 64), server.URL, "credential")
	if err != nil {
		t.Fatal(err)
	}
	for name, mutate := range mutations {
		t.Run(name, func(t *testing.T) {
			req := freshObservationTestRequest()
			mutate(req)
			if ok, err := classifyModelWorkRequest(req, req.Metadata["request_id"]); ok || err == nil {
				t.Fatal("invalid fresh-observation request classified")
			}
			provider := &subscriptionTestProvider{}
			if _, err := admission.send(context.Background(), provider, req); err == nil {
				t.Fatal("invalid fresh-observation request admitted")
			}
			if callbacks.Load() != 0 || provider.calls.Load() != 0 {
				t.Fatal("invalid marker reached authority or provider")
			}
			fixture, starts := newCodexCLIFailureFixture(t, "", "unused", 1)
			req.Messages = []Message{{Role: "user", Content: "Synthetic generation guard test."}}
			if _, err := fixture.provider.Send(context.Background(), req); err == nil {
				t.Fatal("invalid marker reached Codex")
			}
			if _, err := os.Stat(starts); !errors.Is(err, os.ErrNotExist) {
				t.Fatal("invalid marker started a provider process")
			}
		})
	}
}

func TestGenerationGuardSchemaWriteFailureIsBeforeProviderStart(t *testing.T) {
	for _, kind := range []string{"leadership", "fresh_observation"} {
		t.Run(kind, func(t *testing.T) {
			fixture, starts := newCodexCLIFailureFixture(t, "", "unused", 1)
			req := freshObservationTestRequest()
			if kind == "leadership" {
				req = continuationReviewTestRequest("budget_window_exhausted")
				req.Metadata["leadership_evidence_refs"] = evidenceSchemaMetadata(t, evidenceSchemaTestRefs())
			}
			writeErr := errors.New("synthetic schema write failure")
			req.Messages = []Message{{Role: "user", Content: "Synthetic schema write failure test."}}
			var schemaPath string
			fixture.provider.writeOutputSchema = func(file *os.File, body []byte) error {
				schemaPath = file.Name()
				if len(body) == 0 {
					t.Fatal("no guarded schema supplied to writer")
				}
				_, err := file.Write(body[:len(body)/2])
				if err != nil {
					t.Fatal(err)
				}
				return writeErr
			}
			if _, err := fixture.provider.Send(context.Background(), req); !errors.Is(err, writeErr) {
				t.Fatalf("schema failure hidden: %v", err)
			}
			if schemaPath == "" {
				t.Fatal("write failure was not exercised")
			}
			for _, path := range []string{schemaPath, starts} {
				if _, err := os.Stat(path); !errors.Is(err, os.ErrNotExist) {
					t.Fatalf("partial schema retained or provider started: %s: %v", filepath.Base(path), err)
				}
			}
			if len(fixture.provider.sem) != 0 || !fixture.provider.cooldownUntil.IsZero() {
				t.Fatal("pre-provider failure leaked capacity or changed cooldown")
			}
		})
	}
}
