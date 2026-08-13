package proxy

import (
	"context"
	"crypto/subtle"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"unicode"
	"unicode/utf8"
)

type CallerRole string

const (
	CallerRoleExternalCompat       CallerRole = "external_compat"
	CallerRoleAgentRuntime         CallerRole = "agent_runtime"
	CallerRolePlatformControlplane CallerRole = "platform_controlplane"
	CallerRoleEvolution            CallerRole = "evolution"
	CallerRoleJudge                CallerRole = "judge"
)

type CallerCredentials struct {
	AgentRuntime         string
	PlatformControlplane string
	Evolution            string
	Judge                string
}

const (
	directOperatorCredentialEnv = "SENTINEL_OPERATOR_API_KEY"
	operatorCredentialFileEnv   = "SENTINEL_OPERATOR_API_KEY_FILE"
	operatorCredentialName      = "operator-api"
	operatorCredentialMinSize   = 32
	operatorCredentialMaxSize   = 4096
)

type operatorCredentialIdentity struct {
	device uint64
	inode  uint64
	uid    uint32
	gid    uint32
	mode   uint32
	links  uint64
	size   int64
	mtime  syscall.Timespec
	ctime  syscall.Timespec
}

type operatorCredentialDirectoryIdentity struct {
	device uint64
	inode  uint64
	uid    uint32
	gid    uint32
	mode   uint32
}

type openOperatorCredential struct {
	file        *os.File
	identity    operatorCredentialIdentity
	directories []operatorCredentialDirectoryIdentity
}

func operatorIdentity(stat *syscall.Stat_t) operatorCredentialIdentity {
	return operatorCredentialIdentity{
		device: uint64(stat.Dev),
		inode:  stat.Ino,
		uid:    stat.Uid,
		gid:    stat.Gid,
		mode:   stat.Mode,
		links:  uint64(stat.Nlink),
		size:   stat.Size,
		mtime:  stat.Mtim,
		ctime:  stat.Ctim,
	}
}

func operatorDirectoryIdentity(stat *syscall.Stat_t) operatorCredentialDirectoryIdentity {
	return operatorCredentialDirectoryIdentity{
		device: uint64(stat.Dev),
		inode:  stat.Ino,
		uid:    stat.Uid,
		gid:    stat.Gid,
		mode:   stat.Mode,
	}
}

// LoadOperatorCredentialFromFile loads the gateway's operator authority from
// the canonical systemd credential leaf. The value is returned only to the
// in-memory API-CP client and is never logged or copied into configuration.
func LoadOperatorCredentialFromFile() (string, error) {
	if err := RejectDirectOperatorCredentialEnv(); err != nil {
		return "", err
	}
	return loadOperatorCredentialFromFile()
}

// LoadAPICPOperatorCredential validates the process-wide authority contract
// before considering whether API-CP is enabled. Disabled API-CP does not open
// the credential file, but a present plaintext variable is always rejected.
func LoadAPICPOperatorCredential(enabled bool) (string, error) {
	if err := RejectDirectOperatorCredentialEnv(); err != nil {
		return "", err
	}
	if !enabled {
		return "", nil
	}
	return loadOperatorCredentialFromFile()
}

// RejectDirectOperatorCredentialEnv rejects presence, including an explicitly
// empty value, so no caller can retain a second plaintext authority source.
func RejectDirectOperatorCredentialEnv() error {
	if _, present := os.LookupEnv(directOperatorCredentialEnv); present {
		return fmt.Errorf("direct operator credentials are not allowed")
	}
	return nil
}

func loadOperatorCredentialFromFile() (string, error) {
	path := os.Getenv(operatorCredentialFileEnv)
	if path == "" {
		return "", fmt.Errorf("%s is required", operatorCredentialFileEnv)
	}
	if strings.TrimSpace(path) != path {
		return "", fmt.Errorf("operator credential path is invalid")
	}
	return readOperatorCredentialFile(path, func() error { return nil })
}

