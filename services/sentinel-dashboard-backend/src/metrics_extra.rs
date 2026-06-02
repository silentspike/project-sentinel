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
