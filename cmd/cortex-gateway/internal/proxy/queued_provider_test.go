package proxy

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/forwardqueue"
)

func TestQueuedProviderDoesNotDispatchExpiredAvailableGrant(t *testing.T) {
	queue := forwardqueue.NewManager(1)
	provider := &pipelineMockProvider{name: "mock"}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	_, err := NewQueuedProvider(provider, queue).Send(ctx, &LLMRequest{})
	if !errors.Is(err, context.Canceled) || provider.calls != 0 {
		t.Fatalf("expired grant reached provider: err=%v calls=%d", err, provider.calls)
	}
	if stats := queue.Stats(); stats.Active != 0 || stats.Depth != 0 {
		t.Fatalf("expired grant leaked queue capacity: %+v", stats)
	}
}

func TestQueuedProviderDeadlineWhileWaitingDoesNotDispatchOrLeakCapacity(t *testing.T) {
	queue := forwardqueue.NewManager(1)
	release, err := queue.Acquire(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	defer release()
	provider := &pipelineMockProvider{name: "mock"}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	_, err = NewQueuedProvider(provider, queue).Send(ctx, &LLMRequest{})
	if !errors.Is(err, context.DeadlineExceeded) || provider.calls != 0 {
		t.Fatalf("expired waiter reached provider: err=%v calls=%d", err, provider.calls)
	}
	if stats := queue.Stats(); stats.Active != 1 || stats.Depth != 0 {
		t.Fatalf("expired waiter changed the existing lease or leaked a waiter: %+v", stats)
	}
}
