package sequencing

import (
	"testing"
	"time"
)

func TestP1ForwardImmediately(t *testing.T) {
	s := NewSequencer(5*time.Second, true, nil)

	if s.HasActiveP1("room-1") {
		t.Error("should not have active P1 initially")
	}

	s.MarkP1Active("room-1", "AGENT-01")

	if !s.HasActiveP1("room-1") {
		t.Error("should have active P1 after MarkP1Active")
	}
	if s.P1Agent("room-1") != "AGENT-01" {
		t.Errorf("P1Agent = %q, want AGENT-01", s.P1Agent("room-1"))
	}
}

func TestP3WaitsForP1(t *testing.T) {
	s := NewSequencer(5*time.Second, true, nil)
	s.MarkP1Active("room-1", "AGENT-01")

	// Simulate P1 completing after 100ms
	go func() {
		time.Sleep(100 * time.Millisecond)
		s.CompleteP1("room-1", "Hallo, ich bin Thomas und kann helfen.")
	}()

	content, agent, ok := s.WaitForP1("room-1")
	if !ok {
		t.Fatal("WaitForP1 should return ok=true")
	}
	if agent != "AGENT-01" {
		t.Errorf("agent = %q, want AGENT-01", agent)
	}
	if content != "Hallo, ich bin Thomas und kann helfen." {
		t.Errorf("content = %q", content)
	}
}

func TestP3TimeoutRelease(t *testing.T) {
	s := NewSequencer(100*time.Millisecond, true, nil)
	s.MarkP1Active("room-1", "AGENT-01")

	// P1 never completes — P3 should timeout
	start := time.Now()
	_, _, ok := s.WaitForP1("room-1")
	elapsed := time.Since(start)

	if ok {
		t.Error("WaitForP1 should return ok=false on timeout")
	}
	if elapsed < 90*time.Millisecond {
		t.Errorf("should wait at least 90ms, waited %v", elapsed)
	}
}

func TestP3NoActiveP1(t *testing.T) {
	s := NewSequencer(5*time.Second, true, nil)

	// No P1 active — WaitForP1 returns immediately
	_, _, ok := s.WaitForP1("room-1")
	if ok {
		t.Error("should return false when no P1 active")
	}
}

func TestMultipleP3sGetSameContext(t *testing.T) {
	s := NewSequencer(5*time.Second, true, nil)
	s.MarkP1Active("room-1", "AGENT-01")

	results := make(chan string, 3)

	// 3 P3 waiters
	for i := 0; i < 3; i++ {
		go func() {
			content, _, ok := s.WaitForP1("room-1")
			if ok {
				results <- content
			}
		}()
	}

	time.Sleep(50 * time.Millisecond)
	s.CompleteP1("room-1", "P1 says hello")

	for i := 0; i < 3; i++ {
		select {
		case content := <-results:
			if content != "P1 says hello" {
				t.Errorf("P3 #%d got %q", i, content)
			}
		case <-time.After(2 * time.Second):
			t.Fatalf("P3 #%d timed out", i)
		}
	}
}

func TestCleanup(t *testing.T) {
	s := NewSequencer(5*time.Second, true, nil)
	s.MarkP1Active("room-1", "AGENT-01")
	s.CompleteP1("room-1", "done")

	s.Cleanup()
	if s.HasActiveP1("room-1") {
		t.Error("completed room should be cleaned up")
	}
}
