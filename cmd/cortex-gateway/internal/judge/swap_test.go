package judge

import (
	"sync"
	"testing"
)

func TestSwapTrigger_NoSwapGoodScores(t *testing.T) {
	trigger := NewSwapTrigger(5, 2)

	// Record good scores
	for i := 0; i < 5; i++ {
		trigger.RecordScore("agent-1", 4)
	}

	decision := trigger.ShouldSwap("agent-1")

	if decision.ShouldSwap {
		t.Errorf("expected no swap for good scores, but got swap recommendation")
	}
	if decision.Reason != "quality within acceptable range" {
		t.Errorf("expected reason 'quality within acceptable range', got %s", decision.Reason)
	}
}

func TestSwapTrigger_SwapAfterBadScores(t *testing.T) {
	trigger := NewSwapTrigger(5, 2)

	// Record consecutive bad scores
	for i := 0; i < 5; i++ {
		trigger.RecordScore("agent-2", 1)
	}

	decision := trigger.ShouldSwap("agent-2")

	if !decision.ShouldSwap {
		t.Errorf("expected swap recommendation after consecutive bad scores")
	}
	if decision.Reason != "consecutive low quality scores" {
		t.Errorf("expected reason 'consecutive low quality scores', got %s", decision.Reason)
	}
	if decision.FromModel != "bitnet-7b" {
		t.Errorf("expected FromModel 'bitnet-7b', got %s", decision.FromModel)
	}
	if decision.ToModel != "claude-haiku" {
		t.Errorf("expected ToModel 'claude-haiku', got %s", decision.ToModel)
	}
}

func TestSwapTrigger_ResetClearsHistory(t *testing.T) {
	trigger := NewSwapTrigger(5, 2)

	// Record bad scores
	for i := 0; i < 5; i++ {
		trigger.RecordScore("agent-3", 1)
	}

	// Verify swap would be recommended
	decision := trigger.ShouldSwap("agent-3")
	if !decision.ShouldSwap {
		t.Errorf("expected swap before reset")
	}

	// Reset and check again
	trigger.Reset("agent-3")
	decision = trigger.ShouldSwap("agent-3")

	if decision.ShouldSwap {
		t.Errorf("expected no swap after reset")
	}
	if decision.Reason != "insufficient history" {
		t.Errorf("expected reason 'insufficient history' after reset, got %s", decision.Reason)
	}
}

func TestSwapTrigger_InsufficientHistory(t *testing.T) {
	trigger := NewSwapTrigger(5, 2)

	// Record only 3 scores (less than threshold of 5)
	for i := 0; i < 3; i++ {
		trigger.RecordScore("agent-4", 1)
	}

	decision := trigger.ShouldSwap("agent-4")

	if decision.ShouldSwap {
		t.Errorf("expected no swap with insufficient history")
	}
	if decision.Reason != "insufficient history" {
		t.Errorf("expected reason 'insufficient history', got %s", decision.Reason)
	}
}

func TestSwapTrigger_MixedScores(t *testing.T) {
	trigger := NewSwapTrigger(5, 2)

	// Record mixed scores (some bad, some good)
	trigger.RecordScore("agent-5", 1)
	trigger.RecordScore("agent-5", 1)
	trigger.RecordScore("agent-5", 4)
	trigger.RecordScore("agent-5", 1)
	trigger.RecordScore("agent-5", 1)

	decision := trigger.ShouldSwap("agent-5")

	// Should not swap because not ALL recent scores are bad
	if decision.ShouldSwap {
		t.Errorf("expected no swap for mixed scores")
	}
}

func TestSwapTrigger_ThreadSafety(t *testing.T) {
	trigger := NewSwapTrigger(5, 2)

	var wg sync.WaitGroup
	agents := []string{"agent-a", "agent-b", "agent-c"}

	// Concurrent writes
	for _, agent := range agents {
		for i := 0; i < 10; i++ {
			wg.Add(1)
			go func(a string, score int) {
				defer wg.Done()
				trigger.RecordScore(a, score)
			}(agent, i%5+1)
		}
	}

	// Concurrent reads
	for _, agent := range agents {
		for i := 0; i < 10; i++ {
			wg.Add(1)
			go func(a string) {
				defer wg.Done()
				trigger.ShouldSwap(a)
			}(agent)
		}
	}

	wg.Wait()

	// Verify no data races occurred (test will fail if data race detected with -race flag)
	// Just check that we can still read data
	for _, agent := range agents {
		_ = trigger.ShouldSwap(agent)
	}
}
