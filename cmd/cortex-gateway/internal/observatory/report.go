package observatory

import (
	"encoding/json"
	"fmt"
	"strings"
	"time"
)

// ReportGenerator creates comparison reports from observatory data.
type ReportGenerator struct {
	store ObservationStorer
}

// NewReportGenerator creates a ReportGenerator backed by the given store.
func NewReportGenerator(store ObservationStorer) *ReportGenerator {
	return &ReportGenerator{store: store}
}

// ShiftSummary holds aggregated metrics for a single shift and model combination.
type ShiftSummary struct {
	Shift       int
	Model       string
	RecordCount int
	AvgMetrics  MetricsSnapshot
	MinMetrics  MetricsSnapshot
	MaxMetrics  MetricsSnapshot
}

// GetShiftSummaries returns aggregated metrics per shift for the filtered records.
func (r *ReportGenerator) GetShiftSummaries(filter QueryFilter) []ShiftSummary {
	records := r.store.Query(filter)
	if len(records) == 0 {
		return nil
	}

	// Group records by shift
	grouped := make(map[int][]ObservationRecord)
	for _, rec := range records {
		grouped[rec.Shift] = append(grouped[rec.Shift], rec)
	}

	var summaries []ShiftSummary
	for shift := 1; shift <= requiredShifts; shift++ {
		recs, ok := grouped[shift]
		if !ok || len(recs) == 0 {
			continue
		}

		summary := ShiftSummary{
			Shift:       shift,
			Model:       recs[0].Model,
			RecordCount: len(recs),
			MinMetrics:  recs[0].Metrics,
			MaxMetrics:  recs[0].Metrics,
		}

		var sum MetricsSnapshot
		for _, rec := range recs {
			m := rec.Metrics
			sum.InfoPropagation += m.InfoPropagation
			sum.GroupPolarization += m.GroupPolarization
			sum.CommunicationScore += m.CommunicationScore
			sum.PersonalityConsistency += m.PersonalityConsistency
			sum.ResponseCreativity += m.ResponseCreativity
			sum.EmotionalRange += m.EmotionalRange

			summary.MinMetrics = minSnapshot(summary.MinMetrics, m)
			summary.MaxMetrics = maxSnapshot(summary.MaxMetrics, m)
		}

		n := float64(len(recs))
		summary.AvgMetrics = MetricsSnapshot{
			InfoPropagation:        sum.InfoPropagation / n,
			GroupPolarization:      sum.GroupPolarization / n,
			CommunicationScore:     sum.CommunicationScore / n,
			PersonalityConsistency: sum.PersonalityConsistency / n,
			ResponseCreativity:     sum.ResponseCreativity / n,
			EmotionalRange:         sum.EmotionalRange / n,
		}

		summaries = append(summaries, summary)
	}

	return summaries
}

// GenerateMarkdown creates a comparison report in Markdown format.
func (r *ReportGenerator) GenerateMarkdown(filter QueryFilter) string {
	summaries := r.GetShiftSummaries(filter)
	records := r.store.Query(filter)

	var b strings.Builder

	b.WriteString("# MARBLE Observatory Report\n\n")
	fmt.Fprintf(&b, "Generated: %s\n", time.Now().UTC().Format(time.RFC3339))
	fmt.Fprintf(&b, "Records: %d\n\n", len(records))

	if len(summaries) == 0 {
		b.WriteString("No data available.\n")
		return b.String()
	}

	// Shift comparison table
	b.WriteString("## Shift Comparison\n\n")
	b.WriteString("| Metric |")
	for _, s := range summaries {
		fmt.Fprintf(&b, " %s (Shift %d) |", s.Model, s.Shift)
	}
	b.WriteString("\n|--------|")
	for range summaries {
		b.WriteString("-----------------|")
	}
	b.WriteString("\n")

	type metricRow struct {
		name string
		get  func(MetricsSnapshot) float64
	}
	rows := []metricRow{
		{"Info Propagation", func(m MetricsSnapshot) float64 { return m.InfoPropagation }},
		{"Group Polarization", func(m MetricsSnapshot) float64 { return m.GroupPolarization }},
		{"Communication Score", func(m MetricsSnapshot) float64 { return m.CommunicationScore }},
		{"Personality Consistency", func(m MetricsSnapshot) float64 { return m.PersonalityConsistency }},
		{"Response Creativity", func(m MetricsSnapshot) float64 { return m.ResponseCreativity }},
		{"Emotional Range", func(m MetricsSnapshot) float64 { return m.EmotionalRange }},
	}

	for _, row := range rows {
		fmt.Fprintf(&b, "| %s |", row.name)
		for _, s := range summaries {
			fmt.Fprintf(&b, " %.2f |", row.get(s.AvgMetrics))
		}
		b.WriteString("\n")
	}

	// Per-shift details
	b.WriteString("\n## Per-Shift Details\n\n")
	for _, s := range summaries {
		fmt.Fprintf(&b, "### Shift %d: %s (%d records)\n\n", s.Shift, s.Model, s.RecordCount)
		b.WriteString("| Metric | Avg | Min | Max |\n")
		b.WriteString("|--------|-----|-----|-----|\n")
		for _, row := range rows {
			fmt.Fprintf(&b, "| %s | %.2f | %.2f | %.2f |\n",
				row.name,
				row.get(s.AvgMetrics),
				row.get(s.MinMetrics),
				row.get(s.MaxMetrics),
			)
		}
		b.WriteString("\n")
	}

	return b.String()
}

