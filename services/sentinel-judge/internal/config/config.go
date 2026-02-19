// Package config loads and validates the sentinel-judge TOML configuration.
package config

import (
	"fmt"

	"github.com/BurntSushi/toml"
)

// Config represents the full judge service configuration.
type Config struct {
	Server     ServerConfig     `toml:"server"`
	NATS       NATSConfig       `toml:"nats"`
	Thresholds ThresholdConfig  `toml:"thresholds"`
	Evolution  EvolutionConfig  `toml:"evolution"`
	Gateway    GatewayConfig    `toml:"gateway"`
}

type ServerConfig struct {
	Port int `toml:"port"`
}

type NATSConfig struct {
	URL          string `toml:"url"`
	ConsumerName string `toml:"consumer_name"`
}

type ThresholdConfig struct {
	DriftAlertSeverity   string  `toml:"drift_alert_severity"`
	QualityAlertMinScore int     `toml:"quality_alert_min_score"`
	FatigueAlertMinScore float64 `toml:"fatigue_alert_min_score"`
}

type EvolutionConfig struct {
	Path string `toml:"path"`
}

type GatewayConfig struct {
	URL            string  `toml:"url"`
	Model          string  `toml:"model"`
	Temperature    float64 `toml:"temperature"`
	MaxTokens      int     `toml:"max_tokens"`
	TimeoutSeconds int     `toml:"timeout_seconds"`
}

// Load reads and validates a judge config from a TOML file.
func Load(path string) (*Config, error) {
	var cfg Config
	if _, err := toml.DecodeFile(path, &cfg); err != nil {
		return nil, fmt.Errorf("config load: %w", err)
	}
	if cfg.Server.Port <= 0 {
		cfg.Server.Port = 8082
	}
	if cfg.NATS.URL == "" {
		cfg.NATS.URL = "nats://127.0.0.1:4222"
	}
	if cfg.NATS.ConsumerName == "" {
		cfg.NATS.ConsumerName = "judge-heuristic"
	}
	if cfg.Thresholds.DriftAlertSeverity == "" {
		cfg.Thresholds.DriftAlertSeverity = "moderate"
	}
	if cfg.Thresholds.QualityAlertMinScore <= 0 {
		cfg.Thresholds.QualityAlertMinScore = 2
	}
	if cfg.Thresholds.FatigueAlertMinScore <= 0 {
		cfg.Thresholds.FatigueAlertMinScore = 0.6
	}
	if cfg.Gateway.URL == "" {
		cfg.Gateway.URL = "http://localhost:8080"
	}
	if cfg.Gateway.Temperature <= 0 {
		cfg.Gateway.Temperature = 0.2
	}
	if cfg.Gateway.MaxTokens <= 0 {
		cfg.Gateway.MaxTokens = 500
	}
	if cfg.Gateway.TimeoutSeconds <= 0 {
		cfg.Gateway.TimeoutSeconds = 30
	}
	return &cfg, nil
}
