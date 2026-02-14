package observatory

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLoadConfig(t *testing.T) {
	// Use the real observatory.toml from the config directory.
	configPath := filepath.Join("..", "..", "..", "..", "config", "observatory.toml")
	cfg, err := LoadConfig(configPath)
	if err != nil {
		t.Fatalf("LoadConfig(%q): %v", configPath, err)
	}

	// Verify shift 1 (Claude).
	s1 := cfg.Observatory.Shift1
	if s1.Model != "claude-sonnet" {
		t.Errorf("shift_1.model = %q, want %q", s1.Model, "claude-sonnet")
	}
	if s1.Provider != "claude" {
		t.Errorf("shift_1.provider = %q, want %q", s1.Provider, "claude")
	}
	if s1.Agents != 15 {
		t.Errorf("shift_1.agents = %d, want 15", s1.Agents)
	}

	// Verify shift 2 (Llama).
	s2 := cfg.Observatory.Shift2
	if s2.Model != "llama-3.1-70b" {
		t.Errorf("shift_2.model = %q, want %q", s2.Model, "llama-3.1-70b")
	}
	if s2.Provider != "ollama" {
		t.Errorf("shift_2.provider = %q, want %q", s2.Provider, "ollama")
	}
	if s2.Agents != 15 {
		t.Errorf("shift_2.agents = %d, want 15", s2.Agents)
	}

	// Verify shift 3 (Qwen).
	s3 := cfg.Observatory.Shift3
	if s3.Model != "qwen2.5-72b" {
		t.Errorf("shift_3.model = %q, want %q", s3.Model, "qwen2.5-72b")
	}
	if s3.Provider != "ollama" {
		t.Errorf("shift_3.provider = %q, want %q", s3.Provider, "ollama")
	}
	if s3.Agents != 15 {
		t.Errorf("shift_3.agents = %d, want 15", s3.Agents)
	}

	// Verify scenarios.
	sc := cfg.Observatory.Scenarios
	if !sc.DailyRoutine {
		t.Error("scenarios.daily_routine should be true")
	}
	if !sc.CrisisResponse {
		t.Error("scenarios.crisis_response should be true")
	}
	if !sc.CreativeTask {
		t.Error("scenarios.creative_task should be true")
	}
	if !sc.ConflictResolution {
		t.Error("scenarios.conflict_resolution should be true")
	}
}

func TestConfigValidation(t *testing.T) {
	tests := []struct {
		name    string
		toml    string
		wantErr string
	}{
		{
			name: "empty model in shift_1",
			toml: `
[observatory]
enabled = false
[observatory.shift_1]
model = ""
provider = "claude"
agents = 15
[observatory.shift_2]
model = "llama"
provider = "ollama"
agents = 15
[observatory.shift_3]
model = "qwen"
provider = "ollama"
agents = 15
[observatory.scenarios]
`,
			wantErr: "shift_1: model must not be empty",
		},
		{
			name: "empty provider in shift_2",
			toml: `
[observatory]
enabled = false
[observatory.shift_1]
model = "claude-sonnet"
provider = "claude"
agents = 15
[observatory.shift_2]
model = "llama"
provider = ""
agents = 15
[observatory.shift_3]
model = "qwen"
provider = "ollama"
agents = 15
[observatory.scenarios]
`,
			wantErr: "shift_2: provider must not be empty",
		},
		{
			name: "zero agents in shift_3",
			toml: `
[observatory]
enabled = false
[observatory.shift_1]
model = "claude-sonnet"
provider = "claude"
agents = 15
[observatory.shift_2]
model = "llama"
provider = "ollama"
agents = 15
[observatory.shift_3]
model = "qwen"
provider = "ollama"
agents = 0
[observatory.scenarios]
`,
			wantErr: "shift_3: agents must be > 0",
		},
		{
			name: "negative agents",
			toml: `
[observatory]
enabled = false
[observatory.shift_1]
model = "claude-sonnet"
provider = "claude"
agents = -1
[observatory.shift_2]
model = "llama"
provider = "ollama"
agents = 15
[observatory.shift_3]
model = "qwen"
provider = "ollama"
agents = 15
[observatory.scenarios]
`,
			wantErr: "shift_1: agents must be > 0",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			path := writeTempTOML(t, tc.toml)
			_, err := LoadConfig(path)
			if err == nil {
				t.Fatal("expected error, got nil")
			}
			if got := err.Error(); !contains(got, tc.wantErr) {
				t.Errorf("error = %q, want substring %q", got, tc.wantErr)
			}
		})
	}
}

