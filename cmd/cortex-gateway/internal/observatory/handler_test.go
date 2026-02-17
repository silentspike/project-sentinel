package observatory

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"
)

func testHandler(t *testing.T) (*Handler, *http.ServeMux) {
	t.Helper()

	path := tempDBPath(t)
	store, err := OpenSqliteStore(path)
	if err != nil {
		t.Fatalf("OpenSqliteStore: %v", err)
	}
	t.Cleanup(func() { _ = store.Close() })

	// Create a minimal valid config for ConfigHash
	cfgFile := t.TempDir() + "/observatory.toml"
	cfgContent := `[observatory]
enabled = true
[observatory.shift_1]
model = "claude-sonnet"
provider = "claude"
agents = 15
[observatory.shift_2]
model = "llama-3.1-70b"
provider = "ollama"
agents = 15
[observatory.shift_3]
model = "qwen2.5-72b"
provider = "ollama"
agents = 15
[observatory.scenarios]
daily_routine = true
crisis_response = true
creative_task = true
conflict_resolution = true
`
	if err := os.WriteFile(cfgFile, []byte(cfgContent), 0644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	cfg, err := LoadConfig(cfgFile)
	if err != nil {
		t.Fatalf("LoadConfig: %v", err)
	}

	handler := NewHandler(store, cfg, nil)
	mux := http.NewServeMux()
	handler.RegisterRoutes(mux)
	return handler, mux
}

func TestHandleSubmitRun(t *testing.T) {
	_, mux := testHandler(t)

	body := `{"records":[
		{"timestamp":"2026-02-16T10:00:00Z","shift":1,"model":"claude-sonnet","agent":"A1","scenario":"daily_routine","metrics":{"InfoPropagation":0.85,"GroupPolarization":0.15,"CommunicationScore":0.82,"PersonalityConsistency":0.91,"ResponseCreativity":0.67,"EmotionalRange":0.73}},
		{"timestamp":"2026-02-16T10:01:00Z","shift":2,"model":"llama-3.1-70b","agent":"A2","scenario":"crisis_response","metrics":{"InfoPropagation":0.6,"GroupPolarization":0.3,"CommunicationScore":0.5}}
	]}`

	req := httptest.NewRequest("POST", "/observatory/runs", strings.NewReader(body))
	w := httptest.NewRecorder()
	mux.ServeHTTP(w, req)

	if w.Code != http.StatusCreated {
		t.Fatalf("status = %d, want %d; body = %s", w.Code, http.StatusCreated, w.Body.String())
	}

	var resp submitRunResponse
	if err := json.Unmarshal(w.Body.Bytes(), &resp); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if resp.RunID == "" {
		t.Error("run_id is empty")
	}
	if resp.RecordsStored != 2 {
		t.Errorf("records_stored = %d, want 2", resp.RecordsStored)
	}
	if resp.Status != "completed" {
		t.Errorf("status = %q, want %q", resp.Status, "completed")
	}
	if resp.ConfigHash == "" {
		t.Error("config_hash is empty")
	}
}

func TestHandleSubmitRunInvalid(t *testing.T) {
	_, mux := testHandler(t)

	tests := []struct {
		name string
		body string
		code int
	}{
		{"empty body", `{}`, http.StatusBadRequest},
		{"empty records", `{"records":[]}`, http.StatusBadRequest},
		{"invalid json", `{not json}`, http.StatusBadRequest},
		{"invalid shift", `{"records":[{"shift":0,"model":"x","agent":"a","scenario":"s","timestamp":"2026-01-01T00:00:00Z","metrics":{}}]}`, http.StatusBadRequest},
		{"empty model", `{"records":[{"shift":1,"model":"","agent":"a","scenario":"s","timestamp":"2026-01-01T00:00:00Z","metrics":{}}]}`, http.StatusBadRequest},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			req := httptest.NewRequest("POST", "/observatory/runs", strings.NewReader(tt.body))
			w := httptest.NewRecorder()
			mux.ServeHTTP(w, req)
			if w.Code != tt.code {
				t.Errorf("status = %d, want %d; body = %s", w.Code, tt.code, w.Body.String())
			}
		})
	}
}

func TestHandleListRuns(t *testing.T) {
	_, mux := testHandler(t)

	// Submit 2 runs
	for i := 0; i < 2; i++ {
		body := `{"records":[{"timestamp":"2026-02-16T10:00:00Z","shift":1,"model":"claude","agent":"A1","scenario":"daily","metrics":{}}]}`
		req := httptest.NewRequest("POST", "/observatory/runs", strings.NewReader(body))
		w := httptest.NewRecorder()
		mux.ServeHTTP(w, req)
		if w.Code != http.StatusCreated {
			t.Fatalf("submit %d: status = %d", i, w.Code)
		}
	}

	// List runs
	req := httptest.NewRequest("GET", "/observatory/runs", nil)
	w := httptest.NewRecorder()
	mux.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200", w.Code)
	}

	var resp listRunsResponse
	if err := json.Unmarshal(w.Body.Bytes(), &resp); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if len(resp.Runs) != 2 {
		t.Errorf("runs = %d, want 2", len(resp.Runs))
	}
}

