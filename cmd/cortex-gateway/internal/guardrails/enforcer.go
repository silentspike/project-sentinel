package guardrails

// CheckResult holds the outcome of a guardrails pre-flight check.
type CheckResult struct {
	Allowed          bool
	RateLimited      bool
	RateLimitReason  string
	BudgetExhausted  bool
	FallbackProvider string
}

// GuardrailsStatus aggregates all status data for the dashboard endpoint.
type GuardrailsStatus struct {
	Budget   BudgetStatus `json:"budget"`
	Cost     CostStatus   `json:"cost"`
	RateInfo RateStatus   `json:"rate_limit"`
}

// RateStatus holds rate limiter configuration for status reporting.
type RateStatus struct {
	PerAgentRPM int `json:"per_agent_rpm"`
	GlobalRPM   int `json:"global_rpm"`
}

// Enforcer is the main facade combining rate limiter, budget, cost, and fallback.
type Enforcer struct {
	rateLimiter *RateLimiter
	budget      *BudgetTracker
	costs       *CostTracker
	fallback    *FallbackHandler
	enabled     bool
	rateInfo    RateStatus
}

// New creates an Enforcer from the given config. Returns nil if disabled.
func New(cfg Config) *Enforcer {
	if !cfg.Enabled {
		return nil
	}
	return &Enforcer{
		rateLimiter: NewRateLimiter(cfg.RateLimitPerAgent, cfg.RateLimitGlobal, cfg.BurstMultiple),
		budget:      NewBudgetTracker(cfg.BudgetHourlyTokens, cfg.BudgetDailyTokens),
		costs:       NewCostTracker(cfg.ProviderPrices),
		fallback:    NewFallbackHandler(cfg.FallbackProvider),
		enabled:     true,
		rateInfo: RateStatus{
			PerAgentRPM: cfg.RateLimitPerAgent,
			GlobalRPM:   cfg.RateLimitGlobal,
		},
	}
}

// Check performs pre-flight guardrails checks before a provider call.
func (e *Enforcer) Check(agentID string, estimatedTokens int) CheckResult {
	if !e.enabled {
		return CheckResult{Allowed: true}
	}

	// Rate limit check
	allowed, reason := e.rateLimiter.Allow(agentID)
	if !allowed {
		recordRateLimited(agentID, reason)
		return CheckResult{
			Allowed:         false,
			RateLimited:     true,
			RateLimitReason: reason,
		}
	}

	// Budget check
	if !e.budget.Allow(estimatedTokens) {
		budgetExhaustedTotal.Inc()
		fbProvider, _ := e.fallback.ShouldFallback(true)
		return CheckResult{
			Allowed:          true,
			BudgetExhausted:  true,
			FallbackProvider: fbProvider,
		}
	}

	return CheckResult{Allowed: true}
}

// Record updates budget, cost tracking, and metrics after a provider call.
func (e *Enforcer) Record(provider string, inputTokens, outputTokens int) {
	if !e.enabled {
		return
	}

	e.budget.Record(inputTokens, outputTokens)
	cost := e.costs.Record(provider, inputTokens, outputTokens)

	recordTokenMetrics(provider, inputTokens, outputTokens)
	recordCostMetric(provider, cost)
	updateBudgetGauges(e.budget.Status())
}

// Status returns a snapshot of all guardrails state for the dashboard.
func (e *Enforcer) Status() GuardrailsStatus {
	if !e.enabled {
		return GuardrailsStatus{}
	}
	return GuardrailsStatus{
		Budget:   e.budget.Status(),
		Cost:     e.costs.Status(),
		RateInfo: e.rateInfo,
	}
}
