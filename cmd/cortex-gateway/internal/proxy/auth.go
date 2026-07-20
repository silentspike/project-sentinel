package proxy

import (
	"context"
	"crypto/subtle"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"syscall"
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
