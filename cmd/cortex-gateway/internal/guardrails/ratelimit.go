package guardrails

import (
	"sync"
	"time"
)

// bucket implements a token-bucket rate limiter.
type bucket struct {
	tokens    float64
	max       float64
	rate      float64 // tokens/sec refill
	lastCheck time.Time
}

func newBucket(callsPerMin int, burstMultiple int, now time.Time) *bucket {
	rate := float64(callsPerMin) / 60.0
	max := rate * float64(burstMultiple)
	if max < 1 {
		max = 1
	}
	return &bucket{
		tokens:    max,
		max:       max,
		rate:      rate,
		lastCheck: now,
	}
}

func (b *bucket) allow(now time.Time) bool {
	elapsed := now.Sub(b.lastCheck).Seconds()
	b.lastCheck = now
	b.tokens += elapsed * b.rate
	if b.tokens > b.max {
		b.tokens = b.max
	}
	if b.tokens < 1 {
		return false
	}
	b.tokens--
	return true
}

// RateLimiter enforces per-agent and global call rate limits using token buckets.
type RateLimiter struct {
	mu            sync.Mutex
	perAgent      map[string]*bucket
	global        *bucket
	agentRPM      int
	globalRPM     int
	burstMultiple int
	nowFunc       func() time.Time
}

// NewRateLimiter creates a rate limiter with the given calls-per-minute limits.
// A burstMultiple of 0 defaults to 2.
func NewRateLimiter(agentCallsPerMin, globalCallsPerMin, burstMultiple int) *RateLimiter {
	if burstMultiple <= 0 {
		burstMultiple = 2
	}
	now := time.Now()
	return &RateLimiter{
		perAgent:      make(map[string]*bucket),
		global:        newBucket(globalCallsPerMin, burstMultiple, now),
		agentRPM:      agentCallsPerMin,
		globalRPM:     globalCallsPerMin,
		burstMultiple: burstMultiple,
		nowFunc:       time.Now,
	}
}

// Allow checks whether a request from agentID is allowed.
// Returns true if allowed, false if rate-limited.
// The second return value indicates the reason: "per_agent" or "global".
func (rl *RateLimiter) Allow(agentID string) (bool, string) {
	rl.mu.Lock()
	defer rl.mu.Unlock()

	now := rl.nowFunc()

	// Check global limit first
	if !rl.global.allow(now) {
		return false, "global"
	}

	// Check per-agent limit
	if agentID != "" && rl.agentRPM > 0 {
		ab, ok := rl.perAgent[agentID]
		if !ok {
			ab = newBucket(rl.agentRPM, rl.burstMultiple, now)
			rl.perAgent[agentID] = ab
		}
		if !ab.allow(now) {
			return false, "per_agent"
		}
	}

	return true, ""
}

// Reset clears all state (for testing).
func (rl *RateLimiter) Reset() {
	rl.mu.Lock()
	defer rl.mu.Unlock()
	now := rl.nowFunc()
	rl.perAgent = make(map[string]*bucket)
	rl.global = newBucket(rl.globalRPM, rl.burstMultiple, now)
}
