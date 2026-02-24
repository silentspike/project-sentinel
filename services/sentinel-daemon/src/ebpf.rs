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
pub fn init_ebpf() -> (EbpfCollector, MonitoringMode) {
    let result = sentinel_ebpf::loader::init();
    let mode = result.mode;

    #[cfg(feature = "ebpf")]
    let collector = match result.probes {
        Some(probes) => {
            info!("eBPF Kernel-Probes geladen, Collector mit BPF Maps");
            EbpfCollector::with_probes(mode, probes)
        }
        None => EbpfCollector::new(mode),
    };

    #[cfg(not(feature = "ebpf"))]
    let collector = EbpfCollector::new(mode);

    (collector, mode)
}

/// Prometheus Metrics HTTP Server (raw TCP, kein hyper noetig).
///
/// Antwortet auf jede TCP-Verbindung mit dem aktuellen Prometheus-Text.
/// Port default: 9090. Laeuft als tokio::spawn Task.
pub async fn prometheus_server(metrics_text: Arc<RwLock<String>>, port: u16) {
    let listener = match TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            error!(port, error = %e, "Prometheus TCP Listener bind fehlgeschlagen");
            return;
        }
    };
    info!(port, "Prometheus eBPF metrics server gestartet");

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

/// Empfaengt MetricsSnapshots via mpsc und publiziert auf Zenoh + Prometheus.
///
/// 1. Rendert Prometheus-Text und speichert ihn im shared RwLock
/// 2. Publiziert JSON-Daten auf sentinel/ebpf/* Zenoh-Topics
pub async fn ebpf_publisher(
    mut rx: tokio::sync::mpsc::Receiver<MetricsSnapshot>,
    metrics_text: Arc<RwLock<String>>,
) {
    // SentinelBus fuer Zenoh-Publishing erstellen
    let bus = match SentinelBus::new().await {
        Ok(b) => {
            info!("eBPF Zenoh Publisher: SentinelBus verbunden");
            Some(b)
        }
        Err(e) => {
            warn!(error = %e, "eBPF Zenoh Publisher: SentinelBus nicht verfuegbar, nur Prometheus aktiv");
            None
        }
    };

    while let Some(snapshot) = rx.recv().await {
        // 1. Prometheus Text rendern + speichern
        let text = MetricsExporter::export_snapshot(&snapshot);
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
    }

    info!("eBPF Publisher beendet (mpsc Sender gedroppt)");
}
