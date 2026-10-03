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

async fn fetch_bounded_text(
    http: &reqwest::Client,
    url: &str,
    timeout_ms: u64,
    max_bytes: usize,
) -> Result<String, String> {
    tokio::time::timeout(Duration::from_millis(timeout_ms), async {
        let mut resp = http
            .get(url)
            .send()
            .await
            .map_err(|e| format!("fetch {url}: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("{url} returned {}", resp.status()));
        }
        let declared_length = resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        if declared_length.is_some_and(|length| length > max_bytes as u64)
            || resp
                .content_length()
                .is_some_and(|length| length > max_bytes as u64)
        {
            return Err(format!("body exceeds {max_bytes} bytes fetching {url}"));
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| format!("read {url}: {e}"))? {
            if chunk.len() > max_bytes.saturating_sub(body.len()) {
                return Err(format!("body exceeds {max_bytes} bytes fetching {url}"));
            }
            let needed = body.len() + chunk.len();
            if needed > body.capacity() {
                // Grow geometrically, but never request capacity beyond the body limit.
                let capacity = needed.max(body.capacity().saturating_mul(2)).min(max_bytes);
                body.try_reserve_exact(capacity - body.len())
                    .map_err(|e| format!("allocate body for {url}: {e}"))?;
            }
            body.extend_from_slice(&chunk);
        }
        String::from_utf8(body).map_err(|e| format!("decode {url}: {e}"))
    })
    .await
    .map_err(|_| format!("timeout fetching {url}"))?
}

fn label_value(line: &str, label: &str) -> Option<String> {
    let needle = format!("{label}=\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// Bound work on untrusted exposition without changing the other metrics endpoints.
const MAX_RESOURCE_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESOURCE_LINE_BYTES: usize = 16 * 1024;
const MAX_RESOURCE_LABELS: usize = 32;
const AGENT_CGROUP_IO_SOURCE: &str = "agent_cgroup_io_stat";

fn resource_line_parts(line: &str) -> (&str, &str) {
    let line = line.trim_start();
    let end = line
        .find(|ch: char| ch == '{' || ch.is_whitespace())
        .unwrap_or(line.len());
    (&line[..end], &line[end..])
}

#[derive(Default)]
struct ResourceValues {
    count: usize,
    sum: f64,
    mean: f64,
    invalid: bool,
}

impl ResourceValues {
    fn add(&mut self, value: Option<f64>) {
        let Some(value) = value else {
            self.invalid = true;
            return;
        };
        self.count += 1;
        self.sum += value;
        self.mean += (value - self.mean) / self.count as f64;
    }

    fn total(&self) -> Option<f64> {
        (!self.invalid && self.count > 0 && self.sum.is_finite()).then_some(self.sum)
    }

    fn average(&self) -> Option<f64> {
        (!self.invalid && self.count > 0 && self.mean.is_finite()).then_some(self.mean)
    }

    fn single(&self) -> Option<f64> {
        if self.count == 1 {
            self.total()
        } else {
            None
        }
    }
}

struct ResourceSample {
    labels: BTreeMap<String, String>,
    value: Option<f64>,
}

impl ResourceSample {
    fn label(&self, name: &str) -> Option<&str> {
        self.labels.get(name).map(String::as_str)
    }

    fn identity(&self, name: &str) -> Option<&str> {
        self.label(name).filter(|value| !value.is_empty())
    }
}

fn resource_label_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn resource_counter(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.fract() == 0.0)
}

// No suitable exposition parser is a backend dependency. Decode only the text
// format emitted by the exporter, including its three legal label escapes.
fn resource_sample(mut rest: &str) -> Option<ResourceSample> {
    if rest.len() > MAX_RESOURCE_LINE_BYTES {
        return None;
    }
    let mut labels = BTreeMap::new();
    if let Some(tail) = rest.strip_prefix('{') {
        rest = tail.trim_start();
        while !rest.starts_with('}') {
            if labels.len() >= MAX_RESOURCE_LABELS {
                return None;
            }
            let (key, tail) = rest.split_once('=')?;
            let key = key.trim();
            if !resource_label_name(key) {
                return None;
            }
            let tail = tail.trim_start().strip_prefix('"')?;
            let mut value = String::new();
            let mut escaped = false;
            let mut end = None;
            for (index, ch) in tail.char_indices() {
                if escaped {
                    value.push(match ch {
                        '\\' => '\\',
                        '"' => '"',
                        'n' => '\n',
                        _ => return None,
                    });
                    escaped = false;
                } else {
                    match ch {
                        '\\' => escaped = true,
                        '"' => {
                            end = Some(index + 1);
                            break;
                        }
                        '\n' | '\r' => return None,
                        _ => value.push(ch),
                    }
                }
            }
            let end = end?;
            rest = tail.get(end..)?.trim_start();
            if labels.insert(key.to_string(), value).is_some() {
                return None;
            }
            if let Some(tail) = rest.strip_prefix(',') {
                rest = tail.trim_start();
            } else if !rest.starts_with('}') {
                return None;
            }
        }
        rest = rest.strip_prefix('}')?;
    }
    if !rest.starts_with(char::is_whitespace) {
        return Some(ResourceSample {
            labels,
            value: None,
        });
    }
    let mut fields = rest.split_whitespace();
    let mut value = fields
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0);
    // A Prometheus timestamp is optional, but is never the sample value.
    if fields
        .next()
        .is_some_and(|timestamp| timestamp.parse::<i64>().is_err())
        || fields.next().is_some()
    {
        value = None;
    }
    Some(ResourceSample { labels, value })
}

fn empty_ebpf_payload(available: bool) -> Value {
    json!({
        "available": available,
        "mode": if available { "unknown" } else { "unavailable" },
        "stalled_count": null,
        "stalled_agents": [],
        "collection_cycle_us": null,
        "ring_buffer_drops": null,
        "io_read_bytes": null,
        "io_write_bytes": null,
        "io_source": null,
        "avg_stress": null,
    })
}

fn ebpf_payload(text: &str) -> Value {
    if text.len() > MAX_RESOURCE_TEXT_BYTES {
        return empty_ebpf_payload(true);
    }
    let mut reads = ResourceValues::default();
    let mut writes = ResourceValues::default();
    let mut stress = ResourceValues::default();
    let mut cycle = ResourceValues::default();
    let mut drops = ResourceValues::default();
    let mut stalled_count = ResourceValues::default();
    let mut io_sources = ResourceValues::default();
    let mut mode = None;
    let mut modes = ResourceValues::default();
    let mut stalled = BTreeMap::new();
    let mut ages: BTreeMap<String, ResourceValues> = BTreeMap::new();
    let mut ages_unavailable = false;
    let mut cgroup_names: BTreeMap<String, Option<String>> = BTreeMap::new();

    for line in text.lines() {
        let (family, rest) = resource_line_parts(line);
        if !matches!(
            family,
            "sentinel_io_bytes_total"
                | "sentinel_io_collection_source"
                | "sentinel_agent_cpu_pressure_stress"
                | "sentinel_ebpf_collector_cycle_microseconds"
                | "sentinel_ebpf_ring_buffer_drops_total"
                | "sentinel_agent_stalled_total"
                | "sentinel_ebpf_monitoring_mode"
                | "sentinel_agent_stalled"
                | "sentinel_agent_last_write_seconds"
        ) {
            continue;
        }
        let sample = resource_sample(rest);
        let value = sample.as_ref().and_then(|sample| sample.value);
        let counter = resource_counter(value);
        match family {
            "sentinel_io_bytes_total" => {
                match sample.as_ref().and_then(|sample| sample.label("direction")) {
                    Some("read") => reads.add(counter),
                    Some("write") => writes.add(counter),
                    Some(_) => {}
                    None => {
                        // A damaged label set cannot be assigned safely to either direction.
                        reads.add(None);
                        writes.add(None);
                    }
                }
                if let Some(sample) = &sample {
                    if let (Some(id), Some(name)) =
                        (sample.identity("cgroup_id"), sample.identity("cgroup_name"))
                    {
                        cgroup_names
                            .entry(id.to_string())
                            .and_modify(|existing| {
                                if existing.as_deref() != Some(name) {
                                    *existing = None;
                                }
                            })
                            .or_insert_with(|| Some(name.to_string()));
                    }
                }
            }
            "sentinel_agent_cpu_pressure_stress" => {
                stress.add(value.filter(|value| *value <= 1.0));
            }
            "sentinel_ebpf_collector_cycle_microseconds" => cycle.add(counter),
            "sentinel_ebpf_ring_buffer_drops_total" => drops.add(counter),
            "sentinel_agent_stalled_total" => stalled_count.add(counter),
            "sentinel_io_collection_source" => {
                let source = sample
                    .as_ref()
                    .filter(|sample| sample.labels.len() == 1)
                    .and_then(|sample| sample.label("source"));
                io_sources.add(
                    value.filter(|value| *value == 1.0 && source == Some(AGENT_CGROUP_IO_SOURCE)),
                );
            }
            "sentinel_ebpf_monitoring_mode" => {
                modes.add(value);
                if value == Some(1.0) {
                    mode = sample
                        .as_ref()
                        .and_then(|sample| sample.identity("mode"))
                        .map(str::to_string);
                }
            }
            "sentinel_agent_stalled" | "sentinel_agent_last_write_seconds" => {
                let Some(sample) = sample else {
                    ages_unavailable |= family == "sentinel_agent_last_write_seconds";
                    continue;
                };
                let Some(id) = sample.identity("cgroup_id") else {
                    ages_unavailable |= family == "sentinel_agent_last_write_seconds";
                    continue;
                };
                if family == "sentinel_agent_last_write_seconds" {
                    ages.entry(id.to_string()).or_default().add(counter);
                } else if value == Some(1.0) {
                    stalled.insert(
                        id.to_string(),
                        (
                            sample.identity("agent").map(str::to_string),
                            sample.identity("cgroup_name").map(str::to_string),
                        ),
                    );
                }
            }
            _ => {}
        }
    }

    let stalled_agents = stalled
        .into_iter()
        .map(|(id, (agent, name))| {
            let name = name.or_else(|| cgroup_names.get(&id).cloned().flatten());
            let agent = agent
                .or_else(|| name.clone())
                .unwrap_or_else(|| format!("cgroup:{id}"));
            // An unassignable damaged age sample might belong to any cgroup.
            let seconds = if ages_unavailable {
                None
            } else {
                ages.get(&id).and_then(ResourceValues::single)
            };
            json!({
                "agent": agent,
                "cgroup_id": id,
                "cgroup_name": name,
                "seconds": seconds,
            })
        })
        .collect::<Vec<_>>();
    let mode = if modes.single() == Some(1.0) {
        mode
    } else {
        None
    }
    .unwrap_or_else(|| "unknown".into());
    // The marker attests complete parent-cgroup coverage. Legacy or partial
    // counters must not be presented as the new agent-wide block totals.
    let io_source = io_sources.single().map(|_| AGENT_CGROUP_IO_SOURCE);

    json!({
        "available": true,
        "mode": mode,
        "stalled_count": stalled_count.single(),
        "stalled_agents": stalled_agents,
        "collection_cycle_us": cycle.single(),
        "ring_buffer_drops": drops.single(),
        "io_read_bytes": io_source.and_then(|_| reads.total()),
        "io_write_bytes": io_source.and_then(|_| writes.total()),
        "io_source": io_source,
        "avg_stress": stress.average(),
    })
}

pub async fn ebpf(State(st): State<AppState>) -> impl IntoResponse {
    match fetch_bounded_text(
        &st.http,
        &format!("{}/metrics", st.config.prometheus_url),
        2000,
        MAX_RESOURCE_TEXT_BYTES,
    )
    .await
    {
        Ok(text) => Json(ebpf_payload(&text)),
        Err(e) => {
            tracing::warn!(error = %e, "ebpf metrics degraded");
            let mut payload = empty_ebpf_payload(false);
            payload["prometheus"] = json!("offline");
            Json(payload)
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

const TICK_METRICS: [(&str, &str); 5] = [
    ("tick_duration_ms", "sentinel_tick_duration_ms"),
    ("tick_rate_effective_ms", "sentinel_tick_rate_effective_ms"),
    ("psi_cpu_avg10", "sentinel_psi_cpu_avg10"),
    ("psi_mem_avg10", "sentinel_psi_mem_avg10"),
    ("psi_io_avg10", "sentinel_psi_io_avg10"),
];
const PSI_AVG10_SCALE: f64 = 1000.0;
const PSI_AVAILABILITY_METRICS: [&str; 3] = [
    "sentinel_psi_cpu_sample_available",
    "sentinel_psi_mem_sample_available",
    "sentinel_psi_io_sample_available",
];

fn empty_tick_payload(available: bool) -> Value {
    json!({
        "available": available,
        "tick_duration_ms": null,
        "tick_rate_effective_ms": null,
        "psi_cpu_avg10": null,
        "psi_mem_avg10": null,
        "psi_io_avg10": null,
        "prometheus": if available { "ok" } else { "offline" },
    })
}

fn tick_payload(text: &str) -> Value {
    let mut payload = empty_tick_payload(true);
    if text.len() > MAX_RESOURCE_TEXT_BYTES {
        return payload;
    }
    let mut values: [ResourceValues; TICK_METRICS.len()] =
        std::array::from_fn(|_| ResourceValues::default());
    let mut availability: [ResourceValues; PSI_AVAILABILITY_METRICS.len()] =
        std::array::from_fn(|_| ResourceValues::default());
    for line in text.lines() {
        let (family, rest) = resource_line_parts(line);
        if let Some(index) = PSI_AVAILABILITY_METRICS
            .iter()
            .position(|name| *name == family)
        {
            let value = resource_sample(rest)
                .filter(|sample| sample.labels.is_empty())
                .and_then(|sample| sample.value)
                .filter(|value| *value == 1.0);
            availability[index].add(value);
            continue;
        }
        let Some(index) = TICK_METRICS.iter().position(|(_, name)| *name == family) else {
            continue;
        };
        let value = resource_sample(rest)
            .filter(|sample| sample.labels.is_empty())
            .and_then(|sample| resource_counter(sample.value));
        let value = value.filter(|value| {
            if index < 2 {
                // The daemon emits i64 gauges; keep conversion below the rounded 2^63 bound.
                *value < i64::MAX as f64
            } else {
                *value <= PSI_AVG10_SCALE
            }
        });
        values[index].add(value);
    }
    for (index, (field, _)) in TICK_METRICS.iter().enumerate() {
        let value = values[index].single();
        payload[*field] = if index < 2 {
            json!(value.map(|value| value as i64))
        } else {
            // Daemon PSI avg10 is a percentage multiplied by ten, not a fraction.
            json!(value
                .filter(|_| availability[index - 2].single() == Some(1.0))
                .map(|value| value / PSI_AVG10_SCALE))
        };
    }
    payload
}

pub async fn tick(State(st): State<AppState>) -> impl IntoResponse {
    match fetch_bounded_text(
        &st.http,
        &format!("{}/metrics", st.config.prometheus_url),
        2000,
        MAX_RESOURCE_TEXT_BYTES,
    )
    .await
    {
        Ok(text) => Json(tick_payload(&text)),
        Err(e) => {
            tracing::warn!(error = %e, "tick metrics degraded");
            Json(empty_tick_payload(false))
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const RESOURCE_FIELDS: [&str; 6] = [
        "io_read_bytes",
        "io_write_bytes",
        "avg_stress",
        "collection_cycle_us",
        "ring_buffer_drops",
        "stalled_count",
    ];

    fn observed_io_payload(text: &str) -> Value {
        ebpf_payload(&format!(
            "sentinel_io_collection_source{{source=\"{AGENT_CGROUP_IO_SOURCE}\"}} 1\n{text}"
        ))
    }

    fn observed_tick_payload(text: &str) -> Value {
        let markers = PSI_AVAILABILITY_METRICS
            .iter()
            .map(|family| format!("{family} 1\n"))
            .collect::<String>();
        tick_payload(&format!("{markers}{text}"))
    }

    async fn fetch_from_local_http<F, Fut>(
        respond: F,
        timeout_ms: u64,
        max_bytes: usize,
    ) -> Result<String, String>
    where
        F: FnOnce(tokio::net::TcpStream) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let url = format!("http://{}/metrics", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let mut used = 0;
            while !request[..used]
                .windows(4)
                .any(|window| window == b"\r\n\r\n")
            {
                assert!(used < request.len(), "test HTTP request too large");
                let count = socket.read(&mut request[used..]).await.unwrap();
                assert!(count > 0, "test HTTP request ended before its headers");
                used += count;
            }
            respond(socket).await;
        });
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        // Bound the test itself so a removed production deadline fails instead of hanging.
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            fetch_bounded_text(&http, &url, timeout_ms, max_bytes),
        )
        .await;
        server.abort();
        let joined = server.await;
        assert!(
            joined.is_ok()
                || joined
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.is_cancelled())
        );
        result.expect("resource fetch exceeded the test deadline")
    }

    #[tokio::test]
    async fn resource_fetch_rejects_oversized_content_length_before_body() {
        let error = fetch_from_local_http(
            |mut socket| async move {
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 65\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                std::future::pending::<()>().await;
            },
            1000,
            64,
        )
        .await
        .unwrap_err();
        assert!(error.contains("body exceeds 64 bytes"), "{error}");
    }

    #[tokio::test]
    async fn resource_fetch_rejects_chunked_and_close_delimited_oversize() {
        for chunked in [true, false] {
            let error = fetch_from_local_http(move |mut socket| async move {
                let headers: &[u8] = if chunked {
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
                } else {
                    b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n"
                };
                if socket.write_all(headers).await.is_err() { return; }
                for chunk in [&[b'a'; 32][..], &[b'b'; 32][..], &b"c"[..]] {
                    if chunked && socket.write_all(format!("{:x}\r\n", chunk.len()).as_bytes()).await.is_err() {
                        return;
                    }
                    if socket.write_all(chunk).await.is_err() { return; }
                    if chunked && socket.write_all(b"\r\n").await.is_err() { return; }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                if chunked { let _ = socket.write_all(b"0\r\n\r\n").await; }
            }, 1000, 64).await.unwrap_err();
            assert!(error.contains("body exceeds 64 bytes"), "{error}");
        }
    }

    #[tokio::test]
    async fn resource_fetch_accepts_chunked_body_at_exact_limit() {
        let body = "sentinel_tick_duration_ms 0\n";
        let text = fetch_from_local_http(
            |mut socket| async move {
                if socket.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
            ).await.is_err() { return; }
                for chunk in [&b"sentinel_tick_duration_ms "[..], &b"0\n"[..]] {
                    if socket
                        .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if socket.write_all(chunk).await.is_err() {
                        return;
                    }
                    if socket.write_all(b"\r\n").await.is_err() {
                        return;
                    }
                }
                let _ = socket.write_all(b"0\r\n\r\n").await;
            },
            1000,
            body.len(),
        )
        .await
        .unwrap();
        assert_eq!(text, body);
        assert_eq!(tick_payload(&text)["tick_duration_ms"], json!(0));
    }

    #[tokio::test]
    async fn resource_fetch_deadline_covers_continuously_streaming_body() {
        let error = fetch_from_local_http(
            |mut socket| async move {
                if socket.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
            ).await.is_err() { return; }
                for _ in 0..10 {
                    if socket.write_all(b"1\r\nx\r\n").await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(60)).await;
                }
                let _ = socket.write_all(b"0\r\n\r\n").await;
            },
            200,
            64,
        )
        .await
        .unwrap_err();
        assert!(error.contains("timeout fetching"), "{error}");
    }

    #[tokio::test]
    async fn resource_fetch_uses_one_deadline_for_headers_and_body() {
        let error = fetch_from_local_http(
            |mut socket| async move {
                tokio::time::sleep(Duration::from_millis(600)).await;
                if socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n")
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(600)).await;
                let _ = socket.write_all(b"x").await;
            },
            1000,
            64,
        )
        .await
        .unwrap_err();
        assert!(error.contains("timeout fetching"), "{error}");
    }

    #[tokio::test]
    async fn resource_fetch_rejects_http_errors_and_invalid_utf8() {
        for (response, expected) in [
            (&b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"[..], "returned 500"),
            (&b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n\xff"[..], "decode"),
        ] {
            let error = fetch_from_local_http(move |mut socket| async move {
                let _ = socket.write_all(response).await;
            }, 1000, 64).await.unwrap_err();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn ebpf_exporter_shaped_multi_agent_snapshot() {
        let text = r#"# HELP sentinel_io_bytes_total Total I/O bytes per cgroup
# TYPE sentinel_io_bytes_total counter
sentinel_ebpf_monitoring_mode{mode="userspace"} 1
sentinel_ebpf_collector_cycle_microseconds 5000
sentinel_ebpf_ring_buffer_drops_total 3
sentinel_agent_stalled{cgroup_id="42",agent="AGENT-07"} 1
sentinel_agent_last_write_seconds{cgroup_id="42",agent="AGENT-07"} 65
sentinel_agent_stalled_total 1
sentinel_io_ops_total{cgroup_id="42",cgroup_name="agent-07",direction="read"} 999
sentinel_io_bytes_total{cgroup_id="42",cgroup_name="agent-07",direction="read"} 409600
sentinel_io_bytes_total{cgroup_id="42",cgroup_name="agent-07",direction="write"} 8192
sentinel_io_bytes_total{cgroup_id="84",cgroup_name="agent-08",direction="read"} 2048
sentinel_io_bytes_total{cgroup_id="84",cgroup_name="agent-08",direction="write"} 16384
sentinel_agent_cpu_pressure_stress{agent="AGENT-07"} 0.2500
sentinel_agent_cpu_pressure_stress{agent="AGENT-08"} 0.7500
sentinel_agent_cpu_pressure_stress{agent="AGENT-09"} 0.5000
"#;
        let payload = observed_io_payload(text);
        assert_eq!(payload["available"], json!(true));
        assert_eq!(payload["mode"], json!("userspace"));
        assert_eq!(payload["io_read_bytes"], json!(411648.0));
        assert_eq!(payload["io_write_bytes"], json!(24576.0));
        assert_eq!(payload["avg_stress"], json!(0.5));
        assert_eq!(payload["collection_cycle_us"], json!(5000.0));
        assert_eq!(payload["ring_buffer_drops"], json!(3.0));
        assert_eq!(payload["stalled_count"], json!(1.0));
        assert_eq!(
            payload["stalled_agents"],
            json!([{
                "agent": "AGENT-07", "cgroup_id": "42", "cgroup_name": "agent-07",
                "seconds": 65.0,
            }])
        );
        assert_eq!(payload["io_source"], json!(AGENT_CGROUP_IO_SOURCE));
        assert!(payload.get("io_basis").is_none());
    }

    #[test]
    fn ebpf_absent_resources_are_null_but_scrape_is_available() {
        for text in [
            "",
            "# TYPE sentinel_io_bytes_total counter\n",
            "sentinel_tick_duration_ms 42\n",
        ] {
            let payload = ebpf_payload(text);
            assert_eq!(payload["available"], json!(true));
            assert_eq!(payload["mode"], json!("unknown"));
            for field in RESOURCE_FIELDS {
                assert_eq!(payload.get(field), Some(&Value::Null), "{field}");
            }
            assert_eq!(payload["stalled_agents"], json!([]));
            assert_eq!(payload.get("io_source"), Some(&Value::Null));
        }
    }

    #[test]
    fn ebpf_measured_zero_is_not_absence() {
        let payload = observed_io_payload(
            r#"sentinel_io_bytes_total{cgroup_id="1",cgroup_name="one",direction="read"} 0
sentinel_io_bytes_total{cgroup_id="1",cgroup_name="one",direction="write"} 0
sentinel_agent_cpu_pressure_stress{agent="one"} 0.0000
sentinel_ebpf_collector_cycle_microseconds 0
sentinel_ebpf_ring_buffer_drops_total 0
sentinel_agent_stalled_total 0
"#,
        );
        for field in RESOURCE_FIELDS {
            assert_eq!(payload[field], json!(0.0), "{field}");
        }
        let one_direction = observed_io_payload("sentinel_io_bytes_total{direction=\"read\"} 0\n");
        assert_eq!(one_direction["io_read_bytes"], json!(0.0));
        assert!(one_direction["io_write_bytes"].is_null());
    }

    #[test]
    fn ebpf_invalid_samples_poison_their_aggregate_not_valid_directions() {
        for value in [
            "",
            "bad",
            "NaN",
            "+Inf",
            "-Inf",
            "inf",
            "1e999",
            "-1",
            "4 junk",
            "4 123 extra",
        ] {
            let text = format!(
                "sentinel_io_bytes_total{{direction=\"read\"}} 12\n\
                 sentinel_io_bytes_total{{direction=\"read\"}} {value}\n\
                 sentinel_io_bytes_total{{direction=\"write\"}} 0\n\
                 sentinel_agent_cpu_pressure_stress{{agent=\"one\"}} 0.5\n\
                 sentinel_agent_cpu_pressure_stress{{agent=\"two\"}} {value}\n\
                 sentinel_ebpf_collector_cycle_microseconds {value}\n\
                 sentinel_ebpf_ring_buffer_drops_total {value}\n\
                 sentinel_agent_stalled_total {value}\n"
            );
            let payload = observed_io_payload(&text);
            assert_eq!(payload["available"], json!(true));
            assert_eq!(payload["io_write_bytes"], json!(0.0));
            for field in RESOURCE_FIELDS
                .into_iter()
                .filter(|field| *field != "io_write_bytes")
            {
                assert!(payload[field].is_null(), "{field}: {value}");
            }
        }
    }

    #[test]
    fn ebpf_integer_resources_reject_fractions_and_stress_is_bounded() {
        for value in ["-0.5", "0.5", "1.25"] {
            let payload = observed_io_payload(&format!(
                "sentinel_io_bytes_total{{direction=\"read\"}} 0\n\
                 sentinel_io_bytes_total{{direction=\"read\"}} {value}\n\
                 sentinel_io_bytes_total{{direction=\"write\"}} {value}\n\
                 sentinel_ebpf_collector_cycle_microseconds {value}\n\
                 sentinel_ebpf_ring_buffer_drops_total {value}\n\
                 sentinel_agent_stalled_total {value}\n\
                 sentinel_agent_stalled{{cgroup_id=\"1\"}} 1\n\
                 sentinel_agent_last_write_seconds{{cgroup_id=\"1\"}} {value}\n"
            ));
            for field in RESOURCE_FIELDS {
                assert!(payload[field].is_null(), "{field}: {value}");
            }
            assert!(payload["stalled_agents"][0]["seconds"].is_null());
        }
        for value in ["-0.1", "1.01", "1e308"] {
            let payload = ebpf_payload(&format!(
                "sentinel_agent_cpu_pressure_stress{{agent=\"one\"}} 0.5\n\
                 sentinel_agent_cpu_pressure_stress{{agent=\"two\"}} {value}\n"
            ));
            assert!(payload["avg_stress"].is_null(), "{value}");
        }
        let payload = ebpf_payload(
            r#"sentinel_agent_cpu_pressure_stress{agent="one"} 0
sentinel_agent_cpu_pressure_stress{agent="two"} 1
"#,
        );
        assert_eq!(payload["avg_stress"], json!(0.5));
    }

    #[test]
    fn ebpf_only_exact_families_and_direction_labels_are_used() {
        let payload = observed_io_payload(
            r#"sentinel_io_bytes_total_extra{direction="read"} NaN
sentinel_io_bytes_total_sum{direction="read"} 999
sentinel_io_ops_total{direction="write"} 999
sentinel_agent_cpu_pressure_stress_extra{agent="one"} 999
sentinel_ebpf_collector_cycle_microseconds_extra 999
sentinel_agent_stalled_total_extra 999
sentinel_agent_stalled_extra{cgroup_id="1"} 1
sentinel_agent_last_write_seconds_extra{cgroup_id="1"} 999
sentinel_ebpf_ring_buffer_drops_total_extra 999
sentinel_ebpf_monitoring_mode_extra{mode="kernel"} 1
sentinel_io_bytes_total{direction="bread"} 999
sentinel_io_bytes_total{direction="write-more"} 999
sentinel_io_bytes_total{other_direction="write",direction="read"} 4
sentinel_io_bytes_total{direction="write"} 8
"#,
        );
        assert_eq!(payload["io_read_bytes"], json!(4.0));
        assert_eq!(payload["io_write_bytes"], json!(8.0));
        for field in &RESOURCE_FIELDS[2..] {
            assert!(payload[*field].is_null(), "{field}");
        }
        assert_eq!(payload["mode"], json!("unknown"));
        assert_eq!(payload["stalled_agents"], json!([]));
    }

    #[test]
    fn ebpf_decodes_escaped_labels_without_matching_text_inside_values() {
        let payload = observed_io_payload(
            r#"sentinel_agent_stalled{cgroup_id="42",agent="Tobias \"Tobi\" Lehmann"} 1
sentinel_agent_last_write_seconds{agent="different name",cgroup_id="42"} 65
sentinel_io_bytes_total{cgroup_id="42",cgroup_name="cg \"weird\" \\name\nnext } , direction=\"write\"",direction="read"} 7
sentinel_io_bytes_total{direction="write",cgroup_id="42",cgroup_name="cg \"weird\" \\name\nnext } , direction=\"write\""} 9
"#,
        );
        assert_eq!(payload["io_read_bytes"], json!(7.0));
        assert_eq!(payload["io_write_bytes"], json!(9.0));
        assert_eq!(
            payload["stalled_agents"][0]["agent"],
            json!("Tobias \"Tobi\" Lehmann")
        );
        assert_eq!(
            payload["stalled_agents"][0]["cgroup_name"],
            json!("cg \"weird\" \\name\nnext } , direction=\"write\"")
        );
        assert_eq!(payload["stalled_agents"][0]["seconds"], json!(65.0));
    }

    #[test]
    fn ebpf_malformed_label_sets_and_missing_direction_do_not_become_zero() {
        for line in [
            r#"sentinel_io_bytes_total{direction="read" 4"#,
            r#"sentinel_io_bytes_total{direction="read",direction="write"} 4"#,
            r#"sentinel_io_bytes_total{direction="read",cgroup_name="bad\rname"} 4"#,
            r#"sentinel_io_bytes_total{not_direction="read"} 4"#,
            r#"sentinel_io_bytes_total{direction="read" other="one"} 4"#,
            r#"sentinel_io_bytes_total{9direction="read"} 4"#,
            r#"sentinel_io_bytes_total{direction=read} 4"#,
            "sentinel_io_bytes_total",
        ] {
            let text = format!("sentinel_io_bytes_total{{direction=\"read\"}} 0\n{line}\n");
            let payload = observed_io_payload(&text);
            assert!(payload["io_read_bytes"].is_null(), "{line}");
            assert!(payload["io_write_bytes"].is_null(), "{line}");
        }
    }

    #[test]
    fn ebpf_timestamp_is_not_the_value_and_whitespace_is_supported() {
        let payload = observed_io_payload(
            "  sentinel_io_bytes_total{ direction = \"read\" }\t2.5e2 123456\n\
             sentinel_agent_cpu_pressure_stress{agent=\"one\"}\t0.25 123456\n\
             sentinel_agent_cpu_pressure_stress{agent=\"two\"} 0.75\n\
             sentinel_ebpf_monitoring_mode{mode=\"kernel\"} 1.0 123456\n",
        );
        assert_eq!(payload["io_read_bytes"], json!(250.0));
        assert_eq!(payload["avg_stress"], json!(0.5));
        assert_eq!(payload["mode"], json!("kernel"));
    }

    #[test]
    fn ebpf_stalls_join_by_cgroup_not_display_name_or_line_order() {
        let payload = ebpf_payload(
            r#"sentinel_agent_last_write_seconds{cgroup_id="2"} 0
sentinel_agent_last_write_seconds{cgroup_id="1"} 40
sentinel_agent_last_write_seconds{cgroup_id="999",agent="same"} 999
sentinel_agent_stalled{cgroup_id="1"} 1
sentinel_agent_stalled{cgroup_id="2",agent="same"} 1
sentinel_agent_stalled{cgroup_id="3",agent="same"} 1
sentinel_agent_stalled{cgroup_id="4"} 1
sentinel_agent_stalled{cgroup_id="5",cgroup_name="direct-name"} 1
sentinel_agent_stalled{cgroup_id="6",agent="healthy"} 0
sentinel_agent_stalled{agent="no-cgroup"} 1
sentinel_io_bytes_total{cgroup_id="1",cgroup_name="legacy-name",direction="read"} 0
"#,
        );
        let agents = payload["stalled_agents"].as_array().unwrap();
        assert_eq!(agents.len(), 5);
        assert_eq!(agents[0]["agent"], json!("legacy-name"));
        assert_eq!(agents[0]["seconds"], json!(40.0));
        assert_eq!(agents[1]["agent"], json!("same"));
        assert_eq!(agents[1]["seconds"], json!(0.0));
        assert_eq!(agents[2]["agent"], json!("same"));
        assert!(agents[2]["seconds"].is_null());
        assert_eq!(agents[3]["agent"], json!("cgroup:4"));
        assert!(agents[3]["seconds"].is_null());
        assert_eq!(agents[4]["agent"], json!("direct-name"));
        assert!(payload["stalled_count"].is_null());
    }

    #[test]
    fn ebpf_invalid_or_ambiguous_stall_age_is_null() {
        for age in ["bad", "NaN", "+Inf", "-1", ""] {
            let payload = ebpf_payload(&format!(
                "sentinel_agent_stalled{{cgroup_id=\"1\"}} 1\n\
                 sentinel_agent_last_write_seconds{{cgroup_id=\"1\"}} {age}\n"
            ));
            assert_eq!(payload["stalled_agents"][0]["agent"], json!("cgroup:1"));
            assert!(payload["stalled_agents"][0]["seconds"].is_null(), "{age}");
        }
        let payload = ebpf_payload(
            r#"sentinel_agent_stalled{cgroup_id="1"} 1
sentinel_agent_last_write_seconds{cgroup_id="1"} 40
sentinel_agent_last_write_seconds{cgroup_id="1"} 50
sentinel_io_bytes_total{cgroup_id="1",cgroup_name="one",direction="read"} 1
sentinel_io_bytes_total{cgroup_id="1",cgroup_name="different",direction="write"} 1
"#,
        );
        assert!(payload["stalled_agents"][0]["seconds"].is_null());
        assert_eq!(payload["stalled_agents"][0]["agent"], json!("cgroup:1"));
        assert!(payload["stalled_agents"][0]["cgroup_name"].is_null());

        for malformed in [
            r#"sentinel_agent_last_write_seconds{cgroup_id="1",agent="bad\rname"} 50"#,
            r#"sentinel_agent_last_write_seconds{agent="no-cgroup"} 50"#,
            "sentinel_agent_last_write_seconds{",
        ] {
            let payload = ebpf_payload(&format!(
                "sentinel_agent_stalled{{cgroup_id=\"1\"}} 1\n\
                 sentinel_agent_last_write_seconds{{cgroup_id=\"1\"}} 40\n{malformed}\n"
            ));
            assert!(
                payload["stalled_agents"][0]["seconds"].is_null(),
                "{malformed}"
            );
        }
        let payload = ebpf_payload(
            r#"sentinel_agent_stalled{cgroup_id="1"} 1
sentinel_agent_stalled{cgroup_id="2"} 1
sentinel_agent_last_write_seconds{cgroup_id="1"} NaN
sentinel_agent_last_write_seconds{cgroup_id="2"} 80
"#,
        );
        assert!(payload["stalled_agents"][0]["seconds"].is_null());
        assert_eq!(payload["stalled_agents"][1]["seconds"], json!(80.0));
    }

    #[test]
    fn ebpf_overflow_and_ambiguous_singletons_are_not_fabricated() {
        let payload = observed_io_payload(
            r#"sentinel_io_bytes_total{direction="read",cgroup_id="1"} 1e308
sentinel_io_bytes_total{direction="read",cgroup_id="2"} 1e308
sentinel_agent_cpu_pressure_stress{agent="one"} 1e308
sentinel_agent_cpu_pressure_stress{agent="two"} 1e308
sentinel_ebpf_collector_cycle_microseconds 1
sentinel_ebpf_collector_cycle_microseconds 2
sentinel_ebpf_monitoring_mode{mode="userspace"} 1
sentinel_ebpf_monitoring_mode{mode="kernel"} 1
"#,
        );
        assert!(payload["io_read_bytes"].is_null());
        assert!(payload["avg_stress"].is_null());
        assert!(payload["collection_cycle_us"].is_null());
        assert_eq!(payload["mode"], json!("unknown"));
    }

    #[test]
    fn ebpf_parse_limits_return_unavailable_measurements() {
        let oversized = format!(
            "sentinel_io_bytes_total{{direction=\"read\",cgroup_name=\"{}\"}} 0",
            "a".repeat(MAX_RESOURCE_LINE_BYTES)
        );
        assert!(observed_io_payload(&oversized)["io_read_bytes"].is_null());
        let labels = (0..=MAX_RESOURCE_LABELS)
            .map(|index| format!("label_{index}=\"x\""))
            .collect::<Vec<_>>()
            .join(",");
        let too_many = format!("sentinel_io_bytes_total{{direction=\"read\",{labels}}} 0");
        assert!(observed_io_payload(&too_many)["io_read_bytes"].is_null());
        let payload = ebpf_payload(&"x".repeat(MAX_RESOURCE_TEXT_BYTES + 1));
        assert_eq!(payload["available"], json!(true));
        for field in RESOURCE_FIELDS {
            assert_eq!(payload.get(field), Some(&Value::Null));
        }
    }

    #[test]
    fn ebpf_fetch_failure_shape_has_null_resources() {
        let payload = empty_ebpf_payload(false);
        assert_eq!(payload["available"], json!(false));
        assert_eq!(payload["mode"], json!("unavailable"));
        assert_eq!(payload["stalled_agents"], json!([]));
        for field in RESOURCE_FIELDS {
            assert_eq!(payload.get(field), Some(&Value::Null));
        }
        assert_eq!(payload.get("io_source"), Some(&Value::Null));
    }

    #[test]
    fn ebpf_io_source_requires_one_recognized_observed_sample() {
        let payload = ebpf_payload(
            r#"# HELP sentinel_io_collection_source Observed I/O collection source
# TYPE sentinel_io_collection_source gauge
sentinel_io_collection_source{source="agent_cgroup_io_stat"} 1
sentinel_io_collection_source_extra{source="proc"} 1
sentinel_io_bytes_total{cgroup_id="1",cgroup_name="agent-one",direction="read"} 0
sentinel_io_bytes_total{cgroup_id="2",cgroup_name="agent-two",direction="read"} 4096
sentinel_io_bytes_total{cgroup_id="2",cgroup_name="agent-two",direction="write"} 8192
"#,
        );
        assert_eq!(payload["io_source"], json!(AGENT_CGROUP_IO_SOURCE));
        assert_eq!(payload["io_read_bytes"], json!(4096.0));
        assert_eq!(payload["io_write_bytes"], json!(8192.0));
        assert!(payload.get("io_basis").is_none());

        for mode in ["kernel", "userspace"] {
            let payload = ebpf_payload(&format!(
                "sentinel_ebpf_monitoring_mode{{mode=\"{mode}\"}} 1\n\
                 sentinel_io_bytes_total{{direction=\"read\"}} 12\n"
            ));
            assert_eq!(payload.get("io_source"), Some(&Value::Null));
            assert!(payload["io_read_bytes"].is_null());
        }
        let payload =
            ebpf_payload("sentinel_io_collection_source{source=\"agent_cgroup_io_stat\"} 1\n");
        assert_eq!(payload["io_source"], json!(AGENT_CGROUP_IO_SOURCE));
        assert!(payload["io_read_bytes"].is_null());
        assert!(payload["io_write_bytes"].is_null());
    }

    #[test]
    fn ebpf_legacy_and_partial_coverage_bytes_stay_unavailable_without_source() {
        for text in [
            r#"sentinel_ebpf_monitoring_mode{mode="kernel"} 1
sentinel_io_bytes_total{cgroup_id="1",cgroup_name="legacy-one",direction="read"} 4096
sentinel_io_bytes_total{cgroup_id="1",cgroup_name="legacy-one",direction="write"} 8192
"#,
            r#"# TYPE sentinel_io_collection_source gauge
sentinel_ebpf_monitoring_mode{mode="userspace"} 1
sentinel_agent_cpu_pressure_stress{agent="registered-one"} 0.25
sentinel_agent_cpu_pressure_stress{agent="registered-two"} 0.75
sentinel_io_bytes_total{cgroup_id="1",cgroup_name="registered-one",direction="read"} 0
sentinel_io_bytes_total{cgroup_id="1",cgroup_name="registered-one",direction="write"} 0
sentinel_agent_stalled_total 0
"#,
        ] {
            let payload = ebpf_payload(text);
            assert_eq!(payload["available"], json!(true));
            for field in ["io_source", "io_read_bytes", "io_write_bytes"] {
                assert_eq!(payload.get(field), Some(&Value::Null), "{field}");
            }
        }
        let partial = ebpf_payload(
            "sentinel_agent_cpu_pressure_stress{agent=\"one\"} 0.25\n\
             sentinel_io_bytes_total{cgroup_id=\"1\",direction=\"read\"} 4096\n",
        );
        assert_eq!(partial["avg_stress"], json!(0.25));
        assert!(partial["io_read_bytes"].is_null());
    }

    #[test]
    fn ebpf_io_source_invalid_unknown_or_ambiguous_samples_are_null() {
        for value in [
            "", "bad", "NaN", "+Inf", "-1", "0", "0.5", "2", "1e308", "1 junk",
        ] {
            let payload = ebpf_payload(&format!(
                "sentinel_io_collection_source{{source=\"agent_cgroup_io_stat\"}} {value}\n\
                 sentinel_io_bytes_total{{direction=\"read\"}} 0\n\
                 sentinel_io_bytes_total{{direction=\"write\"}} 12\n"
            ));
            assert_eq!(payload.get("io_source"), Some(&Value::Null), "{value}");
            assert!(payload["io_read_bytes"].is_null(), "{value}");
            assert!(payload["io_write_bytes"].is_null(), "{value}");
        }
        for line in [
            r#"sentinel_io_collection_source{source="bpf"} 1"#,
            r#"sentinel_io_collection_source{source="proc"} 1"#,
            r#"sentinel_io_collection_source{source="agent_cgroup_io_stat_extra"} 1"#,
            r#"sentinel_io_collection_source{not_source="agent_cgroup_io_stat"} 1"#,
            r#"sentinel_io_collection_source{source="agent_cgroup_io_stat",source="proc"} 1"#,
            r#"sentinel_io_collection_source{source="agent_cgroup_io_stat\r"} 1"#,
            r#"sentinel_io_collection_source{source="agent_cgroup_io_stat" 1"#,
            "sentinel_io_collection_source 1",
        ] {
            let payload = ebpf_payload(line);
            assert_eq!(payload.get("io_source"), Some(&Value::Null), "{line}");
        }
        for extra in [
            r#"sentinel_io_collection_source{source="agent_cgroup_io_stat"} 1"#,
            r#"sentinel_io_collection_source{source="agent_cgroup_io_stat"} NaN"#,
            r#"sentinel_io_collection_source{source="bpf"} 1"#,
        ] {
            let payload = ebpf_payload(&format!(
                "sentinel_io_collection_source{{source=\"agent_cgroup_io_stat\"}} 1\n{extra}\n\
                 sentinel_io_bytes_total{{direction=\"read\"}} 0\n\
                 sentinel_io_bytes_total{{direction=\"write\"}} 12\n"
            ));
            assert_eq!(payload.get("io_source"), Some(&Value::Null), "{extra}");
            assert!(payload["io_read_bytes"].is_null(), "{extra}");
            assert!(payload["io_write_bytes"].is_null(), "{extra}");
        }
    }

    #[test]
    fn ebpf_io_source_rejects_extra_labels_and_escaped_values_cannot_spoof_source() {
        let payload = ebpf_payload(
            "sentinel_io_collection_source{source=\"agent_cgroup_io_stat\"} 1.0 123456\n",
        );
        assert_eq!(payload["io_source"], json!(AGENT_CGROUP_IO_SOURCE));
        let payload = ebpf_payload(
            r#"sentinel_io_collection_source{note="quoted \"source\" \\name\nnext",source="agent_cgroup_io_stat"} 1.0 123456
"#,
        );
        assert_eq!(payload.get("io_source"), Some(&Value::Null));
        let payload = ebpf_payload(
            r#"sentinel_io_collection_source{note="source=\"agent_cgroup_io_stat\"",source="proc"} 1
sentinel_io_collection_source_extra{source="agent_cgroup_io_stat"} 1
"#,
        );
        assert_eq!(payload.get("io_source"), Some(&Value::Null));
        let oversized = format!(
            "sentinel_io_collection_source{{source=\"agent_cgroup_io_stat\",note=\"{}\"}} 1",
            "x".repeat(MAX_RESOURCE_LINE_BYTES)
        );
        assert_eq!(
            ebpf_payload(&oversized).get("io_source"),
            Some(&Value::Null)
        );
    }

    #[test]
    fn tick_partial_scrape_keeps_each_observed_field_individually() {
        for (index, (field, family)) in TICK_METRICS.iter().enumerate() {
            let payload = observed_tick_payload(&format!("{family} 250\n"));
            assert_eq!(payload["available"], json!(true));
            assert_eq!(payload["prometheus"], json!("ok"));
            let expected = if index < 2 { json!(250) } else { json!(0.25) };
            assert_eq!(payload[*field], expected);
            for (other, _) in TICK_METRICS {
                if other != *field {
                    assert_eq!(payload.get(other), Some(&Value::Null), "{other}");
                }
            }
        }
        for text in [
            "",
            "# HELP sentinel_tick_duration_ms Tick duration\n",
            "unrelated_metric 1\n",
        ] {
            let payload = tick_payload(text);
            assert_eq!(payload["available"], json!(true));
            for (field, _) in TICK_METRICS {
                assert_eq!(payload.get(field), Some(&Value::Null));
            }
        }
    }

    #[test]
    fn tick_measured_zero_and_daemon_psi_scale_are_preserved() {
        let text = TICK_METRICS
            .iter()
            .map(|(_, family)| format!("{family} 0\n"))
            .collect::<String>();
        let payload = observed_tick_payload(&text);
        assert_eq!(payload["available"], json!(true));
        for (field, _) in TICK_METRICS {
            assert_eq!(payload[field].as_f64(), Some(0.0), "{field}");
        }
        let payload = observed_tick_payload(
            "sentinel_tick_duration_ms 42\n\
             sentinel_tick_rate_effective_ms 1000\n\
             sentinel_psi_cpu_avg10 1\n\
             sentinel_psi_mem_avg10 375\n\
             sentinel_psi_io_avg10 1000\n",
        );
        assert_eq!(payload["tick_duration_ms"], json!(42));
        assert_eq!(payload["tick_rate_effective_ms"], json!(1000));
        assert_eq!(payload["psi_cpu_avg10"], json!(0.001));
        assert_eq!(payload["psi_mem_avg10"], json!(0.375));
        assert_eq!(payload["psi_io_avg10"], json!(1.0));
    }

    #[test]
    fn tick_default_zero_is_unavailable_without_latest_successful_observation() {
        for markers in [
            "",
            "sentinel_psi_cpu_sample_available 0\n\
             sentinel_psi_mem_sample_available 0\n\
             sentinel_psi_io_sample_available 0\n",
        ] {
            let payload = tick_payload(&format!(
                "{markers}sentinel_tick_duration_ms 0\n\
                 sentinel_tick_rate_effective_ms 0\n\
                 sentinel_psi_cpu_avg10 0\n\
                 sentinel_psi_mem_avg10 0\n\
                 sentinel_psi_io_avg10 0\n"
            ));
            assert_eq!(payload["available"], json!(true));
            assert_eq!(payload["tick_duration_ms"], json!(0));
            assert_eq!(payload["tick_rate_effective_ms"], json!(0));
            for (field, _) in &TICK_METRICS[2..] {
                assert_eq!(payload.get(*field), Some(&Value::Null));
            }
        }
        let payload = tick_payload(
            "sentinel_psi_cpu_sample_available 1\n\
             sentinel_psi_mem_sample_available 0\n\
             sentinel_psi_cpu_avg10 0\n\
             sentinel_psi_mem_avg10 0\n\
             sentinel_psi_io_avg10 0\n",
        );
        assert_eq!(payload["psi_cpu_avg10"], json!(0.0));
        assert!(payload["psi_mem_avg10"].is_null());
        assert!(payload["psi_io_avg10"].is_null());
    }

    #[test]
    fn tick_each_psi_requires_one_valid_global_availability_marker() {
        for (index, marker) in PSI_AVAILABILITY_METRICS.iter().enumerate() {
            for invalid in [
                String::new(),
                format!("{marker} 0\n"),
                format!("{marker} 2\n"),
                format!("{marker} -1\n"),
                format!("{marker} 0.5\n"),
                format!("{marker} NaN\n"),
                format!("{marker} +Inf\n"),
                format!("{marker} bad\n"),
                format!("{marker} 1 junk\n"),
                format!("{marker} 1\n{marker} 1\n"),
                format!("{marker} 1\n{marker} 0\n"),
                format!("{marker} 1\n{marker} NaN\n"),
                format!("{marker}{{agent=\"one\"}} 1\n"),
                format!("{marker} 1\n{marker}{{agent=\"one\"}} 1\n"),
                format!("{marker}{{broken=\"\\r\"}} 1\n"),
            ] {
                let mut text = format!(
                    "{invalid}sentinel_tick_duration_ms 42\n\
                     sentinel_tick_rate_effective_ms 1000\n\
                     sentinel_psi_cpu_avg10 250\n\
                     sentinel_psi_mem_avg10 250\n\
                     sentinel_psi_io_avg10 250\n"
                );
                for (other, family) in PSI_AVAILABILITY_METRICS.iter().enumerate() {
                    if other != index {
                        text.push_str(&format!("{family} 1\n"));
                    }
                }
                let payload = tick_payload(&text);
                assert_eq!(payload["available"], json!(true));
                assert_eq!(payload["tick_duration_ms"], json!(42));
                assert_eq!(payload["tick_rate_effective_ms"], json!(1000));
                for (other, (field, _)) in TICK_METRICS[2..].iter().enumerate() {
                    if other == index {
                        assert_eq!(payload.get(*field), Some(&Value::Null), "{invalid}");
                    } else {
                        assert_eq!(payload[*field], json!(0.25), "{invalid}");
                    }
                }
            }
        }
    }

    #[test]
    fn tick_observation_marker_cannot_replace_a_missing_or_ambiguous_sample() {
        let payload = observed_tick_payload("");
        for (field, _) in &TICK_METRICS[2..] {
            assert_eq!(payload.get(*field), Some(&Value::Null));
        }
        for (field, family) in &TICK_METRICS[2..] {
            for value in ["bad", "NaN", "-1", "1001", "0.5"] {
                let payload = observed_tick_payload(&format!("{family} {value}\n"));
                assert_eq!(payload.get(*field), Some(&Value::Null), "{family}: {value}");
            }
            let payload = observed_tick_payload(&format!("{family} 0\n{family} 250\n"));
            assert_eq!(payload.get(*field), Some(&Value::Null));
        }
    }

    #[test]
    fn resource_global_gauges_reject_extra_or_malformed_identity_labels() {
        for (field, family) in TICK_METRICS {
            for labels in [
                r#"{agent="one"}"#,
                r#"{instance="node"}"#,
                r#"{note="quoted \"name\" \\path"}"#,
                r#"{broken="bad\r"}"#,
            ] {
                let payload = observed_tick_payload(&format!("{family}{labels} 0\n"));
                assert_eq!(payload.get(field), Some(&Value::Null), "{family}{labels}");
            }
        }
        for labels in [
            r#"{source="agent_cgroup_io_stat",agent="one"}"#,
            r#"{source="agent_cgroup_io_stat",instance="node"}"#,
            r#"{source="agent_cgroup_io_stat",source="agent_cgroup_io_stat"}"#,
            r#"{source="agent_cgroup_io_stat",broken="bad\r"}"#,
        ] {
            let payload = ebpf_payload(&format!(
                "sentinel_io_collection_source{labels} 1\n\
                 sentinel_io_bytes_total{{cgroup_id=\"1\",direction=\"read\"}} 0\n\
                 sentinel_io_bytes_total{{cgroup_id=\"1\",direction=\"write\"}} 12\n"
            ));
            for field in ["io_source", "io_read_bytes", "io_write_bytes"] {
                assert_eq!(payload.get(field), Some(&Value::Null), "{labels}");
            }
        }
    }

    #[test]
    fn tick_availability_matches_exact_global_families_not_labels_or_suffixes() {
        let payload = tick_payload(
            r#"sentinel_psi_cpu_sample_available_extra 1
unrelated{note="sentinel_psi_cpu_sample_available"} 1
sentinel_psi_cpu_avg10 250
sentinel_psi_mem_sample_available 1.0 123456
sentinel_psi_mem_avg10 375 123456
sentinel_psi_io_sample_available{note="global \"marker\" \\path\nnext"} 1
sentinel_psi_io_avg10 500
"#,
        );
        assert!(payload["psi_cpu_avg10"].is_null());
        assert_eq!(payload["psi_mem_avg10"], json!(0.375));
        assert!(payload["psi_io_avg10"].is_null());
        let oversized = format!(
            "sentinel_psi_cpu_sample_available{{note=\"{}\"}} 1\nsentinel_psi_cpu_avg10 0\n",
            "x".repeat(MAX_RESOURCE_LINE_BYTES)
        );
        assert!(tick_payload(&oversized)["psi_cpu_avg10"].is_null());
    }

    #[test]
    fn tick_invalid_nonfinite_negative_fractional_or_out_of_range_is_null() {
        for value in [
            "",
            "bad",
            "NaN",
            "+Inf",
            "-Inf",
            "1e999",
            "-1",
            "0.5",
            "42 junk",
            "42 123 extra",
            "9223372036854775808",
            "1e308",
        ] {
            let text = TICK_METRICS
                .iter()
                .map(|(_, family)| format!("{family} {value}\n"))
                .collect::<String>();
            let payload = observed_tick_payload(&text);
            assert_eq!(payload["available"], json!(true));
            for (field, _) in TICK_METRICS {
                assert_eq!(payload.get(field), Some(&Value::Null), "{field}: {value}");
            }
        }
        let payload = observed_tick_payload(
            "sentinel_tick_duration_ms 1001\n\
             sentinel_psi_cpu_avg10 1001\n\
             sentinel_psi_mem_avg10 1000.5\n\
             sentinel_psi_io_avg10 -1\n",
        );
        assert_eq!(payload["tick_duration_ms"], json!(1001));
        assert!(payload["psi_cpu_avg10"].is_null());
        assert!(payload["psi_mem_avg10"].is_null());
        assert!(payload["psi_io_avg10"].is_null());
    }

    #[test]
    fn tick_exact_family_and_timestamp_matching_preserves_partial_values() {
        let payload = observed_tick_payload(
            "sentinel_tick_duration_ms_extra 999\n\
             sentinel_tick_rate_effective_ms_sum 999\n\
             sentinel_psi_cpu_avg10_extra 1000\n\
             sentinel_psi_mem_avg10_sum 1000\n\
             sentinel_psi_io_avg10_count 1000\n\
             unrelated{note=\"sentinel_tick_rate_effective_ms\"} 999\n\
               sentinel_tick_duration_ms\t42 123456\n\
             sentinel_psi_cpu_avg10\t250 123456\n",
        );
        assert_eq!(payload["tick_duration_ms"], json!(42));
        assert_eq!(payload["psi_cpu_avg10"], json!(0.25));
        assert!(payload["tick_rate_effective_ms"].is_null());
        assert!(payload["psi_mem_avg10"].is_null());
        assert!(payload["psi_io_avg10"].is_null());
    }

    #[test]
    fn tick_ambiguous_or_damaged_samples_do_not_override_valid_other_fields() {
        for extra in [
            "sentinel_tick_duration_ms 50",
            "sentinel_tick_duration_ms NaN",
            r#"sentinel_tick_duration_ms{note="bad\r"} 50"#,
            "sentinel_tick_duration_ms{",
        ] {
            let payload = observed_tick_payload(&format!(
                "sentinel_tick_duration_ms 42\n{extra}\n\
                 sentinel_tick_rate_effective_ms 1000\n\
                 sentinel_psi_cpu_avg10 0\n"
            ));
            assert!(payload["tick_duration_ms"].is_null(), "{extra}");
            assert_eq!(payload["tick_rate_effective_ms"], json!(1000));
            assert_eq!(payload["psi_cpu_avg10"], json!(0.0));
        }
    }

    #[test]
    fn tick_parse_limits_and_fetch_failure_have_null_fields() {
        let oversized = format!(
            "sentinel_tick_duration_ms{{note=\"{}\"}} 42\nsentinel_psi_io_avg10 1000\n",
            "x".repeat(MAX_RESOURCE_LINE_BYTES)
        );
        let payload = observed_tick_payload(&oversized);
        assert!(payload["tick_duration_ms"].is_null());
        assert_eq!(payload["psi_io_avg10"], json!(1.0));
        let labels = (0..=MAX_RESOURCE_LABELS)
            .map(|index| format!("label_{index}=\"x\""))
            .collect::<Vec<_>>()
            .join(",");
        let too_many = format!("sentinel_psi_cpu_avg10{{{labels}}} 250\n");
        assert!(observed_tick_payload(&too_many)["psi_cpu_avg10"].is_null());
        let payload = tick_payload(&"x".repeat(MAX_RESOURCE_TEXT_BYTES + 1));
        assert_eq!(payload["available"], json!(true));
        for (field, _) in TICK_METRICS {
            assert_eq!(payload.get(field), Some(&Value::Null));
        }
        let payload = empty_tick_payload(false);
        assert_eq!(payload["available"], json!(false));
        assert_eq!(payload["prometheus"], json!("offline"));
        for (field, _) in TICK_METRICS {
            assert_eq!(payload.get(field), Some(&Value::Null));
        }
    }

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
