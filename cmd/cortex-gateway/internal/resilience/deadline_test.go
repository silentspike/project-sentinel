package resilience

import (
	"testing"
	"time"
)

func TestZenohDeadlineFromEnvDefault(t *testing.T) {
	// No env set → default 100ms
	d := ZenohDeadlineFromEnv()
	if d != 100*time.Millisecond {
		t.Errorf("ZenohDeadlineFromEnv() = %v, want 100ms", d)
	}
}

func TestZenohDeadlineFromEnvValid(t *testing.T) {
	t.Setenv("SENTINEL_CORTEX_ZENOH_DEADLINE_MS", "75")
	d := ZenohDeadlineFromEnv()
	if d != 75*time.Millisecond {
		t.Errorf("ZenohDeadlineFromEnv() = %v, want 75ms", d)
	}
}

func TestZenohDeadlineFromEnvTooLow(t *testing.T) {
	t.Setenv("SENTINEL_CORTEX_ZENOH_DEADLINE_MS", "10")
	d := ZenohDeadlineFromEnv()
	if d != 100*time.Millisecond {
		t.Errorf("ZenohDeadlineFromEnv() = %v, want 100ms (below min)", d)
	}
}

func TestZenohDeadlineFromEnvTooHigh(t *testing.T) {
	t.Setenv("SENTINEL_CORTEX_ZENOH_DEADLINE_MS", "500")
	d := ZenohDeadlineFromEnv()
	if d != 100*time.Millisecond {
		t.Errorf("ZenohDeadlineFromEnv() = %v, want 100ms (above max)", d)
	}
}

func TestZenohDeadlineFromEnvInvalid(t *testing.T) {
	t.Setenv("SENTINEL_CORTEX_ZENOH_DEADLINE_MS", "abc")
	d := ZenohDeadlineFromEnv()
	if d != 100*time.Millisecond {
		t.Errorf("ZenohDeadlineFromEnv() = %v, want 100ms (invalid)", d)
	}
}

func TestInFlightMapTrackAndAccept(t *testing.T) {
	m := NewInFlightMap(100 * time.Millisecond)
	now := time.Now()
	m.now = func() time.Time { return now }

	m.Track("q1", 10)

	// Accept within deadline, valid tick
	if !m.Accept("q1", 10) {
		t.Error("Accept(q1, 10) = false, want true")
	}

	// Second accept should fail (already removed)
	if m.Accept("q1", 10) {
		t.Error("Accept(q1, 10) second call = true, want false")
	}
}

func TestInFlightMapDeadlineExpired(t *testing.T) {
	m := NewInFlightMap(100 * time.Millisecond)
	now := time.Now()
	m.now = func() time.Time { return now }

	m.Track("q1", 10)

	// Advance past deadline
	now = now.Add(150 * time.Millisecond)

	if m.Accept("q1", 10) {
		t.Error("Accept(q1) = true, want false (deadline expired)")
	}
}

func TestInFlightMapStaleDrop(t *testing.T) {
	m := NewInFlightMap(100 * time.Millisecond)
	now := time.Now()
	m.now = func() time.Time { return now }

	m.Track("q1", 50)

	// Response tick < min_tick → stale drop
	if m.Accept("q1", 30) {
		t.Error("Accept(q1, 30) = true, want false (stale: 30 < min_tick 50)")
	}
}

func TestInFlightMapCancel(t *testing.T) {
	m := NewInFlightMap(100 * time.Millisecond)
	now := time.Now()
	m.now = func() time.Time { return now }

	m.Track("q1", 10)
	m.Cancel("q1")

	if m.Accept("q1", 10) {
		t.Error("Accept(q1) = true, want false (cancelled)")
	}
	if m.Len() != 0 {
		t.Errorf("Len() = %d, want 0", m.Len())
	}
}

func TestInFlightMapPrune(t *testing.T) {
	m := NewInFlightMap(50 * time.Millisecond)
	now := time.Now()
	m.now = func() time.Time { return now }

	m.Track("q1", 10)
	m.Track("q2", 20)
	m.Track("q3", 30)

	// Advance past deadline
	now = now.Add(60 * time.Millisecond)

	pruned := m.Prune()
	if pruned != 3 {
		t.Errorf("Prune() = %d, want 3", pruned)
	}
	if m.Len() != 0 {
		t.Errorf("Len() = %d, want 0 after prune", m.Len())
	}
}

func TestInFlightMapPrunePartial(t *testing.T) {
	m := NewInFlightMap(100 * time.Millisecond)
	now := time.Now()
	m.now = func() time.Time { return now }

	m.Track("q1", 10)

	// Advance 50ms — not past 100ms deadline
	now = now.Add(50 * time.Millisecond)
	m.Track("q2", 20) // q2 tracked at now+50ms

	// Advance another 60ms — total 110ms from q1, 60ms from q2
	now = now.Add(60 * time.Millisecond)

	pruned := m.Prune()
	if pruned != 1 {
		t.Errorf("Prune() = %d, want 1 (only q1 expired)", pruned)
	}
	if m.Len() != 1 {
		t.Errorf("Len() = %d, want 1 (q2 still active)", m.Len())
	}
}

func TestInFlightMapLen(t *testing.T) {
	m := NewInFlightMap(100 * time.Millisecond)

	if m.Len() != 0 {
		t.Errorf("Len() = %d, want 0", m.Len())
	}

	m.Track("q1", 10)
	m.Track("q2", 20)

	if m.Len() != 2 {
		t.Errorf("Len() = %d, want 2", m.Len())
	}

	m.Accept("q1", 10)

	if m.Len() != 1 {
		t.Errorf("Len() = %d, want 1", m.Len())
	}
}

func TestInFlightMapUnknownQueryID(t *testing.T) {
	m := NewInFlightMap(100 * time.Millisecond)

	// Accept for unknown query_id
	if m.Accept("nonexistent", 10) {
		t.Error("Accept(nonexistent) = true, want false")
	}
}

func TestInFlightMapCancelUnknown(t *testing.T) {
	m := NewInFlightMap(100 * time.Millisecond)

	// Cancel unknown should not panic
	m.Cancel("nonexistent")
}

func TestInFlightMapAcceptExactDeadline(t *testing.T) {
	m := NewInFlightMap(100 * time.Millisecond)
	now := time.Now()
	m.now = func() time.Time { return now }

	m.Track("q1", 10)

	// Advance exactly to deadline boundary
	now = now.Add(100 * time.Millisecond)

	// At exact boundary, time.After returns false (not strictly after)
	if !m.Accept("q1", 10) {
		t.Error("Accept(q1) at exact deadline = false, want true (not strictly after)")
	}
}

func TestInFlightMapAcceptExactMinTick(t *testing.T) {
	m := NewInFlightMap(100 * time.Millisecond)
	now := time.Now()
	m.now = func() time.Time { return now }

	m.Track("q1", 50)

	// response_tick == min_tick → should accept
	if !m.Accept("q1", 50) {
		t.Error("Accept(q1, 50) = false, want true (tick == min_tick)")
	}
}
