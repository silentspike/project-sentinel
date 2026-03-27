package apicp

import (
	"encoding/json"
	"hash/fnv"
	"log/slog"
	"os"
	"sync"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
)

const (
	maxBufferSize   = 10000
	maxPatternsPerAgent = 1000
	minSamplesForPromotion = 50
	promotionThreshold = 0.90
	degradationFactor = 0.5
	probeInterval = 100 // every 100 synthesis calls, probe 1 real call
)

var (
	observerRecordedTotal = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_apicp_observations_total",
		Help: "Total API call observations recorded",
	})
	patternsLearnedTotal = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_apicp_patterns_learned_total",
		Help: "Total patterns promoted to synthesis-eligible",
	})
)

// Observation records a single API call for learning.
type Observation struct {
	Fingerprint string    `json:"fingerprint"`
	AgentID     string    `json:"agent_id"`
	ResponseHash uint64   `json:"response_hash"`
	WasSynth    bool      `json:"was_synth"`
	Timestamp   time.Time `json:"timestamp"`
}

// PatternStats tracks confidence for a fingerprint pattern.
type PatternStats struct {
	Count          int            `json:"count"`
	ResponseHashes map[uint64]int `json:"response_hashes"`
	Confidence     float64        `json:"confidence"`
	LastSeen       time.Time      `json:"last_seen"`
}

// PatternSuggestion is a pattern the API-CP recommends for synthesis.
type PatternSuggestion struct {
	Fingerprint string  `json:"fingerprint"`
	Confidence  float64 `json:"confidence"`
	Samples     int     `json:"samples"`
	TopHash     uint64  `json:"top_hash"`
}

// Observer watches all API calls and learns synthesis-eligible patterns.
type Observer struct {
	mu           sync.RWMutex
	buffer       []Observation
	head         int
	bufferFull   bool
	stats        map[string]*PatternStats // fingerprint → stats
	dumpPath     string
	dumpInterval time.Duration
	logger       *slog.Logger
	stopCh       chan struct{}
	synthCount   int64 // counter for probe scheduling
	lastEvolutionVersions map[string]string // agent_id → evolution_version
}

// NewObserver creates an API-CP observer with periodic JSON dump.
func NewObserver(dumpPath string, dumpInterval time.Duration, logger *slog.Logger) *Observer {
	if logger == nil {
		logger = slog.Default()
	}
	o := &Observer{
		buffer:       make([]Observation, maxBufferSize),
		stats:        make(map[string]*PatternStats),
		dumpPath:     dumpPath,
		dumpInterval: dumpInterval,
		logger:       logger,
		stopCh:       make(chan struct{}),
		lastEvolutionVersions: make(map[string]string),
	}
	// Load existing patterns if dump file exists
	o.loadFromDisk()
	// Start periodic dump (time.Ticker, NOT tick-based)
	go o.dumpLoop()
	return o
}

// Record adds an observation for a completed API call.
func (o *Observer) Record(fingerprint, agentID string, responseContent string, wasSynth bool) {
	hash := hashContent(responseContent)

	o.mu.Lock()
	defer o.mu.Unlock()

	// Ring buffer
	obs := Observation{
		Fingerprint:  fingerprint,
		AgentID:      agentID,
		ResponseHash: hash,
		WasSynth:     wasSynth,
		Timestamp:    time.Now(),
	}
	o.buffer[o.head] = obs
	o.head = (o.head + 1) % maxBufferSize
	if o.head == 0 {
		o.bufferFull = true
	}

	// Update pattern stats
	ps, ok := o.stats[fingerprint]
	if !ok {
		ps = &PatternStats{ResponseHashes: make(map[uint64]int)}
		o.stats[fingerprint] = ps
	}
	ps.Count++
	ps.ResponseHashes[hash]++
	ps.LastSeen = time.Now()
	ps.Confidence = o.calcConfidence(ps)

	observerRecordedTotal.Inc()

	if wasSynth {
		o.synthCount++
	}
}

