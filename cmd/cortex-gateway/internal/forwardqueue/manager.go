package forwardqueue

import (
	"context"
	"sync"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
)

var (
	queueDepthGauge = promauto.NewGauge(prometheus.GaugeOpts{
		Name: "sentinel_forward_queue_depth",
		Help: "Number of forward requests waiting in the gateway FIFO queue",
	})
	activeCallsGauge = promauto.NewGauge(prometheus.GaugeOpts{
		Name: "sentinel_forward_active_calls",
		Help: "Number of currently active real forward calls",
	})
	queueWaitSeconds = promauto.NewHistogram(prometheus.HistogramOpts{
		Name:    "sentinel_forward_queue_wait_seconds",
		Help:    "Time spent waiting in the gateway forward queue",
		Buckets: []float64{0.001, 0.01, 0.05, 0.1, 0.5, 1, 2, 5, 10, 20},
	})
	queueTimeoutsTotal = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_forward_queue_timeouts_total",
		Help: "Forward queue waits aborted because the waiting context ended",
	})
)

type Stats struct {
	Depth  int
	Active int
}

type Manager struct {
	mu            sync.Mutex
	maxConcurrent int
	active        int
	queue         []*waiter
}

type waiter struct {
	ch         chan struct{}
	granted    bool
	enqueuedAt time.Time
}

func NewManager(maxConcurrent int) *Manager {
	if maxConcurrent <= 0 {
		maxConcurrent = 1
	}
	return &Manager{maxConcurrent: maxConcurrent}
}

func (m *Manager) Acquire(ctx context.Context) (func(), error) {
	m.mu.Lock()
	if m.active < m.maxConcurrent && len(m.queue) == 0 {
		m.active++
		activeCallsGauge.Set(float64(m.active))
		m.mu.Unlock()
		return m.release, nil
	}

	w := &waiter{
		ch:         make(chan struct{}),
		enqueuedAt: time.Now(),
	}
	m.queue = append(m.queue, w)
	queueDepthGauge.Set(float64(len(m.queue)))
	m.mu.Unlock()

	select {
	case <-w.ch:
		queueWaitSeconds.Observe(time.Since(w.enqueuedAt).Seconds())
		return m.release, nil
	case <-ctx.Done():
		m.mu.Lock()
		if w.granted {
			m.mu.Unlock()
			queueWaitSeconds.Observe(time.Since(w.enqueuedAt).Seconds())
			return m.release, nil
		}
		for i := range m.queue {
			if m.queue[i] != w {
				continue
			}
			m.queue = append(m.queue[:i], m.queue[i+1:]...)
			break
		}
		queueDepthGauge.Set(float64(len(m.queue)))
		queueTimeoutsTotal.Inc()
		m.mu.Unlock()
		return nil, ctx.Err()
	}
}

func (m *Manager) Stats() Stats {
	m.mu.Lock()
	defer m.mu.Unlock()
	return Stats{
		Depth:  len(m.queue),
		Active: m.active,
	}
}

func (m *Manager) SetMaxConcurrent(maxConcurrent int) {
	if maxConcurrent <= 0 {
		maxConcurrent = 1
	}

	var ready []*waiter

	m.mu.Lock()
	m.maxConcurrent = maxConcurrent
	for m.active < m.maxConcurrent && len(m.queue) > 0 {
		next := m.queue[0]
		m.queue = m.queue[1:]
		next.granted = true
		m.active++
		ready = append(ready, next)
	}
	queueDepthGauge.Set(float64(len(m.queue)))
	activeCallsGauge.Set(float64(m.active))
	m.mu.Unlock()

	for _, waiter := range ready {
		close(waiter.ch)
	}
}

func (m *Manager) release() {
	m.mu.Lock()
	defer m.mu.Unlock()

	if len(m.queue) > 0 {
		next := m.queue[0]
		m.queue = m.queue[1:]
		next.granted = true
		queueDepthGauge.Set(float64(len(m.queue)))
		close(next.ch)
		return
	}

	if m.active > 0 {
		m.active--
	}
	activeCallsGauge.Set(float64(m.active))
}
