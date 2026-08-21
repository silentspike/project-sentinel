package proxy

import (
	"fmt"
	"strconv"
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
	maxAgentRuntimeID            = 60

	PolicySourceProviderDefault = "provider_default"
	PolicySourceRequestOverride = "request_override"
	PolicySourceAgentRuntime    = "agent_runtime_policy"
	PolicySourceHierarchyTier   = "hierarchy_tier"
)

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

func isPositiveNumericAgentID(value string) bool {
	value = strings.TrimSpace(value)
	if value == "" || value[0] == '0' {
		return false
	}
	id, err := strconv.ParseUint(value, 10, 16)
	return err == nil && id >= 1 && id <= maxAgentRuntimeID
}
