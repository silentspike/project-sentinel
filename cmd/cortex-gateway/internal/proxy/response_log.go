package proxy

import (
	"sync"
	"time"
)

type ResponseLogEntry struct {
	RequestID      string       `json:"request_id"`
	RequestClass   RequestClass `json:"request_class,omitempty"`
	CallerRole     CallerRole   `json:"caller_role,omitempty"`
	Provider       string       `json:"provider"`
	Model          string       `json:"model,omitempty"` // compatibility alias for effective_model
	EffectiveModel string       `json:"effective_model,omitempty"`
	ModelTier      string       `json:"model_tier,omitempty"`
	HierarchyTier  int          `json:"hierarchy_tier,omitempty"`
	CatalogDigest  string       `json:"catalog_digest,omitempty"`
	CostSource     string       `json:"cost_source,omitempty"`
	PolicySource   string       `json:"policy_source,omitempty"`
	AgentID        string       `json:"agent_id,omitempty"`
	AgentName      string       `json:"agent_name,omitempty"`
	Content        string       `json:"content"`
	// #429: pipeline decision + matched rule + fourth-wall verdict for the Request Inspector.
	Decision   string    `json:"decision,omitempty"`
	Rule       string    `json:"rule,omitempty"`
	FourthWall string    `json:"fourth_wall,omitempty"`
	LoggedAt   time.Time `json:"logged_at"`
}

// ResponseLogBuffer keeps recent response bodies in memory for control-plane
// inspection without adding disk writes to the hot path.
type ResponseLogBuffer struct {
	mu      sync.Mutex
	limit   int
	entries []ResponseLogEntry
	next    int
}

func NewResponseLogBuffer(limit int) *ResponseLogBuffer {
	if limit <= 0 {
		limit = 200
	}
	return &ResponseLogBuffer{limit: limit, entries: make([]ResponseLogEntry, 0, limit)}
}

func (b *ResponseLogBuffer) Add(entry ResponseLogEntry) {
	if b == nil {
		return
	}

	b.mu.Lock()
	defer b.mu.Unlock()

	if entry.LoggedAt.IsZero() {
		entry.LoggedAt = time.Now()
	}

	if len(b.entries) < b.limit {
		b.entries = append(b.entries, entry)
		return
	}

	b.entries[b.next] = entry
	b.next = (b.next + 1) % b.limit
}

func (b *ResponseLogBuffer) Entries() []ResponseLogEntry {
	if b == nil {
		return nil
	}

	b.mu.Lock()
	defer b.mu.Unlock()

	out := b.entriesChronologicalLocked()
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

func (b *ResponseLogBuffer) LastByClass(class RequestClass) (ResponseLogEntry, bool) {
	if b == nil {
		return ResponseLogEntry{}, false
	}

	b.mu.Lock()
	defer b.mu.Unlock()

	entries := b.entriesChronologicalLocked()
	for i := len(entries) - 1; i >= 0; i-- {
		if entries[i].RequestClass == class {
			return entries[i], true
		}
	}
	return ResponseLogEntry{}, false
}

func (b *ResponseLogBuffer) entriesChronologicalLocked() []ResponseLogEntry {
	if len(b.entries) == 0 {
		return nil
	}
	if len(b.entries) < b.limit || b.next == 0 {
		out := make([]ResponseLogEntry, len(b.entries))
		copy(out, b.entries)
		return out
	}

	out := make([]ResponseLogEntry, 0, len(b.entries))
	out = append(out, b.entries[b.next:]...)
	out = append(out, b.entries[:b.next]...)
	return out
}
