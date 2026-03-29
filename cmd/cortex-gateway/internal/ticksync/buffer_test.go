package ticksync

import (
	"net/http/httptest"
	"testing"
	"time"
)

func TestHoldAndFlush(t *testing.T) {
	b := NewBuffer(100*time.Millisecond, true, nil)
	defer b.Stop()

	w := httptest.NewRecorder()
	resp := map[string]string{"content": "hello"}

	b.Hold(100, 1, 1, "req-1", resp, w)

	// Wait for flush
	time.Sleep(700 * time.Millisecond)

	if w.Body.Len() == 0 {
		t.Error("response should have been flushed")
	}
}

func TestPriorityOrdering(t *testing.T) {
	b := NewBuffer(100*time.Millisecond, true, nil)
	defer b.Stop()

	w1 := httptest.NewRecorder()
	w2 := httptest.NewRecorder()
	w3 := httptest.NewRecorder()

	// Add P3 first, then P1, then P2
	b.Hold(100, 5, 3, "req-3", map[string]string{"agent": "P3"}, w1)
	b.Hold(100, 1, 1, "req-1", map[string]string{"agent": "P1"}, w2)
	b.Hold(100, 3, 2, "req-2", map[string]string{"agent": "P2"}, w3)

	time.Sleep(700 * time.Millisecond)

	// All should be flushed
	if w1.Body.Len() == 0 || w2.Body.Len() == 0 || w3.Body.Len() == 0 {
		t.Error("all responses should be flushed")
	}
}

func TestDisabledBuffer(t *testing.T) {
	b := NewBuffer(100*time.Millisecond, false, nil)
	if b.Enabled() {
		t.Error("should be disabled")
	}
}

func TestStatsPending(t *testing.T) {
	b := NewBuffer(time.Second, false, nil)

	b.Hold(100, 1, 1, "req-1", map[string]string{"content": "one"}, httptest.NewRecorder())
	b.Hold(100, 2, 3, "req-2", map[string]string{"content": "two"}, httptest.NewRecorder())

	if got := b.Stats().Pending; got != 2 {
		t.Fatalf("pending = %d, want 2", got)
	}
}

func TestSetEnabledFlushesPendingAndSupportsRuntimeToggle(t *testing.T) {
	b := NewBuffer(time.Second, false, nil)
	defer b.Stop()

	w := httptest.NewRecorder()
	b.Hold(100, 1, 1, "req-1", map[string]string{"content": "toggle"}, w)

	if got := b.Stats().Pending; got != 1 {
		t.Fatalf("pending = %d, want 1", got)
	}

	b.SetEnabled(true)
	if !b.Enabled() {
		t.Fatal("buffer should be enabled after SetEnabled(true)")
	}

	b.SetTimeout(10 * time.Millisecond)
	time.Sleep(700 * time.Millisecond)
	if w.Body.Len() == 0 {
		t.Fatal("response should have been flushed after runtime enable")
	}

	b.SetEnabled(false)
	if b.Enabled() {
		t.Fatal("buffer should be disabled after SetEnabled(false)")
	}
}
