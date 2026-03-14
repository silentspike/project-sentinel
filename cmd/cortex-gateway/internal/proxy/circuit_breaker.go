package proxy

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
)

// BreakerState represents the circuit breaker state machine.
type BreakerState int

const (
	// StateClosed erlaubt alle Requests (Normalbetrieb).
	StateClosed BreakerState = iota
	// StateOpen blockiert alle Requests (Provider kaputt).
	StateOpen
	// StateHalfOpen laesst limitierte Probe-Requests durch.
	StateHalfOpen
)

// String gibt den State-Namen zurueck (fuer Metriken/Logging).
func (s BreakerState) String() string {
	switch s {
	case StateClosed:
		return "closed"
	case StateOpen:
		return "open"
	case StateHalfOpen:
		return "half-open"
	default:
		return "unknown"
	}
}

// BreakerConfig konfiguriert das Circuit-Breaker-Verhalten.
type BreakerConfig struct {
	WindowSeconds    int     // Sliding window fuer Failure-Ratio (default: 20)
	MinRequests      int     // Mindest-Requests im Window vor Ratio-Check (default: 20)
	FailureRatio     float64 // Schwellenwert fuer Open-Transition (default: 0.5)
	FailureThreshold int     // Consecutive failures fuer Open-Transition (default: 5)
	OpenSeconds      int     // Wartezeit bis Half-Open (default: 30)
	HalfOpenProbes   int     // Probe-Requests im Half-Open State (default: 3)
	Enabled          bool    // SENTINEL_CORTEX_CB_ENABLED runtime gate (default: true)
}

// DefaultBreakerConfig gibt die Issue-spezifizierten Defaults zurueck.
func DefaultBreakerConfig() BreakerConfig {
	return BreakerConfig{
		WindowSeconds:    20,
		MinRequests:      20,
		FailureRatio:     0.5,
		FailureThreshold: 5,
		OpenSeconds:      30,
		HalfOpenProbes:   3,
		Enabled:          true,
	}
}

// BreakerConfigFromEnv liest Circuit-Breaker-Config aus ENV-Variablen.
// Fehlende/ungueltige Werte fallen auf Defaults zurueck.
func BreakerConfigFromEnv() BreakerConfig {
	cfg := DefaultBreakerConfig()
	if v := os.Getenv("SENTINEL_CORTEX_CB_WINDOW_SECONDS"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			cfg.WindowSeconds = n
		}
	}
	if v := os.Getenv("SENTINEL_CORTEX_CB_MIN_REQUESTS"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			cfg.MinRequests = n
		}
	}
	if v := os.Getenv("SENTINEL_CORTEX_CB_FAILURE_RATIO"); v != "" {
		if f, err := strconv.ParseFloat(v, 64); err == nil && f > 0 && f <= 1 {
			cfg.FailureRatio = f
		}
	}
	if v := os.Getenv("SENTINEL_CORTEX_CB_FAILURE_THRESHOLD"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			cfg.FailureThreshold = n
		}
	}
	if v := os.Getenv("SENTINEL_CORTEX_CB_OPEN_SECONDS"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			cfg.OpenSeconds = n
		}
	}
	if v := os.Getenv("SENTINEL_CORTEX_CB_HALFOPEN_PROBES"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			cfg.HalfOpenProbes = n
		}
	}
	if v := os.Getenv("SENTINEL_CORTEX_CB_ENABLED"); v != "" {
		lower := strings.ToLower(v)
		if lower == "false" || lower == "0" || lower == "no" || lower == "off" {
			cfg.Enabled = false
			slog.Warn("Feature deaktiviert via ENV", "flag", "SENTINEL_CORTEX_CB_ENABLED")
		}
	}
	return cfg
}

// requestRecord speichert ein Request-Ergebnis im Sliding Window.
type requestRecord struct {
	at     time.Time
	failed bool
}

// CircuitBreaker implementiert das Circuit-Breaker-Pattern pro Provider.
type CircuitBreaker struct {
	mu          sync.Mutex
	state       BreakerState
	records     []requestRecord // Sliding window
	consecutive int             // Consecutive failures
	openedAt    time.Time       // Zeitpunkt der Open-Transition
	probeCount  int             // Erfolgreiche Probes in Half-Open
	config      BreakerConfig
	now         func() time.Time // Fuer Tests injizierbar
}

// NewCircuitBreaker erstellt einen CircuitBreaker im Closed-State.
func NewCircuitBreaker(cfg BreakerConfig) *CircuitBreaker {
	return &CircuitBreaker{
		state:  StateClosed,
		config: cfg,
		now:    time.Now,
	}
}

