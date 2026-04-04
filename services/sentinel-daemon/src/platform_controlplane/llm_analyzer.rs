//! Asynchroner LLM-Analyzer fuer Platform-Controlplane-Eskalationen.
//!
//! Der Worker laeuft daemon-intern, bekommt strukturierte Trigger-Requests
//! ueber eine Queue und nutzt ausschliesslich den internen Gateway-Vertrag
//! `POST /internal/llm`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sentinel_common::{DomainEvent, DomainEventPayload};
use sentinel_limbo::EventStore;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::metrics::PlatformMetrics;
use crate::config::PlatformControlplaneConfig;

const DEFAULT_GATEWAY_URL: &str = "http://localhost:8080";
const DEFAULT_CHANNEL_CAPACITY: usize = 16;
const RECENT_EVENT_SCAN_LIMIT: usize = 256;

#[derive(Debug, Clone)]
pub struct FailedIntervention {
    pub rule_name: String,
    pub target: String,
    pub action: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct PlatformAnalysisRequest {
    pub trigger: String,
    pub tick: u64,
    pub metrics: PlatformMetrics,
    pub verify_results: HashMap<String, bool>,
    pub failed_interventions: Vec<FailedIntervention>,
}

#[derive(Debug, Clone)]
pub struct LlmAnalyzerConfig {
    pub enabled: bool,
    pub gateway_url: String,
    pub request_timeout: Duration,
    pub prompt_template: String,
    pub max_context_events: usize,
    pub max_failed_interventions: usize,
    pub channel_capacity: usize,
}

impl LlmAnalyzerConfig {
    pub fn from_platform_config(config: &PlatformControlplaneConfig, gateway_url: String) -> Self {
        Self {
            enabled: config.llm_enabled,
            gateway_url,
            request_timeout: Duration::from_millis(config.llm_gateway_timeout_ms),
            prompt_template: config.llm_prompt_template.clone(),
            max_context_events: config.llm_max_context_events.max(1),
            max_failed_interventions: config.llm_max_failed_interventions.max(1),
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }
}

impl Default for LlmAnalyzerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            gateway_url: DEFAULT_GATEWAY_URL.to_string(),
            request_timeout: Duration::from_secs(30),
            prompt_template: "platform-controlplane-default".to_string(),
            max_context_events: 10,
            max_failed_interventions: 3,
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }
}

#[derive(Clone)]
pub struct PlatformLlmAnalyzerHandle {
    tx: Option<mpsc::UnboundedSender<PlatformAnalysisRequest>>,
}

impl PlatformLlmAnalyzerHandle {
    pub fn spawn(config: LlmAnalyzerConfig, event_store: Arc<EventStore>) -> Self {
        if !config.enabled {
            info!("Platform LLM Analyzer deaktiviert");
            return Self { tx: None };
        }

        let client = match Client::builder().timeout(config.request_timeout).build() {
            Ok(client) => client,
            Err(error) => {
                warn!(error = %error, "Platform LLM Analyzer Client fehlgeschlagen");
                return Self { tx: None };
            }
        };

        let (tx, mut rx) = mpsc::unbounded_channel::<PlatformAnalysisRequest>();
        let worker_config = config.clone();
        tokio::spawn(async move {
            info!(
                gateway_url = %worker_config.gateway_url,
                timeout_ms = worker_config.request_timeout.as_millis(),
                prompt_template = %worker_config.prompt_template,
                "Platform LLM Analyzer gestartet"
            );

            while let Some(request) = rx.recv().await {
                if let Err(error) =
                    analyze_and_persist(&client, &worker_config, &event_store, request).await
                {
                    warn!(error = %error, "Platform LLM Analyzer fehlgeschlagen");
                }
            }

            info!("Platform LLM Analyzer beendet");
        });

        Self { tx: Some(tx) }
    }

    pub fn is_enabled(&self) -> bool {
        self.tx.is_some()
    }

    pub fn enqueue(&self, request: PlatformAnalysisRequest) -> Result<()> {
        let tx = self
            .tx
            .as_ref()
            .context("platform llm analyzer disabled")?;
        tx.send(request)
            .map_err(|_| anyhow!("platform llm analyzer worker not running"))
    }
}

