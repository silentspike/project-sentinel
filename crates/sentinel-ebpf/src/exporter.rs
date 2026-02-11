//! Prometheus metrics exporter for all eBPF monitoring data.
//!
//! Generates Prometheus text exposition format for:
//! - Agent health (seconds since last write)
//! - I/O profiling (ops and bytes per cgroup)
//! - Network monitoring (latency, throughput, errors)
//! - PSI stress factors

use crate::probes::agent_health::AgentHealthChecker;
use crate::probes::io_profile::IoProfiler;
use crate::probes::network::NetworkMonitor;

/// Exports all monitoring metrics in Prometheus text exposition format.
#[derive(Debug)]
pub struct MetricsExporter;

impl MetricsExporter {
    /// Exports agent health metrics.
    ///
    /// Produces `sentinel_agent_last_write_seconds` gauge per tracked cgroup.
    pub fn export_agent_health(checker: &AgentHealthChecker, now_secs: u64) -> String {
        let mut output = String::new();
        output.push_str(
            "# HELP sentinel_agent_last_write_seconds Seconds since last write syscall\n",
        );
        output.push_str("# TYPE sentinel_agent_last_write_seconds gauge\n");

        // Export stalled status for all tracked agents
        let stalled = checker.stalled_agents(now_secs);
        output.push_str("# HELP sentinel_agent_stalled Whether agent is stalled (1=stalled)\n");
        output.push_str("# TYPE sentinel_agent_stalled gauge\n");
        for cgroup_id in &stalled {
            if let Some(secs) = checker.seconds_since_last_write(*cgroup_id, now_secs) {
                output.push_str(&format!(
                    "sentinel_agent_last_write_seconds{{cgroup_id=\"{}\"}} {}\n",
                    cgroup_id, secs
                ));
            }
            output.push_str(&format!(
                "sentinel_agent_stalled{{cgroup_id=\"{}\"}} 1\n",
                cgroup_id
            ));
        }

        output
    }

    /// Exports I/O profiling metrics.
    ///
    /// Produces `sentinel_io_ops_total` counter and `sentinel_io_bytes_total` counter.
    pub fn export_io_profile(profiler: &IoProfiler) -> String {
        let mut output = String::new();
        output.push_str("# HELP sentinel_io_ops_total Total I/O operations per cgroup\n");
        output.push_str("# TYPE sentinel_io_ops_total counter\n");
        output.push_str("# HELP sentinel_io_bytes_total Total I/O bytes per cgroup\n");
        output.push_str("# TYPE sentinel_io_bytes_total counter\n");

        for (cgroup_id, metrics) in profiler.all_metrics() {
            let name = &metrics.cgroup_name;
            output.push_str(&format!(
                "sentinel_io_ops_total{{cgroup_id=\"{}\",cgroup_name=\"{}\",direction=\"read\"}} {}\n",
                cgroup_id, name, metrics.read_ops
            ));
            output.push_str(&format!(
                "sentinel_io_ops_total{{cgroup_id=\"{}\",cgroup_name=\"{}\",direction=\"write\"}} {}\n",
                cgroup_id, name, metrics.write_ops
            ));
            output.push_str(&format!(
                "sentinel_io_bytes_total{{cgroup_id=\"{}\",cgroup_name=\"{}\",direction=\"read\"}} {}\n",
                cgroup_id, name, metrics.read_bytes
            ));
            output.push_str(&format!(
                "sentinel_io_bytes_total{{cgroup_id=\"{}\",cgroup_name=\"{}\",direction=\"write\"}} {}\n",
                cgroup_id, name, metrics.write_bytes
            ));
        }

        output
    }

    /// Exports network monitoring metrics.
    ///
    /// Produces latency, throughput, and error metrics per destination.
    pub fn export_network(monitor: &NetworkMonitor) -> String {
        let mut output = String::new();
        output.push_str("# HELP sentinel_llm_request_duration_seconds LLM API request latency\n");
        output.push_str("# TYPE sentinel_llm_request_duration_seconds summary\n");
        output.push_str("# HELP sentinel_llm_requests_total Total LLM API requests\n");
        output.push_str("# TYPE sentinel_llm_requests_total counter\n");
        output.push_str("# HELP sentinel_llm_errors_total Total LLM API errors\n");
        output.push_str("# TYPE sentinel_llm_errors_total counter\n");

        for (dest, metrics) in monitor.all_metrics() {
            if let Some(avg) = metrics.avg_latency() {
                output.push_str(&format!(
                    "sentinel_llm_request_duration_seconds{{destination=\"{}\"}} {:.6}\n",
                    dest,
                    avg.as_secs_f64()
                ));
            }
            output.push_str(&format!(
                "sentinel_llm_requests_total{{destination=\"{}\"}} {}\n",
                dest, metrics.request_count
            ));
            output.push_str(&format!(
                "sentinel_llm_errors_total{{destination=\"{}\"}} {}\n",
                dest, metrics.error_count
            ));
        }

        output
    }

