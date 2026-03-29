package synthesis

import (
	"fmt"
	"regexp"
	"strconv"
	"strings"
)

var dbPattern = regexp.MustCompile(`(?i)(\d+(?:[.,]\d+)?)\s*dB`)
var tempPattern = regexp.MustCompile(`(\d+(?:[.,]\d+)?)\s*°\s*C`)

// Context carries rule inputs that are not encoded directly in synth_fp.
type Context struct {
	AgentID     int
	IsAddressed bool
	NoiseHigh   bool
}

// PrepareInputs parses and enriches synthesis inputs from request metadata.
func PrepareInputs(metadata map[string]string) (Fingerprint, Context, error) {
	fpRaw := metadata["synth_fp"]
	if fpRaw == "" {
		return Fingerprint{}, Context{}, fmt.Errorf("empty fingerprint")
	}

	fp, err := Parse(fpRaw)
	if err != nil {
		return Fingerprint{}, Context{}, err
	}
	fp = EnrichFingerprint(fp, metadata)
	ctx := BuildContext(metadata)
	return fp, ctx, nil
}

// CanSynthesize applies the shared high-priority gates for deterministic and learned synthesis.
func CanSynthesize(fp Fingerprint, ctx Context) bool {
	return baseGate(fp, ctx)
}

// EnrichFingerprint supplements synth_fp fields with metadata that may be more
// current or that exists only as formatted text.
func EnrichFingerprint(fp Fingerprint, metadata map[string]string) Fingerprint {
	if strings.TrimSpace(metadata["heard"]) != "" {
		fp.HasHeard = true
	}
	if strings.TrimSpace(metadata["impulse"]) != "" {
		fp.HasImpulse = true
	}
	if personality := strings.TrimSpace(metadata["personality_type"]); personality != "" {
		fp.Personality = personality
	}
	if fp.PresenceCount == 0 {
		if n := parsePresenceCount(metadata["presence"]); n > 0 {
			fp.PresenceCount = n
		}
	}
	if fp.SimHour == 0 {
		if hour, ok := parseSimHour(metadata["circadian"]); ok {
			fp.SimHour = hour
		}
	}
	if !fp.TempHigh && isTempHigh(metadata["environment"]) {
		fp.TempHigh = true
	}

	return fp
}

// BuildContext derives non-fingerprint rule inputs from request metadata.
func BuildContext(metadata map[string]string) Context {
	agentID, _ := strconv.Atoi(strings.TrimSpace(metadata["agent_id"]))
	return Context{
		AgentID:     agentID,
		IsAddressed: strings.EqualFold(strings.TrimSpace(metadata["is_directly_addressed"]), "true"),
		NoiseHigh:   isNoiseHigh(metadata["acoustic"]),
	}
}

func parsePresenceCount(raw string) int {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return 0
	}
	parts := strings.Split(raw, ",")
	count := 0
	for _, part := range parts {
		if strings.TrimSpace(part) != "" {
			count++
		}
	}
	return count
}

func parseSimHour(raw string) (int, bool) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return 0, false
	}
	idx := strings.Index(raw, ":")
	if idx <= 0 {
		return 0, false
	}
	hour, err := strconv.Atoi(strings.TrimSpace(raw[:idx]))
	if err != nil || hour < 0 || hour > 23 {
		return 0, false
	}
	return hour, true
}

func isTempHigh(environment string) bool {
	environment = strings.TrimSpace(environment)
	if environment == "" {
		return false
	}
	if matches := tempPattern.FindStringSubmatch(environment); len(matches) > 1 {
		value := strings.ReplaceAll(matches[1], ",", ".")
		if temp, err := strconv.ParseFloat(value, 64); err == nil && temp > 26.0 {
			return true
		}
	}

	lower := strings.ToLower(environment)
	return strings.Contains(lower, "deutlich zu warm") ||
		strings.Contains(lower, "zu warm") ||
		strings.Contains(lower, "heiss")
}

func isNoiseHigh(acoustic string) bool {
	acoustic = strings.TrimSpace(acoustic)
	if acoustic == "" {
		return false
	}
	if matches := dbPattern.FindStringSubmatch(acoustic); len(matches) > 1 {
		value := strings.ReplaceAll(matches[1], ",", ".")
		if db, err := strconv.ParseFloat(value, 64); err == nil && db >= 70.0 {
			return true
		}
	}

	lower := strings.ToLower(acoustic)
	return strings.Contains(lower, "laut") || strings.Contains(lower, "laerm")
}
