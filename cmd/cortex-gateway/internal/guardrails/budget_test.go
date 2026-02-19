package guardrails

import (
	"testing"
	"time"
)

func TestBudgetTracker_AllowWithinLimits(t *testing.T) {
	bt := NewBudgetTracker(1000, 10000)
	if !bt.Allow(500) {
		t.Fatal("should allow 500 tokens within 1000 hourly limit")
	}
}

func TestBudgetTracker_AllowExceedsHourly(t *testing.T) {
	bt := NewBudgetTracker(1000, 10000)
	if bt.Allow(1001) {
		t.Fatal("should deny when estimated tokens exceed hourly limit")
	}
}

func TestBudgetTracker_AllowExceedsDaily(t *testing.T) {
	bt := NewBudgetTracker(0, 1000) // unlimited hourly, limited daily
	if bt.Allow(1001) {
		t.Fatal("should deny when estimated tokens exceed daily limit")
	}
}

func TestBudgetTracker_AllowUnlimited(t *testing.T) {
	bt := NewBudgetTracker(0, 0) // both unlimited
	if !bt.Allow(999_999_999) {
		t.Fatal("should always allow when limits are 0 (unlimited)")
	}
}

func TestBudgetTracker_RecordAccumulates(t *testing.T) {
	bt := NewBudgetTracker(1000, 10000)
	bt.Record(300, 200) // 500 total
	bt.Record(200, 100) // 300 total, cumulative 800

	status := bt.Status()
	if status.HourlyUsed != 800 {
		t.Fatalf("expected hourly used 800, got %d", status.HourlyUsed)
	}
	if status.DailyUsed != 800 {
		t.Fatalf("expected daily used 800, got %d", status.DailyUsed)
	}
}

func TestBudgetTracker_AllowChecksCumulative(t *testing.T) {
	bt := NewBudgetTracker(1000, 10000)
	bt.Record(400, 400) // 800 used

	// 800 + 300 = 1100 > 1000
	if bt.Allow(300) {
		t.Fatal("should deny when cumulative + estimate exceeds hourly limit")
	}

	// 800 + 100 = 900 <= 1000
	if !bt.Allow(100) {
		t.Fatal("should allow when cumulative + estimate is within hourly limit")
	}
}

func TestBudgetTracker_HourlyReset(t *testing.T) {
	bt := NewBudgetTracker(1000, 100000)
	now := time.Now()
	bt.nowFunc = func() time.Time { return now }

	bt.Record(500, 400) // 900 used

	// Advance past the hour boundary
	now = bt.hourlyReset.Add(time.Second)

	status := bt.Status()
	if status.HourlyUsed != 0 {
		t.Fatalf("expected hourly reset to 0, got %d", status.HourlyUsed)
	}
	// Daily should NOT reset
	if status.DailyUsed != 900 {
		t.Fatalf("expected daily used 900 (not reset), got %d", status.DailyUsed)
	}
}

func TestBudgetTracker_DailyReset(t *testing.T) {
	bt := NewBudgetTracker(100000, 1000)
	now := time.Now()
	bt.nowFunc = func() time.Time { return now }

	bt.Record(400, 400) // 800

	// Advance past midnight
	now = bt.dailyReset.Add(time.Second)

	status := bt.Status()
	if status.DailyUsed != 0 {
		t.Fatalf("expected daily reset to 0, got %d", status.DailyUsed)
	}
}

func TestBudgetTracker_StatusReturnsLimits(t *testing.T) {
	bt := NewBudgetTracker(5000, 50000)
	status := bt.Status()

	if status.HourlyLimit != 5000 {
		t.Fatalf("expected hourly limit 5000, got %d", status.HourlyLimit)
	}
	if status.DailyLimit != 50000 {
		t.Fatalf("expected daily limit 50000, got %d", status.DailyLimit)
	}
}

func TestBudgetTracker_BudgetExhaustionFallback(t *testing.T) {
	// AC-1: Budget=100, send 101 tokens → denied
	bt := NewBudgetTracker(100, 100)
	bt.Record(50, 50) // 100 used
	if bt.Allow(1) {
		t.Fatal("AC-1: should deny when budget is exactly exhausted")
	}
}
