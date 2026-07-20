package proxy

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"strings"

	"github.com/BurntSushi/toml"
	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/modelpolicy"
)

const CatalogDigestAlgorithm = "cortex-catalog-v1"

const GateBAttestationPrefix = "gate-b"

// HierarchyModelMap is a complete provider-specific model map. Hierarchy tier
// and model/pricing tier are deliberately different concepts.
type HierarchyModelMap struct {
	Tier1 string `toml:"tier_1" json:"tier_1"`
	Tier2 string `toml:"tier_2" json:"tier_2"`
	Tier3 string `toml:"tier_3" json:"tier_3"`
}

// ProviderCatalogEntry is immutable after startup. Endpoint and credential
// configuration are deployment concerns and are intentionally not part of its
// semantic digest.
type ProviderCatalogEntry struct {
	Type            string            `toml:"type"`
	BaseURL         string            `toml:"base_url"`
	Binary          string            `toml:"binary"`
	MaxTokens       int               `toml:"max_tokens"`
	Priority        int               `toml:"priority"`
	DefaultModel    string            `toml:"default_model"`
	AllowedModels   []string          `toml:"allowed_models"`
	HierarchyModels HierarchyModelMap `toml:"hierarchy_models"`
}

type ProviderCatalog struct {
	providers map[string]ProviderCatalogEntry
	digest    string
}

type providerCatalogDocument struct {
	Providers map[string]ProviderCatalogEntry `toml:"providers"`
}

func LoadProviderCatalog(path string) (*ProviderCatalog, error) {
	data, err := os.ReadFile(path) //nolint:gosec // operator-selected trusted startup config
	if err != nil {
		return nil, fmt.Errorf("read provider catalog: %w", err)
	}
	var document providerCatalogDocument
	metadata, err := toml.Decode(string(data), &document)
	if err != nil {
		return nil, fmt.Errorf("decode provider catalog: %w", err)
	}
	for _, key := range metadata.Undecoded() {
		parts := key.String()
		if strings.HasPrefix(parts, "providers.") {
			return nil, fmt.Errorf("unknown provider catalog field %q", parts)
		}
	}
	catalog := ProviderCatalog{providers: document.Providers}
	if err := catalog.Validate(); err != nil {
		return nil, err
	}
	digest, err := catalog.SemanticDigest()
	if err != nil {
		return nil, err
	}
	catalog.digest = digest
	return &catalog, nil
}

func (c *ProviderCatalog) Validate() error {
	if c == nil || len(c.providers) == 0 {
		return fmt.Errorf("provider catalog must not be empty")
	}
	for id, entry := range c.providers {
		if strings.TrimSpace(id) == "" || strings.TrimSpace(entry.Type) == "" {
			return fmt.Errorf("provider id and type must not be empty")
		}
		if id != strings.TrimSpace(id) || entry.Type != strings.TrimSpace(entry.Type) {
			return fmt.Errorf("provider id and type must not contain surrounding whitespace")
		}
		if strings.TrimSpace(entry.DefaultModel) == "" || len(entry.AllowedModels) == 0 {
			return fmt.Errorf("provider %q has an incomplete model catalog", id)
		}
		if entry.DefaultModel != strings.TrimSpace(entry.DefaultModel) {
			return fmt.Errorf("provider %q default model must not contain surrounding whitespace", id)
		}
		allowed := make(map[string]struct{}, len(entry.AllowedModels))
		for _, model := range entry.AllowedModels {
			trimmed := strings.TrimSpace(model)
			if trimmed == "" {
				return fmt.Errorf("provider %q has an empty allowed model", id)
			}
			if model != trimmed {
				return fmt.Errorf("provider %q allowed model %q has surrounding whitespace", id, model)
			}
			if _, duplicate := allowed[model]; duplicate {
				return fmt.Errorf("provider %q repeats allowed model %q", id, model)
			}
			allowed[model] = struct{}{}
		}
		for label, model := range map[string]string{
			"default": entry.DefaultModel,
			"tier_1":  entry.HierarchyModels.Tier1,
			"tier_2":  entry.HierarchyModels.Tier2,
			"tier_3":  entry.HierarchyModels.Tier3,
		} {
			if model != strings.TrimSpace(model) {
				return fmt.Errorf("provider %q %s model has surrounding whitespace", id, label)
			}
			if _, ok := allowed[model]; !ok {
				return fmt.Errorf("provider %q %s model %q is not allowed", id, label, model)
			}
		}
	}
	return nil
}

