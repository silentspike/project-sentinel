package forwardqueue

import (
	"context"
	"testing"
	"time"
)

func TestManagerLimitsConcurrencyAndPreservesFIFO(t *testing.T) {
	m := NewManager(3)

	release1, err := m.Acquire(context.Background())
	if err != nil {
		t.Fatalf("acquire 1: %v", err)
	}
	release2, err := m.Acquire(context.Background())
	if err != nil {
		t.Fatalf("acquire 2: %v", err)
	}
	release3, err := m.Acquire(context.Background())
	if err != nil {
		t.Fatalf("acquire 3: %v", err)
	}

	order := make(chan int, 2)

	go func() {
		release, acquireErr := m.Acquire(context.Background())
		if acquireErr != nil {
			t.Errorf("acquire 4: %v", acquireErr)
			return
		}
		order <- 4
		release()
	}()

	deadline := time.After(100 * time.Millisecond)
	for {
		stats := m.Stats()
		if stats.Depth == 1 && stats.Active == 3 {
			break
		}
		select {
		case <-deadline:
			t.Fatalf("expected depth=1 active=3 before second waiter, got %+v", stats)
		default:
			time.Sleep(5 * time.Millisecond)
		}
	}

	go func() {
		release, acquireErr := m.Acquire(context.Background())
		if acquireErr != nil {
			t.Errorf("acquire 5: %v", acquireErr)
			return
		}
		order <- 5
		release()
	}()

	deadline = time.After(100 * time.Millisecond)
	for {
		stats := m.Stats()
		if stats.Depth == 2 && stats.Active == 3 {
			break
		}
		select {
		case <-deadline:
			t.Fatalf("expected depth=2 active=3, got %+v", stats)
		default:
			time.Sleep(5 * time.Millisecond)
		}
	}

	release1()
	if got := <-order; got != 4 {
		t.Fatalf("expected waiter 4 to acquire first, got %d", got)
	}

	release2()
	if got := <-order; got != 5 {
		t.Fatalf("expected waiter 5 to acquire second, got %d", got)
	}

	release3()
}

func TestManagerCancelRemovesQueuedWaiter(t *testing.T) {
	m := NewManager(1)

	release, err := m.Acquire(context.Background())
	if err != nil {
		t.Fatalf("initial acquire: %v", err)
	}
	defer release()

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		_, acquireErr := m.Acquire(ctx)
		done <- acquireErr
	}()

	deadline := time.After(100 * time.Millisecond)
	for {
		if stats := m.Stats(); stats.Depth == 1 {
			break
		}
		select {
		case <-deadline:
			t.Fatalf("expected queued waiter, got %+v", m.Stats())
		default:
			time.Sleep(5 * time.Millisecond)
		}
	}

	cancel()

	select {
	case err := <-done:
		if err == nil {
			t.Fatal("expected acquire to fail after cancel")
		}
	case <-time.After(200 * time.Millisecond):
		t.Fatal("timed out waiting for cancelled acquire")
	}

	if stats := m.Stats(); stats.Depth != 0 {
		t.Fatalf("expected queue depth 0 after cancellation, got %+v", stats)
	}
}

func TestManagerSetMaxConcurrentReleasesWaiters(t *testing.T) {
	m := NewManager(1)

	release1, err := m.Acquire(context.Background())
	if err != nil {
		t.Fatalf("initial acquire: %v", err)
	}
	defer release1()

	done := make(chan struct{})
	go func() {
		release2, acquireErr := m.Acquire(context.Background())
		if acquireErr != nil {
			t.Errorf("second acquire: %v", acquireErr)
			return
		}
		defer release2()
		close(done)
	}()

	deadline := time.After(100 * time.Millisecond)
	for {
		if stats := m.Stats(); stats.Depth == 1 && stats.Active == 1 {
			break
		}
		select {
		case <-deadline:
			t.Fatalf("expected queued waiter, got %+v", m.Stats())
		default:
			time.Sleep(5 * time.Millisecond)
		}
	}

	m.SetMaxConcurrent(2)

	select {
	case <-done:
	case <-time.After(200 * time.Millisecond):
		t.Fatal("timed out waiting for waiter after raising concurrency")
	}
}
