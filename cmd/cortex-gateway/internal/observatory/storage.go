package observatory

import (
	"sync"
	"time"
)

// MetricsSnapshot holds a complete set of MARBLE metric values for one observation.
type MetricsSnapshot struct {
	InfoPropagation        float64
	GroupPolarization      float64
	CommunicationScore     float64
	PersonalityConsistency float64
	ResponseCreativity     float64
	EmotionalRange         float64
}

// ObservationRecord represents a single observation data point.
type ObservationRecord struct {
	Timestamp time.Time
	Shift     int    // 1, 2, 3
	Model     string // "claude-sonnet", "llama-3.1-70b", "qwen2.5-72b"
	Agent     string // Agent name
	Scenario  string // "daily_routine", etc.
	Metrics   MetricsSnapshot
}

// QueryFilter defines criteria for filtering observation records.
type QueryFilter struct {
	Shift    *int       // nil = no filter
	Model    *string    // nil = no filter
	Agent    *string    // nil = no filter
	Scenario *string    // nil = no filter
	From     *time.Time // nil = no lower bound
	To       *time.Time // nil = no upper bound
}

// ObservationStore provides thread-safe in-memory storage for observation records.
type ObservationStore struct {
	mu      sync.RWMutex
	records []ObservationRecord
}

// NewObservationStore creates an empty ObservationStore.
func NewObservationStore() *ObservationStore {
	return &ObservationStore{
		records: make([]ObservationRecord, 0),
	}
}

// Add appends an observation record to the store.
func (s *ObservationStore) Add(record ObservationRecord) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.records = append(s.records, record)
}

// Query returns all records matching the given filter criteria.
func (s *ObservationStore) Query(filter QueryFilter) []ObservationRecord {
	s.mu.RLock()
	defer s.mu.RUnlock()

	var results []ObservationRecord
	for _, r := range s.records {
		if matchesFilter(r, filter) {
			results = append(results, r)
		}
	}
	return results
}

// AllRecords returns a copy of all stored observation records.
func (s *ObservationStore) AllRecords() []ObservationRecord {
	s.mu.RLock()
	defer s.mu.RUnlock()

	result := make([]ObservationRecord, len(s.records))
	copy(result, s.records)
	return result
}

// Len returns the number of stored records.
func (s *ObservationStore) Len() int {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return len(s.records)
}

// matchesFilter checks if a record satisfies all filter criteria.
func matchesFilter(r ObservationRecord, f QueryFilter) bool {
	if f.Shift != nil && r.Shift != *f.Shift {
		return false
	}
	if f.Model != nil && r.Model != *f.Model {
		return false
	}
	if f.Agent != nil && r.Agent != *f.Agent {
		return false
	}
	if f.Scenario != nil && r.Scenario != *f.Scenario {
		return false
	}
	if f.From != nil && r.Timestamp.Before(*f.From) {
		return false
	}
	if f.To != nil && r.Timestamp.After(*f.To) {
		return false
	}
	return true
}
