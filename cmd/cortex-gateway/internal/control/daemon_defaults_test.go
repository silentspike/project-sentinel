package control

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLoadTrafficControlDefaultsMissingFileFallsBack(t *testing.T) {
	updates, err := LoadTrafficControlDefaults(filepath.Join(t.TempDir(), "missing.toml"))
	if err != nil {
		t.Fatalf("LoadTrafficControlDefaults() error = %v", err)
	}

	if updates["tick_sync_timeout_ms"] != defaultTickSyncTimeoutMs {
		t.Fatalf("tick_sync_timeout_ms = %v, want %d", updates["tick_sync_timeout_ms"], defaultTickSyncTimeoutMs)
	}
	if updates["p3_timeout_ms"] != defaultP3TimeoutMs {
		t.Fatalf("p3_timeout_ms = %v, want %d", updates["p3_timeout_ms"], defaultP3TimeoutMs)
	}
	if updates["max_forward_concurrency"] != defaultMaxForwardConcurrency {
		t.Fatalf("max_forward_concurrency = %v, want %d", updates["max_forward_concurrency"], defaultMaxForwardConcurrency)
	}
	if updates["intercept_mode"] != defaultInterceptMode {
		t.Fatalf("intercept_mode = %v, want %q", updates["intercept_mode"], defaultInterceptMode)
	}
}

func TestLoadTrafficControlDefaultsFromDaemonToml(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "daemon.toml")
	content := `
[daemon]
config_dir = "/tmp/cfg"
data_dir = "/tmp/data"

[daemon.traffic_control]
synthesis_enabled = true
sequencing_enabled = true
tick_sync_enabled = true
apicp_enabled = true
tick_sync_timeout_ms = 1500
p3_timeout_ms = 7000
max_forward_concurrency = 5
intercept_mode = "manual"
`
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("WriteFile() error = %v", err)
	}

	updates, err := LoadTrafficControlDefaults(path)
	if err != nil {
		t.Fatalf("LoadTrafficControlDefaults() error = %v", err)
	}

	if updates["synthesis_enabled"] != true {
		t.Fatalf("synthesis_enabled = %v, want true", updates["synthesis_enabled"])
	}
	if updates["sequencing_enabled"] != true {
		t.Fatalf("sequencing_enabled = %v, want true", updates["sequencing_enabled"])
	}
	if updates["tick_sync_enabled"] != true {
		t.Fatalf("tick_sync_enabled = %v, want true", updates["tick_sync_enabled"])
	}
	if updates["apicp_enabled"] != true {
		t.Fatalf("apicp_enabled = %v, want true", updates["apicp_enabled"])
	}
	if updates["tick_sync_timeout_ms"] != 1500 {
		t.Fatalf("tick_sync_timeout_ms = %v, want 1500", updates["tick_sync_timeout_ms"])
	}
	if updates["p3_timeout_ms"] != 7000 {
		t.Fatalf("p3_timeout_ms = %v, want 7000", updates["p3_timeout_ms"])
	}
	if updates["max_forward_concurrency"] != 5 {
		t.Fatalf("max_forward_concurrency = %v, want 5", updates["max_forward_concurrency"])
	}
	if updates["intercept_mode"] != "manual" {
		t.Fatalf("intercept_mode = %v, want manual", updates["intercept_mode"])
	}
}
