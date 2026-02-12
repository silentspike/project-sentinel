package observatory

import (
	"encoding/json"
	"strings"
	"testing"
	"time"
)

func newTestStore() *ObservationStore {
	store := NewObservationStore()
	base := time.Date(2026, 2, 12, 10, 0, 0, 0, time.UTC)

	// Shift 1: Claude
	store.Add(ObservationRecord{
		Timestamp: base,
		Shift:     1,
		Model:     "claude-sonnet",
		Agent:     "AGENT-01",
		Scenario:  "daily_routine",
		Metrics: MetricsSnapshot{
			InfoPropagation:        0.85,
			GroupPolarization:      0.15,
			CommunicationScore:     0.82,
			PersonalityConsistency: 0.91,
			ResponseCreativity:     0.67,
			EmotionalRange:         0.73,
		},
	})
	store.Add(ObservationRecord{
		Timestamp: base.Add(time.Hour),
		Shift:     1,
		Model:     "claude-sonnet",
		Agent:     "AGENT-02",
		Scenario:  "daily_routine",
		Metrics: MetricsSnapshot{
			InfoPropagation:        0.89,
			GroupPolarization:      0.11,
			CommunicationScore:     0.78,
			PersonalityConsistency: 0.93,
			ResponseCreativity:     0.71,
			EmotionalRange:         0.69,
		},
	})

	// Shift 2: Llama
	store.Add(ObservationRecord{
		Timestamp: base,
		Shift:     2,
		Model:     "llama-3.1-70b",
		Agent:     "AGENT-16",
		Scenario:  "daily_routine",
		Metrics: MetricsSnapshot{
			InfoPropagation:        0.72,
			GroupPolarization:      0.22,
			CommunicationScore:     0.65,
			PersonalityConsistency: 0.78,
			ResponseCreativity:     0.59,
			EmotionalRange:         0.55,
		},
	})

	// Shift 3: Qwen
	store.Add(ObservationRecord{
		Timestamp: base,
		Shift:     3,
		Model:     "qwen2.5-72b",
		Agent:     "AGENT-31",
		Scenario:  "daily_routine",
		Metrics: MetricsSnapshot{
			InfoPropagation:        0.78,
			GroupPolarization:      0.18,
			CommunicationScore:     0.71,
			PersonalityConsistency: 0.83,
			ResponseCreativity:     0.63,
			EmotionalRange:         0.61,
		},
	})

	return store
}

func TestGenerateMarkdown(t *testing.T) {
	store := newTestStore()
	gen := NewReportGenerator(store)

	md := gen.GenerateMarkdown(QueryFilter{})

	// Header
	if !strings.Contains(md, "# MARBLE Observatory Report") {
		t.Error("markdown should contain report header")
	}
	if !strings.Contains(md, "Records: 4") {
		t.Error("markdown should show 4 records")
	}

	// Shift comparison table
	if !strings.Contains(md, "## Shift Comparison") {
		t.Error("markdown should contain shift comparison section")
	}
	if !strings.Contains(md, "claude-sonnet (Shift 1)") {
		t.Error("markdown should show claude-sonnet shift 1")
	}
	if !strings.Contains(md, "llama-3.1-70b (Shift 2)") {
		t.Error("markdown should show llama shift 2")
	}
	if !strings.Contains(md, "qwen2.5-72b (Shift 3)") {
		t.Error("markdown should show qwen shift 3")
	}

	// Metric rows
	if !strings.Contains(md, "Info Propagation") {
		t.Error("markdown should contain Info Propagation metric")
	}
	if !strings.Contains(md, "Group Polarization") {
		t.Error("markdown should contain Group Polarization metric")
	}
	if !strings.Contains(md, "Communication Score") {
		t.Error("markdown should contain Communication Score metric")
	}
	if !strings.Contains(md, "Personality Consistency") {
		t.Error("markdown should contain Personality Consistency metric")
	}
	if !strings.Contains(md, "Response Creativity") {
		t.Error("markdown should contain Response Creativity metric")
	}
	if !strings.Contains(md, "Emotional Range") {
		t.Error("markdown should contain Emotional Range metric")
	}

	// Per-shift details
	if !strings.Contains(md, "## Per-Shift Details") {
		t.Error("markdown should contain per-shift details section")
	}
	if !strings.Contains(md, "| Avg | Min | Max |") {
		t.Error("markdown should contain Avg/Min/Max columns")
	}

	// Table pipe character count check (basic structure validation)
	lines := strings.Split(md, "\n")
	for _, line := range lines {
		if strings.HasPrefix(line, "| Info Propagation") {
			pipes := strings.Count(line, "|")
			if pipes < 4 {
				t.Errorf("metric row should have at least 4 pipe characters, got %d", pipes)
			}
		}
	}
}