func (c *ProviderCatalog) Entry(provider string) (ProviderCatalogEntry, bool) {
	if c == nil {
		return ProviderCatalogEntry{}, false
	}
	entry, ok := c.providers[strings.TrimSpace(provider)]
	entry.AllowedModels = append([]string(nil), entry.AllowedModels...)
	return entry, ok
}

// ProviderIDs returns the immutable catalog keys in stable order for
// public-safe diagnostics.
func (c *ProviderCatalog) ProviderIDs() []string {
	if c == nil {
		return nil
	}
	ids := make([]string, 0, len(c.providers))
	for id := range c.providers {
		ids = append(ids, id)
	}
	sort.Strings(ids)
	return ids
}

// RequireProviders binds the startup catalog exactly to the production
// composition root. Test-only providers belong in isolated test catalogs.
func (c *ProviderCatalog) RequireProviders(required map[string]string) error {
	if len(c.providers) != len(required) {
		return fmt.Errorf("startup catalog has %d providers, production composition requires exactly %d", len(c.providers), len(required))
	}
	for id, providerType := range required {
		entry, ok := c.Entry(id)
		if !ok {
			return fmt.Errorf("required provider %q is missing from the startup catalog", id)
		}
		if entry.Type != providerType {
			return fmt.Errorf("provider %q has type %q, expected %q", id, entry.Type, providerType)
		}
	}
	return nil
}

// ValidateInventory compares a token-free provider inventory readback with the
// immutable startup allowlist. It never mutates catalog state.
func (c *ProviderCatalog) ValidateInventory(provider string, availableModels []string) error {
	entry, ok := c.Entry(provider)
	if !ok {
		return fmt.Errorf("provider %q is not in the startup catalog", provider)
	}
	available := make(map[string]struct{}, len(availableModels))
	for _, model := range availableModels {
		trimmed := strings.TrimSpace(model)
		if trimmed == "" {
			return fmt.Errorf("provider %q inventory contains an empty model id", provider)
		}
		if model != trimmed {
			return fmt.Errorf("provider %q inventory model %q has surrounding whitespace", provider, model)
		}
		if _, duplicate := available[model]; duplicate {
			return fmt.Errorf("provider %q inventory repeats model %q", provider, model)
		}
		available[model] = struct{}{}
	}
	for _, required := range entry.AllowedModels {
		if _, ok := available[required]; !ok {
			return fmt.Errorf("provider %q inventory is missing catalog model %q", provider, required)
		}
	}
	if len(available) != len(entry.AllowedModels) {
		for model := range available {
			known := false
			for _, allowed := range entry.AllowedModels {
				if model == allowed {
					known = true
					break
				}
			}
			if !known {
				return fmt.Errorf("provider %q inventory has uncataloged model %q", provider, model)
			}
		}
		return fmt.Errorf("provider %q inventory cardinality differs from startup catalog", provider)
	}
	return nil
}

func (c *ProviderCatalog) ValidatePolicy(policy modelpolicy.Policy) error {
	if legacy, ok := policy.LegacyValue(); ok {
		if legacy == "" || legacy == AgentRuntimeModelPolicyHaiku {
			return nil
		}
		return fmt.Errorf("unsupported legacy model policy %q", legacy)
	}
	for provider, models := range policy.Providers() {
		entry, ok := c.Entry(provider)
		if !ok {
			return fmt.Errorf("policy provider %q is not in the startup catalog", provider)
		}
		for _, model := range []string{models.Tier1, models.Tier2, models.Tier3} {
			allowed := false
			for _, candidate := range entry.AllowedModels {
				if model == candidate {
					allowed = true
					break
				}
			}
			if !allowed {
				return fmt.Errorf("policy model %q is not allowed for provider %q", model, provider)
			}
		}
	}
	return nil
}

