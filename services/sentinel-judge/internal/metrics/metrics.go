// Package metrics provides Prometheus metric definitions for the sentinel-judge service.
package metrics

import (
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
)

var (
	DriftScore = promauto.NewGaugeVec(prometheus.GaugeOpts{
		Name: "judge_drift_score",
		Help: "Current drift score per agent",
	}, []string{"agent"})

	QualityScore = promauto.NewGaugeVec(prometheus.GaugeOpts{
		Name: "judge_quality_score",
		Help: "Current quality score per agent",
	}, []string{"agent"})

	FatigueScore = promauto.NewGaugeVec(prometheus.GaugeOpts{
		Name: "judge_fatigue_score",
		Help: "Current fatigue score per agent",
	}, []string{"agent"})

	AlertsTotal = promauto.NewCounterVec(prometheus.CounterOpts{
		Name: "judge_alerts_total",
		Help: "Total judge alerts by agent, type, and severity",
	}, []string{"agent", "type", "severity"})

	EventsProcessed = promauto.NewCounter(prometheus.CounterOpts{
		Name: "judge_events_processed_total",
		Help: "Total events processed from NATS",
	})

	ConsumerLag = promauto.NewGauge(prometheus.GaugeOpts{
		Name: "judge_nats_consumer_lag",
		Help: "Pending messages in NATS consumer",
	})

	LLMAnalysisDuration = promauto.NewHistogramVec(prometheus.HistogramOpts{
		Name:    "judge_llm_analysis_duration_seconds",
		Help:    "LLM analysis duration by type",
		Buckets: []float64{0.5, 1, 2, 5, 10, 30, 60},
	}, []string{"type"})

	// eBPF metrics (ADR-001: Judge consumes daemon-bridged eBPF data via NATS)
	EBPFStallCount = promauto.NewGaugeVec(prometheus.GaugeOpts{
		Name: "judge_ebpf_stall_count",
		Help: "Number of stalled agents from eBPF agent-health data",
	}, []string{"agent"})

	EBPFIOBytes = promauto.NewGaugeVec(prometheus.GaugeOpts{
		Name: "judge_ebpf_io_bytes",
		Help: "I/O bytes from eBPF profiling",
	}, []string{"direction"})

	EBPFEventsProcessed = promauto.NewCounter(prometheus.CounterOpts{
		Name: "judge_ebpf_events_processed_total",
		Help: "Total eBPF metric events processed from NATS",
	})
)
