//! Platform-Metriken Snapshot (Observe-Phase).

use sentinel_sandbox::cgroup_path;

/// Snapshot aller platform-relevanten Metriken fuer einen Zyklus.
#[derive(Debug, Clone, Default)]
pub struct PlatformMetrics {
    /// Namen gestallter Agents (aus eBPF Collector).
    pub stalled_agents: Vec<String>,
    /// events.db Dateigroesse in Bytes.
    pub event_store_size_bytes: u64,
    /// max(event_id) - projection_offset.
    pub projection_lag: i64,
    /// (Agent-Name, memory.current / memory.max Ratio).
    pub agent_memory_pressure: Vec<(String, f64)>,
    /// (Agent-Name, write_bytes_per_second). Erster Zyklus nach Restart = leer.
    pub agent_write_rates: Vec<(String, f64)>,
    /// Aktueller Tick.
    pub tick: u64,
}

/// Stateful Metrics Collector (behaelt prev_write_bytes fuer Delta-Berechnung).
#[derive(Default)]
pub struct PlatformMetricsCollector {
    /// Vorherige Write-Bytes pro Agent (timestamp_ms, total_bytes).
    prev_write_bytes: std::collections::HashMap<String, (u64, u64)>,
}

/// Sammelt Platform-Metriken aus verschiedenen Quellen.
///
/// Best-effort: Einzelne Fehler werden ignoriert (z.B. fehlende cgroup-Dateien).
pub fn collect(
    collector: &mut PlatformMetricsCollector,
    last_ebpf_snapshot: &Option<sentinel_ebpf::collector::MetricsSnapshot>,
    event_store: &sentinel_limbo::EventStore,
    events_db_path: &str,
    agent_names: &[String],
    tick: u64,
) -> PlatformMetrics {
    let mut metrics = PlatformMetrics {
        tick,
        ..Default::default()
    };

    // 1. Stalled Agents aus eBPF Snapshot
    if let Some(snapshot) = last_ebpf_snapshot {
        metrics.stalled_agents = snapshot
            .stalled_agents
            .iter()
            .map(|s| s.agent_name.clone())
            .collect();
    }

    // 2. Event Store Dateigroesse
    if let Ok(meta) = std::fs::metadata(events_db_path) {
        metrics.event_store_size_bytes = meta.len();
    }

    // 3. Projection Lag
    let latest_id = event_store.get_latest_event_id().unwrap_or(0);
    let offset = event_store
        .get_offset("sentinel-projection")
        .ok()
        .flatten()
        .unwrap_or(latest_id);
    metrics.projection_lag = latest_id - offset;

    // 4. cgroup Memory Pressure pro Agent
    for name in agent_names {
        let path = cgroup_path(name);
        let current = read_cgroup_u64(&format!("{path}/memory.current"));
        let max = read_cgroup_u64(&format!("{path}/memory.max"));
        if max > 0 {
            metrics
                .agent_memory_pressure
                .push((name.clone(), current as f64 / max as f64));
        }
    }

    // 5. Write-Rates aus eBPF IoSnapshot (Delta-Berechnung)
    if let Some(snapshot) = last_ebpf_snapshot {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        for io in snapshot.io_metrics.values() {
            let name = &io.cgroup_name;
            let total_bytes = io.write_bytes;

            if let Some(&(prev_ts, prev_bytes)) = collector.prev_write_bytes.get(name) {
                let dt_ms = now_ms.saturating_sub(prev_ts);
                if dt_ms > 0 {
                    let delta_bytes = total_bytes.saturating_sub(prev_bytes);
                    let rate = (delta_bytes as f64 / dt_ms as f64) * 1000.0; // bytes/sec
                    metrics.agent_write_rates.push((name.clone(), rate));
                }
            }
            // Update prev fuer naechsten Zyklus
            collector
                .prev_write_bytes
                .insert(name.clone(), (now_ms, total_bytes));
        }
    }

    metrics
}

fn read_cgroup_u64(path: &str) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_metrics_default() {
        let m = PlatformMetrics::default();
        assert!(m.stalled_agents.is_empty());
        assert_eq!(m.event_store_size_bytes, 0);
        assert_eq!(m.projection_lag, 0);
        assert!(m.agent_memory_pressure.is_empty());
    }
}
