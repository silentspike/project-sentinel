package synthesis

import (
	"log/slog"
	"strings"
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
}

// NewEngine creates a synthesis engine with the default rule set.
func NewEngine(enabled bool, logger *slog.Logger) *Engine {
	if logger == nil {
		logger = slog.Default()
	}
	return &Engine{
		rules:   DefaultRules(),
		enabled: enabled,
		logger:  logger,
	}
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
