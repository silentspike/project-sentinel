package modelpolicy

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"strings"
)

// TierModels is a complete provider-specific hierarchy map.
type TierModels struct {
	Tier1 string `json:"tier1"`
	Tier2 string `json:"tier2"`
	Tier3 string `json:"tier3"`
}

// Policy preserves the legacy JSON string form while supporting the approved
// provider-specific object form. Its fields are private so callers cannot
// mutate a Config snapshot through a shared map.
type Policy struct {
	legacy    string
	tiered    bool
	providers map[string]TierModels
}

func Legacy(value string) Policy {
	return Policy{legacy: strings.TrimSpace(value)}
}

func (p Policy) IsTiered() bool { return p.tiered }

func (p Policy) LegacyValue() (string, bool) {
	return p.legacy, !p.tiered
}

func (p Policy) Providers() map[string]TierModels {
	result := make(map[string]TierModels, len(p.providers))
	for id, models := range p.providers {
		result[id] = models
	}
	return result
}

func (p Policy) Clone() Policy {
	if !p.tiered {
		return Legacy(p.legacy)
	}
	return Policy{tiered: true, providers: p.Providers()}
}

func (p Policy) Model(provider string, tier int) (string, error) {
	if !p.tiered {
		return "", fmt.Errorf("legacy policy has no provider tier map")
	}
	models, ok := p.providers[provider]
	if !ok {
		return "", fmt.Errorf("model policy has no map for provider %q", provider)
	}
	switch tier {
	case 1:
		return models.Tier1, nil
	case 2:
		return models.Tier2, nil
	case 3:
		return models.Tier3, nil
	default:
		return "", fmt.Errorf("hierarchy tier must be 1, 2, or 3")
	}
}

func ParseValue(value any) (Policy, error) {
	encoded, err := json.Marshal(value)
	if err != nil {
		return Policy{}, fmt.Errorf("encode model policy: %w", err)
	}
	var policy Policy
	if err := json.Unmarshal(encoded, &policy); err != nil {
		return Policy{}, err
	}
	return policy, nil
}

func (p Policy) MarshalJSON() ([]byte, error) {
	if !p.tiered {
		return json.Marshal(p.legacy)
	}
	return json.Marshal(struct {
		Providers map[string]TierModels `json:"providers"`
	}{Providers: p.providers})
}

func (p *Policy) UnmarshalJSON(data []byte) error {
	data = bytes.TrimSpace(data)
	if len(data) == 0 {
		return fmt.Errorf("agent_runtime_model_policy must not be empty JSON")
	}
	if data[0] == '"' {
		var legacy string
		if err := json.Unmarshal(data, &legacy); err != nil {
			return err
		}
		legacy = strings.TrimSpace(legacy)
		if legacy != "" && legacy != "haiku" {
			return fmt.Errorf("legacy agent_runtime_model_policy must be empty or %q", "haiku")
		}
		*p = Legacy(legacy)
		return nil
	}

	var object struct {
		Providers map[string]TierModels `json:"providers"`
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&object); err != nil {
		return fmt.Errorf("decode tiered agent_runtime_model_policy: %w", err)
	}
	if err := decoder.Decode(&struct{}{}); err != io.EOF {
		if err == nil {
			return fmt.Errorf("decode tiered agent_runtime_model_policy: multiple JSON values")
		}
		return fmt.Errorf("decode tiered agent_runtime_model_policy: %w", err)
	}
	if len(object.Providers) == 0 {
		return fmt.Errorf("tiered agent_runtime_model_policy requires providers")
	}
	for id, models := range object.Providers {
		if strings.TrimSpace(id) == "" || strings.TrimSpace(models.Tier1) == "" || strings.TrimSpace(models.Tier2) == "" || strings.TrimSpace(models.Tier3) == "" {
			return fmt.Errorf("provider %q requires non-empty tier1, tier2, and tier3", id)
		}
	}
	*p = Policy{tiered: true, providers: object.Providers}
	return nil
}
