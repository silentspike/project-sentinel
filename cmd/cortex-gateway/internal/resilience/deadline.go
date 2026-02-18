package resilience

import (
	"os"
	"strconv"
	"sync"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
)

// Default und Grenzen fuer Zenoh Query Deadline.
const (
	DefaultZenohDeadlineMs = 100
	MinZenohDeadlineMs     = 50
	MaxZenohDeadlineMs     = 120
)

var (
	queryCancelledTotal = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_query_cancelled_total",
		Help: "Total Zenoh queries cancelled due to deadline expiry",
	})

	queryStaleDroppedTotal = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_query_stale_dropped_total",
		Help: "Total Zenoh responses dropped because query_id expired or response_tick < min_tick",
	})
)

// QueryCancelledTotal returns the Prometheus counter for cancelled queries.
func QueryCancelledTotal() prometheus.Counter { return queryCancelledTotal }

// QueryStaleDroppedTotal returns the Prometheus counter for stale-dropped responses.
func QueryStaleDroppedTotal() prometheus.Counter { return queryStaleDroppedTotal }

// ZenohDeadlineFromEnv liest die Zenoh Query Deadline aus ENV.
// Range: 50-120ms, Default: 100ms.
func ZenohDeadlineFromEnv() time.Duration {
	v := os.Getenv("SENTINEL_CORTEX_ZENOH_DEADLINE_MS")
	if v == "" {
		return time.Duration(DefaultZenohDeadlineMs) * time.Millisecond
	}
	n, err := strconv.Atoi(v)
	if err != nil || n < MinZenohDeadlineMs || n > MaxZenohDeadlineMs {
		return time.Duration(DefaultZenohDeadlineMs) * time.Millisecond
	}
	return time.Duration(n) * time.Millisecond
}

// inflightEntry repraesentiert eine laufende Zenoh Query.
type inflightEntry struct {
	deadline time.Time
	minTick  int64
}

// InFlightMap verwaltet laufende Zenoh Queries mit TTL-basierter Deadline.
// Queries die ihre Deadline ueberschreiten werden als abgelaufen markiert.
// Responses mit response_tick < min_tick werden als stale verworfen.
type InFlightMap struct {
	mu       sync.Mutex
	entries  map[string]inflightEntry
	deadline time.Duration
	now      func() time.Time // Injizierbar fuer Tests
}

// NewInFlightMap erstellt eine neue InFlightMap mit der angegebenen Deadline.
func NewInFlightMap(deadline time.Duration) *InFlightMap {
	return &InFlightMap{
		entries:  make(map[string]inflightEntry),
		deadline: deadline,
		now:      time.Now,
	}
}

// Track registriert eine neue Query mit der angegebenen query_id und min_tick.
// Gibt die Deadline zurueck bis zu der eine Response akzeptiert wird.
func (m *InFlightMap) Track(queryID string, minTick int64) time.Time {
	m.mu.Lock()
	defer m.mu.Unlock()

	dl := m.now().Add(m.deadline)
	m.entries[queryID] = inflightEntry{
		deadline: dl,
		minTick:  minTick,
	}
	return dl
}

// Accept prueft ob eine Response fuer die gegebene query_id akzeptiert wird.
// Gibt true zurueck wenn die Query noch aktiv ist, die Deadline nicht ueberschritten
// wurde, und der response_tick >= min_tick ist.
// Bei Akzeptanz wird die Query aus der Map entfernt.
// Aktualisiert Prometheus-Metriken bei Cancellation oder Stale-Drop.
func (m *InFlightMap) Accept(queryID string, responseTick int64) bool {
	m.mu.Lock()
	defer m.mu.Unlock()

	entry, ok := m.entries[queryID]
	if !ok {
		// Query-ID unbekannt (schon entfernt oder nie registriert)
		queryCancelledTotal.Inc()
		return false
	}

	now := m.now()

	// Deadline ueberschritten → Query abgelaufen
	if now.After(entry.deadline) {
		delete(m.entries, queryID)
		queryCancelledTotal.Inc()
		return false
	}

	// Stale-Drop: response_tick < min_tick
	if responseTick < entry.minTick {
		delete(m.entries, queryID)
		queryStaleDroppedTotal.Inc()
		return false
	}

	// Akzeptiert — Query aus Map entfernen
	delete(m.entries, queryID)
	return true
}

// Cancel entfernt eine Query manuell aus der InFlightMap (z.B. bei Context-Cancellation).
func (m *InFlightMap) Cancel(queryID string) {
	m.mu.Lock()
	defer m.mu.Unlock()

	if _, ok := m.entries[queryID]; ok {
		delete(m.entries, queryID)
		queryCancelledTotal.Inc()
	}
}

// Prune entfernt alle abgelaufenen Queries aus der Map.
// Sollte regelmaessig aufgerufen werden um Memory-Leaks zu vermeiden.
func (m *InFlightMap) Prune() int {
	m.mu.Lock()
	defer m.mu.Unlock()

	now := m.now()
	pruned := 0
	for id, entry := range m.entries {
		if now.After(entry.deadline) {
			delete(m.entries, id)
			queryCancelledTotal.Inc()
			pruned++
		}
	}
	return pruned
}

// Len gibt die Anzahl aktiver (nicht abgelaufener) Queries zurueck.
func (m *InFlightMap) Len() int {
	m.mu.Lock()
	defer m.mu.Unlock()
	return len(m.entries)
}
