package judge

import (
	"strings"
)

// DriftResult holds the drift analysis for an agent.
type DriftResult struct {
	AgentName  string
	DriftScore float64 // 0.0 = perfect in-character, 1.0 = completely out of character
	Severity   string  // "none", "mild", "moderate", "critical"
	Details    string
}

// DriftDetector checks if agents deviate from their personality profiles.
type DriftDetector struct {
	// Map of agent name to personality traits
	Profiles map[string]PersonalityProfile
}

type PersonalityProfile struct {
	Role         string
	Extraversion float64 // 0.0-1.0
	Neuroticism  float64 // 0.0-1.0
	KeyTraits    []string
}

func NewDriftDetector() *DriftDetector {
	return &DriftDetector{
		Profiles: make(map[string]PersonalityProfile),
	}
}

// Reset clears all accumulated state (called after snapshot restore).
// Profiles are kept — only runtime state is reset.
func (d *DriftDetector) Reset() {
	// DriftDetector is stateless between calls (no message history stored).
	// Reset is a no-op but provided for forward compatibility.
}

// CheckDrift compares recent messages against the agent's personality profile.
// Returns DriftResult with score and severity.
func (d *DriftDetector) CheckDrift(agentName string, recentMessages []string) DriftResult {
	profile, exists := d.Profiles[agentName]
	if !exists {
		return DriftResult{
			AgentName:  agentName,
			DriftScore: 0.0,
			Severity:   "none",
			Details:    "unknown agent",
		}
	}

	if len(recentMessages) == 0 {
		return DriftResult{
			AgentName:  agentName,
			DriftScore: 0.0,
			Severity:   "none",
			Details:    "no messages to analyze",
		}
	}

	// Analyze exclamation marks (extroversion signal)
	totalExclamations := 0
	totalLength := 0
	for _, msg := range recentMessages {
		totalExclamations += strings.Count(msg, "!")
		totalLength += len(msg)
	}

	avgExclamationsPerMsg := float64(totalExclamations) / float64(len(recentMessages))
	avgLength := float64(totalLength) / float64(len(recentMessages))

	// Expected behavior based on extraversion
	expectedExclamations := profile.Extraversion * 3.0 // extroverted agents use more !
	expectedVerbosity := profile.Extraversion * 200.0  // extroverted agents write more

	// Calculate drift signals
	exclamationDrift := 0.0
	if expectedExclamations > 1.0 {
		exclamationDrift = abs(avgExclamationsPerMsg-expectedExclamations) / expectedExclamations
	} else {
		// For introverts, any exclamations are drift
		exclamationDrift = avgExclamationsPerMsg / 3.0
	}

	verbosityDrift := 0.0
	if expectedVerbosity > 50.0 {
		verbosityDrift = abs(avgLength-expectedVerbosity) / expectedVerbosity
	} else {
		// For introverts, verbosity is drift
		verbosityDrift = avgLength / 200.0
	}

	// Combine drift signals
	driftScore := (exclamationDrift + verbosityDrift) / 2.0
	if driftScore > 1.0 {
		driftScore = 1.0
	}

	// Determine severity
	severity := "none"
	if driftScore >= 0.7 {
		severity = "critical"
	} else if driftScore >= 0.5 {
		severity = "moderate"
	} else if driftScore >= 0.2 {
		severity = "mild"
	}

	details := "drift detected in communication patterns"
	if driftScore < 0.2 {
		details = "behavior consistent with profile"
	}

	return DriftResult{
		AgentName:  agentName,
		DriftScore: driftScore,
		Severity:   severity,
		Details:    details,
	}
}

// RegisterProfile adds or updates an agent's personality profile.
func (d *DriftDetector) RegisterProfile(name string, profile PersonalityProfile) {
	d.Profiles[name] = profile
}

func abs(x float64) float64 {
	if x < 0 {
		return -x
	}
	return x
}