func (c *ProviderCatalog) ResolvePolicy(provider string, class RequestClass, hierarchyTier int, explicitModel string, policy modelpolicy.Policy) (ModelPolicyResolution, error) {
	if strings.TrimSpace(explicitModel) != "" || class != RequestClassAgentRuntime {
		return c.Resolve(provider, 0, explicitModel)
	}
	if legacy, ok := policy.LegacyValue(); ok {
		switch legacy {
		case "":
			return c.Resolve(provider, hierarchyTier, "")
		case AgentRuntimeModelPolicyHaiku:
			if provider != "claude-code" && provider != LocalLoopProviderName && provider != "mock" {
				return ModelPolicyResolution{}, fmt.Errorf("legacy model policy %q is not supported for provider %q", legacy, provider)
			}
			entry, exists := c.Entry(provider)
			if !exists {
				return ModelPolicyResolution{}, fmt.Errorf("provider %q is not in the startup catalog", provider)
			}
			resolved, err := c.Resolve(provider, hierarchyTier, entry.HierarchyModels.Tier3)
			resolved.Source = PolicySourceAgentRuntime
			return resolved, err
		}
	}
	model, err := policy.Model(provider, hierarchyTier)
	if err != nil {
		return ModelPolicyResolution{}, err
	}
	resolved, err := c.Resolve(provider, hierarchyTier, model)
	resolved.Source = PolicySourceAgentRuntime
	return resolved, err
}

func (c *ProviderCatalog) Resolve(provider string, hierarchyTier int, explicitModel string) (ModelPolicyResolution, error) {
	entry, ok := c.Entry(provider)
	if !ok {
		return ModelPolicyResolution{}, fmt.Errorf("provider %q is not in the startup catalog", provider)
	}
	model := strings.TrimSpace(explicitModel)
	source := PolicySourceRequestOverride
	if model == "" {
		source = PolicySourceHierarchyTier
		switch hierarchyTier {
		case 1:
			model = entry.HierarchyModels.Tier1
		case 2:
			model = entry.HierarchyModels.Tier2
		case 3:
			model = entry.HierarchyModels.Tier3
		case 0:
			model = entry.DefaultModel
			source = PolicySourceProviderDefault
		default:
			return ModelPolicyResolution{}, fmt.Errorf("hierarchy tier must be 1, 2, or 3")
		}
	}
	for _, allowed := range entry.AllowedModels {
		if model == allowed {
			return ModelPolicyResolution{Model: model, Source: source}, nil
		}
	}
	return ModelPolicyResolution{}, fmt.Errorf("model %q is not allowed for provider %q", model, provider)
}

func (c *ProviderCatalog) SemanticDigest() (string, error) {
	// cortex-catalog-v1 intentionally excludes hierarchy_models. They are
	// validated above and pinned by Gate C's full-file/blob hashes; the semantic
	// digest covers only provider identity/type, default, and complete allowlist.
	entries := make([]map[string]any, 0, len(c.providers))
	for id, provider := range c.providers {
		allowed := append([]string(nil), provider.AllowedModels...)
		sort.Strings(allowed)
		entries = append(entries, map[string]any{
			"id": id, "type": provider.Type, "default_model": provider.DefaultModel, "allowed_models": allowed,
		})
	}
	sort.Slice(entries, func(i, j int) bool { return entries[i]["id"].(string) < entries[j]["id"].(string) })
	payload, err := json.Marshal(map[string]any{"algorithm": CatalogDigestAlgorithm, "providers": entries})
	if err != nil {
		return "", fmt.Errorf("encode semantic provider catalog: %w", err)
	}
	sum := sha256.Sum256(append(payload, '\n'))
	return hex.EncodeToString(sum[:]), nil
}

func (c *ProviderCatalog) Digest() string {
	if c == nil {
		return ""
	}
	return c.digest
}

// ExpectedGateBAttestation binds an explicit deployment approval to both the
// active provider and the immutable semantic catalog. It is public deployment
// metadata, not a credential.
func (c *ProviderCatalog) ExpectedGateBAttestation(provider string) string {
	return strings.Join([]string{GateBAttestationPrefix, strings.TrimSpace(provider), c.Digest()}, ":")
}

// ValidateProviderActivation keeps providers without a token-free inventory
// contract disabled until Gate B records an exact provider+catalog attestation.
// The deterministic local-loop fixture is the sole no-inventory exception.
func (c *ProviderCatalog) ValidateProviderActivation(provider string, inventoryCapable bool, attestation string) error {
	provider = strings.TrimSpace(provider)
	if _, ok := c.Entry(provider); !ok {
		return fmt.Errorf("provider %q is not in the startup catalog", provider)
	}
	if provider == LocalLoopProviderName || inventoryCapable {
		return nil
	}
	if strings.TrimSpace(attestation) != c.ExpectedGateBAttestation(provider) {
		return fmt.Errorf("provider %q requires an explicit Gate B model-catalog attestation", provider)
	}
	return nil
}
