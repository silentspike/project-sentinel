package proxy

import (
	"sync"
	"time"
)

type ResponseLogEntry struct {
	RequestID string    `json:"request_id"`
	Provider  string    `json:"provider"`
	Content   string    `json:"content"`
	LoggedAt  time.Time `json:"logged_at"`
}

// ResponseLogBuffer keeps recent response bodies in memory for control-plane
// inspection without adding disk writes to the hot path.
type ResponseLogBuffer struct {
	mu      sync.Mutex
	limit   int
	entries []ResponseLogEntry
}

func NewResponseLogBuffer(limit int) *ResponseLogBuffer {
	if limit <= 0 {
		limit = 200
	}
	return &ResponseLogBuffer{limit: limit}
}

func (b *ResponseLogBuffer) Add(requestID, provider, content string) {
	if b == nil {
		return
	}

	b.mu.Lock()
	defer b.mu.Unlock()

	b.entries = append(b.entries, ResponseLogEntry{
		RequestID: requestID,
		Provider:  provider,
		Content:   content,
		LoggedAt:  time.Now(),
	})
	if len(b.entries) > b.limit {
		b.entries = append([]ResponseLogEntry(nil), b.entries[len(b.entries)-b.limit:]...)
	}
}

func (b *ResponseLogBuffer) Entries() []ResponseLogEntry {
	if b == nil {
		return nil
	}

	b.mu.Lock()
	defer b.mu.Unlock()

	out := make([]ResponseLogEntry, len(b.entries))
	copy(out, b.entries)
	return out
}

func (b *ResponseLogBuffer) Len() int {
	if b == nil {
		return 0
	}

	b.mu.Lock()
	defer b.mu.Unlock()
	return len(b.entries)
}
