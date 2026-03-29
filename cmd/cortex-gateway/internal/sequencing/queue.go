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
	active []*p1State
}

type p1State struct {
	requestID string
	agentName string
	p1Done    chan struct{} // closed when P1 completes
	p1Content string        // P1's response text (filled on completion)
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
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.enabled
}

// SetEnabled toggles sequencing at runtime.
func (s *Sequencer) SetEnabled(v bool) {
	s.mu.Lock()
	s.enabled = v
	s.mu.Unlock()
}

// SetTimeout updates the maximum wait time for pending P3 requests.
func (s *Sequencer) SetTimeout(timeout time.Duration) {
	if timeout <= 0 {
		return
	}
	s.mu.Lock()
	s.timeout = timeout
	s.mu.Unlock()
}

// HasActiveP1 returns whether a P1 call is currently in progress for this room.
func (s *Sequencer) HasActiveP1(roomID string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	state, ok := s.rooms[roomID]
	return ok && len(state.active) > 0
}

// MarkP1Active marks a P1 call as active for a room.
func (s *Sequencer) MarkP1Active(roomID, requestID, agentName string) {
	s.mu.Lock()
	defer s.mu.Unlock()

	state, ok := s.rooms[roomID]
	if !ok {
		state = &roomState{}
		s.rooms[roomID] = state
	}
	state.active = append(state.active, &p1State{
		requestID: requestID,
		agentName: agentName,
		p1Done:    make(chan struct{}),
	})
	p1ForwardedTotal.Inc()
	s.logger.Info("p1 active", "room", roomID, "agent", agentName, "request_id", requestID, "active_p1", len(state.active))
}

// CompleteP1 marks the P1 call as completed and stores its response content.
// This unblocks all P3 waiters for this room.
func (s *Sequencer) CompleteP1(roomID, requestID, content string) {
	s.mu.Lock()
	state, ok := s.rooms[roomID]
	if !ok || len(state.active) == 0 {
		s.mu.Unlock()
		return
	}

	idx := -1
	var current *p1State
	for i, candidate := range state.active {
		if candidate.requestID == requestID {
			idx = i
			current = candidate
			break
		}
	}
	if current == nil {
		s.mu.Unlock()
		return
	}

	current.p1Content = content
	close(current.p1Done) // unblock all P3 waiters for this P1
	state.active = append(state.active[:idx], state.active[idx+1:]...)
	if len(state.active) == 0 {
		delete(s.rooms, roomID)
	}
	s.mu.Unlock()

	s.logger.Info("p1 completed", "room", roomID, "agent", current.agentName,
		"request_id", requestID, "content_len", len(content))
}

// P1Agent returns the oldest active P1 agent for a room.
func (s *Sequencer) P1Agent(roomID string) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	state, ok := s.rooms[roomID]
	if !ok || len(state.active) == 0 {
		return ""
	}
	return state.active[0].agentName
}

// WaitForP1 blocks until the P1 call for roomID completes or timeout expires.
// Returns the P1 response content and true if P1 completed, or empty and false on timeout.
// AC-10: timeout releases P3 without context.
func (s *Sequencer) WaitForP1(roomID string) (content string, p1Agent string, ok bool) {
	s.mu.Lock()
	state, exists := s.rooms[roomID]
	if !exists || len(state.active) == 0 {
		s.mu.Unlock()
		return "", "", false
	}
	active := state.active[0]
	ch := active.p1Done
	agent := active.agentName
	s.mu.Unlock()

	p3QueuedTotal.Inc()

	// Wait WITHOUT holding mutex — Go goroutine blocks cheaply
	select {
	case <-ch:
		// P1 completed — inject context
		content = active.p1Content
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
		if len(state.active) == 0 {
			delete(s.rooms, roomID)
		}
	}
}
