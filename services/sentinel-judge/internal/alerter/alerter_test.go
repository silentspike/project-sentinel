package alerter

import (
	"bytes"
	"log/slog"
	"testing"
)

func TestAlertEmitWithoutNATS(t *testing.T) {
	var buf bytes.Buffer
	logger := slog.New(slog.NewJSONHandler(&buf, nil))

	a := New(nil, logger)

	a.Emit(Alert{
		AgentID:  "AGENT-07",
		Type:     "drift",
		Severity: "moderate",
		Score:    0.65,
		Details:  "drift detected in communication patterns",
	})

	if buf.Len() == 0 {
		t.Error("expected log output, got none")
	}

	logOutput := buf.String()
	if !containsAll(logOutput, "AGENT-07", "drift", "moderate") {
		t.Errorf("log missing expected fields: %s", logOutput)
	}
}

func TestAlertEmitMultiple(t *testing.T) {
	var buf bytes.Buffer
	logger := slog.New(slog.NewJSONHandler(&buf, nil))

	a := New(nil, logger)

	alerts := []Alert{
		{AgentID: "AGENT-01", Type: "quality", Severity: "mild", Score: 2.0},
		{AgentID: "AGENT-02", Type: "fatigue", Severity: "critical", Score: 0.9},
		{AgentID: "AGENT-03", Type: "swap", Severity: "moderate", Score: 0.0},
	}

	for _, alert := range alerts {
		a.Emit(alert)
	}

	// All 3 alerts should produce log output
	logOutput := buf.String()
	if !containsAll(logOutput, "AGENT-01", "AGENT-02", "AGENT-03") {
		t.Errorf("log missing agents: %s", logOutput)
	}
}

func containsAll(s string, substrings ...string) bool {
	for _, sub := range substrings {
		if !bytes.Contains([]byte(s), []byte(sub)) {
			return false
		}
	}
	return true
}
