package service

import (
	"log/slog"
	"os"
	"path/filepath"
	"testing"

	"github.com/silentspike/project-sentinel/pkg/sentinel-go/judge"
)

func TestLoadProfiles(t *testing.T) {
	// Create temp dir with test agent TOML
	dir := t.TempDir()

	agentTOML := `[identity]
id = 7
name = "Kai Becker"
role = "Senior Developer"
department = "Entwicklung"
shift_set = 1

[personality]
openness = 0.7
conscientiousness = 0.6
extraversion = 0.4
agreeableness = 0.5
neuroticism = 0.6
caffeine_tolerance = 0.8
morning_person = false

[preferences]
favorite_room = "buero-dev-2"
`
	if err := os.WriteFile(filepath.Join(dir, "AGENT-07-KAI-DEV.toml"), []byte(agentTOML), 0o644); err != nil {
		t.Fatal(err)
	}

	// Also test with a second agent
	agent2 := `[identity]
id = 1
name = "Thomas Mueller"
role = "CEO"
shift_set = 1

[personality]
openness = 0.8
conscientiousness = 0.8
extraversion = 0.6
agreeableness = 0.7
neuroticism = 0.3
caffeine_tolerance = 0.7
morning_person = true
`
	if err := os.WriteFile(filepath.Join(dir, "AGENT-01-THOMAS-CEO.toml"), []byte(agent2), 0o644); err != nil {
		t.Fatal(err)
	}

	// Non-agent file should be skipped
	if err := os.WriteFile(filepath.Join(dir, "README.md"), []byte("skip me"), 0o644); err != nil {
		t.Fatal(err)
	}

	drift := judge.NewDriftDetector()
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelDebug}))

	n, err := LoadProfiles(dir, drift, logger)
	if err != nil {
		t.Fatalf("LoadProfiles: %v", err)
	}
	if n != 2 {
		t.Errorf("loaded %d profiles, want 2", n)
	}

	// Verify AGENT-07 profile
	result := drift.CheckDrift("AGENT-07", []string{"Hello world", "Test message"})
	if result.Details == "unknown agent" {
		t.Error("AGENT-07 profile not registered")
	}

	// Verify AGENT-01 profile
	result = drift.CheckDrift("AGENT-01", []string{"Important update!!!", "Let's discuss!!"})
	if result.Details == "unknown agent" {
		t.Error("AGENT-01 profile not registered")
	}

	// Verify unregistered agent still returns unknown
	result = drift.CheckDrift("AGENT-99", []string{"test"})
	if result.Details != "unknown agent" {
		t.Error("AGENT-99 should be unknown")
	}
}

func TestLoadProfiles_EmptyDir(t *testing.T) {
	dir := t.TempDir()
	drift := judge.NewDriftDetector()
	logger := slog.New(slog.NewTextHandler(os.Stderr, nil))

	n, err := LoadProfiles(dir, drift, logger)
	if err != nil {
		t.Fatalf("LoadProfiles: %v", err)
	}
	if n != 0 {
		t.Errorf("loaded %d profiles, want 0", n)
	}
}

func TestLoadProfiles_InvalidDir(t *testing.T) {
	drift := judge.NewDriftDetector()
	logger := slog.New(slog.NewTextHandler(os.Stderr, nil))

	_, err := LoadProfiles("/nonexistent/path", drift, logger)
	if err == nil {
		t.Error("expected error for nonexistent directory")
	}
}
