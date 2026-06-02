package messaging

import (
	"testing"
)

func TestBuildEventSubject(t *testing.T) {
	tests := []struct {
		eventType string
		agentID   string
		want      string
	}{
		{"agent_action_received", "AGENT-07", "sentinel.events.agent_action_received.AGENT-07"},
		{"agent_chat", "AGENT-12", "sentinel.events.agent_chat.AGENT-12"},
		{"bio_state_updated", "AGENT-01", "sentinel.events.bio_state_updated.AGENT-01"},
		// #475: display-name aggregate IDs (spaces) must be sanitized, not break the subject.
		{"platform_intervention", "Michael Hartmann", "sentinel.events.platform_intervention.Michael_Hartmann"},
		{"platform_intervention", "Thomas Mueller", "sentinel.events.platform_intervention.Thomas_Mueller"},
		{"platform_intervention", "system", "sentinel.events.platform_intervention.system"},
		// Reserved NATS chars in either token must not leak into the subject.
		{"weird.type", "a*b>c d", "sentinel.events.weird_type.a_b_c_d"},
		{"agent_spawned", "", "sentinel.events.agent_spawned._"},
	}

	for _, tt := range tests {
		got := BuildEventSubject(tt.eventType, tt.agentID)
		if got != tt.want {
			t.Errorf("BuildEventSubject(%q, %q) = %q, want %q", tt.eventType, tt.agentID, got, tt.want)
		}
	}
}

func TestBuildAlertSubject(t *testing.T) {
	got := BuildAlertSubject("AGENT-07")
	want := "sentinel.judge.alert.AGENT-07"
	if got != want {
		t.Errorf("BuildAlertSubject = %q, want %q", got, want)
	}
}

func TestParseEventSubject(t *testing.T) {
	tests := []struct {
		subject   string
		wantType  string
		wantAgent string
		wantErr   bool
	}{
		{"sentinel.events.agent_chat.AGENT-12", "agent_chat", "AGENT-12", false},
		{"sentinel.events.bio_state_updated.AGENT-01", "bio_state_updated", "AGENT-01", false},
		{"invalid.subject", "", "", true},
		{"sentinel.judge.alert.AGENT-07", "", "", true},
		{"sentinel.events", "", "", true},
	}

	for _, tt := range tests {
		eventType, agentID, err := ParseEventSubject(tt.subject)
		if (err != nil) != tt.wantErr {
			t.Errorf("ParseEventSubject(%q) error = %v, wantErr %v", tt.subject, err, tt.wantErr)
			continue
		}
		if eventType != tt.wantType {
			t.Errorf("ParseEventSubject(%q) eventType = %q, want %q", tt.subject, eventType, tt.wantType)
		}
		if agentID != tt.wantAgent {
			t.Errorf("ParseEventSubject(%q) agentID = %q, want %q", tt.subject, agentID, tt.wantAgent)
		}
	}
}

func TestEventsStreamConfig(t *testing.T) {
	cfg := EventsStreamConfig()
	if cfg.Name != StreamEvents {
		t.Errorf("stream name = %q, want %q", cfg.Name, StreamEvents)
	}
	if len(cfg.Subjects) != 1 || cfg.Subjects[0] != "sentinel.events.>" {
		t.Errorf("subjects = %v, want [sentinel.events.>]", cfg.Subjects)
	}
	if cfg.MaxBytes != 1<<30 {
		t.Errorf("max bytes = %d, want %d", cfg.MaxBytes, 1<<30)
	}
}

func TestJudgeStreamConfig(t *testing.T) {
	cfg := JudgeStreamConfig()
	if cfg.Name != StreamJudge {
		t.Errorf("stream name = %q, want %q", cfg.Name, StreamJudge)
	}
	if len(cfg.Subjects) != 1 || cfg.Subjects[0] != "sentinel.judge.>" {
		t.Errorf("subjects = %v, want [sentinel.judge.>]", cfg.Subjects)
	}
	if cfg.MaxBytes != 100<<20 {
		t.Errorf("max bytes = %d, want %d", cfg.MaxBytes, 100<<20)
	}
}
