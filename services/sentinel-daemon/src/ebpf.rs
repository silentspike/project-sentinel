//! eBPF Monitoring Integration fuer den Daemon.
//!
//! Verbindet sentinel-ebpf (Collector, Exporter) mit dem Daemon:
//! - Initialisierung + Probe-Loading (oder Userspace-Fallback)
//! - Prometheus Metrics Endpoint (Port 9090)
//! - Zenoh Publishing auf sentinel/ebpf/* Topics
//!
//! Architektur:
//! - EbpfCollector laeuft sync im ECS-Thread (liest BPF Maps)
//! - MetricsSnapshot wird via mpsc an tokio-Runtime gesendet
//! - Tokio: Prometheus HTTP + Zenoh Publish

use std::sync::{Arc, RwLock};

use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

use sentinel_ebpf::collector::MetricsSnapshot;
use sentinel_ebpf::exporter::MetricsExporter;
use sentinel_ebpf::loader::MonitoringMode;
use sentinel_ebpf::EbpfCollector;
use sentinel_zenoh::topics;
use sentinel_zenoh::SentinelBus;

/// eBPF Status payload fuer Zenoh sentinel/ebpf/status Topic.
#[derive(serde::Serialize)]
struct EbpfStatus {
    mode: MonitoringMode,
}

/// Initialisiert eBPF Monitoring (Probe-Loading oder Userspace-Fallback).
///
/// Gibt den konfigurierten Collector und den aktiven Modus zurueck.
/// LoadedProbes werden vom Collector uebernommen (Ownership → Drop = Detach).
pub fn init_ebpf(stall_threshold_secs: u64) -> (EbpfCollector, MonitoringMode) {
    let result = sentinel_ebpf::loader::init();
    let mode = result.mode;
    let stall_threshold_secs = stall_threshold_secs.max(1);

    #[cfg(feature = "ebpf")]
    let collector = match result.probes {
        Some(probes) => {
            info!("eBPF Kernel-Probes geladen, Collector mit BPF Maps");
            EbpfCollector::with_probes_and_stall_threshold(mode, probes, stall_threshold_secs)
        }
        None => EbpfCollector::new_with_stall_threshold(mode, stall_threshold_secs),
    };

    #[cfg(not(feature = "ebpf"))]
    let collector = EbpfCollector::new_with_stall_threshold(mode, stall_threshold_secs);

    (collector, mode)
}

/// Prometheus Metrics HTTP Server (raw TCP, kein hyper noetig).
///
/// Antwortet auf jede TCP-Verbindung mit dem aktuellen Prometheus-Text.
/// Default-Bind: 127.0.0.1:9090 (loopback secure default, #525; uebersteuerbar via
/// `[daemon.metrics] bind_addr`). Laeuft als tokio::spawn Task.
pub async fn prometheus_server(metrics_text: Arc<RwLock<String>>, bind_addr: String) {
    let listener = match TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            error!(bind_addr = %bind_addr, error = %e, "Prometheus TCP Listener bind fehlgeschlagen");
            return;
        }
    };
    info!(bind_addr = %bind_addr, "Prometheus eBPF metrics server gestartet");

    loop {
        match listener.accept().await {
            Ok((mut stream, _addr)) => {
                let text = metrics_text
                    .read()
                    .unwrap_or_else(|e| {
                        warn!(error = %e, "RwLock poisoned, leerer Metrics-Text");
                        e.into_inner()
                    })
                    .clone();
                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: text/plain; version=0.0.4; charset=utf-8\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\
                     \r\n\
                     {}",
                    text.len(),
                    text
                );
                if let Err(e) = stream.write_all(response.as_bytes()).await {
                    warn!(error = %e, "Prometheus response write fehlgeschlagen");
                }
            }
            Err(e) => {
                warn!(error = %e, "Prometheus TCP accept fehlgeschlagen");
            }
        }
    }
}

/// NATS subjects fuer eBPF metrics bridge (ADR-001: Daemon bridged Zenoh→NATS).
#[cfg(feature = "nats")]
mod nats_subjects {
    pub const AGENT_HEALTH: &str = "sentinel.ebpf.agent-health";
    pub const IO_PROFILE: &str = "sentinel.ebpf.io-profile";
    pub const NETWORK: &str = "sentinel.ebpf.network";
    pub const PSI: &str = "sentinel.ebpf.psi";
    pub const STATUS: &str = "sentinel.ebpf.status";
}

