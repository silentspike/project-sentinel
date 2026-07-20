package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestReadCredentialFileRequiresOwnerOnlyPermissions(t *testing.T) {
	path := filepath.Join(t.TempDir(), "caller-token")
	if err := os.WriteFile(path, []byte("judge-token\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if got, err := readCredentialFile(path); err != nil || got != "judge-token" {
		t.Fatalf("owner-only credential=%q err=%v", got, err)
	}
	if err := os.Chmod(path, 0o604); err != nil { //nolint:gosec // negative permission test
		t.Fatal(err)
	}
	if _, err := readCredentialFile(path); err == nil {
		t.Fatal("other-readable credential accepted")
	}
}

func TestSystemdCredentialPermissions(t *testing.T) {
	const directory = "/run/credentials/sentinel-judge.service"
	path := filepath.Join(directory, "caller-judge")
	if !secureCredentialMode(0o440, 0, 0, path, directory) {
		t.Fatal("systemd root:root 0440 credential rejected")
	}
	if secureCredentialMode(0o440, 1000, 0, path, directory) ||
		secureCredentialMode(0o440, 0, 0, "/tmp/caller-judge", directory) ||
		secureCredentialMode(0o444, 0, 0, path, directory) {
		t.Fatal("insecure systemd credential permissions accepted")
	}
}