func TestGenerateJSON(t *testing.T) {
	store := newTestStore()
	gen := NewReportGenerator(store)

	data, err := gen.GenerateJSON(QueryFilter{})
	if err != nil {
		t.Fatalf("GenerateJSON failed: %v", err)
	}

	// Must be valid JSON
	var report struct {
		Generated string `json:"generated"`
		Records   int    `json:"records"`
		Summaries []struct {
			Shift       int             `json:"shift"`
			Model       string          `json:"model"`
			RecordCount int             `json:"record_count"`
			AvgMetrics  MetricsSnapshot `json:"avg_metrics"`
			MinMetrics  MetricsSnapshot `json:"min_metrics"`
			MaxMetrics  MetricsSnapshot `json:"max_metrics"`
		} `json:"summaries"`
	}

	if err := json.Unmarshal(data, &report); err != nil {
		t.Fatalf("JSON unmarshal failed: %v", err)
	}

	if report.Records != 4 {
		t.Errorf("expected 4 records, got %d", report.Records)
	}
	if report.Generated == "" {
		t.Error("generated timestamp should not be empty")
	}
	if len(report.Summaries) != 3 {
		t.Fatalf("expected 3 summaries (one per shift), got %d", len(report.Summaries))
	}

	// Verify shift order
	if report.Summaries[0].Shift != 1 {
		t.Errorf("first summary shift = %d, want 1", report.Summaries[0].Shift)
	}
	if report.Summaries[1].Shift != 2 {
		t.Errorf("second summary shift = %d, want 2", report.Summaries[1].Shift)
	}
	if report.Summaries[2].Shift != 3 {
		t.Errorf("third summary shift = %d, want 3", report.Summaries[2].Shift)
	}

	// Verify models
	if report.Summaries[0].Model != "claude-sonnet" {
		t.Errorf("shift 1 model = %s, want claude-sonnet", report.Summaries[0].Model)
	}

	// Verify record counts
	if report.Summaries[0].RecordCount != 2 {
		t.Errorf("shift 1 record count = %d, want 2", report.Summaries[0].RecordCount)
	}
	if report.Summaries[1].RecordCount != 1 {
		t.Errorf("shift 2 record count = %d, want 1", report.Summaries[1].RecordCount)
	}

	// Verify avg metrics for shift 1 (2 records: 0.85 and 0.89 -> avg 0.87)
	avgIP := report.Summaries[0].AvgMetrics.InfoPropagation
	if avgIP < 0.86 || avgIP > 0.88 {
		t.Errorf("shift 1 avg InfoPropagation = %f, want ~0.87", avgIP)
	}
}