func openOperatorCredentialFile(path string) (openOperatorCredential, error) {
	cleanPath := filepath.Clean(path)
	if !filepath.IsAbs(path) || path != cleanPath || filepath.Base(path) != operatorCredentialName {
		return openOperatorCredential{}, fmt.Errorf("operator credential path must be the canonical systemd credential leaf")
	}
	credentialsDirectory := os.Getenv("CREDENTIALS_DIRECTORY")
	if credentialsDirectory == "" || !filepath.IsAbs(credentialsDirectory) ||
		strings.TrimSpace(credentialsDirectory) != credentialsDirectory ||
		credentialsDirectory != filepath.Clean(credentialsDirectory) ||
		filepath.Dir(path) != credentialsDirectory {
		return openOperatorCredential{}, fmt.Errorf("operator credential path is outside CREDENTIALS_DIRECTORY")
	}

	components := strings.Split(strings.TrimPrefix(credentialsDirectory, string(filepath.Separator)), string(filepath.Separator))
	directoryFD, err := syscall.Open(string(filepath.Separator), syscall.O_RDONLY|syscall.O_CLOEXEC|syscall.O_DIRECTORY|syscall.O_NOFOLLOW, 0)
	if err != nil {
		return openOperatorCredential{}, fmt.Errorf("open operator credential root: %w", err)
	}
	directories := make([]operatorCredentialDirectoryIdentity, 0, len(components)+1)
	closeDirectory := true
	defer func() {
		if closeDirectory {
			_ = syscall.Close(directoryFD)
		}
	}()

	validateDirectory := func(fd int) error {
		var stat syscall.Stat_t
		if err := syscall.Fstat(fd, &stat); err != nil {
			return err
		}
		mode := stat.Mode & 0o7777
		if stat.Mode&syscall.S_IFMT != syscall.S_IFDIR ||
			(stat.Uid != 0 && stat.Uid != uint32(os.Geteuid())) || mode&0o7022 != 0 {
			return fmt.Errorf("operator credential directory metadata is invalid")
		}
		directories = append(directories, operatorDirectoryIdentity(&stat))
		return nil
	}
	if err := validateDirectory(directoryFD); err != nil {
		return openOperatorCredential{}, err
	}
	for _, component := range components {
		if component == "" || component == "." || component == ".." {
			return openOperatorCredential{}, fmt.Errorf("operator credential path is invalid")
		}
		nextFD, err := syscall.Openat(directoryFD, component, syscall.O_RDONLY|syscall.O_CLOEXEC|syscall.O_DIRECTORY|syscall.O_NOFOLLOW, 0)
		if err != nil {
			return openOperatorCredential{}, fmt.Errorf("open operator credential directory: %w", err)
		}
		_ = syscall.Close(directoryFD)
		directoryFD = nextFD
		if err := validateDirectory(directoryFD); err != nil {
			return openOperatorCredential{}, err
		}
	}

	fileFD, err := syscall.Openat(directoryFD, operatorCredentialName, syscall.O_RDONLY|syscall.O_CLOEXEC|syscall.O_NOFOLLOW, 0)
	if err != nil {
		return openOperatorCredential{}, fmt.Errorf("open operator credential: %w", err)
	}
	_ = syscall.Close(directoryFD)
	closeDirectory = false
	file := os.NewFile(uintptr(fileFD), operatorCredentialName)
	if file == nil {
		_ = syscall.Close(fileFD)
		return openOperatorCredential{}, fmt.Errorf("open operator credential: invalid descriptor")
	}
	var stat syscall.Stat_t
	if err := syscall.Fstat(fileFD, &stat); err != nil {
		_ = file.Close()
		return openOperatorCredential{}, fmt.Errorf("stat opened operator credential: %w", err)
	}
	identity := operatorIdentity(&stat)
	permission := stat.Mode & 0o7777
	rootSystemdCredential := stat.Uid == 0 && stat.Gid == 0 && (permission == 0o400 || permission == 0o440)
	serviceCredential := stat.Uid == uint32(os.Geteuid()) && stat.Gid == uint32(os.Getegid()) &&
		(permission == 0o400 || permission == 0o600)
	if stat.Mode&syscall.S_IFMT != syscall.S_IFREG || stat.Nlink != 1 ||
		stat.Size < operatorCredentialMinSize || stat.Size > operatorCredentialMaxSize ||
		(!rootSystemdCredential && !serviceCredential) {
		_ = file.Close()
		return openOperatorCredential{}, fmt.Errorf("operator credential metadata is invalid")
	}
	return openOperatorCredential{file: file, identity: identity, directories: directories}, nil
}

func readOperatorCredentialFile(path string, afterOpen func() error) (string, error) {
	opened, err := openOperatorCredentialFile(path)
	if err != nil {
		return "", err
	}
	defer func() { _ = opened.file.Close() }()
	if err := afterOpen(); err != nil {
		return "", fmt.Errorf("operator credential validation hook: %w", err)
	}

	data, err := io.ReadAll(io.LimitReader(opened.file, operatorCredentialMaxSize+1))
	if err != nil {
		return "", fmt.Errorf("read operator credential: %w", err)
	}
	if len(data) < operatorCredentialMinSize || len(data) > operatorCredentialMaxSize {
		return "", fmt.Errorf("operator credential length is invalid")
	}
	var after syscall.Stat_t
	if err := syscall.Fstat(int(opened.file.Fd()), &after); err != nil {
		return "", fmt.Errorf("recheck opened operator credential metadata: %w", err)
	}
	reopened, err := openOperatorCredentialFile(path)
	if err != nil {
		return "", fmt.Errorf("reopen operator credential: %w", err)
	}
	defer func() { _ = reopened.file.Close() }()
	if operatorIdentity(&after) != opened.identity || reopened.identity != opened.identity ||
		len(data) != int(opened.identity.size) ||
		!equalOperatorCredentialDirectories(reopened.directories, opened.directories) {
		return "", fmt.Errorf("operator credential identity changed while reading")
	}
	if !utf8.Valid(data) {
		return "", fmt.Errorf("operator credential encoding is invalid")
	}
	value := string(data)
	if strings.TrimSpace(value) != value || strings.IndexFunc(value, unicode.IsControl) >= 0 {
		return "", fmt.Errorf("operator credential content is invalid")
	}
	return value, nil
}

