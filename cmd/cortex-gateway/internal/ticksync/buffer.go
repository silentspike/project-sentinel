package ticksync

import (
	"encoding/json"
	"log/slog"
	"net/http"
	"sort"
	"sync"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
)

var (
	tickSyncHeldTotal = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_ticksync_held_total",
		Help: "Responses held for tick synchronization",
	})
	tickSyncFlushedTotal = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_ticksync_flushed_total",
		Help: "Responses flushed after tick synchronization",
	})
)

// Entry is a pending response waiting for tick-boundary flush.
type Entry struct {
	AgentID   int
	Priority  int    // 0=P0, 1=P1, 2=P2, 3=P3 (lower = higher priority)
	Response  interface{} // *PipelineResponse (untyped to avoid import cycle)
	Writer    http.ResponseWriter
	HeldAt    time.Time
}

// Buffer holds responses grouped by tick for synchronized delivery.
// Responses are held until the tick's flush timeout expires, then delivered
// in deterministic order (P1 before P3, then by agent_id).
type Buffer struct {
	mu       sync.Mutex
	pending  map[uint64][]*Entry // tick → pending responses
	timeout  time.Duration       // flush timeout (default 2s, configurable AC-17)
	logger   *slog.Logger
	enabled  bool
	stopCh   chan struct{}
}

// NewBuffer creates a tick-sync buffer with the given flush timeout.
func NewBuffer(timeout time.Duration, enabled bool, logger *slog.Logger) *Buffer {
	if logger == nil {
		logger = slog.Default()
	}
	b := &Buffer{
		pending: make(map[uint64][]*Entry),
		timeout: timeout,
		logger:  logger,
		enabled: enabled,
		stopCh:  make(chan struct{}),
	}
	if enabled {
		go b.flushLoop()
	}
	return b
}

// Enabled returns whether tick-sync is active.
func (b *Buffer) Enabled() bool {
	return b.enabled
}

// Hold adds a response to the tick buffer instead of sending it immediately.
// The response will be flushed when the tick's timeout expires.
func (b *Buffer) Hold(tick uint64, agentID, priority int, resp interface{}, w http.ResponseWriter) {
	b.mu.Lock()
	defer b.mu.Unlock()

	b.pending[tick] = append(b.pending[tick], &Entry{
		AgentID:  agentID,
		Priority: priority,
		Response: resp,
		Writer:   w,
		HeldAt:   time.Now(),
	})
	tickSyncHeldTotal.Inc()
	b.logger.Debug("tick_sync held", "tick", tick, "agent", agentID, "priority", priority)
}

// flushLoop periodically checks for ticks whose timeout has expired and flushes them.
func (b *Buffer) flushLoop() {
	ticker := time.NewTicker(500 * time.Millisecond)
	defer ticker.Stop()

	for {
		select {
		case <-b.stopCh:
			return
		case <-ticker.C:
			b.flushExpired()
		}
	}
}

// flushExpired flushes all tick groups whose oldest entry exceeds the timeout.
func (b *Buffer) flushExpired() {
	b.mu.Lock()
	now := time.Now()
	var toFlush []uint64

	for tick, entries := range b.pending {
		if len(entries) == 0 {
			continue
		}
		// Flush if oldest entry exceeds timeout
		oldest := entries[0].HeldAt
		if now.Sub(oldest) >= b.timeout {
			toFlush = append(toFlush, tick)
		}
	}

	// Extract entries to flush (release lock before writing to ResponseWriters)
	flushing := make(map[uint64][]*Entry, len(toFlush))
	for _, tick := range toFlush {
		flushing[tick] = b.pending[tick]
		delete(b.pending, tick)
	}
	b.mu.Unlock()

	// Sort ticks numerically
	sort.Slice(toFlush, func(i, j int) bool { return toFlush[i] < toFlush[j] })

	for _, tick := range toFlush {
		entries := flushing[tick]
		// Sort: P1 before P3 (lower priority number first), then by agent_id
		sort.Slice(entries, func(i, j int) bool {
			if entries[i].Priority != entries[j].Priority {
				return entries[i].Priority < entries[j].Priority
			}
			return entries[i].AgentID < entries[j].AgentID
		})

		for _, entry := range entries {
			entry.Writer.Header().Set("Content-Type", "application/json")
			if err := json.NewEncoder(entry.Writer).Encode(entry.Response); err != nil {
				b.logger.Error("tick_sync flush encode error", "tick", tick, "agent", entry.AgentID, "error", err)
			}
			tickSyncFlushedTotal.Inc()
		}

		b.logger.Info("tick_sync flushed", "tick", tick, "count", len(entries))
	}
}

// Stop shuts down the flush goroutine.
func (b *Buffer) Stop() {
	close(b.stopCh)
}
