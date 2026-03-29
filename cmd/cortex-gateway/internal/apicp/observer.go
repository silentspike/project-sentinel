package apicp

import (
	"bytes"
	"context"
	"encoding/json"
	"hash/fnv"
	"io"
	"log/slog"
	"net/http"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
)

const (
	maxBufferSize          = 10000
	maxPatternsPerAgent    = 1000
	minSamplesForPromotion = 50
	promotionThreshold     = 0.90
	degradationFactor      = 0.5
	probeInterval          = 100
	maxPatternContentBytes = 4096
	defaultSyncTimeout     = 5 * time.Second
	operatorKeyHeader      = "x-sentinel-operator-key"
)

var (
	bootstrapRetryDelay    = 2 * time.Second
	bootstrapRetryAttempts = 5
	observerRecordedTotal = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_apicp_observations_total",
		Help: "Total API call observations recorded",
	})
	patternsLearnedTotal = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_apicp_patterns_learned_total",
		Help: "Total patterns promoted to synthesis-eligible",
	})
)

// Config configures daemon-backed API-CP snapshot sync.
type Config struct {
	SyncURL      string
	SyncInterval time.Duration
	SharedSecret string
	HTTPClient   *http.Client
}

// Observation records a single API call for learning.
type Observation struct {
	Fingerprint  string    `json:"fingerprint"`
	AgentID      string    `json:"agent_id"`
	ResponseHash uint64    `json:"response_hash"`
	WasSynth     bool      `json:"was_synth"`
	Timestamp    time.Time `json:"timestamp"`
}

// PatternStats tracks confidence for a fingerprint pattern.
type PatternStats struct {
	AgentID        string         `json:"agent_id"`
	Fingerprint    string         `json:"fingerprint"`
	Count          int            `json:"count"`
	ResponseHashes map[uint64]int `json:"response_hashes"`
	TopHash        uint64         `json:"top_hash"`
	TopContent     string         `json:"top_content,omitempty"`
	Confidence     float64        `json:"confidence"`
	LastSeen       time.Time      `json:"last_seen"`
	Promoted       bool           `json:"promoted"`
}

// PatternSuggestion is a pattern the API-CP recommends for synthesis.
type PatternSuggestion struct {
	AgentID     string  `json:"agent_id"`
	Fingerprint string  `json:"fingerprint"`
	Confidence  float64 `json:"confidence"`
	Samples     int     `json:"samples"`
	TopHash     uint64  `json:"top_hash"`
}

// LearnedPattern is a synthesis-ready promoted pattern with exemplar content.
type LearnedPattern struct {
	AgentID     string
	Fingerprint string
	Confidence  float64
	Samples     int
	TopHash     uint64
	Content     string
}

// Snapshot is the persisted daemon-owned API-CP state.
type Snapshot struct {
	Patterns              []*PatternStats    `json:"patterns"`
	SynthCount            int64              `json:"synth_count"`
	LastEvolutionVersions map[string]string  `json:"last_evolution_versions"`
}

// Observer watches all API calls and learns synthesis-eligible patterns.
type Observer struct {
	mu                    sync.RWMutex
	buffer                []Observation
	head                  int
	bufferFull            bool
	stats                 map[string]*PatternStats
	syncURL               string
	syncInterval          time.Duration
	sharedSecret          string
	httpClient            *http.Client
	logger                *slog.Logger
	stopCh                chan struct{}
	stopOnce              sync.Once
	synthCount            int64
	lastEvolutionVersions map[string]string
}

// NewObserver creates an API-CP observer with daemon-backed snapshot sync.
func NewObserver(cfg Config, logger *slog.Logger) *Observer {
	if logger == nil {
		logger = slog.Default()
	}
	if cfg.HTTPClient == nil {
		cfg.HTTPClient = &http.Client{Timeout: defaultSyncTimeout}
	}
	if cfg.SyncInterval <= 0 {
		cfg.SyncInterval = 5 * time.Minute
	}

	o := &Observer{
		buffer:                make([]Observation, maxBufferSize),
		stats:                 make(map[string]*PatternStats),
		syncURL:               strings.TrimRight(cfg.SyncURL, "/"),
		syncInterval:          cfg.SyncInterval,
		sharedSecret:          cfg.SharedSecret,
		httpClient:            cfg.HTTPClient,
		logger:                logger,
		stopCh:                make(chan struct{}),
		lastEvolutionVersions: make(map[string]string),
	}

	if o.syncURL != "" {
		if !o.loadRemote() {
			go o.bootstrapLoadRetry()
		}
		go o.syncLoop()
	}
	return o
}

