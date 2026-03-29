package intercept

import (
	"context"
	"sort"
	"sync"
	"time"
)

type ResponseAction string

const (
	ResponseForward ResponseAction = "forward"
	ResponseModify  ResponseAction = "modify"
	ResponseDrop    ResponseAction = "drop"
	ResponseReplace ResponseAction = "replace"
)

type ResponseDecision struct {
	Action  ResponseAction
	Reason  string
	Content string
}

type PendingResponse struct {
	ID        string    `json:"id"`
	RoomID    string    `json:"room_id"`
	AgentName string    `json:"agent_name"`
	Provider  string    `json:"provider"`
	Content   string    `json:"content"`
	CreatedAt time.Time `json:"created_at"`
}

type pendingResponseEntry struct {
	response   PendingResponse
	decisionCh chan ResponseDecision
}

type ResponseManager struct {
	mu      sync.Mutex
	pending map[string]*pendingResponseEntry
}

func NewResponseManager() *ResponseManager {
	return &ResponseManager{
		pending: make(map[string]*pendingResponseEntry),
	}
}

func (m *ResponseManager) AwaitDecision(ctx context.Context, response PendingResponse) (ResponseDecision, bool) {
	entry := &pendingResponseEntry{
		response:   response,
		decisionCh: make(chan ResponseDecision, 1),
	}

	m.mu.Lock()
	m.pending[response.ID] = entry
	m.mu.Unlock()

	defer func() {
		m.mu.Lock()
		delete(m.pending, response.ID)
		m.mu.Unlock()
	}()

	select {
	case decision := <-entry.decisionCh:
		return decision, true
	case <-ctx.Done():
		return ResponseDecision{Action: ResponseForward, Reason: "manual response intercept timeout"}, false
	}
}

func (m *ResponseManager) Resolve(id string, decision ResponseDecision) bool {
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

func (m *ResponseManager) Pending() []PendingResponse {
	m.mu.Lock()
	defer m.mu.Unlock()

	items := make([]PendingResponse, 0, len(m.pending))
	for _, entry := range m.pending {
		items = append(items, entry.response)
	}
	sort.Slice(items, func(i, j int) bool {
		return items[i].CreatedAt.Before(items[j].CreatedAt)
	})
	return items
}
