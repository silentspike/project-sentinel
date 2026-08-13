package proxy

import (
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
)

func operatorCredentialDirectory(t *testing.T) string {
	t.Helper()
	directory, err := os.MkdirTemp(".", ".operator-credential-test-")
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(directory, 0o700); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(directory) })
	absolute, err := filepath.Abs(directory)
	if err != nil {
		t.Fatal(err)
	}
	return absolute
}

func writeOperatorCredential(t *testing.T, directory, value string, mode os.FileMode) string {
	t.Helper()
	path := filepath.Join(directory, operatorCredentialName)
	if err := os.WriteFile(path, []byte(value), mode); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(path, mode); err != nil {
		t.Fatal(err)
	}
	return path
}

func operatorCredentialEnvironment(t *testing.T, path string) {
	t.Helper()
	t.Setenv("CREDENTIALS_DIRECTORY", filepath.Dir(path))
	t.Setenv(operatorCredentialFileEnv, path)
}

func unsetEnvironmentForTest(t *testing.T, name string) {
	t.Helper()
	value, present := os.LookupEnv(name)
	if err := os.Unsetenv(name); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if present {
			_ = os.Setenv(name, value)
		} else {
			_ = os.Unsetenv(name)
		}
	})
}

func TestLoadOperatorCredentialFromFile(t *testing.T) {
	directory := operatorCredentialDirectory(t)
	value := strings.Repeat("a", operatorCredentialMinSize)
	path := writeOperatorCredential(t, directory, value, 0o600)
	operatorCredentialEnvironment(t, path)

	got, err := LoadOperatorCredentialFromFile()
	if err != nil {
		t.Fatal(err)
	}
	if got != value {
		t.Fatal("operator credential was not read exactly")
	}
}

func TestLoadOperatorCredentialRejectsMissingAndMalformedFiles(t *testing.T) {
	t.Run("missing env", func(t *testing.T) {
		t.Setenv(operatorCredentialFileEnv, "")
		if _, err := LoadOperatorCredentialFromFile(); err == nil {
			t.Fatal("missing operator credential was accepted")
		}
	})

	for name, testCase := range map[string]struct {
		value string
		mode  os.FileMode
	}{
		"short":       {value: "short", mode: 0o600},
		"oversized":   {value: strings.Repeat("x", operatorCredentialMaxSize+1), mode: 0o600},
		"whitespace":  {value: " " + strings.Repeat("x", operatorCredentialMinSize), mode: 0o600},
		"control":     {value: strings.Repeat("x", operatorCredentialMinSize) + "\n", mode: 0o600},
		"unsafe mode": {value: strings.Repeat("x", operatorCredentialMinSize), mode: 0o644},
	} {
		t.Run(name, func(t *testing.T) {
			directory := operatorCredentialDirectory(t)
			path := writeOperatorCredential(t, directory, testCase.value, testCase.mode)
			operatorCredentialEnvironment(t, path)
			if _, err := LoadOperatorCredentialFromFile(); err == nil {
				t.Fatalf("%s operator credential was accepted", name)
			}
		})
	}
}

func TestLoadOperatorCredentialRejectsDirectAuthorityAmbiguity(t *testing.T) {
	directory := operatorCredentialDirectory(t)
	path := writeOperatorCredential(t, directory, strings.Repeat("a", operatorCredentialMinSize), 0o600)
	operatorCredentialEnvironment(t, path)
	t.Setenv("SENTINEL_OPERATOR_API_KEY", strings.Repeat("b", operatorCredentialMinSize))
	if _, err := LoadOperatorCredentialFromFile(); err == nil {
		t.Fatal("direct and file operator credential authorities were accepted together")
	}
}