/// Empfaengt MetricsSnapshots via mpsc und publiziert auf Zenoh + NATS + Prometheus.
///
/// 1. Rendert Prometheus-Text und speichert ihn im shared RwLock
/// 2. Publiziert JSON-Daten auf sentinel/ebpf/* Zenoh-Topics
/// 3. Bridged dieselben Daten auf sentinel.ebpf.* NATS-Subjects (ADR-001)
pub async fn ebpf_publisher(
    mut rx: tokio::sync::mpsc::Receiver<MetricsSnapshot>,
    metrics_text: Arc<RwLock<String>>,
    nats_url: Option<String>,
    bus: Option<SentinelBus>,
) {
    // Suppress unused warning when nats feature is disabled
    #[cfg(not(feature = "nats"))]
    let _ = &nats_url;

    if bus.is_some() {
        info!("eBPF Zenoh Publisher: SentinelBus verbunden (shared)");
    } else {
        warn!("eBPF Zenoh Publisher: SentinelBus nicht verfuegbar, nur Prometheus aktiv");
    }

    // NATS Client fuer eBPF→NATS Bridge (ADR-001: Daemon bridged fuer Go-Services)
    #[cfg(feature = "nats")]
    let nats = if let Some(ref url) = nats_url {
        match async_nats::connect(url.as_str()).await {
            Ok(client) => {
                info!(url = url.as_str(), "eBPF NATS Bridge verbunden");
                Some(client)
            }
            Err(e) => {
                warn!(error = %e, "eBPF NATS Bridge: Verbindung fehlgeschlagen, nur Zenoh aktiv");
                None
            }
        }
    } else {
        None
    };

    let mut snapshot_count = 0u64;

    while let Some(snapshot) = rx.recv().await {
        snapshot_count += 1;

        // 1. Prometheus Text rendern + speichern
        let mut text = MetricsExporter::export_snapshot(&snapshot);

        // Global MetricsRegistry Gauges anhaengen (Tick-Dauer, PSI, etc.)
        // Explizit alle Daemon-Gauges rendern (inkl. Nullwerte, damit Dashboard sie findet).
        {
            use std::fmt::Write;
            let reg = sentinel_telemetry::MetricsRegistry::global();
            for name in &[
                "sentinel_tick_duration_ms",
                "sentinel_tick_rate_effective_ms",
                "sentinel_psi_cpu_avg10",
                "sentinel_psi_mem_avg10",
                "sentinel_psi_io_avg10",
            ] {
                let g = reg.gauge(name);
                let _ = writeln!(text, "{} {}", name, g.get());
            }

            // Per-Phase-Histogramme (#381) als Prometheus-Summary anhaengen.
            let (_counters, histograms, _gauges) = reg.snapshot_raw();
            render_phase_histograms(&mut text, &histograms);
        }

        match metrics_text.write() {
            Ok(mut guard) => *guard = text,
            Err(e) => {
                warn!(error = %e, "Prometheus RwLock poisoned");
            }
        }

        // 2. Zenoh publish (nur wenn Bus verfuegbar)
        if let Some(ref bus) = bus {
            // Agent Health (stalled agents)
            if let Ok(payload) = serde_json::to_vec(&snapshot.stalled_agents) {
                if let Err(e) = bus.publish(topics::EBPF_AGENT_HEALTH, &payload).await {
                    warn!(error = %e, "Zenoh publish agent-health fehlgeschlagen");
                }
            }

            // I/O Profile
            if let Ok(payload) = serde_json::to_vec(&snapshot.io_metrics) {
                if let Err(e) = bus.publish(topics::EBPF_IO_PROFILE, &payload).await {
                    warn!(error = %e, "Zenoh publish io-profile fehlgeschlagen");
                }
            }

            // Network
            if let Ok(payload) = serde_json::to_vec(&snapshot.network_metrics) {
                if let Err(e) = bus.publish(topics::EBPF_NETWORK, &payload).await {
                    warn!(error = %e, "Zenoh publish network fehlgeschlagen");
                }
            }

            // PSI Stress
            if let Ok(payload) = serde_json::to_vec(&snapshot.psi_metrics) {
                if let Err(e) = bus.publish(topics::EBPF_PSI, &payload).await {
                    warn!(error = %e, "Zenoh publish psi-stress fehlgeschlagen");
                }
            }

            // Status (Monitoring Mode)
            let status = EbpfStatus {
                mode: snapshot.mode,
            };
            if let Ok(payload) = serde_json::to_vec(&status) {
                if let Err(e) = bus.publish(topics::EBPF_STATUS, &payload).await {
                    warn!(error = %e, "Zenoh publish status fehlgeschlagen");
                }
            }
        }

        // 3. NATS Bridge publish (fire-and-forget, ADR-001)
        #[cfg(feature = "nats")]
        if let Some(ref nc) = nats {
            if let Ok(payload) = serde_json::to_vec(&snapshot.stalled_agents) {
                if let Err(e) = nc
                    .publish(nats_subjects::AGENT_HEALTH, payload.into())
                    .await
                {
                    warn!(error = %e, "NATS publish agent-health fehlgeschlagen");
                }
            }
            if let Ok(payload) = serde_json::to_vec(&snapshot.io_metrics) {
                if let Err(e) = nc.publish(nats_subjects::IO_PROFILE, payload.into()).await {
                    warn!(error = %e, "NATS publish io-profile fehlgeschlagen");
                }
            }
            if let Ok(payload) = serde_json::to_vec(&snapshot.network_metrics) {
                if let Err(e) = nc.publish(nats_subjects::NETWORK, payload.into()).await {
                    warn!(error = %e, "NATS publish network fehlgeschlagen");
                }
            }
            if let Ok(payload) = serde_json::to_vec(&snapshot.psi_metrics) {
                if let Err(e) = nc.publish(nats_subjects::PSI, payload.into()).await {
                    warn!(error = %e, "NATS publish psi fehlgeschlagen");
                }
            }
            let status = EbpfStatus {
                mode: snapshot.mode,
            };
            if let Ok(payload) = serde_json::to_vec(&status) {
                if let Err(e) = nc.publish(nats_subjects::STATUS, payload.into()).await {
                    warn!(error = %e, "NATS publish status fehlgeschlagen");
                }
            }
        }

        // Periodic logging to confirm publisher is alive
        if snapshot_count.is_multiple_of(60) {
            info!(
                snapshots = snapshot_count,
                mode = %snapshot.mode,
                stalled = snapshot.stalled_agents.len(),
                "eBPF Publisher alive"
            );
        }
    }

    // mpsc sender was dropped — ECS thread exited or daemon is shutting down.
    // This is an ERROR if it happens unexpectedly (daemon crash/restart).
    error!(
        snapshots_processed = snapshot_count,
        "eBPF Publisher beendet: mpsc Sender gedroppt (ECS-Thread beendet)"
    );
}

