// Package alerter emits judge alerts via Prometheus metrics, slog, and NATS.
package alerter

import (
	"encoding/json"
	"log/slog"

	"github.com/nats-io/nats.go"

	"github.com/obtFusi/project-sentinel/pkg/sentinel-go/messaging"
	"github.com/obtFusi/project-sentinel/services/sentinel-judge/internal/metrics"
)

// Alert represents a judge alert.
type Alert struct {
	AgentID  string  `json:"agent_id"`
	Type     string  `json:"type"`     // "drift", "quality", "fatigue", "swap"
	Severity string  `json:"severity"` // "mild", "moderate", "critical"
	Score    float64 `json:"score"`
	Details  string  `json:"details"`
}

// Alerter emits alerts to multiple targets.
type Alerter struct {
	nc     *nats.Conn // optional: nil disables NATS publishing
	logger *slog.Logger
}

// New creates an Alerter. nc may be nil to disable NATS publishing.
func New(nc *nats.Conn, logger *slog.Logger) *Alerter {
	return &Alerter{nc: nc, logger: logger}
}

// Emit sends an alert to Prometheus, slog, and NATS.
func (a *Alerter) Emit(alert Alert) {
	// Prometheus counter
	metrics.AlertsTotal.WithLabelValues(alert.AgentID, alert.Type, alert.Severity).Inc()

	// Structured log
	a.logger.Warn("judge alert",
		"agent", alert.AgentID,
		"type", alert.Type,
		"severity", alert.Severity,
		"score", alert.Score,
		"details", alert.Details,
	)

	// NATS publish (best-effort)
	if a.nc != nil {
		subject := messaging.BuildAlertSubject(alert.AgentID)
		data, err := json.Marshal(alert)
		if err != nil {
			a.logger.Error("alert marshal failed", "error", err)
			return
		}
		if err := a.nc.Publish(subject, data); err != nil {
			a.logger.Error("alert publish failed", "subject", subject, "error", err)
		}
	}
}