// Record adds an observation for a completed API call.
// When a non-empty signature is provided, it is used as the learning hash input
// while the full response content remains the exemplar payload.
func (o *Observer) Record(fingerprint, agentID string, responseContent string, wasSynth bool, signature ...string) {
	hashInput := responseContent
	if len(signature) > 0 && strings.TrimSpace(signature[0]) != "" {
		hashInput = signature[0]
	}
	hash := hashContent(hashInput)
	now := time.Now()

	o.mu.Lock()
	defer o.mu.Unlock()

	obs := Observation{
		Fingerprint:  fingerprint,
		AgentID:      agentID,
		ResponseHash: hash,
		WasSynth:     wasSynth,
		Timestamp:    now,
	}
	o.buffer[o.head] = obs
	o.head = (o.head + 1) % maxBufferSize
	if o.head == 0 {
		o.bufferFull = true
	}

	key := patternKey(agentID, fingerprint)
	ps, ok := o.stats[key]
	if !ok {
		ps = &PatternStats{
			AgentID:        agentID,
			Fingerprint:    fingerprint,
			ResponseHashes: make(map[uint64]int),
		}
		o.stats[key] = ps
	}
	ps.Count++
	ps.ResponseHashes[hash]++
	ps.LastSeen = now
	o.updateTopContentLocked(ps, hash, responseContent)
	ps.Confidence = o.calcConfidence(ps)
	o.updatePromotionLocked(ps)
	o.enforcePatternLimitLocked(agentID)

	observerRecordedTotal.Inc()
	if wasSynth {
		o.synthCount++
	}
}

// CheckEvolutionDegradation halves confidences when an agent's evolution_version changes.
func (o *Observer) CheckEvolutionDegradation(agentID, evolutionVersion string) {
	o.mu.Lock()
	defer o.mu.Unlock()

	prev, exists := o.lastEvolutionVersions[agentID]
	o.lastEvolutionVersions[agentID] = evolutionVersion

	if !exists || prev == evolutionVersion || evolutionVersion == "" {
		return
	}

	for _, ps := range o.stats {
		if ps.AgentID != agentID {
			continue
		}
		ps.Confidence *= degradationFactor
		o.updatePromotionLocked(ps)
	}
	o.logger.Info("evolution degradation applied",
		"agent", agentID,
		"old_version", prev,
		"new_version", evolutionVersion,
	)
}

// ShouldProbe returns true if a synthesis call should be probed.
func (o *Observer) ShouldProbe() bool {
	o.mu.RLock()
	defer o.mu.RUnlock()
	return o.synthCount > 0 && o.synthCount%probeInterval == 0
}

// ShouldProbeNext reports whether the next synthesis-eligible call should be probed.
func (o *Observer) ShouldProbeNext() bool {
	o.mu.RLock()
	defer o.mu.RUnlock()
	return o.synthCount > 0 && (o.synthCount+1)%probeInterval == 0
}

// MarkSynthesisCandidate increments the synthesis cadence counter without
// reinforcing the learned pattern stats with synthetic content.
func (o *Observer) MarkSynthesisCandidate() {
	o.mu.Lock()
	defer o.mu.Unlock()
	o.synthCount++
}

// Suggestions returns patterns with high enough confidence for synthesis promotion.
func (o *Observer) Suggestions() []PatternSuggestion {
	o.mu.RLock()
	defer o.mu.RUnlock()
	return o.suggestionsLocked()
}

// LearnedPatternFor returns a promoted synthesis-ready pattern for agent+fingerprint.
func (o *Observer) LearnedPatternFor(agentID, fingerprint string) (LearnedPattern, bool) {
	o.mu.RLock()
	defer o.mu.RUnlock()

	ps, ok := o.stats[patternKey(agentID, fingerprint)]
	if !ok || !ps.Promoted || strings.TrimSpace(ps.TopContent) == "" {
		return LearnedPattern{}, false
	}
	return LearnedPattern{
		AgentID:     ps.AgentID,
		Fingerprint: ps.Fingerprint,
		Confidence:  ps.Confidence,
		Samples:     ps.Count,
		TopHash:     ps.TopHash,
		Content:     ps.TopContent,
	}, true
}

// ApplyProbeResult degrades a promoted pattern if the probed real response
// diverges from the expected top hash.
func (o *Observer) ApplyProbeResult(agentID, fingerprint string, expectedHash uint64, responseContent string) {
	o.mu.Lock()
	defer o.mu.Unlock()

	ps, ok := o.stats[patternKey(agentID, fingerprint)]
	if !ok || expectedHash == 0 {
		return
	}
	if hashContent(responseContent) == expectedHash {
		return
	}
	ps.Confidence *= degradationFactor
	o.updatePromotionLocked(ps)
}

// Snapshot returns the current persistable observer state.
func (o *Observer) Snapshot() Snapshot {
	o.mu.RLock()
	defer o.mu.RUnlock()
	return o.snapshotLocked()
}

