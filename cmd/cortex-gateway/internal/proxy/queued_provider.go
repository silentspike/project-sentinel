package proxy

import (
	"context"
	"fmt"

	"github.com/silentspike/project-sentinel/cmd/cortex-gateway/internal/forwardqueue"
)

type queuedProvider struct {
	wrapped Provider
	queue   *forwardqueue.Manager
}

func NewQueuedProvider(wrapped Provider, queue *forwardqueue.Manager) Provider {
	if wrapped == nil || queue == nil {
		return wrapped
	}
	queued := &queuedProvider{
		wrapped: wrapped,
		queue:   queue,
	}
	_, inventoryCapable := wrapped.(ModelInventoryProvider)
	_, readinessCapable := wrapped.(ProviderReadinessChecker)
	switch {
	case inventoryCapable && readinessCapable:
		return &queuedInventoryReadinessProvider{queuedProvider: queued}
	case inventoryCapable:
		return &queuedInventoryProvider{queuedProvider: queued}
	case readinessCapable:
		return &queuedReadinessProvider{queuedProvider: queued}
	default:
		return queued
	}
}

type queuedInventoryProvider struct {
	*queuedProvider
}

type queuedReadinessProvider struct {
	*queuedProvider
}

type queuedInventoryReadinessProvider struct {
	*queuedProvider
}

func (p *queuedProvider) Name() string {
	return p.wrapped.Name()
}

func (p *queuedProvider) Send(ctx context.Context, req *LLMRequest) (*LLMResponse, error) {
	release, err := p.queue.Acquire(ctx)
	if err != nil {
		return nil, fmt.Errorf("forward queue wait: %w", err)
	}
	defer release()
	return p.wrapped.Send(ctx, req)
}

func (p *queuedProvider) HealthCheck(ctx context.Context) error {
	return p.wrapped.HealthCheck(ctx)
}

func (p *queuedInventoryProvider) ModelInventory(ctx context.Context) ([]string, error) {
	inventory := p.wrapped.(ModelInventoryProvider) // constructor preserves this invariant
	return inventory.ModelInventory(ctx)
}

func (p *queuedReadinessProvider) ReadinessCheck(ctx context.Context) error {
	checker := p.wrapped.(ProviderReadinessChecker) // constructor preserves this invariant
	return checker.ReadinessCheck(ctx)
}

func (p *queuedInventoryReadinessProvider) ModelInventory(ctx context.Context) ([]string, error) {
	inventory := p.wrapped.(ModelInventoryProvider) // constructor preserves this invariant
	return inventory.ModelInventory(ctx)
}

func (p *queuedInventoryReadinessProvider) ReadinessCheck(ctx context.Context) error {
	checker := p.wrapped.(ProviderReadinessChecker) // constructor preserves this invariant
	return checker.ReadinessCheck(ctx)
}
