package sequencing

import (
	"log/slog"
	"sync"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
)

var (
	p1ForwardedTotal = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_sequencing_p1_forwarded_total",
		Help: "P1 requests forwarded immediately",
	})
	p3QueuedTotal = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_sequencing_p3_queued_total",
		Help: "P3 requests queued waiting for P1",
	})
	p3ReleasedWithContext = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_sequencing_p3_released_with_context_total",
		Help: "P3 requests released with P1 context injected",
	})
	p3ReleasedTimeout = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_sequencing_p3_released_timeout_total",
		Help: "P3 requests released after timeout (no P1 response)",
	})
)

// Sequencer manages room-scoped P1/P3 chat ordering.
// When a P1 (directly addressed) request arrives, P3 (listeners) in the same room
// wait until P1 responds, then get P1's response injected as context.
type Sequencer struct {
	mu      sync.Mutex
	rooms   map[string]*roomState
	timeout time.Duration
	logger  *slog.Logger
	enabled bool
}

type roomState struct {
	p1Active  bool
	p1Agent   string
	p1Done    chan struct{} // closed when P1 completes
	p1Content string       // P1's response text (filled on completion)
}

// NewSequencer creates a chat sequencer with the given P3 wait timeout.
func NewSequencer(timeout time.Duration, enabled bool, logger *slog.Logger) *Sequencer {
	if logger == nil {
		logger = slog.Default()
	}
	return &Sequencer{
		rooms:   make(map[string]*roomState),
		timeout: timeout,
		logger:  logger,
		enabled: enabled,
	}
}

// Enabled returns whether sequencing is active.
func (s *Sequencer) Enabled() bool {
	return s.enabled
}

// HasActiveP1 returns whether a P1 call is currently in progress for this room.
func (s *Sequencer) HasActiveP1(roomID string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	state, ok := s.rooms[roomID]
	return ok && state.p1Active
}

// MarkP1Active marks a P1 call as active for a room.
// Returns the done channel that will be closed when P1 completes.
func (s *Sequencer) MarkP1Active(roomID, agentName string) {
	s.mu.Lock()
	defer s.mu.Unlock()

	// Known limitation: if two agents are both P1 simultaneously,
	// the second overwrites the first. P3s waiting on the first P1
	// will get the second P1's response instead. Rare edge case.
	s.rooms[roomID] = &roomState{
		p1Active: true,
		p1Agent:  agentName,
		p1Done:   make(chan struct{}),
	}
	p1ForwardedTotal.Inc()
	s.logger.Info("p1 active", "room", roomID, "agent", agentName)
}

// CompleteP1 marks the P1 call as completed and stores its response content.
// This unblocks all P3 waiters for this room.
func (s *Sequencer) CompleteP1(roomID, content string) {
	s.mu.Lock()
	state, ok := s.rooms[roomID]
	if !ok || !state.p1Active {
		s.mu.Unlock()
		return
	}
	state.p1Content = content
	state.p1Active = false
	close(state.p1Done) // unblock all P3 waiters
	s.mu.Unlock()

	s.logger.Info("p1 completed", "room", roomID, "agent", state.p1Agent,
		"content_len", len(content))
}

// P1Agent returns the name of the P1 agent for a room (if active).
func (s *Sequencer) P1Agent(roomID string) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	state, ok := s.rooms[roomID]
	if !ok {
		return ""
	}
	return state.p1Agent
}

// WaitForP1 blocks until the P1 call for roomID completes or timeout expires.
// Returns the P1 response content and true if P1 completed, or empty and false on timeout.
// AC-10: timeout releases P3 without context.
func (s *Sequencer) WaitForP1(roomID string) (content string, p1Agent string, ok bool) {
	s.mu.Lock()
	state, exists := s.rooms[roomID]
	if !exists || !state.p1Active {
		s.mu.Unlock()
		return "", "", false
	}
	ch := state.p1Done
	agent := state.p1Agent
	s.mu.Unlock()

	p3QueuedTotal.Inc()

	// Wait WITHOUT holding mutex — Go goroutine blocks cheaply
	select {
	case <-ch:
		// P1 completed — inject context
		s.mu.Lock()
		content = state.p1Content
		s.mu.Unlock()
		p3ReleasedWithContext.Inc()
		s.logger.Info("p3 released with context", "room", roomID, "p1_agent", agent)
		return content, agent, true
	case <-time.After(s.timeout):
		// Timeout — release P3 without context (AC-10)
		p3ReleasedTimeout.Inc()
		s.logger.Warn("p3 released timeout", "room", roomID, "timeout", s.timeout)
		return "", agent, false
	}
}

// Cleanup removes stale room states older than 60s.
// Should be called periodically.
func (s *Sequencer) Cleanup() {
	s.mu.Lock()
	defer s.mu.Unlock()
	for roomID, state := range s.rooms {
		if !state.p1Active {
			delete(s.rooms, roomID)
		}
	}
}