// Allow prueft ob der naechste Request durchgelassen wird.
// Im Open-State wird false zurueckgegeben (ausser Timeout abgelaufen → Half-Open).
// Im Half-Open-State wird nur bei < HalfOpenProbes-Limit true zurueckgegeben.
func (cb *CircuitBreaker) Allow() bool {
	cb.mu.Lock()
	defer cb.mu.Unlock()

	switch cb.state {
	case StateClosed:
		return true

	case StateOpen:
		if cb.now().Sub(cb.openedAt) >= time.Duration(cb.config.OpenSeconds)*time.Second {
			cb.state = StateHalfOpen
			cb.probeCount = 0
			return true
		}
		return false

	case StateHalfOpen:
		return cb.probeCount < cb.config.HalfOpenProbes
	}
	return false
}

// Record meldet das Ergebnis eines Requests.
// err == nil → Erfolg. err != nil → wird auf Failure-Klasse geprueft.
func (cb *CircuitBreaker) Record(err error) {
	cb.mu.Lock()
	defer cb.mu.Unlock()

	failed := err != nil && isCircuitBreakerFailure(err)
	now := cb.now()

	switch cb.state {
	case StateClosed:
		cb.records = append(cb.records, requestRecord{at: now, failed: failed})
		cb.pruneWindow(now)

		if failed {
			cb.consecutive++
		} else {
			cb.consecutive = 0
		}

		if cb.shouldTrip() {
			cb.tripOpen(now)
		}

	case StateHalfOpen:
		if failed {
			cb.tripOpen(now)
		} else {
			cb.probeCount++
			if cb.probeCount >= cb.config.HalfOpenProbes {
				cb.state = StateClosed
				cb.records = cb.records[:0]
				cb.consecutive = 0
			}
		}

	case StateOpen:
		// Ignoriert (sollte nicht passieren bei korrekter Allow()-Nutzung)
	}
}

// State gibt den aktuellen Breaker-State als String zurueck.
func (cb *CircuitBreaker) State() string {
	cb.mu.Lock()
	defer cb.mu.Unlock()
	return cb.state.String()
}

// pruneWindow entfernt Records ausserhalb des Sliding Windows.
func (cb *CircuitBreaker) pruneWindow(now time.Time) {
	cutoff := now.Add(-time.Duration(cb.config.WindowSeconds) * time.Second)
	i := 0
	for i < len(cb.records) && cb.records[i].at.Before(cutoff) {
		i++
	}
	if i > 0 {
		cb.records = cb.records[i:]
	}
}

// shouldTrip prueft ob der Breaker oeffnen soll.
func (cb *CircuitBreaker) shouldTrip() bool {
	// Consecutive failure threshold
	if cb.consecutive >= cb.config.FailureThreshold {
		return true
	}

	// Ratio-basiert (nur wenn genug Requests im Window)
	total := len(cb.records)
	if total < cb.config.MinRequests {
		return false
	}
	failures := 0
	for _, r := range cb.records {
		if r.failed {
			failures++
		}
	}
	ratio := float64(failures) / float64(total)
	return ratio >= cb.config.FailureRatio
}

// tripOpen wechselt in den Open-State.
func (cb *CircuitBreaker) tripOpen(now time.Time) {
	cb.state = StateOpen
	cb.openedAt = now
	cb.probeCount = 0
}

// ProviderError repraesentiert einen Fehler mit HTTP-Statuscode.
type ProviderError struct {
	StatusCode int
	Message    string
}

func (e *ProviderError) Error() string {
	return fmt.Sprintf("provider error: HTTP %d: %s", e.StatusCode, e.Message)
}

// isCircuitBreakerFailure bestimmt ob ein Error als Breaker-Failure zaehlt.
// Failure-Klassen: timeout, transport error, HTTP 5xx, HTTP 429.
// Semantische 4xx (400, 401, 403, 422) zaehlen NICHT.
func isCircuitBreakerFailure(err error) bool {
	if err == nil {
		return false
	}

	// Timeout
	if errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
		return true
	}

	// Network/transport error
	var netErr net.Error
	if errors.As(err, &netErr) {
		return true
	}

	// HTTP-Status basiert
	var provErr *ProviderError
	if errors.As(err, &provErr) {
		if provErr.StatusCode >= 500 {
			return true
		}
		if provErr.StatusCode == 429 {
			return true
		}
		// 4xx (ausser 429) zaehlt nicht
		return false
	}

	// Unbekannter Fehler → sicherheitshalber als Failure
	return true
}
