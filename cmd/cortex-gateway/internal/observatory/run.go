package observatory

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"time"
)

// RunSummary holds metadata for a single benchmark run.
type RunSummary struct {
	RunID       string    `json:"run_id"`
	StartedAt   time.Time `json:"started_at"`
	FinishedAt  time.Time `json:"finished_at,omitempty"`
	ConfigHash  string    `json:"config_hash"`
	Status      string    `json:"status"`
	RecordCount int       `json:"record_count"`
}

// ConfigHash computes a SHA-256 hash of the observatory configuration
// for reproducibility tracking (AC-1).
func ConfigHash(cfg *ObservatoryConfig) string {
	h := sha256.New()
	shifts := cfg.Shifts()
	for i, s := range shifts {
		fmt.Fprintf(h, "shift_%d:%s:%s:%d;", i+1, s.Model, s.Provider, s.Agents)
	}
	sc := cfg.Observatory.Scenarios
	fmt.Fprintf(h, "daily=%t;crisis=%t;creative=%t;conflict=%t",
		sc.DailyRoutine, sc.CrisisResponse, sc.CreativeTask, sc.ConflictResolution)
	return hex.EncodeToString(h.Sum(nil))
}

// generateUUID returns a new random UUIDv4 string.
func generateUUID() string {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic("crypto/rand failed: " + err.Error())
	}
	b[6] = (b[6] & 0x0f) | 0x40 // version 4
	b[8] = (b[8] & 0x3f) | 0x80 // variant 2
	return fmt.Sprintf("%s-%s-%s-%s-%s",
		hex.EncodeToString(b[0:4]),
		hex.EncodeToString(b[4:6]),
		hex.EncodeToString(b[6:8]),
		hex.EncodeToString(b[8:10]),
		hex.EncodeToString(b[10:16]),
	)
}
