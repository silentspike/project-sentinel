package proxy

import (
	"fmt"
	"strings"
)

type RequestClass string

const (
	RequestClassExternalCompat       RequestClass = "external_compat"
	RequestClassAgentRuntime         RequestClass = "agent_runtime"
	RequestClassPlatformControlplane RequestClass = "platform_controlplane"
	RequestClassServiceInternal      RequestClass = "service_internal"
	RequestClassInternalOther        RequestClass = "internal_other"
)

const (
	AgentRuntimeModelPolicyHaiku = "haiku"

	PolicySourceProviderDefault = "provider_default"
	PolicySourceRequestOverride = "request_override"
	PolicySourceAgentRuntime    = "agent_runtime_policy"
)

func ClassifyRequest(path string, req *LLMRequest) RequestClass {
	if isAnthropicMessagesPath(path) {
		return RequestClassExternalCompat
	}
	if req == nil {
		return RequestClassInternalOther
	}

	metadata := req.Metadata
	agentName := strings.TrimSpace(metadata["agent_name"])
	if strings.EqualFold(strings.TrimSpace(metadata["platform_analysis"]), "true") ||
		strings.EqualFold(agentName, "PLATFORM-CONTROLPLANE") {
		return RequestClassPlatformControlplane
	}

	if strings.TrimSpace(metadata["request_type"]) != "" || isServiceIdentity(agentName) {
		return RequestClassServiceInternal
	}

	if isPositiveNumericAgentID(metadata["agent_id"]) {
		return RequestClassAgentRuntime
	}

	return RequestClassInternalOther
}

type ModelPolicyResolution struct {
	Model  string
	Source string
}

func ResolveModelPolicy(providerName string, class RequestClass, explicitModel, agentRuntimePolicy string) (ModelPolicyResolution, error) {
	if model := strings.TrimSpace(explicitModel); model != "" {
		return ModelPolicyResolution{Model: model, Source: PolicySourceRequestOverride}, nil
	}

	if class != RequestClassAgentRuntime {
		return ModelPolicyResolution{Source: PolicySourceProviderDefault}, nil
	}

	policy := strings.TrimSpace(agentRuntimePolicy)
	if policy == "" {
		return ModelPolicyResolution{Source: PolicySourceProviderDefault}, nil
	}

	model, err := resolveAgentRuntimePolicyModel(providerName, policy)
	if err != nil {
		return ModelPolicyResolution{}, err
	}
	return ModelPolicyResolution{Model: model, Source: PolicySourceAgentRuntime}, nil
}

func ValidateAgentRuntimeModelPolicy(policy string) error {
	policy = strings.TrimSpace(policy)
	switch policy {
	case "", AgentRuntimeModelPolicyHaiku:
		return nil
	default:
		return fmt.Errorf("agent_runtime_model_policy must be empty or %q, got %q", AgentRuntimeModelPolicyHaiku, policy)
	}
}

func resolveAgentRuntimePolicyModel(providerName, policy string) (string, error) {
	switch strings.TrimSpace(policy) {
	case AgentRuntimeModelPolicyHaiku:
		switch strings.TrimSpace(providerName) {
		case "claude-code", "mock", LocalLoopProviderName:
			return "haiku", nil
		default:
			return "", fmt.Errorf("agent_runtime_model_policy %q is not supported for provider %q", policy, providerName)
		}
	default:
		return "", fmt.Errorf("unknown agent_runtime_model_policy %q", policy)
	}
}

func isServiceIdentity(agentName string) bool {
	switch strings.ToLower(strings.TrimSpace(agentName)) {
	case "sentinel-judge":
		return true
	default:
		return false
	}
}

func isPositiveNumericAgentID(value string) bool {
	value = strings.TrimSpace(value)
	if value == "" || value[0] == '0' {
		return false
	}
	for _, r := range value {
		if r < '0' || r > '9' {
			return false
		}
	}
	return true
}
