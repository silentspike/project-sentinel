package capability

import (
	"fmt"
	"path/filepath"
	"strconv"
	"strings"
	"sync"

	"github.com/BurntSushi/toml"
)

const (
	wildcardTarget = "*"
	actionToolUse  = "tool_use"
)

var baselineActions = map[string]bool{
	"chat":  true,
	"emote": true,
	"move":  true,
	"work":  true,
	"break": true,
	"think": true,
}

// ActionRequest is the normalized action-validation input.
type ActionRequest struct {
	AgentID    string
	AgentName  string
	ActionType string
	Tool       string
	Target     string
	Content    string
}

// ActionDecision records why an action was allowed or denied.
type ActionDecision struct {
	Allowed    bool
	AgentKey   string
	ActionType string
	Tool       string
	Target     string
	Reason     string
}

// AgentActionCapability is the per-agent least-privilege action policy.
type AgentActionCapability struct {
	AgentID     string
	AgentName   string
	ToolTargets map[string][]string
}

// AgentActionPolicy validates extracted actions against per-agent capabilities.
type AgentActionPolicy struct {
	mu       sync.RWMutex
	byAgent  map[string]AgentActionCapability
	byName   map[string]string
	baseline map[string]bool
}

// NewAgentActionPolicy creates a policy from explicit per-agent definitions.
func NewAgentActionPolicy(defs []AgentActionCapability) *AgentActionPolicy {
	p := &AgentActionPolicy{
		byAgent:  make(map[string]AgentActionCapability),
		byName:   make(map[string]string),
		baseline: cloneBaselineActions(),
	}
	for _, def := range defs {
		p.SetAgent(def)
	}
	return p
}

// LoadAgentActionPolicy loads [capabilities].tools from agent TOML files.
func LoadAgentActionPolicy(agentDir string) (*AgentActionPolicy, error) {
	pattern := filepath.Join(agentDir, "AGENT-*.toml")
	matches, err := filepath.Glob(pattern)
	if err != nil {
		return nil, fmt.Errorf("glob agent capabilities: %w", err)
	}
	if len(matches) == 0 {
		return nil, fmt.Errorf("no agent capability TOMLs found for pattern %s", pattern)
	}

	defs := make([]AgentActionCapability, 0, len(matches))
	for _, path := range matches {
		var raw agentCapabilityTOML
		if _, err := toml.DecodeFile(path, &raw); err != nil {
			return nil, fmt.Errorf("parse agent capabilities %s: %w", path, err)
		}
		if raw.Identity.ID <= 0 {
			return nil, fmt.Errorf("agent capabilities %s missing identity.id", path)
		}
		defs = append(defs, AgentActionCapability{
			AgentID:     canonicalAgentID(strconv.Itoa(raw.Identity.ID)),
			AgentName:   raw.Identity.Name,
			ToolTargets: toolTargetsFromList(raw.Capabilities.Tools),
		})
	}
	return NewAgentActionPolicy(defs), nil
}

// SetAgent inserts or replaces a single agent policy.
func (p *AgentActionPolicy) SetAgent(def AgentActionCapability) {
	if p == nil {
		return
	}
	agentKey := canonicalAgentID(def.AgentID)
	if agentKey == "" {
		return
	}
	def.AgentID = agentKey
	def.ToolTargets = normalizeToolTargets(def.ToolTargets)

	p.mu.Lock()
	defer p.mu.Unlock()
	p.byAgent[agentKey] = def
	if nameKey := canonicalName(def.AgentName); nameKey != "" {
		p.byName[nameKey] = agentKey
	}
}

// Definitions returns a defensive copy of all per-agent policies.
func (p *AgentActionPolicy) Definitions() map[string]AgentActionCapability {
	if p == nil {
		return nil
	}
	p.mu.RLock()
	defer p.mu.RUnlock()

	result := make(map[string]AgentActionCapability, len(p.byAgent))
	for agent, def := range p.byAgent {
		result[agent] = AgentActionCapability{
			AgentID:     def.AgentID,
			AgentName:   def.AgentName,
			ToolTargets: cloneToolTargets(def.ToolTargets),
		}
	}
	return result
}

