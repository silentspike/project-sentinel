package guardrails

import (
	"sync"
	"time"
)

// BudgetStatus represents the current budget usage for the dashboard.
type BudgetStatus struct {
	HourlyUsed  int64 `json:"hourly_used"`
	HourlyLimit int64 `json:"hourly_limit"`
	DailyUsed   int64 `json:"daily_used"`
	DailyLimit  int64 `json:"daily_limit"`
}

// BudgetTracker tracks token usage against hourly and daily limits.
type BudgetTracker struct {
	mu          sync.Mutex
	hourlyLimit int64
	dailyLimit  int64
	hourlyUsed  int64
	dailyUsed   int64
	hourlyReset time.Time
	dailyReset  time.Time
	nowFunc     func() time.Time
}

// NewBudgetTracker creates a budget tracker. Limits of 0 mean unlimited.
func NewBudgetTracker(hourlyLimit, dailyLimit int64) *BudgetTracker {
	now := time.Now()
	return &BudgetTracker{
		hourlyLimit: hourlyLimit,
		dailyLimit:  dailyLimit,
		hourlyReset: now.Truncate(time.Hour).Add(time.Hour),
		dailyReset:  nextMidnight(now),
		nowFunc:     time.Now,
	}
}

func nextMidnight(t time.Time) time.Time {
	y, m, d := t.Date()
	return time.Date(y, m, d+1, 0, 0, 0, 0, t.Location())
}

// maybeReset resets counters if the time window has elapsed.
// Must be called with mu held.
func (bt *BudgetTracker) maybeReset() {
	now := bt.nowFunc()
	if now.After(bt.hourlyReset) || now.Equal(bt.hourlyReset) {
		bt.hourlyUsed = 0
		bt.hourlyReset = now.Truncate(time.Hour).Add(time.Hour)
	}
	if now.After(bt.dailyReset) || now.Equal(bt.dailyReset) {
		bt.dailyUsed = 0
		bt.dailyReset = nextMidnight(now)
	}
}

// Allow checks whether a request with estimatedTokens fits within budget.
func (bt *BudgetTracker) Allow(estimatedTokens int) bool {
	bt.mu.Lock()
	defer bt.mu.Unlock()

	bt.maybeReset()

	est := int64(estimatedTokens)

	if bt.hourlyLimit > 0 && bt.hourlyUsed+est > bt.hourlyLimit {
		return false
	}
	if bt.dailyLimit > 0 && bt.dailyUsed+est > bt.dailyLimit {
		return false
	}
	return true
}

// Record updates the budget with actual token usage after a provider call.
func (bt *BudgetTracker) Record(inputTokens, outputTokens int) {
	bt.mu.Lock()
	defer bt.mu.Unlock()

	bt.maybeReset()

	total := int64(inputTokens + outputTokens)
	bt.hourlyUsed += total
	bt.dailyUsed += total
}

// Status returns the current budget usage for the dashboard endpoint.
func (bt *BudgetTracker) Status() BudgetStatus {
	bt.mu.Lock()
	defer bt.mu.Unlock()

	bt.maybeReset()

	return BudgetStatus{
		HourlyUsed:  bt.hourlyUsed,
		HourlyLimit: bt.hourlyLimit,
		DailyUsed:   bt.dailyUsed,
		DailyLimit:  bt.dailyLimit,
	}
}
