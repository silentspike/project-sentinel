package compiler

import (
	"fmt"
	"os"
	"path/filepath"
	"sync"

	"github.com/BurntSushi/toml"
)

// AgentIdentity holds the identity section from agent TOML.
type AgentIdentity struct {
	ID            int      `toml:"id"`
	Name          string   `toml:"name"`
	Role          string   `toml:"role"`
	Department    string   `toml:"department"`
	ShiftSet      int      `toml:"shift_set"`
	KPIs          []string `toml:"kpis"`
	ReportsTo     string   `toml:"reports_to"`
	DirectReports []string `toml:"direct_reports"`
}

// AgentPersonality holds Big Five and related traits.
type AgentPersonality struct {
	Openness          float64 `toml:"openness"`
	Conscientiousness float64 `toml:"conscientiousness"`
	Extraversion      float64 `toml:"extraversion"`
	Agreeableness     float64 `toml:"agreeableness"`
	Neuroticism       float64 `toml:"neuroticism"`
	CaffeineTolerance float64 `toml:"caffeine_tolerance"`
	MorningPerson     bool    `toml:"morning_person"`
}

// AgentBackground holds bio and quirks.
type AgentBackground struct {
	Bio    string   `toml:"bio"`
	Quirks []string `toml:"quirks"`
}

// AgentPreferences holds agent preferences.
type AgentPreferences struct {
	FavoriteRoom     string `toml:"favorite_room"`
	CoffeePreference string `toml:"coffee_preference"`
	LunchTime        string `toml:"lunch_time"`
}

// AgentDNA is the full parsed agent TOML definition.
type AgentDNA struct {
	Identity    AgentIdentity    `toml:"identity"`
	Personality AgentPersonality `toml:"personality"`
	Background  AgentBackground  `toml:"background"`
	Preferences AgentPreferences `toml:"preferences"`
}

// TOMLLoader reads and caches agent TOML definitions from disk.
type TOMLLoader struct {
	mu       sync.RWMutex
	cache    map[int]*AgentDNA
	agentDir string
}

// NewTOMLLoader creates a loader that reads from agentDir.
func NewTOMLLoader(agentDir string) *TOMLLoader {
	return &TOMLLoader{
		cache:    make(map[int]*AgentDNA),
		agentDir: agentDir,
	}
}

// Load returns the AgentDNA for the given agent ID, using cache when available.
func (l *TOMLLoader) Load(agentID int) (*AgentDNA, error) {
	l.mu.RLock()
	if dna, ok := l.cache[agentID]; ok {
		l.mu.RUnlock()
		return dna, nil
	}
	l.mu.RUnlock()

	l.mu.Lock()
	defer l.mu.Unlock()

	// Double-check after acquiring write lock
	if dna, ok := l.cache[agentID]; ok {
		return dna, nil
	}

	dna, err := l.loadFromDisk(agentID)
	if err != nil {
		return nil, err
	}
	l.cache[agentID] = dna
	return dna, nil
}

// Invalidate removes a cached entry, forcing re-read on next Load.
func (l *TOMLLoader) Invalidate(agentID int) {
	l.mu.Lock()
	defer l.mu.Unlock()
	delete(l.cache, agentID)
}

// InvalidateAll clears the entire cache.
func (l *TOMLLoader) InvalidateAll() {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.cache = make(map[int]*AgentDNA)
}

func (l *TOMLLoader) loadFromDisk(agentID int) (*AgentDNA, error) {
	pattern := filepath.Join(l.agentDir, fmt.Sprintf("AGENT-%02d-*.toml", agentID))
	matches, err := filepath.Glob(pattern)
	if err != nil {
		return nil, fmt.Errorf("glob agent TOML: %w", err)
	}
	if len(matches) == 0 {
		return nil, fmt.Errorf("no TOML file found for agent %d (pattern: %s)", agentID, pattern)
	}

	data, err := os.ReadFile(matches[0])
	if err != nil {
		return nil, fmt.Errorf("read agent TOML %s: %w", matches[0], err)
	}

	var dna AgentDNA
	if err := toml.Unmarshal(data, &dna); err != nil {
		return nil, fmt.Errorf("parse agent TOML %s: %w", matches[0], err)
	}
	return &dna, nil
}
