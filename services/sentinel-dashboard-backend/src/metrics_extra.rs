//! Metrics extras for the SolidJS console (#433).

use std::collections::BTreeMap;
use std::time::Duration;

use axum::{extract::State, response::IntoResponse, Json};
use serde_json::{json, Value};

use crate::AppState;

async fn fetch_text(st: &AppState, url: String, timeout_ms: u64) -> Result<String, String> {
    let resp = tokio::time::timeout(Duration::from_millis(timeout_ms), st.http.get(&url).send())
        .await
        .map_err(|_| format!("timeout fetching {url}"))?
        .map_err(|e| format!("fetch {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{url} returned {}", resp.status()));
    }
    resp.text().await.map_err(|e| format!("read {url}: {e}"))
}

fn metric_number(text: &str, name: &str) -> f64 {
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            if line == name || line.starts_with(&format!("{name} ")) {
                line.split_whitespace().nth(1)?.parse::<f64>().ok()
            } else if line.starts_with(&format!("{name}{{")) {
                line.split_whitespace().last()?.parse::<f64>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0.0)
}

fn label_value(line: &str, label: &str) -> Option<String> {
    let needle = format!("{label}=\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn ebpf_payload(text: &str) -> Value {
    let mode = text
        .lines()
        .find(|line| {
            line.starts_with("sentinel_ebpf_monitoring_mode{")
                && line.split_whitespace().last() == Some("1")
        })
        .and_then(|line| label_value(line, "mode"))
        .unwrap_or_else(|| "unknown".into());

    let stalled_agents = text
        .lines()
        .filter(|line| line.starts_with("sentinel_agent_stalled{"))
        .filter(|line| line.split_whitespace().last() == Some("1"))
        .map(|line| {
            json!({
                "agent": label_value(line, "agent").unwrap_or_default(),
                "seconds": 0,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "available": true,
        "mode": mode,
        "stalled_count": metric_number(text, "sentinel_agent_stalled_total") as i64,
        "stalled_agents": stalled_agents,
        "collection_cycle_us": metric_number(text, "sentinel_ebpf_collector_cycle_microseconds") as i64,
        "ring_buffer_drops": metric_number(text, "sentinel_ebpf_ring_buffer_drops_total") as i64,
        "io_read_bytes": 0,
        "io_write_bytes": 0,
        "avg_stress": metric_number(text, "sentinel_agent_cpu_pressure_stress"),
    })
}

pub async fn ebpf(State(st): State<AppState>) -> impl IntoResponse {
    match fetch_text(&st, format!("{}/metrics", st.config.prometheus_url), 2000).await {
        Ok(text) => Json(ebpf_payload(&text)),
        Err(e) => {
            tracing::warn!(error = %e, "ebpf metrics degraded");
            Json(json!({
                "available": false,
                "mode": "unavailable",
                "stalled_count": 0,
                "stalled_agents": [],
                "prometheus": "offline",
            }))
        }
    }
}

fn pipeline_payload(text: &str) -> Value {
    let mut providers: BTreeMap<String, serde_json::Map<String, Value>> = BTreeMap::new();
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let Some(provider) = label_value(line, "provider") else {
            continue;
        };
        let value = line
            .split_whitespace()
            .last()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        let item = providers.entry(provider.clone()).or_insert_with(|| {
            let mut map = serde_json::Map::new();
            map.insert("provider".into(), Value::String(provider));
            map.insert("latency_avg_s".into(), json!(0.0));
            map.insert("latency_count".into(), json!(0));
            map.insert("requests_ok".into(), json!(0));
            map.insert("requests_error".into(), json!(0));
            map.insert("tokens_input".into(), json!(0));
            map.insert("tokens_output".into(), json!(0));
            map
        });

        if line.starts_with("sentinel_pipeline_latency_seconds_sum") {
            item.insert("latency_avg_s".into(), json!(value));
        } else if line.starts_with("sentinel_pipeline_latency_seconds_count") {
            let count = value as i64;
            let sum = item
                .get("latency_avg_s")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            item.insert("latency_count".into(), json!(count));
            if count > 0 {
                item.insert("latency_avg_s".into(), json!(sum / count as f64));
            }
        } else if line.starts_with("sentinel_pipeline_requests_total") {
            let key = if label_value(line, "status").as_deref() == Some("ok") {
                "requests_ok"
            } else {
                "requests_error"
            };
            item.insert(key.into(), json!(value as i64));
        } else if line.starts_with("sentinel_pipeline_tokens_total") {
            let key = if label_value(line, "direction").as_deref() == Some("input") {
                "tokens_input"
            } else {
                "tokens_output"
            };
            item.insert(key.into(), json!(value as i64));
        }
    }

    let providers = providers
        .into_values()
        .map(Value::Object)
        .collect::<Vec<_>>();
    json!({ "available": true, "providers": providers, "gateway": "ok" })
}

pub async fn pipeline(State(st): State<AppState>) -> impl IntoResponse {
    match fetch_text(
        &st,
        format!("{}/metrics", st.config.gateway_proxy_url),
        1500,
    )
    .await
    {
        Ok(text) => Json(pipeline_payload(&text)),
        Err(e) => {
            tracing::warn!(error = %e, "pipeline metrics degraded");
            Json(json!({ "available": false, "providers": [], "gateway": "offline" }))
        }
    }
}

pub async fn tick(State(st): State<AppState>) -> impl IntoResponse {
    match fetch_text(&st, format!("{}/metrics", st.config.prometheus_url), 2000).await {
        Ok(text) => Json(json!({
            "available": true,
            "tick_duration_ms": metric_number(&text, "sentinel_tick_duration_ms") as i64,
            "tick_rate_effective_ms": metric_number(&text, "sentinel_tick_rate_effective_ms") as i64,
            "psi_cpu_avg10": metric_number(&text, "sentinel_psi_cpu_avg10") / 1000.0,
            "psi_mem_avg10": metric_number(&text, "sentinel_psi_mem_avg10") / 1000.0,
            "psi_io_avg10": metric_number(&text, "sentinel_psi_io_avg10") / 1000.0,
            "prometheus": "ok",
        })),
        Err(e) => {
            tracing::warn!(error = %e, "tick metrics degraded");
            Json(json!({
                "available": false,
                "tick_duration_ms": 0,
                "tick_rate_effective_ms": 0,
                "psi_cpu_avg10": 0.0,
                "psi_mem_avg10": 0.0,
                "psi_io_avg10": 0.0,
                "prometheus": "offline",
            }))
        }
    }
}

/// Kanonische Phasen-Reihenfolge der ECS-Simulation (#381).
const PHASE_ORDER: [&str; 10] = [
    "input",
    "biology",
    "physics",
    "transit",
    "chaos",
    "mood",
    "perception",
    "decision",
    "output",
    "persist",
];

/// Parst die `sentinel_phase_duration_ms`-Summary von :9090 in das
/// Profiling-JSON der Console (#381). Reihenfolge = `PHASE_ORDER`,
/// unbekannte Phasen folgen alphabetisch dahinter.
fn phases_payload(text: &str) -> Value {
    // (p50_ms, p95_ms, count, sum_ms)
    let mut by_phase: BTreeMap<String, (f64, f64, i64, f64)> = BTreeMap::new();
    for line in text
        .lines()
        .filter(|l| l.starts_with("sentinel_phase_duration_ms"))
    {
        let Some(phase) = label_value(line, "phase") else {
            continue;
        };
        let value = line
            .split_whitespace()
            .last()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        let entry = by_phase.entry(phase).or_insert((0.0, 0.0, 0, 0.0));
        // _count/_sum VOR den quantile-Zeilen pruefen: gleicher Basisname!
        if line.starts_with("sentinel_phase_duration_ms_count") {
            entry.2 = value as i64;
        } else if line.starts_with("sentinel_phase_duration_ms_sum") {
            entry.3 = value;
        } else if label_value(line, "quantile").as_deref() == Some("0.5") {
            entry.0 = value;
        } else if label_value(line, "quantile").as_deref() == Some("0.95") {
            entry.1 = value;
        }
    }

    let mut ordered: Vec<(String, (f64, f64, i64, f64))> = Vec::new();
    for p in PHASE_ORDER {
        if let Some(v) = by_phase.remove(p) {
            ordered.push((p.to_string(), v));
        }
    }
    ordered.extend(by_phase);

    let phases: Vec<Value> = ordered
        .into_iter()
        .map(|(phase, (p50, p95, count, sum))| {
            json!({
                "phase": phase,
                "p50_ms": p50,
                "p95_ms": p95,
                "count": count,
                "sum_ms": sum,
                "avg_ms": if count > 0 { sum / count as f64 } else { 0.0 },
            })
        })
        .collect();

    json!({ "available": !phases.is_empty(), "phases": phases, "prometheus": "ok" })
}

pub async fn phases(State(st): State<AppState>) -> impl IntoResponse {
    match fetch_text(&st, format!("{}/metrics", st.config.prometheus_url), 2000).await {
        Ok(text) => Json(phases_payload(&text)),
        Err(e) => {
            tracing::warn!(error = %e, "phase metrics degraded");
            Json(json!({ "available": false, "phases": [], "prometheus": "offline" }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# HELP sentinel_phase_duration_ms ECS SimulationPhase duration per tick (ms)
# TYPE sentinel_phase_duration_ms summary
sentinel_phase_duration_ms{phase=\"persist\",quantile=\"0.5\"} 5
sentinel_phase_duration_ms{phase=\"persist\",quantile=\"0.95\"} 25
sentinel_phase_duration_ms{phase=\"persist\",quantile=\"0.99\"} 100
sentinel_phase_duration_ms_sum{phase=\"persist\"} 600
sentinel_phase_duration_ms_count{phase=\"persist\"} 100
sentinel_phase_duration_ms{phase=\"input\",quantile=\"0.5\"} 0.05
sentinel_phase_duration_ms{phase=\"input\",quantile=\"0.95\"} 0.25
sentinel_phase_duration_ms{phase=\"input\",quantile=\"0.99\"} 1
sentinel_phase_duration_ms_sum{phase=\"input\"} 7.5
sentinel_phase_duration_ms_count{phase=\"input\"} 100
sentinel_tick_duration_ms 42
";

    #[test]
    fn phases_payload_parses_summary_in_canonical_order() {
        let payload = phases_payload(SAMPLE);
        assert_eq!(payload["available"], json!(true));
        let phases = payload["phases"].as_array().unwrap();
        assert_eq!(phases.len(), 2);
        // Kanonische Reihenfolge: input vor persist (trotz umgekehrter Text-Reihenfolge).
        assert_eq!(phases[0]["phase"], json!("input"));
        assert_eq!(phases[0]["p50_ms"], json!(0.05));
        assert_eq!(phases[0]["p95_ms"], json!(0.25));
        assert_eq!(phases[1]["phase"], json!("persist"));
        assert_eq!(phases[1]["count"], json!(100));
        assert_eq!(phases[1]["sum_ms"], json!(600.0));
        assert_eq!(phases[1]["avg_ms"], json!(6.0));
    }

    #[test]
    fn phases_payload_empty_text_is_unavailable() {
        let payload = phases_payload("sentinel_tick_duration_ms 42\n");
        assert_eq!(payload["available"], json!(false));
        assert!(payload["phases"].as_array().unwrap().is_empty());
    }

    #[test]
    fn phases_payload_missing_quantile_defaults_to_zero() {
        let text = "\
sentinel_phase_duration_ms{phase=\"mood\",quantile=\"0.5\"} 0.01
sentinel_phase_duration_ms_count{phase=\"mood\"} 3
sentinel_phase_duration_ms_sum{phase=\"mood\"} 0.03
";
        let payload = phases_payload(text);
        let phases = payload["phases"].as_array().unwrap();
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0]["p95_ms"], json!(0.0));
        assert_eq!(phases[0]["avg_ms"], json!(0.01));
    }

    #[test]
    fn phases_payload_unknown_phase_is_appended_after_canonical() {
        let text = "\
sentinel_phase_duration_ms{phase=\"persist\",quantile=\"0.5\"} 1
sentinel_phase_duration_ms_count{phase=\"persist\"} 1
sentinel_phase_duration_ms_sum{phase=\"persist\"} 1
sentinel_phase_duration_ms{phase=\"zukunft\",quantile=\"0.5\"} 2
sentinel_phase_duration_ms_count{phase=\"zukunft\"} 1
sentinel_phase_duration_ms_sum{phase=\"zukunft\"} 2
";
        let payload = phases_payload(text);
        let phases = payload["phases"].as_array().unwrap();
        assert_eq!(phases[0]["phase"], json!("persist"));
        assert_eq!(phases[1]["phase"], json!("zukunft"));
    }
}
