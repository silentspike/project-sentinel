package synthesis

import (
	"log/slog"
	"strings"
	"sync"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
)

// Decision indicates whether a request should be synthesized or forwarded to the LLM.
type Decision int

const (
	// Forward sends the request to the real LLM provider.
	Forward Decision = iota
	// Synthesize generates a response locally without an API call.
	Synthesize
)

var (
	synthesizedTotal = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_synthesis_total",
		Help: "Total requests answered by synthesis (no API call)",
	})
	forwardedTotal = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_synthesis_forwarded_total",
		Help: "Total requests forwarded to LLM provider",
	})
	synthesisLatency = promauto.NewHistogram(prometheus.HistogramOpts{
		Name:    "sentinel_synthesis_decision_seconds",
		Help:    "Time to make synthesis decision",
		Buckets: []float64{0.0001, 0.0005, 0.001, 0.005, 0.01},
	})
)

// Engine is the synthesis decision engine. It evaluates fingerprints against
// deterministic rules and generates responses locally when possible.
type Engine struct {
	rules   []Rule
	enabled bool
	logger  *slog.Logger

	// mu guards ruleEnabled for live per-rule toggling from the control plane (#429).
	mu sync.RWMutex
	// ruleEnabled holds the per-rule active state. A missing key counts as enabled.
	ruleEnabled map[string]bool
}

// NewEngine creates a synthesis engine with the default rule set.
func NewEngine(enabled bool, logger *slog.Logger) *Engine {
	if logger == nil {
		logger = slog.Default()
	}
	rules := DefaultRules()
	// Init per-rule state by iterating the actual rule set (not a hardcoded list),
	// so every default rule is enabled and a future rule can never be silently OFF.
	ruleEnabled := make(map[string]bool, len(rules))
	for _, r := range rules {
		ruleEnabled[r.Name] = true
	}
	return &Engine{
		rules:       rules,
		enabled:     enabled,
		logger:      logger,
		ruleEnabled: ruleEnabled,
	}
}

// isRuleEnabled reports whether the named rule is active. A missing key counts as
// enabled, so a rule newly added to DefaultRules is never silently OFF (#429).
func (e *Engine) isRuleEnabled(name string) bool {
	e.mu.RLock()
	defer e.mu.RUnlock()
	en, ok := e.ruleEnabled[name]
	return !ok || en
}

// RuleStates returns the per-rule enable state in DefaultRules order (#429).
func (e *Engine) RuleStates() []RuleState {
	e.mu.RLock()
	defer e.mu.RUnlock()
	states := make([]RuleState, 0, len(e.rules))
	for _, r := range e.rules {
		en, ok := e.ruleEnabled[r.Name]
		states = append(states, RuleState{Name: r.Name, Enabled: !ok || en})
	}
	return states
}

// SetRuleEnabled toggles a single rule by name. Returns false for an unknown rule (#429).
func (e *Engine) SetRuleEnabled(name string, enabled bool) bool {
	e.mu.Lock()
	defer e.mu.Unlock()
	if _, ok := e.ruleEnabled[name]; !ok {
		return false
	}
	e.ruleEnabled[name] = enabled
	return true
}

// Enabled returns whether synthesis is active.
func (e *Engine) Enabled() bool {
	return e.enabled
}

// SetEnabled toggles synthesis on/off (for Control Plane).
func (e *Engine) SetEnabled(v bool) {
	e.enabled = v
}

// Result holds the synthesis decision outcome.
type Result struct {
	Decision Decision
	Content  string   // synthesized response text (only if Synthesize)
	Rule     string   // matched rule name (only if Synthesize)
	Actions  []Action // pre-built actions (only if Synthesize)
}

// Decide evaluates the request metadata and returns a synthesis decision.
// agentName is used to personalize synthesis templates (e.g. "AGENT-05" or "Thomas Fischer").
// Returns Forward if synthesis is disabled, fingerprint is invalid,
// or no rule matches.
func (e *Engine) Decide(metadata map[string]string, agentName string) Result {
	start := time.Now()
	defer func() {
		synthesisLatency.Observe(time.Since(start).Seconds())
	}()

	fpRaw := metadata["synth_fp"]
	if fpRaw == "" {
		forwardedTotal.Inc()
		return Result{Decision: Forward}
	}

	fp, ctx, err := PrepareInputs(metadata)
	if err != nil {
		e.logger.Warn("fingerprint parse error", "error", err, "raw", fpRaw)
		forwardedTotal.Inc()
		return Result{Decision: Forward}
	}

	personalityType := fp.Personality
	if personalityType == "" {
		personalityType = "E" // default extrovert
	}

	for _, rule := range e.rules {
		if !e.isRuleEnabled(rule.Name) {
			continue
		}
		if rule.Match(fp, ctx) {
			template, ok := rule.Templates[personalityType]
			if !ok {
				template = rule.Templates["E"] // fallback
			}

			// Personalize template with agent name for Drift-Guard compatibility
			content := strings.Replace(template, "{name}", agentName, 1)

			e.logger.Info("synthesis match",
				"rule", rule.Name,
				"agent_id", ctx.AgentID,
				"agent_name", agentName,
				"personality", personalityType,
				"room", fp.RoomID,
			)
			synthesizedTotal.Inc()

			actions := rule.Actions
			if rule.Build != nil {
				actions = rule.Build(fp, ctx)
			}

			return Result{
				Decision: Synthesize,
				Content:  content,
				Rule:     rule.Name,
				Actions:  actions,
			}
		}
	}

	forwardedTotal.Inc()
	return Result{Decision: Forward}
}
