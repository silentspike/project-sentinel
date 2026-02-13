package detection

import (
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
)

var (
	fourthWallDetected = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_fourth_wall_detected_total",
		Help: "Total fourth wall breaks detected",
	})
	fourthWallFalsePositive = promauto.NewCounter(prometheus.CounterOpts{
		Name: "sentinel_fourth_wall_false_positive_total",
		Help: "Regex matches overridden by LLM judge",
	})
	fourthWallRegenLatency = promauto.NewHistogram(prometheus.HistogramOpts{
		Name:    "sentinel_fourth_wall_regen_seconds",
		Help:    "Latency of re-generation after fourth wall break",
		Buckets: prometheus.DefBuckets,
	})
)

// RegenLatency returns the histogram for measuring re-generation latency.
// Exposed for use by the proxy package when timing re-generation.
func RegenLatency() prometheus.Histogram {
	return fourthWallRegenLatency
}
