package proxy

import (
	"strings"
	"testing"
)

func TestClassifyRequestStrictAgentRuntime(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name string
		path string
		meta map[string]string
		want RequestClass
	}{
		{
			name: "anthropic messages stays external even with agent id",
			path: "/v1/messages",
			meta: map[string]string{"agent_id": "12", "agent_name": "Thomas Mueller"},
			want: RequestClassExternalCompat,
		},
		{
			name: "platform analysis metadata wins over agent id",
			path: "/internal/llm",
			meta: map[string]string{"agent_id": "12", "platform_analysis": "true"},
			want: RequestClassPlatformControlplane,
		},
		{
			name: "platform controlplane identity wins over agent id",
			path: "/internal/llm",
			meta: map[string]string{"agent_id": "12", "agent_name": "PLATFORM-CONTROLPLANE"},
			want: RequestClassPlatformControlplane,
		},
		{
			name: "request type marks service internal",
			path: "/internal/llm",
			meta: map[string]string{"agent_id": "12", "request_type": "evolution_analysis"},
			want: RequestClassServiceInternal,
		},
		{
			name: "sentinel judge identity marks service internal",
			path: "/internal/llm",
			meta: map[string]string{"agent_id": "12", "agent_name": "sentinel-judge"},
			want: RequestClassServiceInternal,
		},
		{
			name: "positive numeric agent id marks runtime",
			path: "/internal/llm",
			meta: map[string]string{"agent_id": "12", "agent_name": "Thomas Mueller"},
			want: RequestClassAgentRuntime,
		},
		{
			name: "zero is not a runtime agent",
			path: "/internal/llm",
			meta: map[string]string{"agent_id": "0", "agent_name": "Thomas Mueller"},
			want: RequestClassInternalOther,
		},
		{
			name: "leading zero is not a runtime agent",
			path: "/internal/llm",
			meta: map[string]string{"agent_id": "012", "agent_name": "Thomas Mueller"},
			want: RequestClassInternalOther,
		},
		{
			name: "non numeric agent id is not a runtime agent",
			path: "/internal/llm",
			meta: map[string]string{"agent_id": "agent-12", "agent_name": "Thomas Mueller"},
			want: RequestClassInternalOther,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			got := ClassifyRequest(tt.path, &LLMRequest{Metadata: tt.meta})
			if got != tt.want {
				t.Fatalf("ClassifyRequest() = %q, want %q", got, tt.want)
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