func equalOperatorCredentialDirectories(left, right []operatorCredentialDirectoryIdentity) bool {
	if len(left) != len(right) {
		return false
	}
	for index := range left {
		if left[index] != right[index] {
			return false
		}
	}
	return true
}

func LoadCallerCredentialsFromFiles() (CallerCredentials, error) {
	read := func(env string) (string, error) {
		path := strings.TrimSpace(os.Getenv(env))
		if path == "" {
			return "", fmt.Errorf("%s is required", env)
		}
		return readCredentialFile(path, env)
	}
	agent, err := read("CORTEX_AGENT_RUNTIME_CREDENTIAL_FILE")
	if err != nil {
		return CallerCredentials{}, err
	}
	platform, err := read("CORTEX_PLATFORM_CREDENTIAL_FILE")
	if err != nil {
		return CallerCredentials{}, err
	}
	evolution, err := read("CORTEX_EVOLUTION_CREDENTIAL_FILE")
	if err != nil {
		return CallerCredentials{}, err
	}
	judge, err := read("CORTEX_JUDGE_CREDENTIAL_FILE")
	if err != nil {
		return CallerCredentials{}, err
	}
	credentials := CallerCredentials{AgentRuntime: agent, PlatformControlplane: platform, Evolution: evolution, Judge: judge}
	if err := credentials.Validate(); err != nil {
		return CallerCredentials{}, err
	}
	return credentials, nil
}

func readCredentialFile(path, env string) (string, error) {
	info, err := os.Stat(path) //nolint:gosec // operator-provided credential path
	if err != nil {
		return "", fmt.Errorf("stat %s credential: %w", env, err)
	}
	if !info.Mode().IsRegular() || !secureCredentialFile(path, info) {
		return "", fmt.Errorf("%s credential must be an owner-only regular file", env)
	}
	value, err := os.ReadFile(path) //nolint:gosec // path is an operator-supplied systemd credential
	if err != nil {
		return "", fmt.Errorf("read %s: %w", env, err)
	}
	if token := trimCredentialLineEnding(string(value)); token != "" {
		if strings.TrimSpace(token) != token {
			return "", fmt.Errorf("%s credential contains surrounding whitespace", env)
		}
		return token, nil
	}
	return "", fmt.Errorf("%s credential is empty", env)
}

