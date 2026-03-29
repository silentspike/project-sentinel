package control

import (
	"os"

	"github.com/BurntSushi/toml"
)

const (
	defaultDaemonConfigPath      = "config/daemon.toml"
	defaultTickSyncTimeoutMs     = 2000
	defaultP3TimeoutMs           = 5000
	defaultMaxForwardConcurrency = 3
	defaultInterceptMode         = "auto"
)

type daemonTrafficControlFile struct {
	Daemon struct {
		TrafficControl daemonTrafficControlDefaults `toml:"traffic_control"`
	} `toml:"daemon"`
}

type daemonTrafficControlDefaults struct {
	SynthesisEnabled      bool   `toml:"synthesis_enabled"`
	SequencingEnabled     bool   `toml:"sequencing_enabled"`
	TickSyncEnabled       bool   `toml:"tick_sync_enabled"`
	APICPEnabled          bool   `toml:"apicp_enabled"`
	TickSyncTimeoutMs     int    `toml:"tick_sync_timeout_ms"`
	P3TimeoutMs           int    `toml:"p3_timeout_ms"`
	MaxForwardConcurrency int    `toml:"max_forward_concurrency"`
	InterceptMode         string `toml:"intercept_mode"`
}

func DefaultTrafficControlUpdates() map[string]interface{} {
	return map[string]interface{}{
		"synthesis_enabled":       false,
		"sequencing_enabled":      false,
		"tick_sync_enabled":       false,
		"apicp_enabled":           false,
		"tick_sync_timeout_ms":    defaultTickSyncTimeoutMs,
		"p3_timeout_ms":           defaultP3TimeoutMs,
		"max_forward_concurrency": defaultMaxForwardConcurrency,
		"intercept_mode":          defaultInterceptMode,
	}
}

func DaemonConfigPath() string {
	if path := os.Getenv("SENTINEL_DAEMON_CONFIG"); path != "" {
		return path
	}
	return defaultDaemonConfigPath
}

// LoadTrafficControlDefaults loads daemon traffic control bootstrap values
// from daemon.toml. Missing files simply return hardcoded defaults.
func LoadTrafficControlDefaults(path string) (map[string]interface{}, error) {
	if path == "" {
		path = defaultDaemonConfigPath
	}

	cfg := daemonTrafficControlFile{}
	cfg.Daemon.TrafficControl = daemonTrafficControlDefaults{
		TickSyncTimeoutMs:     defaultTickSyncTimeoutMs,
		P3TimeoutMs:           defaultP3TimeoutMs,
		MaxForwardConcurrency: defaultMaxForwardConcurrency,
		InterceptMode:         defaultInterceptMode,
	}

	if _, err := os.Stat(path); err != nil {
		if os.IsNotExist(err) {
			return DefaultTrafficControlUpdates(), nil
		}
		return nil, err
	}

	if _, err := toml.DecodeFile(path, &cfg); err != nil {
		return nil, err
	}

	return map[string]interface{}{
		"synthesis_enabled":       cfg.Daemon.TrafficControl.SynthesisEnabled,
		"sequencing_enabled":      cfg.Daemon.TrafficControl.SequencingEnabled,
		"tick_sync_enabled":       cfg.Daemon.TrafficControl.TickSyncEnabled,
		"apicp_enabled":           cfg.Daemon.TrafficControl.APICPEnabled,
		"tick_sync_timeout_ms":    cfg.Daemon.TrafficControl.TickSyncTimeoutMs,
		"p3_timeout_ms":           cfg.Daemon.TrafficControl.P3TimeoutMs,
		"max_forward_concurrency": cfg.Daemon.TrafficControl.MaxForwardConcurrency,
		"intercept_mode":          cfg.Daemon.TrafficControl.InterceptMode,
	}, nil
}
