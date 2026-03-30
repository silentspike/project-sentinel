package proxy

import (
	"context"
	"fmt"
	"net/http"

	"github.com/obtFusi/project-sentinel/cmd/cortex-gateway/internal/forwardqueue"
)

type queuedProvider struct {
	wrapped Provider
	queue   *forwardqueue.Manager
}

func NewQueuedProvider(wrapped Provider, queue *forwardqueue.Manager) Provider {
	if wrapped == nil || queue == nil {
		return wrapped
	}
	return &queuedProvider{
		wrapped: wrapped,
		queue:   queue,
	}
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

func (p *queuedProvider) StreamHTTP(ctx context.Context, req *LLMRequest, w http.ResponseWriter) error {
	streamer, ok := p.wrapped.(StreamingProvider)
	if !ok {
		return fmt.Errorf("wrapped provider %q does not support streaming", p.wrapped.Name())
	}
	release, err := p.queue.Acquire(ctx)
	if err != nil {
		return fmt.Errorf("forward queue wait: %w", err)
	}
	defer release()
	return streamer.StreamHTTP(ctx, req, w)
}

func (p *queuedProvider) HealthCheck(ctx context.Context) error {
	return p.wrapped.HealthCheck(ctx)
}
