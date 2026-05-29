package capability

import (
	"os"
	"path/filepath"
	"strconv"
	"testing"
)

func TestLoadAgentActionPolicyFromAgentTOML(t *testing.T) {
	dir := t.TempDir()
	writeAgentCapability(t, dir, "AGENT-01-CEO.toml", 1, "Thomas Mueller", []string{"calendar", "search", "chat"})
	writeAgentCapability(t, dir, "AGENT-14-IT.toml", 14, "Florian Braun", []string{"file_read", "file_write", "search", "chat"})

	policy, err := LoadAgentActionPolicy(dir)
	if err != nil {
		t.Fatalf("LoadAgentActionPolicy: %v", err)
	}

	defs := policy.Definitions()
	if len(defs) != 2 {
		t.Fatalf("expected 2 agent definitions, got %d", len(defs))
	}
	if _, ok := defs["AGENT-01"].ToolTargets["calendar"]; !ok {
		t.Fatalf("AGENT-01 should have calendar tool capability: %+v", defs["AGENT-01"])
	}
	if _, ok := defs["AGENT-14"].ToolTargets["file_write"]; !ok {
		t.Fatalf("AGENT-14 should have file_write tool capability: %+v", defs["AGENT-14"])
	}
}

func TestAgentActionPolicyAllowsConfiguredTool(t *testing.T) {
	policy := NewAgentActionPolicy([]AgentActionCapability{
		{
			AgentID:   "AGENT-14",
			AgentName: "Florian Braun",
			ToolTargets: map[string][]string{
				"file_write": {"project-repo"},
			},
		},
	})

	decision := policy.Allows(ActionRequest{
		AgentName:  "Florian Braun",
		ActionType: "tool_use",
		Target:     "file_write:project-repo",
	})

	if !decision.Allowed {
		t.Fatalf("expected action allowed, got decision %+v", decision)
	}
	if decision.AgentKey != "AGENT-14" {
		t.Fatalf("agent key = %q, want AGENT-14", decision.AgentKey)
	}
	if decision.Tool != "file_write" || decision.Target != "project-repo" {
		t.Fatalf("tool/target = %q/%q", decision.Tool, decision.Target)
	}
}

func TestAgentActionPolicyRejectsUnconfiguredTool(t *testing.T) {
	policy := NewAgentActionPolicy([]AgentActionCapability{
		{
			AgentID:   "AGENT-01",
			AgentName: "Thomas Mueller",
			ToolTargets: map[string][]string{
				"calendar": {"*"},
			},
		},
	})

	decision := policy.Allows(ActionRequest{
		AgentID:    "1",
		ActionType: "tool_use",
		Target:     "file_write:payroll.csv",
	})

	if decision.Allowed {
		t.Fatalf("expected file_write denied for AGENT-01, got %+v", decision)
	}
	if decision.Reason != "tool_not_allowed" {
		t.Fatalf("reason = %q, want tool_not_allowed", decision.Reason)
	}
}

func TestAgentActionPolicyRejectsUnconfiguredTarget(t *testing.T) {
	policy := NewAgentActionPolicy([]AgentActionCapability{
		{
			AgentID: "AGENT-14",
			ToolTargets: map[string][]string{
				"file_write": {"project-repo"},
			},
		},
	})

	decision := policy.Allows(ActionRequest{
		AgentID:    "14",
		ActionType: "tool_use",
		Target:     "file_write:/etc/passwd",
	})

	if decision.Allowed {
		t.Fatalf("expected target denied, got %+v", decision)
	}
	if decision.Reason != "target_not_allowed" {
		t.Fatalf("reason = %q, want target_not_allowed", decision.Reason)
	}
}

func TestAgentActionPolicyAllowsBaselineNonToolActions(t *testing.T) {
	policy := NewAgentActionPolicy(nil)

	for _, actionType := range []string{"chat", "emote", "move", "work", "break", "think"} {
		decision := policy.Allows(ActionRequest{
			ActionType: actionType,
			Content:    "normal simulation action",
		})
		if !decision.Allowed {
			t.Fatalf("%s should be baseline-allowed, got %+v", actionType, decision)
		}
	}
}

func writeAgentCapability(t *testing.T, dir, name string, id int, agentName string, tools []string) {
	t.Helper()
	quotedTools := ""
	for i, tool := range tools {
		if i > 0 {
			quotedTools += ", "
		}
		quotedTools += `"` + tool + `"`
	}
	body := `[identity]
id = ` + strconv.Itoa(id) + `
name = "` + agentName + `"

[capabilities]
tools = [` + quotedTools + `]
`
	if err := os.WriteFile(filepath.Join(dir, name), []byte(body), 0o644); err != nil {
		t.Fatalf("write agent TOML: %v", err)
	}
}
