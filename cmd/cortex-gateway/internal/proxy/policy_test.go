package proxy

import (
	"strings"
	"testing"
)

func TestClassifyRequestRequiresServerSideRole(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		path    string
		role    CallerRole
		meta    map[string]string
		want    RequestClass
		wantErr bool
	}{
		{
			name: "public role stays external and strips forged agent claims",
			path: "/v1/messages",
			role: CallerRoleExternalCompat,
			meta: map[string]string{"agent_id": "12", "agent_name": "Thomas Mueller"},
			want: RequestClassExternalCompat,
		},
		{
			name: "platform role creates platform class",
			path: "/internal/llm",
			role: CallerRolePlatformControlplane,
			meta: map[string]string{"platform_analysis": "true"},
			want: RequestClassPlatformControlplane,
		},
		{
			name: "evolution role creates service class",
			path: "/internal/llm",
			role: CallerRoleEvolution,
			meta: map[string]string{"request_type": "evolution_analysis"},
			want: RequestClassServiceInternal,
		},
		{
			name:    "judge role rejects agent claims",
			path:    "/internal/llm",
			role:    CallerRoleJudge,
			meta:    map[string]string{"agent_id": "12"},
			wantErr: true,
		},
		{
			name: "agent role creates runtime only on dedicated path",
			path: "/internal/agent-runtime",
			role: CallerRoleAgentRuntime,
			meta: map[string]string{"agent_id": "12", "hierarchy_tier": "2"},
			want: RequestClassAgentRuntime,
		},
		{
			name:    "agent role rejects identity beyond configured roster",
			path:    "/internal/agent-runtime",
			role:    CallerRoleAgentRuntime,
			meta:    map[string]string{"agent_id": "61", "hierarchy_tier": "2"},
			wantErr: true,
		},
		{
			name:    "agent role rejects shared internal path",
			path:    "/internal/llm",
			role:    CallerRoleAgentRuntime,
			meta:    map[string]string{"agent_id": "12", "hierarchy_tier": "2"},
			wantErr: true,
		},
		{
			name:    "agent role rejects invalid tier",
			path:    "/internal/agent-runtime",
			role:    CallerRoleAgentRuntime,
			meta:    map[string]string{"agent_id": "12", "hierarchy_tier": "4"},
			wantErr: true,
		},
		{
			name:    "unknown server role fails closed",
			path:    "/internal/llm",
			role:    CallerRole("forged"),
			meta:    map[string]string{},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			request := &LLMRequest{Metadata: tt.meta}
			got, err := ClassifyRequest(tt.path, request, tt.role)
			if tt.wantErr {
				if err == nil {
					t.Fatalf("ClassifyRequest() = %q, want error", got)
				}
				return
			}
			if err != nil {
				t.Fatalf("ClassifyRequest() error = %v", err)
			}
			if got != tt.want {
				t.Fatalf("ClassifyRequest() = %q, want %q", got, tt.want)
			}
			if tt.role == CallerRoleExternalCompat {
				if _, retained := request.Metadata["agent_id"]; retained {
					t.Fatalf("public agent claim retained: %#v", request.Metadata)
				}
			}
		})
	}
}

func TestResolveModelPolicy(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name          string
		provider      string
		class         RequestClass
		explicitModel string
		policy        string
		wantModel     string
		wantSource    string
		wantErr       string
	}{
		{
			name:          "explicit request model wins",
			provider:      "claude-code",
			class:         RequestClassAgentRuntime,
			explicitModel: "claude-opus-4-6",
			policy:        AgentRuntimeModelPolicyHaiku,
			wantModel:     "claude-opus-4-6",
			wantSource:    PolicySourceRequestOverride,
		},
		{
			name:       "agent runtime defaults to haiku on claude-code",
			provider:   "claude-code",
			class:      RequestClassAgentRuntime,
			policy:     AgentRuntimeModelPolicyHaiku,
			wantModel:  "haiku",
			wantSource: PolicySourceAgentRuntime,
		},
		{
			name:       "agent runtime defaults to haiku on mock provider",
			provider:   "mock",
			class:      RequestClassAgentRuntime,
			policy:     AgentRuntimeModelPolicyHaiku,
			wantModel:  "haiku",
			wantSource: PolicySourceAgentRuntime,
		},
		{
			name:       "agent runtime defaults to haiku on local-loop provider",
			provider:   LocalLoopProviderName,
			class:      RequestClassAgentRuntime,
			policy:     AgentRuntimeModelPolicyHaiku,
			wantModel:  "haiku",
			wantSource: PolicySourceAgentRuntime,
		},
		{
			name:       "external compat keeps provider default",
			provider:   "claude-code",
			class:      RequestClassExternalCompat,
			policy:     AgentRuntimeModelPolicyHaiku,
			wantSource: PolicySourceProviderDefault,
		},
		{
			name:     "unsupported provider fails closed",
			provider: "anthropic-direct",
			class:    RequestClassAgentRuntime,
			policy:   AgentRuntimeModelPolicyHaiku,
			wantErr:  "not supported",
		},
		{
			name:     "unknown policy fails closed",
			provider: "claude-code",
			class:    RequestClassAgentRuntime,
			policy:   "opus",
			wantErr:  "unknown",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			got, err := ResolveModelPolicy(tt.provider, tt.class, tt.explicitModel, tt.policy)
			if tt.wantErr != "" {
				if err == nil || !strings.Contains(err.Error(), tt.wantErr) {
					t.Fatalf("ResolveModelPolicy() error = %v, want containing %q", err, tt.wantErr)
				}
				return
			}
			if err != nil {
				t.Fatalf("ResolveModelPolicy() unexpected error: %v", err)
			}
			if got.Model != tt.wantModel || got.Source != tt.wantSource {
				t.Fatalf("ResolveModelPolicy() = %+v, want model=%q source=%q", got, tt.wantModel, tt.wantSource)
			}
		})
	}
}