#[derive(Debug, Serialize)]
struct GatewayRequest {
    messages: Vec<GatewayMessage>,
    temperature: f64,
    max_tokens: i32,
    model: String,
    metadata: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
struct GatewayMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct GatewayResponse {
    #[serde(default)]
    content: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    model: String,
}

#[derive(Debug, Deserialize)]
struct AnalyzerResponse {
    severity: String,
    summary: String,
    recommendation: String,
    #[serde(default)]
    suggested_action: Option<String>,
    #[serde(default)]
    target: String,
    #[serde(default)]
    parameters: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
struct PromptContext {
    trigger: String,
    tick: u64,
    unresolved_keys: Vec<String>,
    metrics: Value,
    recent_interventions: Vec<Value>,
    failed_interventions: Vec<Value>,
}

async fn analyze_and_persist(
    client: &Client,
    config: &LlmAnalyzerConfig,
    event_store: &EventStore,
    request: PlatformAnalysisRequest,
) -> Result<()> {
    let recent_interventions =
        load_recent_platform_interventions(event_store, config.max_context_events)?;
    let unresolved_keys = request
        .verify_results
        .iter()
        .filter(|(_, resolved)| !**resolved)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();

    let prompt_context = PromptContext {
        trigger: request.trigger.clone(),
        tick: request.tick,
        unresolved_keys: unresolved_keys.clone(),
        metrics: serialize_metrics(&request.metrics),
        recent_interventions,
        failed_interventions: request
            .failed_interventions
            .iter()
            .take(config.max_failed_interventions)
            .map(|failed| {
                json!({
                    "rule_name": failed.rule_name,
                    "target": failed.target,
                    "action": failed.action,
                    "reason": failed.reason,
                })
            })
            .collect(),
    };

    let gateway_request = build_gateway_request(config, &prompt_context);
    let response = client
        .post(format!("{}/internal/llm", config.gateway_url.trim_end_matches('/')))
        .json(&gateway_request)
        .send()
        .await
        .context("gateway request failed")?;

    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("gateway returned status {}", status.as_u16()));
    }

    let gateway_response: GatewayResponse = response
        .json()
        .await
        .context("gateway response decode failed")?;
    let analysis = parse_analyzer_response(&gateway_response.content)?;
    persist_platform_analysis(
        event_store,
        request.tick,
        &request.trigger,
        unresolved_keys,
        &gateway_response,
        analysis,
    )?;
    Ok(())
}

fn build_gateway_request(config: &LlmAnalyzerConfig, context: &PromptContext) -> GatewayRequest {
    let mut metadata = HashMap::new();
    metadata.insert("request_id".to_string(), uuid::Uuid::new_v4().to_string());
    metadata.insert("agent_id".to_string(), "0".to_string());
    metadata.insert("agent_name".to_string(), "PLATFORM-CONTROLPLANE".to_string());
    metadata.insert("agent_role".to_string(), "platform-analyst".to_string());
    metadata.insert("room_id".to_string(), "system".to_string());
    metadata.insert("platform_trigger".to_string(), context.trigger.clone());
    metadata.insert(
        "platform_prompt_template".to_string(),
        config.prompt_template.clone(),
    );
    metadata.insert("platform_analysis".to_string(), "true".to_string());

    let prompt = format!(
        "Du bist der Platform-Controlplane-Analyzer von Project Sentinel.\n\
         Nutze das Prompt-Template '{template}'.\n\
         Antworte NUR mit gueltigem JSON im Format:\n\
         {{\"severity\":\"info|warning|critical\",\"summary\":\"...\",\"recommendation\":\"...\",\"suggested_action\":\"force_profile|adjust_threshold|escalate_to_operator|null\",\"target\":\"system|AGENT-XX|Dienstname\",\"parameters\":{{...}}}}\n\
         Wenn keine Aktion sinnvoll ist, setze suggested_action auf null und parameters auf {{}}.\n\
         Kontext:\n{context}",
        template = config.prompt_template,
        context = serde_json::to_string_pretty(context).unwrap_or_else(|_| "{}".to_string()),
    );

    GatewayRequest {
        messages: vec![GatewayMessage {
            role: "user".to_string(),
            content: prompt,
        }],
        temperature: 0.0,
        max_tokens: 768,
        model: String::new(),
        metadata,
    }
}

fn parse_analyzer_response(content: &str) -> Result<AnalyzerResponse> {
    let payload = extract_json_object(content)?;
    let response: AnalyzerResponse =
        serde_json::from_str(payload).context("analyzer json parse failed")?;
    if response.severity.trim().is_empty() {
        return Err(anyhow!("analyzer response missing severity"));
    }
    if response.summary.trim().is_empty() {
        return Err(anyhow!("analyzer response missing summary"));
    }
    if response.recommendation.trim().is_empty() {
        return Err(anyhow!("analyzer response missing recommendation"));
    }
    Ok(response)
}

fn extract_json_object(content: &str) -> Result<&str> {
    let trimmed = content.trim();
    if trimmed.starts_with("```") {
        let stripped = trimmed.trim_start_matches("```json").trim_start_matches("```");
        let stripped = stripped.trim_end_matches("```").trim();
        if stripped.starts_with('{') && stripped.ends_with('}') {
            return Ok(stripped);
        }
    }

    let start = trimmed
        .find('{')
        .context("analyzer response missing json object start")?;
    let end = trimmed
        .rfind('}')
        .context("analyzer response missing json object end")?;
    trimmed
        .get(start..=end)
        .context("analyzer response json slice invalid")
}