func TestShiftSummaries(t *testing.T) {
	store := newTestStore()
	gen := NewReportGenerator(store)

	summaries := gen.GetShiftSummaries(QueryFilter{})

	if len(summaries) != 3 {
		t.Fatalf("expected 3 summaries for 3 shifts, got %d", len(summaries))
	}

	// Shift 1 has 2 records
	if summaries[0].RecordCount != 2 {
		t.Errorf("shift 1 record count = %d, want 2", summaries[0].RecordCount)
	}

	// Verify min/max for shift 1 InfoPropagation (0.85, 0.89)
	if summaries[0].MinMetrics.InfoPropagation != 0.85 {
		t.Errorf("shift 1 min InfoPropagation = %f, want 0.85", summaries[0].MinMetrics.InfoPropagation)
	}
	if summaries[0].MaxMetrics.InfoPropagation != 0.89 {
		t.Errorf("shift 1 max InfoPropagation = %f, want 0.89", summaries[0].MaxMetrics.InfoPropagation)
	}

	// Shift 2 has 1 record, so min/max/avg should be the same
	if summaries[1].AvgMetrics.InfoPropagation != 0.72 {
		t.Errorf("shift 2 avg InfoPropagation = %f, want 0.72", summaries[1].AvgMetrics.InfoPropagation)
	}
	if summaries[1].MinMetrics.InfoPropagation != 0.72 {
		t.Errorf("shift 2 min InfoPropagation = %f, want 0.72", summaries[1].MinMetrics.InfoPropagation)
	}
	if summaries[1].MaxMetrics.InfoPropagation != 0.72 {
		t.Errorf("shift 2 max InfoPropagation = %f, want 0.72", summaries[1].MaxMetrics.InfoPropagation)
	}
}

func TestEmptyStore(t *testing.T) {
	store := NewObservationStore()
	gen := NewReportGenerator(store)

	// Markdown should not panic on empty store
	md := gen.GenerateMarkdown(QueryFilter{})
	if !strings.Contains(md, "No data available") {
		t.Error("empty markdown should contain 'No data available'")
	}
	if !strings.Contains(md, "Records: 0") {
		t.Error("empty markdown should show 0 records")
	}

	// JSON should not panic on empty store
	data, err := gen.GenerateJSON(QueryFilter{})
	if err != nil {
		t.Fatalf("GenerateJSON on empty store failed: %v", err)
	}

	var report struct {
		Records   int           `json:"records"`
		Summaries []interface{} `json:"summaries"`
	}
	if err := json.Unmarshal(data, &report); err != nil {
		t.Fatalf("empty JSON unmarshal failed: %v", err)
	}
	if report.Records != 0 {
		t.Errorf("empty report records = %d, want 0", report.Records)
	}

	// Summaries should not panic on empty store
	summaries := gen.GetShiftSummaries(QueryFilter{})
	if len(summaries) != 0 {
		t.Errorf("empty store summaries = %d, want 0", len(summaries))
	}
}

func TestGenerateMarkdownWithFilter(t *testing.T) {
	store := newTestStore()
	gen := NewReportGenerator(store)

	shift := 1
	md := gen.GenerateMarkdown(QueryFilter{Shift: &shift})

	if !strings.Contains(md, "Records: 2") {
		t.Error("filtered markdown should show 2 records for shift 1")
	}
	// Only shift 1 model should appear in table
	if !strings.Contains(md, "claude-sonnet (Shift 1)") {
		t.Error("filtered markdown should show claude-sonnet")
	}
	if strings.Contains(md, "llama-3.1-70b (Shift 2)") {
		t.Error("filtered markdown should not show llama (filtered out)")
	}
}

func TestGenerateJSONWithFilter(t *testing.T) {
	store := newTestStore()
	gen := NewReportGenerator(store)

	model := "qwen2.5-72b"
	data, err := gen.GenerateJSON(QueryFilter{Model: &model})
	if err != nil {
		t.Fatalf("GenerateJSON with filter failed: %v", err)
	}

	var report struct {
		Records   int `json:"records"`
		Summaries []struct {
			Model string `json:"model"`
		} `json:"summaries"`
	}
	if err := json.Unmarshal(data, &report); err != nil {
		t.Fatalf("filtered JSON unmarshal failed: %v", err)
	}

	if report.Records != 1 {
		t.Errorf("filtered report records = %d, want 1", report.Records)
	}
	if len(report.Summaries) != 1 {
		t.Fatalf("filtered summaries = %d, want 1", len(report.Summaries))
	}
	if report.Summaries[0].Model != "qwen2.5-72b" {
		t.Errorf("filtered summary model = %s, want qwen2.5-72b", report.Summaries[0].Model)
	}
}
