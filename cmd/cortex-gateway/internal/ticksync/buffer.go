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
	RequestID string
	Priority  int         // 0=P0, 1=P1, 2=P2, 3=P3 (lower = higher priority)
	Response  interface{} // *PipelineResponse (untyped to avoid import cycle)
	Writer    http.ResponseWriter
	HeldAt    time.Time
	Done      chan error
}

// Buffer holds responses grouped by tick for synchronized delivery.
// Responses are held until the tick's flush timeout expires, then delivered
// in deterministic order (P1 before P3, then by agent_id).
type Buffer struct {
	mu      sync.Mutex
	pending map[uint64][]*Entry // tick → pending responses
	timeout time.Duration       // flush timeout (default 2s, configurable AC-17)
	logger  *slog.Logger
	enabled bool
	stopCh  chan struct{}
}

type Stats struct {
	Pending int `json:"pending"`
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
		go b.flushLoop(b.stopCh)
	}
	return b
}

// Enabled returns whether tick-sync is active.
func (b *Buffer) Enabled() bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.enabled
}

// SetEnabled toggles tick-sync at runtime.
func (b *Buffer) SetEnabled(v bool) {
	var (
		stopCh chan struct{}
		flush  map[uint64][]*Entry
	)

	b.mu.Lock()
	if b.enabled == v {
		b.mu.Unlock()
		return
	}
	b.enabled = v
	if v {
		b.stopCh = make(chan struct{})
		stopCh = b.stopCh
		b.mu.Unlock()
		go b.flushLoop(stopCh)
		return
	}

	stopCh = b.stopCh
	flush = b.pending
	b.pending = make(map[uint64][]*Entry)
	b.stopCh = make(chan struct{})
	b.mu.Unlock()

	if stopCh != nil {
		close(stopCh)
	}
	b.flushEntries(flush)
}

// SetTimeout updates the maximum hold duration for pending responses.
func (b *Buffer) SetTimeout(timeout time.Duration) {
	if timeout <= 0 {
		return
	}
	b.mu.Lock()
	b.timeout = timeout
	b.mu.Unlock()
}

// Hold adds a response to the tick buffer instead of sending it immediately.
// The response will be flushed when the tick's timeout expires.
func (b *Buffer) Hold(tick uint64, agentID, priority int, requestID string, resp interface{}, w http.ResponseWriter) <-chan error {
	done := make(chan error, 1)

	b.mu.Lock()
	b.pending[tick] = append(b.pending[tick], &Entry{
		AgentID:   agentID,
		RequestID: requestID,
		Priority:  priority,
		Response:  resp,
		Writer:    w,
		HeldAt:    time.Now(),
		Done:      done,
	})
	b.mu.Unlock()

	tickSyncHeldTotal.Inc()
	b.logger.Debug("tick_sync held", "tick", tick, "agent", agentID, "priority", priority)
	return done
}

// flushLoop periodically checks for ticks whose timeout has expired and flushes them.
func (b *Buffer) flushLoop(stopCh <-chan struct{}) {
	ticker := time.NewTicker(500 * time.Millisecond)
	defer ticker.Stop()

	for {
		select {
		case <-stopCh:
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

	b.flushEntries(flushing, toFlush...)
}

func (b *Buffer) flushEntries(groups map[uint64][]*Entry, orderedTicks ...uint64) {
	if len(groups) == 0 {
		return
	}

	if len(orderedTicks) == 0 {
		orderedTicks = make([]uint64, 0, len(groups))
		for tick := range groups {
			orderedTicks = append(orderedTicks, tick)
		}
	}
	sort.Slice(orderedTicks, func(i, j int) bool { return orderedTicks[i] < orderedTicks[j] })

		for _, tick := range orderedTicks {
			entries := groups[tick]
			sort.Slice(entries, func(i, j int) bool {
				if entries[i].Priority != entries[j].Priority {
					return entries[i].Priority < entries[j].Priority
			}
			if entries[i].AgentID != entries[j].AgentID {
				return entries[i].AgentID < entries[j].AgentID
			}
			return entries[i].RequestID < entries[j].RequestID
		})

			for idx, entry := range entries {
				entry.Writer.Header().Set("Content-Type", "application/json")
				if err := json.NewEncoder(entry.Writer).Encode(entry.Response); err != nil {
					entry.Done <- err
					close(entry.Done)
					b.logger.Error("tick_sync flush encode error", "tick", tick, "agent", entry.AgentID, "error", err)
					continue
				}
				entry.Done <- nil
				close(entry.Done)
				tickSyncFlushedTotal.Inc()
				if len(entries) > 1 {
					b.logger.Info("tick_sync flush order",
						"tick", tick,
						"order", idx+1,
						"count", len(entries),
						"request_id", entry.RequestID,
						"agent", entry.AgentID,
						"priority", entry.Priority,
					)
				}
			}

			b.logger.Info("tick_sync flushed", "tick", tick, "count", len(entries))
		}
}

func (b *Buffer) Stats() Stats {
	b.mu.Lock()
	defer b.mu.Unlock()

	pending := 0
	for _, entries := range b.pending {
		pending += len(entries)
	}
	return Stats{Pending: pending}
}

// Stop shuts down the flush goroutine.
func (b *Buffer) Stop() {
	b.SetEnabled(false)
}
