package judge

import "sync"

// SwapDecision represents whether a model swap should happen.
type SwapDecision struct {
	ShouldSwap bool
	AgentName  string
	Reason     string
	FromModel  string
	ToModel    string
}

// SwapTrigger tracks quality history and triggers model swaps.
type SwapTrigger struct {
	mu             sync.RWMutex
	history        map[string][]int // agent -> recent quality scores
	threshold      int              // consecutive bad scores needed
	badScoreCutoff int              // scores <= this are "bad"
	defaultModel   string           // fallback model
	upgradeModel   string           // model to upgrade to
}

func NewSwapTrigger(threshold int, badScoreCutoff int) *SwapTrigger {
	return &SwapTrigger{
		history:        make(map[string][]int),
		threshold:      threshold,
		badScoreCutoff: badScoreCutoff,
		defaultModel:   "bitnet-7b",
		upgradeModel:   "claude-haiku",
	}
}

// RecordScore records a quality score for an agent.
func (s *SwapTrigger) RecordScore(agentName string, score int) {
	s.mu.Lock()
	defer s.mu.Unlock()

	scores, exists := s.history[agentName]
	if !exists {
		scores = []int{}
	}

	scores = append(scores, score)

	// Keep only last threshold entries
	if len(scores) > s.threshold {
		scores = scores[len(scores)-s.threshold:]
	}

	s.history[agentName] = scores
}

// ShouldSwap checks if an agent needs a model swap based on recent history.
func (s *SwapTrigger) ShouldSwap(agentName string) SwapDecision {
	s.mu.RLock()
	defer s.mu.RUnlock()

	scores, exists := s.history[agentName]
	if !exists || len(scores) < s.threshold {
		return SwapDecision{
			ShouldSwap: false,
			AgentName:  agentName,
			Reason:     "insufficient history",
			FromModel:  s.defaultModel,
			ToModel:    s.upgradeModel,
		}
	}

	// Check if all recent scores are bad
	allBad := true
	for i := len(scores) - s.threshold; i < len(scores); i++ {
		if scores[i] > s.badScoreCutoff {
			allBad = false
			break
		}
	}

	if allBad {
		return SwapDecision{
			ShouldSwap: true,
			AgentName:  agentName,
			Reason:     "consecutive low quality scores",
			FromModel:  s.defaultModel,
			ToModel:    s.upgradeModel,
		}
	}

	return SwapDecision{
		ShouldSwap: false,
		AgentName:  agentName,
		Reason:     "quality within acceptable range",
		FromModel:  s.defaultModel,
		ToModel:    s.upgradeModel,
	}
}

// Reset clears history for an agent (e.g., after swap).
func (s *SwapTrigger) Reset(agentName string) {
	s.mu.Lock()
	defer s.mu.Unlock()

	delete(s.history, agentName)
}
