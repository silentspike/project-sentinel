package observatory

import (
	"fmt"
	"os"
	"strings"

	"github.com/BurntSushi/toml"
)

// requiredShifts is the number of shifts the observatory requires.
const requiredShifts = 3

// envObservatory is the environment variable that overrides the config enabled flag.
const envObservatory = "SENTINEL_OBSERVATORY"

// ObservatoryConfig is the top-level configuration for the MARBLE observatory.
type ObservatoryConfig struct {
	Observatory observatorySection `toml:"observatory"`
}

// observatorySection holds the observatory-specific settings.
type observatorySection struct {
	Enabled   bool           `toml:"enabled"`
	Shift1    ShiftConfig    `toml:"shift_1"`
	Shift2    ShiftConfig    `toml:"shift_2"`
	Shift3    ShiftConfig    `toml:"shift_3"`
	Scenarios ScenarioConfig `toml:"scenarios"`
}

// ShiftConfig describes one provider shift in the observatory.
type ShiftConfig struct {
	Model    string `toml:"model"`
	Provider string `toml:"provider"`
	Agents   int    `toml:"agents"`
}

// ScenarioConfig lists the scenarios to run in each shift.
type ScenarioConfig struct {
	DailyRoutine       bool `toml:"daily_routine"`
	CrisisResponse     bool `toml:"crisis_response"`
	CreativeTask       bool `toml:"creative_task"`
	ConflictResolution bool `toml:"conflict_resolution"`
}

// LoadConfig reads and validates an observatory TOML configuration from path.
func LoadConfig(path string) (*ObservatoryConfig, error) {
	var cfg ObservatoryConfig
	if _, err := toml.DecodeFile(path, &cfg); err != nil {
		return nil, fmt.Errorf("decode observatory config: %w", err)
	}
	if err := cfg.validate(); err != nil {
		return nil, fmt.Errorf("validate observatory config: %w", err)
	}
	return &cfg, nil
}

// IsEnabled returns true when the observatory is active. The environment
// variable SENTINEL_OBSERVATORY=true overrides the config file value.
func (c *ObservatoryConfig) IsEnabled() bool {
	if v := os.Getenv(envObservatory); v != "" {
		return strings.EqualFold(v, "true") || v == "1"
	}
	return c.Observatory.Enabled
}

// Shifts returns the three shift configurations in order.
func (c *ObservatoryConfig) Shifts() [requiredShifts]ShiftConfig {
	return [requiredShifts]ShiftConfig{
		c.Observatory.Shift1,
		c.Observatory.Shift2,
		c.Observatory.Shift3,
	}
}

// validate checks that the configuration satisfies all invariants.
func (c *ObservatoryConfig) validate() error {
	shifts := c.Shifts()
	for i, s := range shifts {
		if s.Model == "" {
			return fmt.Errorf("shift_%d: model must not be empty", i+1)
		}
		if s.Provider == "" {
			return fmt.Errorf("shift_%d: provider must not be empty", i+1)
		}
		if s.Agents <= 0 {
			return fmt.Errorf("shift_%d: agents must be > 0, got %d", i+1, s.Agents)
		}
	}
	return nil
}