// CheckEvolutionDegradation halves all confidences when an agent's evolution_version changes (AC-13).
func (o *Observer) CheckEvolutionDegradation(agentID, evolutionVersion string) {
	o.mu.Lock()
	defer o.mu.Unlock()

	prev, exists := o.lastEvolutionVersions[agentID]
	o.lastEvolutionVersions[agentID] = evolutionVersion

	if exists && prev != evolutionVersion && evolutionVersion != "" {
		// Evolution changed — degrade ALL pattern confidences
		for _, ps := range o.stats {
			ps.Confidence *= degradationFactor
		}
		o.logger.Info("evolution degradation applied",
			"agent", agentID,
			"old_version", prev,
			"new_version", evolutionVersion,
		)
	}
}

// ShouldProbe returns true if a synthesis call should be probed (verified against real LLM).
// AC-14: every 100 synthesis calls, probe 1.
func (o *Observer) ShouldProbe() bool {
	o.mu.RLock()
	defer o.mu.RUnlock()
	return o.synthCount > 0 && o.synthCount%probeInterval == 0
}

// Suggestions returns patterns with high enough confidence for synthesis promotion.
// AC-12: confidence > 0.90 AND count > 50.
func (o *Observer) Suggestions() []PatternSuggestion {
	o.mu.RLock()
	defer o.mu.RUnlock()
	return o.suggestionsLocked()
}

func (o *Observer) suggestionsLocked() []PatternSuggestion {
	var suggestions []PatternSuggestion
	for fp, ps := range o.stats {
		if ps.Confidence >= promotionThreshold && ps.Count >= minSamplesForPromotion {
			topHash, _ := o.topResponseHash(ps)
			suggestions = append(suggestions, PatternSuggestion{
				Fingerprint: fp,
				Confidence:  ps.Confidence,
				Samples:     ps.Count,
				TopHash:     topHash,
			})
		}
	}
	return suggestions
}

// Stats returns a summary of the observer state.
func (o *Observer) Stats() map[string]interface{} {
	o.mu.RLock()
	defer o.mu.RUnlock()

	bufferUsed := o.head
	if o.bufferFull {
		bufferUsed = maxBufferSize
	}

	return map[string]interface{}{
		"buffer_used":    bufferUsed,
		"buffer_max":     maxBufferSize,
		"patterns_total": len(o.stats),
		"synth_count":    o.synthCount,
		"suggestions":    len(o.suggestionsLocked()),
	}
}

func (o *Observer) calcConfidence(ps *PatternStats) float64 {
	if ps.Count == 0 {
		return 0
	}
	_, topCount := o.topResponseHash(ps)
	return float64(topCount) / float64(ps.Count)
}

func (o *Observer) topResponseHash(ps *PatternStats) (uint64, int) {
	var topHash uint64
	var topCount int
	for h, c := range ps.ResponseHashes {
		if c > topCount {
			topHash = h
			topCount = c
		}
	}
	return topHash, topCount
}

func hashContent(content string) uint64 {
	h := fnv.New64a()
	h.Write([]byte(content))
	return h.Sum64()
}

// dumpLoop periodically writes patterns to disk (time.Ticker, NOT tick-based).
// AC-20: ZERO disk writes in hot path — dump is async background.
func (o *Observer) dumpLoop() {
	ticker := time.NewTicker(o.dumpInterval)
	defer ticker.Stop()

	for {
		select {
		case <-o.stopCh:
			return
		case <-ticker.C:
			o.dumpToDisk()
		}
	}
}

func (o *Observer) dumpToDisk() {
	o.mu.RLock()
	data, err := json.Marshal(o.stats)
	o.mu.RUnlock()

	if err != nil {
		o.logger.Error("apicp dump marshal error", "error", err)
		return
	}

	if err := os.WriteFile(o.dumpPath, data, 0644); err != nil {
		o.logger.Error("apicp dump write error", "error", err, "path", o.dumpPath)
		return
	}

	o.logger.Debug("apicp patterns dumped", "path", o.dumpPath, "patterns", len(o.stats))
}

func (o *Observer) loadFromDisk() {
	data, err := os.ReadFile(o.dumpPath)
	if err != nil {
		return // file doesn't exist yet — fresh start
	}

	var stats map[string]*PatternStats
	if err := json.Unmarshal(data, &stats); err != nil {
		o.logger.Warn("apicp load parse error", "error", err)
		return
	}

	o.stats = stats
	o.logger.Info("apicp patterns loaded", "path", o.dumpPath, "patterns", len(stats))
}

// Stop shuts down the dump goroutine.
func (o *Observer) Stop() {
	close(o.stopCh)
	o.dumpToDisk() // final dump
}