// Allows validates an extracted action against the agent's policy.
func (p *AgentActionPolicy) Allows(req ActionRequest) ActionDecision {
	actionType := normalizeToken(req.ActionType)
	agentKey := p.resolveAgent(req.AgentID, req.AgentName)
	tool, target := normalizeToolAndTarget(req.Tool, req.Target)
	if tool == "" && actionType == actionToolUse {
		tool, target = inferToolFromContent(req.Content)
	}

	decision := ActionDecision{
		Allowed:    false,
		AgentKey:   agentKey,
		ActionType: actionType,
		Tool:       tool,
		Target:     target,
	}

	if actionType == "" {
		decision.Reason = "missing_action_type"
		return decision
	}
	if p == nil {
		decision.Reason = "policy_unavailable"
		return decision
	}
	if p.baseline[actionType] {
		decision.Allowed = true
		decision.Reason = "baseline_action_allowed"
		return decision
	}
	if actionType != actionToolUse {
		decision.Reason = "unknown_action_type"
		return decision
	}
	if agentKey == "" {
		decision.Reason = "missing_agent"
		return decision
	}
	if tool == "" {
		decision.Reason = "missing_tool"
		return decision
	}

	p.mu.RLock()
	def, ok := p.byAgent[agentKey]
	p.mu.RUnlock()
	if !ok {
		decision.Reason = "unknown_agent"
		return decision
	}
	targets, ok := def.ToolTargets[tool]
	if !ok {
		decision.Reason = "tool_not_allowed"
		return decision
	}
	if targetAllowed(targets, target) {
		decision.Allowed = true
		decision.Reason = "tool_allowed"
		return decision
	}
	decision.Reason = "target_not_allowed"
	return decision
}

func (p *AgentActionPolicy) resolveAgent(agentID, agentName string) string {
	if p == nil {
		return ""
	}
	if key := canonicalAgentID(agentID); key != "" {
		return key
	}
	nameKey := canonicalName(agentName)
	if nameKey == "" {
		return ""
	}
	p.mu.RLock()
	defer p.mu.RUnlock()
	return p.byName[nameKey]
}

type agentCapabilityTOML struct {
	Identity struct {
		ID   int    `toml:"id"`
		Name string `toml:"name"`
	} `toml:"identity"`
	Capabilities struct {
		Tools []string `toml:"tools"`
	} `toml:"capabilities"`
}

func cloneBaselineActions() map[string]bool {
	result := make(map[string]bool, len(baselineActions))
	for action, allowed := range baselineActions {
		result[action] = allowed
	}
	return result
}

func toolTargetsFromList(tools []string) map[string][]string {
	result := make(map[string][]string, len(tools))
	for _, tool := range tools {
		key := normalizeToken(tool)
		if key != "" {
			result[key] = []string{wildcardTarget}
		}
	}
	return result
}

func normalizeToolTargets(in map[string][]string) map[string][]string {
	result := make(map[string][]string, len(in))
	for tool, targets := range in {
		key := normalizeToken(tool)
		if key == "" {
			continue
		}
		normalizedTargets := make([]string, 0, len(targets))
		for _, target := range targets {
			t := normalizeTarget(target)
			if t != "" {
				normalizedTargets = append(normalizedTargets, t)
			}
		}
		if len(normalizedTargets) == 0 {
			normalizedTargets = append(normalizedTargets, wildcardTarget)
		}
		result[key] = normalizedTargets
	}
	return result
}

func cloneToolTargets(in map[string][]string) map[string][]string {
	result := make(map[string][]string, len(in))
	for tool, targets := range in {
		result[tool] = append([]string(nil), targets...)
	}
	return result
}

func canonicalAgentID(id string) string {
	id = strings.TrimSpace(id)
	if id == "" {
		return ""
	}
	upper := strings.ToUpper(id)
	if strings.HasPrefix(upper, "AGENT-") {
		return upper
	}
	n, err := strconv.Atoi(id)
	if err != nil || n <= 0 {
		return ""
	}
	return fmt.Sprintf("AGENT-%02d", n)
}

func canonicalName(name string) string {
	return strings.ToLower(strings.TrimSpace(name))
}

func normalizeToken(v string) string {
	v = strings.ToLower(strings.TrimSpace(v))
	v = strings.ReplaceAll(v, "-", "_")
	v = strings.ReplaceAll(v, " ", "_")
	return v
}

func normalizeTarget(v string) string {
	v = strings.TrimSpace(v)
	if v == wildcardTarget {
		return wildcardTarget
	}
	return strings.ToLower(v)
}

func normalizeToolAndTarget(tool, target string) (string, string) {
	rawTool := strings.TrimSpace(tool)
	rawTarget := strings.TrimSpace(target)
	if rawTool == "" && strings.Contains(rawTarget, ":") {
		parts := strings.SplitN(rawTarget, ":", 2)
		rawTool = parts[0]
		rawTarget = parts[1]
	}
	if rawTool == "" {
		rawTool = rawTarget
		rawTarget = ""
	}
	return normalizeToken(rawTool), normalizeTarget(rawTarget)
}

func inferToolFromContent(content string) (string, string) {
	lower := strings.ToLower(content)
	for _, tool := range []string{"file_write", "file_read", "calendar", "search", "chat"} {
		if strings.Contains(lower, tool) {
			return tool, ""
		}
	}
	return "", ""
}

func targetAllowed(allowed []string, target string) bool {
	target = normalizeTarget(target)
	for _, candidate := range allowed {
		normalized := normalizeTarget(candidate)
		if normalized == wildcardTarget || normalized == target {
			return true
		}
	}
	return false
}