// jsonReport is the wire format for JSON export.
type jsonReport struct {
	Generated string        `json:"generated"`
	Records   int           `json:"records"`
	Summaries []jsonSummary `json:"summaries"`
}

// jsonSummary is the wire format for a single shift summary.
type jsonSummary struct {
	Shift       int             `json:"shift"`
	Model       string          `json:"model"`
	RecordCount int             `json:"record_count"`
	AvgMetrics  MetricsSnapshot `json:"avg_metrics"`
	MinMetrics  MetricsSnapshot `json:"min_metrics"`
	MaxMetrics  MetricsSnapshot `json:"max_metrics"`
}

// GenerateJSON creates a structured JSON export of the comparison data.
func (r *ReportGenerator) GenerateJSON(filter QueryFilter) ([]byte, error) {
	summaries := r.GetShiftSummaries(filter)
	records := r.store.Query(filter)

	report := jsonReport{
		Generated: time.Now().UTC().Format(time.RFC3339),
		Records:   len(records),
	}

	for _, s := range summaries {
		report.Summaries = append(report.Summaries, jsonSummary(s))
	}

	return json.MarshalIndent(report, "", "  ")
}

// minSnapshot returns a MetricsSnapshot with the minimum of each field.
func minSnapshot(a, b MetricsSnapshot) MetricsSnapshot {
	return MetricsSnapshot{
		InfoPropagation:        minF(a.InfoPropagation, b.InfoPropagation),
		GroupPolarization:      minF(a.GroupPolarization, b.GroupPolarization),
		CommunicationScore:     minF(a.CommunicationScore, b.CommunicationScore),
		PersonalityConsistency: minF(a.PersonalityConsistency, b.PersonalityConsistency),
		ResponseCreativity:     minF(a.ResponseCreativity, b.ResponseCreativity),
		EmotionalRange:         minF(a.EmotionalRange, b.EmotionalRange),
	}
}

// maxSnapshot returns a MetricsSnapshot with the maximum of each field.
func maxSnapshot(a, b MetricsSnapshot) MetricsSnapshot {
	return MetricsSnapshot{
		InfoPropagation:        maxF(a.InfoPropagation, b.InfoPropagation),
		GroupPolarization:      maxF(a.GroupPolarization, b.GroupPolarization),
		CommunicationScore:     maxF(a.CommunicationScore, b.CommunicationScore),
		PersonalityConsistency: maxF(a.PersonalityConsistency, b.PersonalityConsistency),
		ResponseCreativity:     maxF(a.ResponseCreativity, b.ResponseCreativity),
		EmotionalRange:         maxF(a.EmotionalRange, b.EmotionalRange),
	}
}

func minF(a, b float64) float64 {
	if a < b {
		return a
	}
	return b
}

func maxF(a, b float64) float64 {
	if a > b {
		return a
	}
	return b
}