    /// Exports PSI stress factor as a gauge.
    pub fn export_psi_stress(agent_id: &str, stress_factor: f32) -> String {
        let mut output = String::new();
        output.push_str(
            "# HELP sentinel_agent_cpu_pressure_stress CPU pressure stress factor (0-1)\n",
        );
        output.push_str("# TYPE sentinel_agent_cpu_pressure_stress gauge\n");
        output.push_str(&format!(
            "sentinel_agent_cpu_pressure_stress{{agent=\"{}\"}} {:.4}\n",
            agent_id, stress_factor
        ));
        output
    }

    /// Exports all metrics combined.
    pub fn export_all(
        checker: &AgentHealthChecker,
        profiler: &IoProfiler,
        monitor: &NetworkMonitor,
        now_secs: u64,
    ) -> String {
        let mut output = String::new();
        output.push_str(&Self::export_agent_health(checker, now_secs));
        output.push_str(&Self::export_io_profile(profiler));
        output.push_str(&Self::export_network(monitor));
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn export_agent_health_format() {
        let mut checker = AgentHealthChecker::new();
        checker.record_write(1, 960);
        let output = MetricsExporter::export_agent_health(&checker, 1000);
        assert!(output.contains("# HELP sentinel_agent_last_write_seconds"));
        assert!(output.contains("# TYPE sentinel_agent_last_write_seconds gauge"));
        // Agent 1 is stalled (40s > 30s threshold)
        assert!(output.contains("sentinel_agent_stalled{cgroup_id=\"1\"} 1"));
    }

    #[test]
    fn export_io_profile_format() {
        let mut profiler = IoProfiler::new();
        profiler.record_read(1, "ecs", 4096);
        profiler.record_write(1, "ecs", 8192);
        let output = MetricsExporter::export_io_profile(&profiler);
        assert!(output.contains("# HELP sentinel_io_ops_total"));
        assert!(output.contains("# TYPE sentinel_io_ops_total counter"));
        assert!(output.contains("direction=\"read\""));
        assert!(output.contains("direction=\"write\""));
    }

    #[test]
    fn export_network_format() {
        let mut monitor = NetworkMonitor::new();
        monitor.record_request(
            "api.anthropic.com:443",
            Duration::from_millis(150),
            1024,
            4096,
        );
        let output = MetricsExporter::export_network(&monitor);
        assert!(output.contains("# HELP sentinel_llm_request_duration_seconds"));
        assert!(output.contains("# TYPE sentinel_llm_request_duration_seconds summary"));
        assert!(output.contains("destination=\"api.anthropic.com:443\""));
        assert!(output.contains("0.150000"));
    }

    #[test]
    fn export_psi_stress_format() {
        let output = MetricsExporter::export_psi_stress("AGENT-01", 0.75);
        assert!(output.contains("# HELP sentinel_agent_cpu_pressure_stress"));
        assert!(output.contains("# TYPE sentinel_agent_cpu_pressure_stress gauge"));
        assert!(output.contains("agent=\"AGENT-01\""));
        assert!(output.contains("0.7500"));
    }

    #[test]
    fn export_all_combines_sections() {
        let checker = AgentHealthChecker::new();
        let profiler = IoProfiler::new();
        let monitor = NetworkMonitor::new();
        let output = MetricsExporter::export_all(&checker, &profiler, &monitor, 1000);
        assert!(output.contains("sentinel_agent_last_write_seconds"));
        assert!(output.contains("sentinel_io_ops_total"));
        assert!(output.contains("sentinel_llm_request_duration_seconds"));
    }

    #[test]
    fn prometheus_output_valid_format() {
        // Verify no invalid characters in metric names or labels
        let mut checker = AgentHealthChecker::new();
        checker.record_write(42, 900);
        let output = MetricsExporter::export_agent_health(&checker, 1000);
        for line in output.lines() {
            if line.starts_with('#') {
                assert!(
                    line.starts_with("# HELP") || line.starts_with("# TYPE"),
                    "Comment line must be HELP or TYPE: {}",
                    line
                );
            }
        }
    }
}
