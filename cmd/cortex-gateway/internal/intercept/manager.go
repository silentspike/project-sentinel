package intercept

import (
	"context"
	"sort"
	"sync"
)

type pendingEntry struct {
	request    PendingRequest
	decisionCh chan RequestDecision
}

// Manager keeps pending inbound requests for manual interception.
type Manager struct {
	mu      sync.Mutex
	pending map[string]*pendingEntry
}

func NewManager() *Manager {
	return &Manager{
		pending: make(map[string]*pendingEntry),
	}
}

func (m *Manager) AwaitRequestDecision(ctx context.Context, request PendingRequest) (RequestDecision, bool) {
	entry := &pendingEntry{
		request:    request,
		decisionCh: make(chan RequestDecision, 1),
	}

	m.mu.Lock()
	m.pending[request.ID] = entry
	m.mu.Unlock()

	defer func() {
		m.mu.Lock()
		delete(m.pending, request.ID)
		m.mu.Unlock()
	}()

	select {
	case decision := <-entry.decisionCh:
		return decision, true
	case <-ctx.Done():
		return Forward("manual intercept timeout"), false
	}
}

func (m *Manager) ResolveRequest(id string, decision RequestDecision) bool {
	m.mu.Lock()
	entry, ok := m.pending[id]
	m.mu.Unlock()
	if !ok {
		return false
	}

	select {
	case entry.decisionCh <- decision:
		return true
	default:
		return false
	}
}

func (m *Manager) Pending() []PendingRequest {
	m.mu.Lock()
	defer m.mu.Unlock()

	items := make([]PendingRequest, 0, len(m.pending))
	for _, entry := range m.pending {
		items = append(items, entry.request)
	}
	sort.Slice(items, func(i, j int) bool {
		return items[i].CreatedAt.Before(items[j].CreatedAt)
	})
	return items
}