fn persist_platform_analysis(
    event_store: &EventStore,
    tick: u64,
    trigger: &str,
    unresolved_keys: Vec<String>,
    gateway_response: &GatewayResponse,
    analysis: AnalyzerResponse,
) -> Result<()> {
    let target = if analysis.target.trim().is_empty() {
        "system".to_string()
    } else {
        analysis.target.clone()
    };

    let payload = DomainEventPayload::PlatformAnalysis {
        trigger: trigger.to_string(),
        severity: analysis.severity,
        summary: analysis.summary,
        recommendation: analysis.recommendation,
        suggested_action: analysis.suggested_action,
        target: target.clone(),
        provider: (!gateway_response.provider.is_empty()).then(|| gateway_response.provider.clone()),
        model: (!gateway_response.model.is_empty()).then(|| gateway_response.model.clone()),
        unresolved_keys,
        parameters: analysis.parameters,
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let op_id = format!("platform-analysis-{trigger}-{tick}-{ts}");
    let event = DomainEvent::new(
        payload.event_type_str(),
        &target,
        &payload.to_json(),
        &op_id,
        tick,
    );
    let topic = format!("sentinel/events/platform_analysis/{target}");
    event_store
        .append_with_outbox(&event, &topic)
        .context("persist platform_analysis")?;
    debug!(trigger, target, "platform_analysis event persisted");
    Ok(())
}

fn load_recent_platform_interventions(
    event_store: &EventStore,
    limit: usize,
) -> Result<Vec<Value>> {
    let latest_id = event_store.get_latest_event_id().unwrap_or(0);
    let after_id = latest_id.saturating_sub(RECENT_EVENT_SCAN_LIMIT as i64);
    let mut events = event_store
        .get_events_since(after_id, RECENT_EVENT_SCAN_LIMIT)
        .context("load recent platform interventions")?;
    events.reverse();

    let mut summaries = Vec::new();
    for event in events {
        if event.event_type != "platform_intervention" {
            continue;
        }
        let Ok(payload) = serde_json::from_str::<DomainEventPayload>(&event.payload) else {
            continue;
        };
        if let DomainEventPayload::PlatformIntervention {
            rule_name,
            target,
            action,
            description,
        } = payload
        {
            summaries.push(json!({
                "rule_name": rule_name,
                "target": target,
                "action": action,
                "description": description,
                "tick": event.tick,
                "timestamp_ms": event.timestamp_ms,
            }));
            if summaries.len() >= limit {
                break;
            }
        }
    }
    Ok(summaries)
}

fn serialize_metrics(metrics: &PlatformMetrics) -> Value {
    json!({
        "stalled_agents": metrics.stalled_agents,
        "event_store_size_bytes": metrics.event_store_size_bytes,
        "projection_lag": metrics.projection_lag,
        "agent_memory_pressure": metrics.agent_memory_pressure.iter().map(|(name, pressure)| json!({
            "agent": name,
            "pressure": pressure,
        })).collect::<Vec<_>>(),
        "agent_write_rates": metrics.agent_write_rates.iter().map(|(name, rate)| json!({
            "agent": name,
            "bytes_per_sec": rate,
        })).collect::<Vec<_>>(),
        "failed_services": metrics.failed_services,
        "tick": metrics.tick,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[derive(Debug, Clone)]
    struct CapturedRequest {
        path: String,
        body: String,
    }

    async fn spawn_gateway(
        status_code: u16,
        response_body: String,
        delay: Duration,
    ) -> (String, Arc<Mutex<Option<CapturedRequest>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(None));
        let captured_clone = Arc::clone(&captured);

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut header_end = None;
            let mut content_length = 0usize;

            loop {
                let mut chunk = [0u8; 1024];
                let read = stream.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..read]);
                if header_end.is_none() {
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = Some(pos + 4);
                        let headers = String::from_utf8_lossy(&buf[..pos + 4]);
                        for line in headers.lines() {
                            if let Some((name, value)) = line.split_once(':') {
                                if name.trim().eq_ignore_ascii_case("content-length") {
                                    content_length = value.trim().parse().unwrap_or(0);
                                }
                            }
                        }
                    }
                }
                if let Some(end) = header_end {
                    if buf.len() >= end + content_length {
                        break;
                    }
                }
            }

            let header_end = header_end.unwrap();
            let header_text = String::from_utf8_lossy(&buf[..header_end]);
            let path = header_text
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            let body = String::from_utf8(buf[header_end..header_end + content_length].to_vec())
                .unwrap();
            *captured_clone.lock().unwrap() = Some(CapturedRequest { path, body });

            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let response = format!(
                "HTTP/1.1 {status_code} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        (format!("http://{addr}"), captured)
    }

    fn temp_store() -> (tempfile::TempDir, EventStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("events.db").to_str().unwrap()).unwrap();
        (dir, store)
    }

    fn request(trigger: &str) -> PlatformAnalysisRequest {
        PlatformAnalysisRequest {
            trigger: trigger.to_string(),
            tick: 42,
            metrics: PlatformMetrics {
                projection_lag: 123,
                event_store_size_bytes: 456,
                tick: 42,
                ..Default::default()
            },
            verify_results: HashMap::from([
                ("projection_lag:system".to_string(), false),
                ("event_store_size:system".to_string(), true),
            ]),
            failed_interventions: vec![FailedIntervention {
                rule_name: "projection_lag".to_string(),
                target: "system".to_string(),
                action: "restart_projection".to_string(),
                reason: "lag remained high".to_string(),
            }],
        }
    }

    #[tokio::test]
    async fn analyzer_persists_platform_analysis_event() {
        let (_dir, store) = temp_store();
        let body = json!({
            "content": "{\"severity\":\"critical\",\"summary\":\"Projection stuck\",\"recommendation\":\"Restart projection worker\",\"suggested_action\":\"escalate_to_operator\",\"target\":\"system\",\"parameters\":{\"lag\":123}}",
            "provider": "claude-code",
            "model": "haiku"
        })
        .to_string();
        let (gateway_url, captured) = spawn_gateway(200, body, Duration::ZERO).await;

        let config = LlmAnalyzerConfig {
            gateway_url,
            request_timeout: Duration::from_secs(2),
            ..LlmAnalyzerConfig::default()
        };
        analyze_and_persist(&Client::new(), &config, &store, request("manual"))
            .await
            .unwrap();

        let events = store.get_events_since(0, 10).unwrap();
        let event = events
            .iter()
            .find(|event| event.event_type == "platform_analysis")
            .expect("platform_analysis event");
        let payload: DomainEventPayload = serde_json::from_str(&event.payload).unwrap();
        match payload {
            DomainEventPayload::PlatformAnalysis {
                trigger,
                severity,
                summary,
                recommendation,
                suggested_action,
                target,
                provider,
                model,
                unresolved_keys,
                parameters,
            } => {
                assert_eq!(trigger, "manual");
                assert_eq!(severity, "critical");
                assert_eq!(summary, "Projection stuck");
                assert_eq!(recommendation, "Restart projection worker");
                assert_eq!(suggested_action.as_deref(), Some("escalate_to_operator"));
                assert_eq!(target, "system");
                assert_eq!(provider.as_deref(), Some("claude-code"));
                assert_eq!(model.as_deref(), Some("haiku"));
                assert_eq!(unresolved_keys, vec!["projection_lag:system".to_string()]);
                assert_eq!(parameters.get("lag"), Some(&json!(123)));
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        let captured = captured.lock().unwrap().clone().unwrap();
        assert_eq!(captured.path, "/internal/llm");
        let body: Value = serde_json::from_str(&captured.body).unwrap();
        assert_eq!(body["metadata"]["platform_analysis"], "true");
        assert_eq!(body["metadata"]["platform_trigger"], "manual");
    }

    #[tokio::test]
    async fn analyzer_handles_gateway_timeout() {
        let (_dir, store) = temp_store();
        let (gateway_url, _) = spawn_gateway(
            200,
            json!({"content":"{}"}).to_string(),
            Duration::from_millis(150),
        )
        .await;
        let config = LlmAnalyzerConfig {
            gateway_url,
            request_timeout: Duration::from_millis(25),
            ..LlmAnalyzerConfig::default()
        };
        let client = Client::builder()
            .timeout(config.request_timeout)
            .build()
            .unwrap();

        let error = analyze_and_persist(&client, &config, &store, request("scheduled"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("gateway request failed"));
        assert!(store.get_events_since(0, 10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn handle_enqueue_is_non_blocking_and_worker_persists() {
        let (_dir, store) = temp_store();
        let store = Arc::new(store);
        let body = json!({
            "content": "```json\n{\"severity\":\"warning\",\"summary\":\"Need review\",\"recommendation\":\"Observe\",\"suggested_action\":null,\"target\":\"system\",\"parameters\":{}}\n```",
            "provider": "claude-code",
            "model": "haiku"
        })
        .to_string();
        let (gateway_url, _) = spawn_gateway(200, body, Duration::ZERO).await;
        let handle = PlatformLlmAnalyzerHandle::spawn(
            LlmAnalyzerConfig {
                gateway_url,
                request_timeout: Duration::from_secs(2),
                ..LlmAnalyzerConfig::default()
            },
            Arc::clone(&store),
        );

        handle.enqueue(request("scheduled")).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let events = store.get_events_since(0, 10).unwrap();
        assert!(events.iter().any(|event| event.event_type == "platform_analysis"));
    }
}
