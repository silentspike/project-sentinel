//! Prometheus metrics exporter for all eBPF monitoring data.
//!
//! Generates Prometheus text exposition format for:
//! - Agent health (seconds since last write)
//! - I/O profiling (ops and bytes per cgroup)
//! - Network monitoring (latency, throughput, errors)
//! - PSI stress factors
//! - Collector meta-metrics (cycle time, mode, drops)

use std::borrow::Cow;

use crate::collector::MetricsSnapshot;
use crate::loader::MonitoringMode;
use crate::probes::agent_health::AgentHealthChecker;
use crate::probes::io_profile::IoProfiler;
use crate::probes::network::NetworkMonitor;

/// Escapes a Prometheus label **value** per the text exposition format (#25): `\` -> `\\`,
/// `"` -> `\"`, line feed -> `\n` (escaped, spec-compliant). Carriage return is **stripped**: a
/// strict Prometheus parser only un-escapes `\\`, `\"`, `\n` and rejects a `\r` escape as an
/// invalid escape sequence, so CR must not be emitted as `\r`.
///
/// Single-pass char-by-char so an inserted backslash is never re-escaped (a chained
/// `.replace("\"", ..).replace("\\", ..)` would double-escape). Returns [`Cow::Borrowed`] with no
/// allocation when the value contains none of the four characters — the common case (real agent /
/// cgroup / destination names are clean), which keeps the 1:n principle (no copy when unnecessary).
/// Only dynamic string label values are routed through this; numeric and static values are not.
fn escape_label_value(value: &str) -> Cow<'_, str> {
    if !value
        .bytes()
        .any(|b| matches!(b, b'\\' | b'"' | b'\n' | b'\r'))
    {
        return Cow::Borrowed(value);
    }
    let mut out = String::with_capacity(value.len() + 8);
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => {} // strip: a `\r` escape is rejected by strict Prometheus parsers
            _ => out.push(c),
        }
    }
    Cow::Owned(out)
}

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
            let name = escape_label_value(&metrics.cgroup_name);
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
        output.push_str("# HELP sentinel_llm_bytes_total LLM API network bytes\n");
        output.push_str("# TYPE sentinel_llm_bytes_total counter\n");

        for (dest, metrics) in monitor.all_metrics() {
            let dest = escape_label_value(dest);
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
            output.push_str(&format!(
                "sentinel_llm_bytes_total{{destination=\"{}\",direction=\"sent\"}} {}\n",
                dest, metrics.bytes_sent
            ));
            output.push_str(&format!(
                "sentinel_llm_bytes_total{{destination=\"{}\",direction=\"received\"}} {}\n",
                dest, metrics.bytes_received
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
            escape_label_value(agent_id),
            stress_factor
        ));
        output
    }

    /// Exports collector meta-metrics (monitoring mode, cycle time, drop rate).
    pub fn export_collector_meta(mode: MonitoringMode, cycle_us: u64, drops: u64) -> String {
        let mut output = String::new();

        output
            .push_str("# HELP sentinel_ebpf_monitoring_mode Current monitoring mode (1=active)\n");
        output.push_str("# TYPE sentinel_ebpf_monitoring_mode gauge\n");
        output.push_str(&format!(
            "sentinel_ebpf_monitoring_mode{{mode=\"{}\"}} 1\n",
            escape_label_value(mode.as_str())
        ));

        output.push_str(
            "# HELP sentinel_ebpf_collector_cycle_microseconds Collection cycle duration\n",
        );
        output.push_str("# TYPE sentinel_ebpf_collector_cycle_microseconds gauge\n");
        output.push_str(&format!(
            "sentinel_ebpf_collector_cycle_microseconds {}\n",
            cycle_us
        ));

        output
            .push_str("# HELP sentinel_ebpf_ring_buffer_drops_total Ring buffer events dropped\n");
        output.push_str("# TYPE sentinel_ebpf_ring_buffer_drops_total counter\n");
        output.push_str(&format!(
            "sentinel_ebpf_ring_buffer_drops_total {}\n",
            drops
        ));

        output
    }

    /// Exports all metrics combined (legacy API).
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

    /// Exports all metrics from a collected snapshot.
    pub fn export_snapshot(snapshot: &MetricsSnapshot) -> String {
        let mut output = String::new();

        // Collector meta-metrics.
        output.push_str(&Self::export_collector_meta(
            snapshot.mode,
            snapshot.cycle_duration.as_micros() as u64,
            snapshot.ring_buffer_drops,
        ));

        // Stalled agents (with agent name and seconds since last write).
        output.push_str("# HELP sentinel_agent_stalled Whether agent is stalled (1=stalled)\n");
        output.push_str("# TYPE sentinel_agent_stalled gauge\n");
        output.push_str(
            "# HELP sentinel_agent_last_write_seconds Seconds since last write syscall\n",
        );
        output.push_str("# TYPE sentinel_agent_last_write_seconds gauge\n");
        for agent in &snapshot.stalled_agents {
            let agent_name = escape_label_value(&agent.agent_name);
            output.push_str(&format!(
                "sentinel_agent_stalled{{cgroup_id=\"{}\",agent=\"{}\"}} 1\n",
                agent.cgroup_id, agent_name
            ));
            output.push_str(&format!(
                "sentinel_agent_last_write_seconds{{cgroup_id=\"{}\",agent=\"{}\"}} {}\n",
                agent.cgroup_id, agent_name, agent.seconds_since_write
            ));
        }
        // Non-stalled agents count
        output.push_str("# HELP sentinel_agent_stalled_total Total number of stalled agents\n");
        output.push_str("# TYPE sentinel_agent_stalled_total gauge\n");
        output.push_str(&format!(
            "sentinel_agent_stalled_total {}\n",
            snapshot.stalled_agents.len()
        ));

        // I/O metrics from snapshot.
        output.push_str("# HELP sentinel_io_ops_total Total I/O operations per cgroup\n");
        output.push_str("# TYPE sentinel_io_ops_total counter\n");
        output.push_str("# HELP sentinel_io_bytes_total Total I/O bytes per cgroup\n");
        output.push_str("# TYPE sentinel_io_bytes_total counter\n");
        for (cgroup_id, io) in &snapshot.io_metrics {
            let name = escape_label_value(&io.cgroup_name);
            output.push_str(&format!(
                "sentinel_io_ops_total{{cgroup_id=\"{}\",cgroup_name=\"{}\",direction=\"read\"}} {}\n",
                cgroup_id, name, io.read_ops
            ));
            output.push_str(&format!(
                "sentinel_io_ops_total{{cgroup_id=\"{}\",cgroup_name=\"{}\",direction=\"write\"}} {}\n",
                cgroup_id, name, io.write_ops
            ));
            output.push_str(&format!(
                "sentinel_io_bytes_total{{cgroup_id=\"{}\",cgroup_name=\"{}\",direction=\"read\"}} {}\n",
                cgroup_id, name, io.read_bytes
            ));
            output.push_str(&format!(
                "sentinel_io_bytes_total{{cgroup_id=\"{}\",cgroup_name=\"{}\",direction=\"write\"}} {}\n",
                cgroup_id, name, io.write_bytes
            ));
        }

        // Network metrics from snapshot.
        output.push_str("# HELP sentinel_llm_request_duration_seconds LLM API request latency\n");
        output.push_str("# TYPE sentinel_llm_request_duration_seconds summary\n");
        output.push_str("# HELP sentinel_llm_requests_total Total LLM API requests\n");
        output.push_str("# TYPE sentinel_llm_requests_total counter\n");
        output.push_str("# HELP sentinel_llm_errors_total Total LLM API errors\n");
        output.push_str("# TYPE sentinel_llm_errors_total counter\n");
        for net in snapshot.network_metrics.values() {
            let destination = escape_label_value(&net.destination);
            if net.avg_latency_us > 0 {
                output.push_str(&format!(
                    "sentinel_llm_request_duration_seconds{{destination=\"{}\"}} {:.6}\n",
                    destination,
                    net.avg_latency_us as f64 / 1_000_000.0
                ));
            }
            output.push_str(&format!(
                "sentinel_llm_requests_total{{destination=\"{}\"}} {}\n",
                destination, net.request_count
            ));
            output.push_str(&format!(
                "sentinel_llm_errors_total{{destination=\"{}\"}} {}\n",
                destination, net.error_count
            ));
            output.push_str(&format!(
                "sentinel_llm_bytes_total{{destination=\"{}\",direction=\"sent\"}} {}\n",
                destination, net.bytes_sent
            ));
            output.push_str(&format!(
                "sentinel_llm_bytes_total{{destination=\"{}\",direction=\"received\"}} {}\n",
                destination, net.bytes_received
            ));
        }

        // PSI metrics from snapshot.
        output.push_str(
            "# HELP sentinel_agent_cpu_pressure_stress CPU pressure stress factor (0-1)\n",
        );
        output.push_str("# TYPE sentinel_agent_cpu_pressure_stress gauge\n");
        for (agent, psi) in &snapshot.psi_metrics {
            let agent = escape_label_value(agent);
            output.push_str(&format!(
                "sentinel_agent_cpu_pressure_stress{{agent=\"{}\"}} {:.4}\n",
                agent, psi.combined_stress
            ));
        }

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
        assert!(output.contains("# HELP sentinel_llm_bytes_total"));
        assert!(output.contains("# TYPE sentinel_llm_bytes_total counter"));
        assert!(output.contains("destination=\"api.anthropic.com:443\""));
        assert!(output.contains("0.150000"));
        assert!(output
            .contains("sentinel_llm_bytes_total{destination=\"api.anthropic.com:443\",direction=\"sent\"} 1024"));
        assert!(output
            .contains("sentinel_llm_bytes_total{destination=\"api.anthropic.com:443\",direction=\"received\"} 4096"));
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

    #[test]
    fn export_collector_meta_format() {
        let output = MetricsExporter::export_collector_meta(MonitoringMode::Userspace, 5000, 0);
        assert!(output.contains("sentinel_ebpf_monitoring_mode{mode=\"userspace\"} 1"));
        assert!(output.contains("sentinel_ebpf_collector_cycle_microseconds 5000"));
        assert!(output.contains("sentinel_ebpf_ring_buffer_drops_total 0"));
    }

    #[test]
    fn export_collector_meta_kernel_mode() {
        let output = MetricsExporter::export_collector_meta(MonitoringMode::Kernel, 50, 3);
        assert!(output.contains("sentinel_ebpf_monitoring_mode{mode=\"kernel\"} 1"));
        assert!(output.contains("sentinel_ebpf_collector_cycle_microseconds 50"));
        assert!(output.contains("sentinel_ebpf_ring_buffer_drops_total 3"));
    }

    #[test]
    fn export_snapshot_empty() {
        use crate::collector::MetricsSnapshot;
        use std::collections::HashMap;

        let snapshot = MetricsSnapshot {
            stalled_agents: vec![],
            io_metrics: HashMap::new(),
            network_metrics: HashMap::new(),
            psi_metrics: HashMap::new(),
            cycle_duration: Duration::from_micros(100),
            mode: MonitoringMode::Userspace,
            ring_buffer_drops: 0,
        };
        let output = MetricsExporter::export_snapshot(&snapshot);
        assert!(output.contains("sentinel_ebpf_monitoring_mode{mode=\"userspace\"} 1"));
        assert!(output.contains("sentinel_ebpf_collector_cycle_microseconds 100"));
        assert!(output.contains("sentinel_agent_stalled_total 0"));
    }

    #[test]
    fn export_snapshot_with_data() {
        use crate::collector::{
            IoSnapshot, MetricsSnapshot, NetworkSnapshot, PsiSnapshot, StalledAgent,
        };
        use std::collections::HashMap;

        let mut io_metrics = HashMap::new();
        io_metrics.insert(
            1,
            IoSnapshot {
                cgroup_name: "agent-01".to_string(),
                read_ops: 100,
                write_ops: 50,
                read_bytes: 409600,
                write_bytes: 204800,
            },
        );

        let mut network_metrics = HashMap::new();
        network_metrics.insert(
            "api.anthropic.com:443".to_string(),
            NetworkSnapshot {
                destination: "api.anthropic.com:443".to_string(),
                request_count: 10,
                avg_latency_us: 150_000,
                bytes_sent: 10240,
                bytes_received: 40960,
                error_count: 1,
            },
        );

        let mut psi_metrics = HashMap::new();
        psi_metrics.insert(
            "AGENT-01".to_string(),
            PsiSnapshot {
                cpu_avg10: 25.0,
                memory_avg10: 10.0,
                io_avg10: 5.0,
                combined_stress: 0.175,
            },
        );

        let snapshot = MetricsSnapshot {
            stalled_agents: vec![StalledAgent {
                cgroup_id: 42,
                agent_name: "AGENT-07".to_string(),
                seconds_since_write: 65,
            }],
            io_metrics,
            network_metrics,
            psi_metrics,
            cycle_duration: Duration::from_micros(500),
            mode: MonitoringMode::Userspace,
            ring_buffer_drops: 0,
        };

        let output = MetricsExporter::export_snapshot(&snapshot);
        assert!(output.contains("sentinel_agent_stalled{cgroup_id=\"42\",agent=\"AGENT-07\"} 1"));
        assert!(output
            .contains("sentinel_agent_last_write_seconds{cgroup_id=\"42\",agent=\"AGENT-07\"} 65"));
        assert!(output.contains("sentinel_agent_stalled_total 1"));
        assert!(output.contains("cgroup_name=\"agent-01\""));
        assert!(output
            .contains("sentinel_llm_requests_total{destination=\"api.anthropic.com:443\"} 10"));
        assert!(output
            .contains("sentinel_llm_bytes_total{destination=\"api.anthropic.com:443\",direction=\"sent\"} 10240"));
        assert!(output
            .contains("sentinel_llm_bytes_total{destination=\"api.anthropic.com:443\",direction=\"received\"} 40960"));
        assert!(output.contains("sentinel_agent_cpu_pressure_stress{agent=\"AGENT-01\"}"));
    }

    // ── #25: label-value escaping ──────────────────────────────────────────────────────────────

    #[test]
    fn escape_label_value_escapes_quotes() {
        // AC-1: inner quotes in real agent nicknames (e.g. Tobias "Tobi" Lehmann).
        assert_eq!(
            escape_label_value(r#"Tobias "Tobi" Lehmann"#).as_ref(),
            r#"Tobias \"Tobi\" Lehmann"#
        );
    }

    #[test]
    fn escape_label_value_escapes_backslash() {
        // AC-2.
        assert_eq!(escape_label_value(r"A\B").as_ref(), r"A\\B");
    }

    #[test]
    fn escape_label_value_escapes_newline() {
        // AC-3: LF -> \n (backslash+n); no raw 0x0A remains.
        let escaped = escape_label_value("line1\nline2");
        assert_eq!(escaped.as_ref(), "line1\\nline2");
        assert!(!escaped.contains('\n'));
    }

    #[test]
    fn escape_label_value_strips_carriage_return() {
        // AC-8: CR is stripped (a strict parser rejects a `\r` escape).
        assert_eq!(escape_label_value("a\rb").as_ref(), "ab");
        assert!(!escape_label_value("a\r\nb").contains('\r'));
    }

    #[test]
    fn escape_label_value_clean_is_borrowed() {
        // AC-9 / 1:n: a clean value takes the no-allocation fast path; a special char allocates.
        assert!(matches!(escape_label_value("AGENT-07"), Cow::Borrowed(_)));
        assert!(matches!(escape_label_value("a\"b"), Cow::Owned(_)));
    }

    /// Minimal Prometheus label-value parser (local replacement for `promtool check metrics`,
    /// AC-6): for every `{...}` label set each `"..."` value may only contain the escapes
    /// `\"`, `\\`, `\n` — never a raw `"`, 0x0A, or 0x0D. Panics on a violation.
    fn assert_no_raw_special_chars_in_labels(metrics: &str) {
        for line in metrics.lines() {
            if line.starts_with('#') {
                continue;
            }
            let (Some(start), Some(end)) = (line.find('{'), line.rfind('}')) else {
                continue;
            };
            let bytes = &line.as_bytes()[start + 1..end];
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] != b'"' {
                    i += 1;
                    continue;
                }
                i += 1; // enter a label value
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => {
                            assert!(
                                i + 1 < bytes.len() && matches!(bytes[i + 1], b'\\' | b'"' | b'n'),
                                "invalid escape in label value: {line}"
                            );
                            i += 2;
                        }
                        b'"' => {
                            i += 1;
                            break; // end of value
                        }
                        b'\n' | b'\r' => panic!("raw newline/CR in label value: {line:?}"),
                        _ => i += 1,
                    }
                }
            }
        }
    }

    #[test]
    fn export_snapshot_escapes_quoted_agent_name() {
        // AC-4: a quoted agent name produces a valid, escaped sentinel_agent_stalled line; cgroup
        // name with quotes+backslash and a destination with a raw newline are also escaped/stripped.
        use crate::collector::{IoSnapshot, MetricsSnapshot, NetworkSnapshot, StalledAgent};
        use std::collections::HashMap;

        let mut io_metrics = HashMap::new();
        io_metrics.insert(
            7,
            IoSnapshot {
                cgroup_name: r#"cg "weird" \name"#.to_string(),
                read_ops: 1,
                write_ops: 1,
                read_bytes: 1,
                write_bytes: 1,
            },
        );
        let mut network_metrics = HashMap::new();
        network_metrics.insert(
            "x".to_string(),
            NetworkSnapshot {
                destination: "host\"evil\nline".to_string(),
                request_count: 1,
                avg_latency_us: 1,
                bytes_sent: 1,
                bytes_received: 1,
                error_count: 0,
            },
        );

        let snapshot = MetricsSnapshot {
            stalled_agents: vec![StalledAgent {
                cgroup_id: 42,
                agent_name: r#"Tobias "Tobi" Lehmann"#.to_string(),
                seconds_since_write: 65,
            }],
            io_metrics,
            network_metrics,
            psi_metrics: HashMap::new(),
            cycle_duration: Duration::from_micros(100),
            mode: MonitoringMode::Userspace,
            ring_buffer_drops: 0,
        };

        let output = MetricsExporter::export_snapshot(&snapshot);
        assert!(output.contains(
            r#"sentinel_agent_stalled{cgroup_id="42",agent="Tobias \"Tobi\" Lehmann"} 1"#
        ));
        assert!(output.contains(r#"cgroup_name="cg \"weird\" \\name""#));
        assert!(
            !output.contains("evil\nline"),
            "destination newline not stripped/escaped"
        );
        assert_no_raw_special_chars_in_labels(&output);
    }

    #[test]
    fn parser_check_passes_on_special_char_snapshot() {
        // AC-6 local equivalent: the full snapshot export passes the minimal label parser.
        use crate::collector::{MetricsSnapshot, PsiSnapshot, StalledAgent};
        use std::collections::HashMap;

        let mut psi_metrics = HashMap::new();
        psi_metrics.insert(
            r#"Katharina "Kathi" Wiegand"#.to_string(),
            PsiSnapshot {
                cpu_avg10: 1.0,
                memory_avg10: 1.0,
                io_avg10: 1.0,
                combined_stress: 0.5,
            },
        );
        let snapshot = MetricsSnapshot {
            stalled_agents: vec![StalledAgent {
                cgroup_id: 1,
                agent_name: r#"Gabriele "Gabi" Fuchs"#.to_string(),
                seconds_since_write: 40,
            }],
            io_metrics: HashMap::new(),
            network_metrics: HashMap::new(),
            psi_metrics,
            cycle_duration: Duration::from_micros(100),
            mode: MonitoringMode::Userspace,
            ring_buffer_drops: 0,
        };
        let output = MetricsExporter::export_snapshot(&snapshot);
        assert_no_raw_special_chars_in_labels(&output);
        assert!(output.contains(r#"agent="Gabriele \"Gabi\" Fuchs""#));
        assert!(output.contains(r#"agent="Katharina \"Kathi\" Wiegand""#));
    }
}