/// Rendert alle `sentinel.ecs.phase.*`-Histogramme als Prometheus-Summary (#381).
///
/// Eine Metric-Family `sentinel_phase_duration_ms` mit `phase`-Label,
/// Quantile p50/p95/p99 plus `_sum`/`_count` pro Phase. Deterministisch
/// sortiert; Nicht-Phase-Histogramme werden ignoriert. Leere Map = kein Output
/// (Lazy-Filter: vor dem ersten Tick erscheinen keine Zeilen).
fn render_phase_histograms(
    text: &mut String,
    histograms: &std::collections::HashMap<String, sentinel_telemetry::metrics::HistogramSnapshot>,
) {
    use std::fmt::Write;

    let mut rows: Vec<(&str, &sentinel_telemetry::metrics::HistogramSnapshot)> = histograms
        .iter()
        .filter_map(|(key, snap)| sentinel_telemetry::phase_label(key).map(|p| (p, snap)))
        .collect();
    if rows.is_empty() {
        return;
    }
    rows.sort_by_key(|(phase, _)| *phase);

    let name = sentinel_telemetry::PHASE_DURATION_PROM_NAME;
    let _ = writeln!(
        text,
        "# HELP {name} ECS SimulationPhase duration per tick (ms)"
    );
    let _ = writeln!(text, "# TYPE {name} summary");
    for (phase, snap) in rows {
        let _ = writeln!(
            text,
            "{name}{{phase=\"{phase}\",quantile=\"0.5\"}} {}",
            snap.p50
        );
        let _ = writeln!(
            text,
            "{name}{{phase=\"{phase}\",quantile=\"0.95\"}} {}",
            snap.p95
        );
        let _ = writeln!(
            text,
            "{name}{{phase=\"{phase}\",quantile=\"0.99\"}} {}",
            snap.p99
        );
        let _ = writeln!(text, "{name}_sum{{phase=\"{phase}\"}} {}", snap.sum);
        let _ = writeln!(text, "{name}_count{{phase=\"{phase}\"}} {}", snap.count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_telemetry::metrics::HistogramSnapshot;
    use std::collections::HashMap;

    fn snap(p50: f64, p95: f64, p99: f64, sum: f64, count: u64) -> HistogramSnapshot {
        HistogramSnapshot {
            boundaries: vec![1.0, 10.0],
            bucket_counts: vec![count, 0, 0],
            sum,
            count,
            p50,
            p95,
            p99,
        }
    }

    #[test]
    fn render_emits_summary_lines_for_phase_keys_only() {
        let mut histograms = HashMap::new();
        histograms.insert(
            sentinel_telemetry::phase_metric_name("input"),
            snap(0.5, 1.0, 1.0, 12.5, 25),
        );
        histograms.insert(
            sentinel_telemetry::phase_metric_name("persist"),
            snap(1.0, 10.0, 10.0, 99.0, 25),
        );
        histograms.insert(
            "sentinel.redb.get_agent_state.duration_us".to_string(),
            snap(2.0, 4.0, 8.0, 1.0, 1),
        );

        let mut text = String::new();
        render_phase_histograms(&mut text, &histograms);

        assert_eq!(
            text.matches("# TYPE sentinel_phase_duration_ms summary")
                .count(),
            1,
            "exactly one TYPE header"
        );
        assert!(text.contains("sentinel_phase_duration_ms{phase=\"input\",quantile=\"0.5\"} 0.5"));
        assert!(text.contains("sentinel_phase_duration_ms{phase=\"input\",quantile=\"0.95\"} 1"));
        assert!(text.contains("sentinel_phase_duration_ms_sum{phase=\"persist\"} 99"));
        assert!(text.contains("sentinel_phase_duration_ms_count{phase=\"persist\"} 25"));
        assert!(
            !text.contains("redb"),
            "non-phase histograms must not be rendered"
        );
        // Deterministische Sortierung: input vor persist.
        let input_pos = text.find("phase=\"input\"").unwrap();
        let persist_pos = text.find("phase=\"persist\"").unwrap();
        assert!(input_pos < persist_pos);
    }

    #[test]
    fn render_with_empty_map_emits_nothing() {
        let mut text = String::new();
        render_phase_histograms(&mut text, &HashMap::new());
        assert!(text.is_empty());
    }

    #[test]
    fn render_with_only_foreign_keys_emits_nothing() {
        let mut histograms = HashMap::new();
        histograms.insert(
            "sentinel.zenoh.publish.duration_us".to_string(),
            snap(1.0, 2.0, 3.0, 4.0, 5),
        );
        let mut text = String::new();
        render_phase_histograms(&mut text, &histograms);
        assert!(text.is_empty());
    }

    /// #525: prometheus_server bindet auf der uebergebenen loopback-Adresse
    /// (kein hardcoded 0.0.0.0) und serviert den Prometheus-Text.
    #[tokio::test]
    async fn prometheus_server_binds_loopback_and_serves() {
        use tokio::io::AsyncReadExt;
        // Freien ephemeralen Port reservieren + freigeben (Server re-bindet loopback).
        let reserve = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = reserve.local_addr().unwrap().port();
        drop(reserve);
        let bind_addr = format!("127.0.0.1:{port}");

        let metrics_text: Arc<RwLock<String>> =
            Arc::new(RwLock::new("# test\nsentinel_loopback_probe 1\n".to_string()));
        let server_text = Arc::clone(&metrics_text);
        let handle = tokio::spawn(async move { prometheus_server(server_text, bind_addr).await });

        // Kurz warten, bis der Listener aktiv ist.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .expect("Prometheus loopback listener erreichbar");
        let mut buf = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(2), stream.read_to_end(&mut buf))
            .await
            .expect("read innerhalb timeout")
            .expect("response gelesen");

        let resp = String::from_utf8_lossy(&buf);
        assert!(resp.starts_with("HTTP/1.1 200"), "response = {resp}");
        assert!(
            resp.contains("sentinel_loopback_probe 1"),
            "metrics body fehlt: {resp}"
        );

        handle.abort();
    }
}