func TestHandleGetRun(t *testing.T) {
	_, mux := testHandler(t)

	// Submit a run
	body := `{"records":[
		{"timestamp":"2026-02-16T10:00:00Z","shift":1,"model":"claude","agent":"A1","scenario":"daily","metrics":{"InfoPropagation":0.8}},
		{"timestamp":"2026-02-16T10:01:00Z","shift":2,"model":"llama","agent":"A2","scenario":"daily","metrics":{"InfoPropagation":0.6}}
	]}`
	req := httptest.NewRequest("POST", "/observatory/runs", strings.NewReader(body))
	w := httptest.NewRecorder()
	mux.ServeHTTP(w, req)

	var submitResp submitRunResponse
	_ = json.Unmarshal(w.Body.Bytes(), &submitResp)

	// Get run records
	req = httptest.NewRequest("GET", "/observatory/runs/"+submitResp.RunID, nil)
	w = httptest.NewRecorder()
	mux.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200; body = %s", w.Code, w.Body.String())
	}

	var resp getRunResponse
	if err := json.Unmarshal(w.Body.Bytes(), &resp); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if resp.RecordCount != 2 {
		t.Errorf("record_count = %d, want 2", resp.RecordCount)
	}

	// Filter by shift
	req = httptest.NewRequest("GET", "/observatory/runs/"+submitResp.RunID+"?shift=1", nil)
	w = httptest.NewRecorder()
	mux.ServeHTTP(w, req)

	_ = json.Unmarshal(w.Body.Bytes(), &resp)
	if resp.RecordCount != 1 {
		t.Errorf("filtered record_count = %d, want 1", resp.RecordCount)
	}
}

func TestHandleReportJSON(t *testing.T) {
	_, mux := testHandler(t)

	// Submit data
	body := `{"records":[
		{"timestamp":"2026-02-16T10:00:00Z","shift":1,"model":"claude-sonnet","agent":"A1","scenario":"daily","metrics":{"InfoPropagation":0.8,"GroupPolarization":0.1,"CommunicationScore":0.7,"PersonalityConsistency":0.9,"ResponseCreativity":0.6,"EmotionalRange":0.5}},
		{"timestamp":"2026-02-16T10:01:00Z","shift":2,"model":"llama-70b","agent":"A2","scenario":"daily","metrics":{"InfoPropagation":0.6,"GroupPolarization":0.2,"CommunicationScore":0.5,"PersonalityConsistency":0.8,"ResponseCreativity":0.4,"EmotionalRange":0.3}}
	]}`
	req := httptest.NewRequest("POST", "/observatory/runs", strings.NewReader(body))
	w := httptest.NewRecorder()
	mux.ServeHTTP(w, req)

	// Get JSON report
	req = httptest.NewRequest("GET", "/observatory/report?format=json", nil)
	w = httptest.NewRecorder()
	mux.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200; body = %s", w.Code, w.Body.String())
	}
	if ct := w.Header().Get("Content-Type"); ct != "application/json" {
		t.Errorf("Content-Type = %q, want application/json", ct)
	}

	// Verify it's valid jsonReport structure
	var report jsonReport
	if err := json.Unmarshal(w.Body.Bytes(), &report); err != nil {
		t.Fatalf("unmarshal report: %v", err)
	}
	if report.Records != 2 {
		t.Errorf("report records = %d, want 2", report.Records)
	}
	if len(report.Summaries) != 2 {
		t.Errorf("report summaries = %d, want 2", len(report.Summaries))
	}
}

func TestHandleReportMarkdown(t *testing.T) {
	_, mux := testHandler(t)

	// Submit data
	body := `{"records":[{"timestamp":"2026-02-16T10:00:00Z","shift":1,"model":"claude","agent":"A1","scenario":"daily","metrics":{"InfoPropagation":0.8}}]}`
	req := httptest.NewRequest("POST", "/observatory/runs", strings.NewReader(body))
	w := httptest.NewRecorder()
	mux.ServeHTTP(w, req)

	// Get markdown report
	req = httptest.NewRequest("GET", "/observatory/report?format=markdown", nil)
	w = httptest.NewRecorder()
	mux.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200", w.Code)
	}
	if ct := w.Header().Get("Content-Type"); !strings.HasPrefix(ct, "text/markdown") {
		t.Errorf("Content-Type = %q, want text/markdown", ct)
	}
	if !strings.Contains(w.Body.String(), "# MARBLE Observatory Report") {
		t.Error("markdown report missing header")
	}
}

func TestHandleReportEmpty(t *testing.T) {
	_, mux := testHandler(t)

	req := httptest.NewRequest("GET", "/observatory/report?format=json", nil)
	w := httptest.NewRecorder()
	mux.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200", w.Code)
	}

	var report jsonReport
	if err := json.Unmarshal(w.Body.Bytes(), &report); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if report.Records != 0 {
		t.Errorf("empty report records = %d, want 0", report.Records)
	}
}

func TestHandleReportInvalidFormat(t *testing.T) {
	_, mux := testHandler(t)

	req := httptest.NewRequest("GET", "/observatory/report?format=xml", nil)
	w := httptest.NewRecorder()
	mux.ServeHTTP(w, req)

	if w.Code != http.StatusBadRequest {
		t.Errorf("status = %d, want 400", w.Code)
	}
}

func TestHandleListRunsEmpty(t *testing.T) {
	_, mux := testHandler(t)

	req := httptest.NewRequest("GET", "/observatory/runs", nil)
	w := httptest.NewRecorder()
	mux.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200", w.Code)
	}

	var resp listRunsResponse
	_ = json.Unmarshal(w.Body.Bytes(), &resp)
	if len(resp.Runs) != 0 {
		t.Errorf("empty runs = %d, want 0", len(resp.Runs))
	}
}
