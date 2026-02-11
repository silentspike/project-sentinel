//! Network monitoring for LLM API latency and throughput.
//!
//! Tracks TCP connection metrics to known LLM API endpoints.

use std::collections::HashMap;
use std::time::Duration;

/// Known LLM API ports for filtering network events.
const LLM_API_PORTS: &[u16] = &[
    443,   // HTTPS (Anthropic, OpenAI, etc.)
    11434, // Ollama local
];

/// Network metrics for a single destination.
#[derive(Debug, Clone)]
pub struct NetworkMetrics {
    /// Destination address (ip:port).
    pub destination: String,
    /// Total number of completed requests.
    pub request_count: u64,
    /// Total bytes sent.
    pub bytes_sent: u64,
    /// Total bytes received.
    pub bytes_received: u64,
    /// Sum of all latencies (for average calculation).
    latency_sum_us: u64,
    /// Number of latency samples.
    latency_count: u64,
    /// Minimum observed latency.
    min_latency_us: u64,
    /// Maximum observed latency.
    max_latency_us: u64,
    /// Number of failed connections.
    pub error_count: u64,
}

impl NetworkMetrics {
    fn new(destination: String) -> Self {
        Self {
            destination,
            request_count: 0,
            bytes_sent: 0,
            bytes_received: 0,
            latency_sum_us: 0,
            latency_count: 0,
            min_latency_us: u64::MAX,
            max_latency_us: 0,
            error_count: 0,
        }
    }

    /// Average latency as Duration, or None if no samples.
    pub fn avg_latency(&self) -> Option<Duration> {
        if self.latency_count == 0 {
            None
        } else {
            Some(Duration::from_micros(
                self.latency_sum_us / self.latency_count,
            ))
        }
    }

    /// Minimum observed latency, or None if no samples.
    pub fn min_latency(&self) -> Option<Duration> {
        if self.latency_count == 0 {
            None
        } else {
            Some(Duration::from_micros(self.min_latency_us))
        }
    }

    /// Maximum observed latency, or None if no samples.
    pub fn max_latency(&self) -> Option<Duration> {
        if self.latency_count == 0 {
            None
        } else {
            Some(Duration::from_micros(self.max_latency_us))
        }
    }

    /// Error rate as fraction (0.0 to 1.0).
    pub fn error_rate(&self) -> f64 {
        let total = self.request_count + self.error_count;
        if total == 0 {
            0.0
        } else {
            self.error_count as f64 / total as f64
        }
    }
}

/// Tracks network metrics per destination.
#[derive(Debug, Default)]
pub struct NetworkMonitor {
    /// Maps destination string -> metrics.
    metrics: HashMap<String, NetworkMetrics>,
}

impl NetworkMonitor {
    /// Creates a new network monitor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks if a port is a known LLM API port.
    pub fn is_llm_port(port: u16) -> bool {
        LLM_API_PORTS.contains(&port)
    }

    /// Records a completed request with latency.
    pub fn record_request(
        &mut self,
        destination: &str,
        latency: Duration,
        bytes_sent: u64,
        bytes_received: u64,
    ) {
        let entry = self
            .metrics
            .entry(destination.to_string())
            .or_insert_with(|| NetworkMetrics::new(destination.to_string()));

        let latency_us = latency.as_micros() as u64;
        entry.request_count += 1;
        entry.bytes_sent += bytes_sent;
        entry.bytes_received += bytes_received;
        entry.latency_sum_us += latency_us;
        entry.latency_count += 1;
        entry.min_latency_us = entry.min_latency_us.min(latency_us);
        entry.max_latency_us = entry.max_latency_us.max(latency_us);
    }

    /// Records a connection error.
    pub fn record_error(&mut self, destination: &str) {
        let entry = self
            .metrics
            .entry(destination.to_string())
            .or_insert_with(|| NetworkMetrics::new(destination.to_string()));
        entry.error_count += 1;
    }

    /// Returns metrics for a specific destination.
    pub fn get_metrics(&self, destination: &str) -> Option<&NetworkMetrics> {
        self.metrics.get(destination)
    }

    /// Returns all tracked destinations.
    pub fn all_metrics(&self) -> &HashMap<String, NetworkMetrics> {
        &self.metrics
    }

    /// Resets all counters.
    pub fn reset(&mut self) {
        self.metrics.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_llm_port_detects_https() {
        assert!(NetworkMonitor::is_llm_port(443));
        assert!(NetworkMonitor::is_llm_port(11434));
        assert!(!NetworkMonitor::is_llm_port(80));
        assert!(!NetworkMonitor::is_llm_port(8080));
    }

    #[test]
    fn record_request_tracks_metrics() {
        let mut monitor = NetworkMonitor::new();
        monitor.record_request("api.anthropic.com:443", Duration::from_millis(150), 1024, 4096);
        let m = monitor.get_metrics("api.anthropic.com:443").unwrap();
        assert_eq!(m.request_count, 1);
        assert_eq!(m.bytes_sent, 1024);
        assert_eq!(m.bytes_received, 4096);
        assert_eq!(m.avg_latency(), Some(Duration::from_millis(150)));
    }

    #[test]
    fn latency_statistics() {
        let mut monitor = NetworkMonitor::new();
        let dest = "api.anthropic.com:443";
        monitor.record_request(dest, Duration::from_millis(100), 0, 0);
        monitor.record_request(dest, Duration::from_millis(200), 0, 0);
        monitor.record_request(dest, Duration::from_millis(300), 0, 0);

        let m = monitor.get_metrics(dest).unwrap();
        assert_eq!(m.avg_latency(), Some(Duration::from_millis(200)));
        assert_eq!(m.min_latency(), Some(Duration::from_millis(100)));
        assert_eq!(m.max_latency(), Some(Duration::from_millis(300)));
    }

    #[test]
    fn error_rate_calculation() {
        let mut monitor = NetworkMonitor::new();
        let dest = "api.anthropic.com:443";
        monitor.record_request(dest, Duration::from_millis(100), 0, 0);
        monitor.record_request(dest, Duration::from_millis(100), 0, 0);
        monitor.record_error(dest);

        let m = monitor.get_metrics(dest).unwrap();
        assert!((m.error_rate() - 1.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn no_samples_returns_none() {
        let m = NetworkMetrics::new("test".to_string());
        assert_eq!(m.avg_latency(), None);
        assert_eq!(m.min_latency(), None);
        assert_eq!(m.max_latency(), None);
        assert!((m.error_rate() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn reset_clears_all() {
        let mut monitor = NetworkMonitor::new();
        monitor.record_request("test:443", Duration::from_millis(100), 0, 0);
        monitor.reset();
        assert!(monitor.all_metrics().is_empty());
    }
}