func TestLoadAPICPOperatorCredentialRejectsAnyPlaintextPresence(t *testing.T) {
	for name, value := range map[string]string{
		"nonempty": strings.Repeat("b", operatorCredentialMinSize),
		"empty":    "",
	} {
		t.Run(name, func(t *testing.T) {
			t.Setenv(directOperatorCredentialEnv, value)
			if _, err := LoadAPICPOperatorCredential(false); err == nil {
				t.Fatal("disabled API-CP accepted a present plaintext operator credential")
			}
		})
	}
}

func TestLoadAPICPOperatorCredentialDisabledDoesNotReadFile(t *testing.T) {
	unsetEnvironmentForTest(t, directOperatorCredentialEnv)
	t.Setenv(operatorCredentialFileEnv, "/nonexistent/operator-api")
	secret, err := LoadAPICPOperatorCredential(false)
	if err != nil {
		t.Fatal(err)
	}
	if secret != "" {
		t.Fatal("disabled API-CP returned operator authority")
	}
}

func TestLoadOperatorCredentialRejectsSymlinkAndReplacement(t *testing.T) {
	directory := operatorCredentialDirectory(t)
	value := strings.Repeat("a", operatorCredentialMinSize)
	target := filepath.Join(directory, "target")
	if err := os.WriteFile(target, []byte(value), 0o600); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(directory, operatorCredentialName)
	if err := os.Symlink(target, path); err != nil {
		t.Fatal(err)
	}
	operatorCredentialEnvironment(t, path)
	if _, err := LoadOperatorCredentialFromFile(); err == nil {
		t.Fatal("symlink operator credential was accepted")
	}

	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	path = writeOperatorCredential(t, directory, value, 0o600)
	_, err := readOperatorCredentialFile(path, func() error {
		old := filepath.Join(directory, "operator-api.old")
		if err := os.Rename(path, old); err != nil {
			return err
		}
		replacement, writeErr := os.Create(path)
		if replacement != nil {
			_ = replacement.Close()
		}
		return writeErr
	})
	if err == nil {
		t.Fatal("replaced operator credential was accepted")
	}
}

func TestLoadOperatorCredentialRejectsSymlinkedParent(t *testing.T) {
	directory := operatorCredentialDirectory(t)
	realDirectory := filepath.Join(directory, "real")
	if err := os.Mkdir(realDirectory, 0o700); err != nil {
		t.Fatal(err)
	}
	path := writeOperatorCredential(t, realDirectory, strings.Repeat("a", operatorCredentialMinSize), 0o600)
	linkedDirectory := filepath.Join(directory, "linked")
	if err := os.Symlink(realDirectory, linkedDirectory); err != nil {
		t.Fatal(err)
	}
	linkedPath := filepath.Join(linkedDirectory, operatorCredentialName)
	t.Setenv("CREDENTIALS_DIRECTORY", linkedDirectory)
	t.Setenv(operatorCredentialFileEnv, linkedPath)
	if _, err := LoadOperatorCredentialFromFile(); err == nil {
		t.Fatal("symlinked operator credential parent was accepted")
	}
	if _, err := os.Stat(path); err != nil {
		t.Fatal(err)
	}
}

func TestLoadOperatorCredentialRejectsForeignOwner(t *testing.T) {
	if os.Geteuid() != 0 {
		t.Skip("foreign-owner representation requires root")
	}
	directory := operatorCredentialDirectory(t)
	path := writeOperatorCredential(t, directory, strings.Repeat("a", operatorCredentialMinSize), 0o600)
	if err := os.Chown(path, 65534, 65534); err != nil {
		t.Fatal(err)
	}
	operatorCredentialEnvironment(t, path)
	if _, err := LoadOperatorCredentialFromFile(); err == nil {
		t.Fatal("foreign-owned operator credential was accepted")
	}
	if stat, err := os.Stat(path); err != nil {
		t.Fatal(err)
	} else if raw, ok := stat.Sys().(*syscall.Stat_t); !ok || raw.Uid != 65534 {
		t.Fatal("foreign-owned test fixture did not retain its owner")
	}
}