func secureCredentialFile(path string, info os.FileInfo) bool {
	if info.Mode().Perm()&0o077 == 0 {
		return true
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok {
		return false
	}
	return secureCredentialMode(
		info.Mode().Perm(), stat.Uid, stat.Gid, path, os.Getenv("CREDENTIALS_DIRECTORY"),
	)
}

// systemd exposes credentials to non-root services as root:root 0440 below the
// private CREDENTIALS_DIRECTORY. That directory is inaccessible to unrelated
// services, so this is equivalent to an owner-only standalone credential.
func secureCredentialMode(mode os.FileMode, uid, gid uint32, path, credentialsDirectory string) bool {
	return mode == 0o440 && uid == 0 && gid == 0 && credentialsDirectory != "" &&
		filepath.Clean(filepath.Dir(path)) == filepath.Clean(credentialsDirectory)
}

func trimCredentialLineEnding(value string) string {
	if strings.HasSuffix(value, "\r\n") {
		return strings.TrimSuffix(value, "\r\n")
	}
	return strings.TrimSuffix(value, "\n")
}

func (c CallerCredentials) Validate() error {
	values := []string{c.AgentRuntime, c.PlatformControlplane, c.Evolution, c.Judge}
	for _, value := range values {
		if strings.TrimSpace(value) == "" {
			return fmt.Errorf("all four caller credentials are required")
		}
	}
	for i := range values {
		for j := i + 1; j < len(values); j++ {
			if subtle.ConstantTimeCompare([]byte(values[i]), []byte(values[j])) == 1 {
				return fmt.Errorf("caller credentials must be pairwise distinct")
			}
		}
	}
	return nil
}

type callerRoleContextKey struct{}

func callerRoleFromContext(ctx context.Context) (CallerRole, bool) {
	role, ok := ctx.Value(callerRoleContextKey{}).(CallerRole)
	return role, ok
}

func callerRoleContext(ctx context.Context, role CallerRole) context.Context {
	return context.WithValue(ctx, callerRoleContextKey{}, role)
}

func (c CallerCredentials) roleForToken(token string) (CallerRole, bool) {
	for _, candidate := range []struct {
		token string
		role  CallerRole
	}{
		{c.AgentRuntime, CallerRoleAgentRuntime},
		{c.PlatformControlplane, CallerRolePlatformControlplane},
		{c.Evolution, CallerRoleEvolution},
		{c.Judge, CallerRoleJudge},
	} {
		if subtle.ConstantTimeCompare([]byte(token), []byte(candidate.token)) == 1 {
			return candidate.role, true
		}
	}
	return "", false
}

func (c CallerCredentials) Middleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.HasPrefix(r.URL.Path, "/v1/") {
			next.ServeHTTP(w, r.WithContext(callerRoleContext(r.Context(), CallerRoleExternalCompat)))
			return
		}
		if r.URL.Path != "/internal/agent-runtime" && r.URL.Path != "/internal/llm" {
			next.ServeHTTP(w, r)
			return
		}
		const prefix = "Bearer "
		authorization := r.Header.Get("Authorization")
		if !strings.HasPrefix(authorization, prefix) {
			http.Error(w, "authentication required", http.StatusUnauthorized)
			return
		}
		role, ok := c.roleForToken(strings.TrimPrefix(authorization, prefix))
		if !ok {
			http.Error(w, "authentication failed", http.StatusUnauthorized)
			return
		}
		if r.URL.Path == "/internal/agent-runtime" && role != CallerRoleAgentRuntime {
			http.Error(w, "caller role is not authorized for agent runtime", http.StatusForbidden)
			return
		}
		if r.URL.Path == "/internal/llm" && role == CallerRoleAgentRuntime {
			http.Error(w, "agent runtime caller must use its dedicated endpoint", http.StatusForbidden)
			return
		}
		stripInternalCallerHeaders(r.Header)
		next.ServeHTTP(w, r.WithContext(callerRoleContext(r.Context(), role)))
	})
}

func stripInternalCallerHeaders(header http.Header) {
	header.Del("Authorization")
	header.Del("Proxy-Authorization")
	for name := range header {
		lower := strings.ToLower(name)
		if strings.HasPrefix(lower, "x-sentinel-internal-") ||
			strings.HasPrefix(lower, "x-sentinel-caller-") ||
			lower == "x-internal-caller" {
			header.Del(name)
		}
	}
}

// ClassifyRequest derives the request class exclusively from a server-side
// caller role. Client metadata can validate an already-authorized agent
// runtime request but can never create an internal role.
func ClassifyRequest(path string, req *LLMRequest, role CallerRole) (RequestClass, error) {
	switch role {
	case CallerRoleExternalCompat:
		stripInternalClaims(req.Metadata)
		return RequestClassExternalCompat, nil
	case CallerRoleAgentRuntime:
		if path != "/internal/agent-runtime" || !isPositiveNumericAgentID(req.Metadata["agent_id"]) {
			return RequestClassInternalOther, fmt.Errorf("agent runtime requires a positive agent_id")
		}
		tier, err := parseHierarchyTier(req.Metadata["hierarchy_tier"])
		if err != nil {
			return RequestClassInternalOther, err
		}
		req.HierarchyTier = tier
		return RequestClassAgentRuntime, nil
	case CallerRolePlatformControlplane:
		if hasAgentRuntimeClaims(req.Metadata) {
			return RequestClassInternalOther, fmt.Errorf("agent runtime claims are not allowed on /internal/llm")
		}
		return RequestClassPlatformControlplane, nil
	case CallerRoleEvolution, CallerRoleJudge:
		if hasAgentRuntimeClaims(req.Metadata) {
			return RequestClassInternalOther, fmt.Errorf("agent runtime claims are not allowed on /internal/llm")
		}
		return RequestClassServiceInternal, nil
	default:
		return RequestClassInternalOther, fmt.Errorf("unknown caller role")
	}
}

func hasAgentRuntimeClaims(metadata map[string]string) bool {
	for _, key := range []string{"agent_id", "agent_role", "hierarchy_tier", "tier"} {
		if _, present := metadata[key]; present {
			return true
		}
	}
	return false
}

func stripInternalClaims(metadata map[string]string) {
	for _, key := range []string{"agent_id", "agent_name", "agent_role", "hierarchy_tier", "tier", "platform_analysis", "request_type"} {
		delete(metadata, key)
	}
}

func parseHierarchyTier(value string) (int, error) {
	switch strings.TrimSpace(value) {
	case "1":
		return 1, nil
	case "2":
		return 2, nil
	case "3":
		return 3, nil
	default:
		return 0, fmt.Errorf("hierarchy_tier must be 1, 2, or 3")
	}
}
