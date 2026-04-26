package service

import (
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"regexp"
	"strings"

	"github.com/BurntSushi/toml"

	"github.com/silentspike/project-sentinel/pkg/sentinel-go/judge"
)

// agentTOML represents the structure of an AGENT-XX-NAME.toml file.
type agentTOML struct {
	Identity    agentIdentity    `toml:"identity"`
	Personality agentPersonality `toml:"personality"`
}

type agentIdentity struct {
	ID         int    `toml:"id"`
	Name       string `toml:"name"`
	Role       string `toml:"role"`
	Department string `toml:"department"`
	ShiftSet   int    `toml:"shift_set"`
}

type agentPersonality struct {
	Openness            float64 `toml:"openness"`
	Conscientiousness   float64 `toml:"conscientiousness"`
	Extraversion        float64 `toml:"extraversion"`
	Agreeableness       float64 `toml:"agreeableness"`
	Neuroticism         float64 `toml:"neuroticism"`
	CaffeineTolerance   float64 `toml:"caffeine_tolerance"`
	MorningPerson       bool    `toml:"morning_person"`
}

// agentFilePattern matches AGENT-XX filenames to extract the numeric ID.
var agentFilePattern = regexp.MustCompile(`^AGENT-(\d+)`)

// LoadProfiles reads all AGENT-*.toml files from configDir and registers them
// with the DriftDetector. Returns the number of profiles loaded.
func LoadProfiles(configDir string, drift *judge.DriftDetector, logger *slog.Logger) (int, error) {
	entries, err := os.ReadDir(configDir)
	if err != nil {
		return 0, fmt.Errorf("read agent config dir %q: %w", configDir, err)
	}

	loaded := 0
	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		name := entry.Name()
		if !strings.HasPrefix(name, "AGENT-") || !strings.HasSuffix(name, ".toml") {
			continue
		}

		path := filepath.Join(configDir, name)
		var agent agentTOML
		if _, err := toml.DecodeFile(path, &agent); err != nil {
			logger.Warn("failed to parse agent config", "file", name, "error", err)
			continue
		}

		// Extract AGENT-XX from filename for consistent ID format
		matches := agentFilePattern.FindStringSubmatch(name)
		if len(matches) < 2 {
			logger.Warn("agent file has unexpected name format", "file", name)
			continue
		}

		// Agent ID as used in NATS subjects: AGENT-01, AGENT-02, etc.
		agentID := fmt.Sprintf("AGENT-%02d", agent.Identity.ID)

		drift.RegisterProfile(agentID, judge.PersonalityProfile{
			Role:         agent.Identity.Role,
			Extraversion: agent.Personality.Extraversion,
			Neuroticism:  agent.Personality.Neuroticism,
			KeyTraits:    nil, // Populated by LLM analysis later
		})

		loaded++
		logger.Debug("loaded agent profile",
			"agent_id", agentID,
			"name", agent.Identity.Name,
			"extraversion", agent.Personality.Extraversion,
			"neuroticism", agent.Personality.Neuroticism,
		)
	}

	return loaded, nil
}