func TestFeatureFlag(t *testing.T) {
	validTOML := `
[observatory]
enabled = false
[observatory.shift_1]
model = "claude-sonnet"
provider = "claude"
agents = 15
[observatory.shift_2]
model = "llama"
provider = "ollama"
agents = 15
[observatory.shift_3]
model = "qwen"
provider = "ollama"
agents = 15
[observatory.scenarios]
`
	path := writeTempTOML(t, validTOML)
	cfg, err := LoadConfig(path)
	if err != nil {
		t.Fatalf("LoadConfig: %v", err)
	}

	// Default: disabled in config.
	if cfg.IsEnabled() {
		t.Error("expected IsEnabled()=false with enabled=false and no env var")
	}

	// Env var override: "true".
	t.Setenv("SENTINEL_OBSERVATORY", "true")
	if !cfg.IsEnabled() {
		t.Error("expected IsEnabled()=true with SENTINEL_OBSERVATORY=true")
	}

	// Env var override: "1".
	t.Setenv("SENTINEL_OBSERVATORY", "1")
	if !cfg.IsEnabled() {
		t.Error("expected IsEnabled()=true with SENTINEL_OBSERVATORY=1")
	}

	// Env var override: "TRUE" (case-insensitive).
	t.Setenv("SENTINEL_OBSERVATORY", "TRUE")
	if !cfg.IsEnabled() {
		t.Error("expected IsEnabled()=true with SENTINEL_OBSERVATORY=TRUE")
	}

	// Env var override: "false" -> disabled.
	t.Setenv("SENTINEL_OBSERVATORY", "false")
	if cfg.IsEnabled() {
		t.Error("expected IsEnabled()=false with SENTINEL_OBSERVATORY=false")
	}
}

func TestConfigDefaults(t *testing.T) {
	validTOML := `
[observatory]
[observatory.shift_1]
model = "claude-sonnet"
provider = "claude"
agents = 15
[observatory.shift_2]
model = "llama"
provider = "ollama"
agents = 15
[observatory.shift_3]
model = "qwen"
provider = "ollama"
agents = 15
[observatory.scenarios]
`
	path := writeTempTOML(t, validTOML)
	cfg, err := LoadConfig(path)
	if err != nil {
		t.Fatalf("LoadConfig: %v", err)
	}

	// enabled defaults to false (zero value).
	if cfg.Observatory.Enabled {
		t.Error("expected enabled=false by default")
	}

	// Scenarios default to false (zero values).
	sc := cfg.Observatory.Scenarios
	if sc.DailyRoutine || sc.CrisisResponse || sc.CreativeTask || sc.ConflictResolution {
		t.Error("expected all scenarios false by default when not set")
	}
}

func TestShifts(t *testing.T) {
	validTOML := `
[observatory]
enabled = true
[observatory.shift_1]
model = "a"
provider = "p1"
agents = 10
[observatory.shift_2]
model = "b"
provider = "p2"
agents = 20
[observatory.shift_3]
model = "c"
provider = "p3"
agents = 30
[observatory.scenarios]
`
	path := writeTempTOML(t, validTOML)
	cfg, err := LoadConfig(path)
	if err != nil {
		t.Fatalf("LoadConfig: %v", err)
	}

	shifts := cfg.Shifts()
	if len(shifts) != 3 {
		t.Fatalf("Shifts() returned %d entries, want 3", len(shifts))
	}
	if shifts[0].Model != "a" || shifts[1].Model != "b" || shifts[2].Model != "c" {
		t.Errorf("Shifts() models = [%q, %q, %q], want [a, b, c]",
			shifts[0].Model, shifts[1].Model, shifts[2].Model)
	}
	if shifts[0].Agents != 10 || shifts[1].Agents != 20 || shifts[2].Agents != 30 {
		t.Errorf("Shifts() agents = [%d, %d, %d], want [10, 20, 30]",
			shifts[0].Agents, shifts[1].Agents, shifts[2].Agents)
	}
}

// writeTempTOML writes content to a temporary TOML file and returns its path.
func writeTempTOML(t *testing.T, content string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "test.toml")
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatalf("write temp TOML: %v", err)
	}
	return path
}

// contains reports whether s contains substr.
func contains(s, substr string) bool {
	return len(s) >= len(substr) && searchString(s, substr)
}

func searchString(s, substr string) bool {
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}