func (o *Observer) suggestionsLocked() []PatternSuggestion {
	suggestions := make([]PatternSuggestion, 0, len(o.stats))
	for _, ps := range o.stats {
		if !ps.Promoted {
			continue
		}
		topHash, _ := o.topResponseHash(ps)
		suggestions = append(suggestions, PatternSuggestion{
			AgentID:     ps.AgentID,
			Fingerprint: ps.Fingerprint,
			Confidence:  ps.Confidence,
			Samples:     ps.Count,
			TopHash:     topHash,
		})
	}
	sort.Slice(suggestions, func(i, j int) bool {
		if suggestions[i].AgentID == suggestions[j].AgentID {
			return suggestions[i].Fingerprint < suggestions[j].Fingerprint
		}
		return suggestions[i].AgentID < suggestions[j].AgentID
	})
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

	promoted := 0
	for _, ps := range o.stats {
		if ps.Promoted {
			promoted++
		}
	}

	return map[string]interface{}{
		"buffer_used":       bufferUsed,
		"buffer_max":        maxBufferSize,
		"patterns_total":    len(o.stats),
		"synth_count":       o.synthCount,
		"suggestions":       len(o.suggestionsLocked()),
		"promoted_patterns": promoted,
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
	_, _ = h.Write([]byte(content))
	return h.Sum64()
}

func trimPatternContent(content string) string {
	if len(content) <= maxPatternContentBytes {
		return content
	}
	return content[:maxPatternContentBytes]
}

func patternKey(agentID, fingerprint string) string {
	return agentID + "\x1f" + fingerprint
}

func (o *Observer) updatePromotionLocked(ps *PatternStats) {
	promoted := ps.Confidence >= promotionThreshold && ps.Count >= minSamplesForPromotion
	if promoted && !ps.Promoted {
		patternsLearnedTotal.Inc()
	}
	ps.Promoted = promoted
}

func (o *Observer) updateTopContentLocked(ps *PatternStats, hash uint64, responseContent string) {
	if ps.Count == 1 {
		ps.TopHash = hash
		ps.TopContent = trimPatternContent(responseContent)
		return
	}
	currentTopCount := ps.ResponseHashes[ps.TopHash]
	newCount := ps.ResponseHashes[hash]
	if newCount > currentTopCount || ps.TopContent == "" {
		ps.TopHash = hash
		ps.TopContent = trimPatternContent(responseContent)
	}
}

func (o *Observer) enforcePatternLimitLocked(agentID string) {
	if agentID == "" {
		return
	}
	type candidate struct {
		key      string
		lastSeen time.Time
	}
	var candidates []candidate
	for key, ps := range o.stats {
		if ps.AgentID != agentID {
			continue
		}
		candidates = append(candidates, candidate{key: key, lastSeen: ps.LastSeen})
	}
	if len(candidates) <= maxPatternsPerAgent {
		return
	}
	sort.Slice(candidates, func(i, j int) bool {
		return candidates[i].lastSeen.Before(candidates[j].lastSeen)
	})
	for _, evict := range candidates[:len(candidates)-maxPatternsPerAgent] {
		delete(o.stats, evict.key)
	}
}

func (o *Observer) snapshotLocked() Snapshot {
	patterns := make([]*PatternStats, 0, len(o.stats))
	for _, ps := range o.stats {
		clone := &PatternStats{
			AgentID:        ps.AgentID,
			Fingerprint:    ps.Fingerprint,
			Count:          ps.Count,
			ResponseHashes: cloneResponseHashes(ps.ResponseHashes),
			TopHash:        ps.TopHash,
			TopContent:     ps.TopContent,
			Confidence:     ps.Confidence,
			LastSeen:       ps.LastSeen,
			Promoted:       ps.Promoted,
		}
		patterns = append(patterns, clone)
	}
	sort.Slice(patterns, func(i, j int) bool {
		if patterns[i].AgentID == patterns[j].AgentID {
			return patterns[i].Fingerprint < patterns[j].Fingerprint
		}
		return patterns[i].AgentID < patterns[j].AgentID
	})

	versions := make(map[string]string, len(o.lastEvolutionVersions))
	for k, v := range o.lastEvolutionVersions {
		versions[k] = v
	}

	return Snapshot{
		Patterns:              patterns,
		SynthCount:            o.synthCount,
		LastEvolutionVersions: versions,
	}
}

func (o *Observer) restore(snapshot Snapshot) {
	o.mu.Lock()
	defer o.mu.Unlock()

	o.stats = make(map[string]*PatternStats, len(snapshot.Patterns))
	for _, ps := range snapshot.Patterns {
		if ps == nil {
			continue
		}
		restored := &PatternStats{
			AgentID:        ps.AgentID,
			Fingerprint:    ps.Fingerprint,
			Count:          ps.Count,
			ResponseHashes: cloneResponseHashes(ps.ResponseHashes),
			TopHash:        ps.TopHash,
			TopContent:     ps.TopContent,
			Confidence:     ps.Confidence,
			LastSeen:       ps.LastSeen,
			Promoted:       ps.Promoted,
		}
		o.updatePromotionLocked(restored)
		o.stats[patternKey(restored.AgentID, restored.Fingerprint)] = restored
	}

	o.synthCount = snapshot.SynthCount
	o.lastEvolutionVersions = make(map[string]string, len(snapshot.LastEvolutionVersions))
	for k, v := range snapshot.LastEvolutionVersions {
		o.lastEvolutionVersions[k] = v
	}
}

func cloneResponseHashes(src map[uint64]int) map[uint64]int {
	if len(src) == 0 {
		return map[uint64]int{}
	}
	dst := make(map[uint64]int, len(src))
	for k, v := range src {
		dst[k] = v
	}
	return dst
}

func (o *Observer) syncLoop() {
	ticker := time.NewTicker(o.syncInterval)
	defer ticker.Stop()

	for {
		select {
		case <-o.stopCh:
			return
		case <-ticker.C:
			o.syncToRemote()
		}
	}
}

func (o *Observer) loadRemote() bool {
	if o.syncURL == "" {
		return true
	}

	ctx, cancel := context.WithTimeout(context.Background(), defaultSyncTimeout)
	defer cancel()

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, o.syncURL, nil) //nolint:gosec // trusted local operator API URL
	if err != nil {
		o.logger.Error("apicp snapshot request build failed", "error", err)
		return false
	}
	if o.sharedSecret != "" {
		req.Header.Set(operatorKeyHeader, o.sharedSecret)
	}

	resp, err := o.httpClient.Do(req) //nolint:gosec // trusted local operator API URL
	if err != nil {
		o.logger.Warn("apicp snapshot load failed", "error", err, "url", o.syncURL)
		return false
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusNotFound {
		return true
	}
	if resp.StatusCode >= 300 {
		body, _ := io.ReadAll(io.LimitReader(resp.Body, 4096))
		o.logger.Warn("apicp snapshot load rejected", "status", resp.StatusCode, "body", string(body))
		return false
	}

	var snapshot Snapshot
	if err := json.NewDecoder(resp.Body).Decode(&snapshot); err != nil {
		o.logger.Warn("apicp snapshot parse failed", "error", err)
		return false
	}
	o.restore(snapshot)
	o.logger.Info("apicp snapshot loaded", "patterns", len(snapshot.Patterns), "url", o.syncURL)
	return true
}

func (o *Observer) bootstrapLoadRetry() {
	for attempt := 1; attempt <= bootstrapRetryAttempts; attempt++ {
		timer := time.NewTimer(bootstrapRetryDelay)
		select {
		case <-o.stopCh:
			timer.Stop()
			return
		case <-timer.C:
		}

		if o.loadRemote() {
			return
		}

		o.logger.Info("apicp snapshot bootstrap retry scheduled",
			"attempt", attempt,
			"max_attempts", bootstrapRetryAttempts,
			"url", o.syncURL,
		)
	}
}

func (o *Observer) syncToRemote() {
	if o.syncURL == "" {
		return
	}

	snapshot := o.Snapshot()
	body, err := json.Marshal(snapshot)
	if err != nil {
		o.logger.Error("apicp snapshot marshal failed", "error", err)
		return
	}

	ctx, cancel := context.WithTimeout(context.Background(), defaultSyncTimeout)
	defer cancel()

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, o.syncURL, bytes.NewReader(body)) //nolint:gosec // trusted local operator API URL
	if err != nil {
		o.logger.Error("apicp snapshot store request build failed", "error", err)
		return
	}
	req.Header.Set("Content-Type", "application/json")
	if o.sharedSecret != "" {
		req.Header.Set(operatorKeyHeader, o.sharedSecret)
	}

	resp, err := o.httpClient.Do(req) //nolint:gosec // trusted local operator API URL
	if err != nil {
		o.logger.Warn("apicp snapshot store failed", "error", err, "url", o.syncURL)
		return
	}
	defer resp.Body.Close()
	if resp.StatusCode >= 300 {
		respBody, _ := io.ReadAll(io.LimitReader(resp.Body, 4096))
		o.logger.Warn("apicp snapshot store rejected", "status", resp.StatusCode, "body", string(respBody))
		return
	}
}

// Stop shuts down the sync goroutine and performs a final remote flush.
func (o *Observer) Stop() {
	o.stopOnce.Do(func() {
		close(o.stopCh)
		o.syncToRemote()
	})
}
