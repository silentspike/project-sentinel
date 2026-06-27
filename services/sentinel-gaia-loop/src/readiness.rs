use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures::StreamExt;
use sentinel_common::DomainEvent;
#[cfg(test)]
use sentinel_common::DomainEventPayload;
use sentinel_limbo::EventStore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::time::{interval_at, Instant, MissedTickBehavior};
use tracing::{info, warn};

use crate::config::GaiaLoopConfig;
use crate::storage::{AlertStore, GaiaLoopState};
use crate::types::GaiaAlert;

const PLATFORM_ANALYSIS_EVENT: &str = "platform_analysis";
const PLATFORM_ANALYSIS_TYPE: &str = "PlatformAnalysis";
const ESCALATE_TO_OPERATOR: &str = "escalate_to_operator";
const UNRESOLVED_ESCALATION_TRIGGER: &str = "unresolved_escalation";
const NATS_EVENT_FILTER: &str = "sentinel.events.platform_analysis.>";

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct PlatformAnalysisPayload {
    #[serde(rename = "type")]
    payload_type: String,
    trigger: String,
    severity: String,
    summary: String,
    recommendation: String,
    #[serde(default)]
    suggested_action: Option<String>,
    target: String,
    #[serde(default)]
    unresolved_keys: Vec<String>,
    #[serde(default)]
    parameters: BTreeMap<String, Value>,
}

impl PlatformAnalysisPayload {
    fn is_operator_escalation(&self) -> bool {
        self.payload_type == PLATFORM_ANALYSIS_TYPE
            && (self.suggested_action.as_deref() == Some(ESCALATE_TO_OPERATOR)
                || self.trigger == UNRESOLVED_ESCALATION_TRIGGER
                || self.parameters.get("sink").and_then(Value::as_str)
                    == Some(ESCALATE_TO_OPERATOR))
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReadinessScanSummary {
    pub events_seen: usize,
    pub platform_analysis_seen: usize,
    pub alerts_created: usize,
    pub duplicates_skipped: usize,
    pub ignored: usize,
    pub last_event_row_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadinessOutcome {
    AlertCreated(GaiaAlert),
    DuplicateSkipped,
    Ignored,
}

pub struct ReadinessProcessor {
    store: AlertStore,
    state: GaiaLoopState,
    seen_dedupe_keys: HashSet<String>,
}

impl ReadinessProcessor {
    pub fn new(store: AlertStore) -> Result<Self> {
        store.ensure_layout()?;
        let state = store.load_state()?;
        let seen_dedupe_keys = store.load_alert_dedupe_keys()?;
        Ok(Self {
            store,
            state,
            seen_dedupe_keys,
        })
    }

    pub fn state(&self) -> &GaiaLoopState {
        &self.state
    }

    pub fn scan_event_store_once(
        &mut self,
        events_db: &str,
        limit: usize,
    ) -> Result<ReadinessScanSummary> {
        let event_store = EventStore::open_readonly(events_db)
            .with_context(|| format!("open read-only EventStore {events_db}"))?;
        let events = event_store
            .get_events_since_with_id(self.state.last_event_row_id, limit)
            .context("read Gaia readiness events")?;
        self.process_events(events)
    }

    pub fn process_events(
        &mut self,
        events: Vec<(i64, DomainEvent)>,
    ) -> Result<ReadinessScanSummary> {
        let mut summary = ReadinessScanSummary {
            events_seen: events.len(),
            platform_analysis_seen: 0,
            alerts_created: 0,
            duplicates_skipped: 0,
            ignored: 0,
            last_event_row_id: self.state.last_event_row_id,
        };

        for (row_id, event) in events {
            summary.last_event_row_id = summary.last_event_row_id.max(row_id);
            if event.event_type == PLATFORM_ANALYSIS_EVENT {
                summary.platform_analysis_seen += 1;
            }
            match self.process_domain_event(row_id, &event)? {
                ReadinessOutcome::AlertCreated(_) => summary.alerts_created += 1,
                ReadinessOutcome::DuplicateSkipped => summary.duplicates_skipped += 1,
                ReadinessOutcome::Ignored => summary.ignored += 1,
            }
        }

        if summary.last_event_row_id != self.state.last_event_row_id {
            self.state.last_event_row_id = summary.last_event_row_id;
            self.store.save_state(&self.state)?;
        }
        Ok(summary)
    }

    pub fn process_domain_event(
        &mut self,
        row_id: i64,
        event: &DomainEvent,
    ) -> Result<ReadinessOutcome> {
        if event.event_type != PLATFORM_ANALYSIS_EVENT {
            self.state.last_event_row_id = self.state.last_event_row_id.max(row_id);
            return Ok(ReadinessOutcome::Ignored);
        }

        let payload = parse_platform_analysis(&event.payload)?;
        self.process_platform_analysis_payload(
            row_id,
            &event.event_id,
            event.tick,
            event.timestamp_ms,
            payload,
        )
    }

    pub fn process_nats_platform_analysis(
        &mut self,
        source_event_id: &str,
        tick: u64,
        timestamp_ms: u64,
        payload: &[u8],
    ) -> Result<ReadinessOutcome> {
        let payload =
            std::str::from_utf8(payload).context("NATS platform_analysis payload UTF-8")?;
        let payload = parse_platform_analysis(payload)?;
        self.process_platform_analysis_payload(0, source_event_id, tick, timestamp_ms, payload)
    }

    fn process_platform_analysis_payload(
        &mut self,
        row_id: i64,
        source_event_id: &str,
        tick: u64,
        timestamp_ms: u64,
        payload: PlatformAnalysisPayload,
    ) -> Result<ReadinessOutcome> {
        if !payload.is_operator_escalation() {
            self.state.last_event_row_id = self.state.last_event_row_id.max(row_id);
            return Ok(ReadinessOutcome::Ignored);
        }

        let alert = GaiaAlert {
            alert_id: format!("gaia-alert-{source_event_id}"),
            source_event_id: source_event_id.to_string(),
            tick,
            timestamp_ms,
            trigger: payload.trigger,
            severity: payload.severity,
            target: payload.target,
            summary: payload.summary,
            recommendation: payload.recommendation,
            unresolved_keys: payload.unresolved_keys,
        };
        let dedupe_key = alert.dedupe_key();
        if !self.seen_dedupe_keys.insert(dedupe_key) {
            self.state.last_event_row_id = self.state.last_event_row_id.max(row_id);
            return Ok(ReadinessOutcome::DuplicateSkipped);
        }

        self.store.append_alert(&alert)?;
        self.state.last_event_row_id = self.state.last_event_row_id.max(row_id);
        self.state.alerts_created = self.state.alerts_created.saturating_add(1);
        self.state.last_alert_timestamp_ms = Some(alert.timestamp_ms);
        self.store.save_state(&self.state)?;
        Ok(ReadinessOutcome::AlertCreated(alert))
    }
}

fn parse_platform_analysis(raw: &str) -> Result<PlatformAnalysisPayload> {
    serde_json::from_str(raw).context("parse PlatformAnalysis payload")
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn scan_once(config: &GaiaLoopConfig, limit: usize) -> Result<ReadinessScanSummary> {
    let store = AlertStore::from_config(config);
    let mut processor = ReadinessProcessor::new(store)?;
    processor.scan_event_store_once(&config.events_db.to_string_lossy(), limit)
}

pub async fn run_readiness_loop(config: GaiaLoopConfig) -> Result<()> {
    let store = AlertStore::from_config(&config);
    let mut processor = ReadinessProcessor::new(store)?;
    match processor.scan_event_store_once(&config.events_db.to_string_lossy(), 1_000) {
        Ok(summary) => info!(
            alerts_created = summary.alerts_created,
            last_event_row_id = summary.last_event_row_id,
            "Gaia readiness recovery scan complete"
        ),
        Err(error) => warn!(%error, "Gaia readiness recovery scan skipped"),
    }

    let client = async_nats::connect(config.nats_url.as_str())
        .await
        .with_context(|| format!("connect NATS {}", config.nats_url))?;
    let jetstream = async_nats::jetstream::new(client);
    let stream = jetstream
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: "SENTINEL_EVENTS".to_string(),
            subjects: vec!["sentinel.events.>".to_string()],
            ..Default::default()
        })
        .await
        .context("get/create SENTINEL_EVENTS stream")?;

    let consumer = stream
        .get_or_create_consumer(
            "sentinel-gaia-loop",
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some("sentinel-gaia-loop".to_string()),
                filter_subject: NATS_EVENT_FILTER.to_string(),
                deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::New,
                ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                inactive_threshold: Duration::from_secs(30),
                ..Default::default()
            },
        )
        .await
        .context("get/create sentinel-gaia-loop consumer")?;
    info!(filter = NATS_EVENT_FILTER, "Gaia readiness loop subscribed");

    let mut messages = consumer.messages().await.context("open NATS messages")?;
    let scan_every = config.readiness_scan_interval();
    let mut scan_interval = interval_at(Instant::now() + scan_every, scan_every);
    scan_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            msg_result = messages.next() => {
                let Some(msg_result) = msg_result else {
                    anyhow::bail!("Gaia readiness NATS message stream ended");
                };
                process_nats_message(&mut processor, msg_result).await;
            }
            _ = scan_interval.tick() => {
                match processor.scan_event_store_once(&config.events_db.to_string_lossy(), 1_000) {
                    Ok(summary) => info!(
                        alerts_created = summary.alerts_created,
                        duplicates_skipped = summary.duplicates_skipped,
                        last_event_row_id = summary.last_event_row_id,
                        "Gaia readiness scheduled scan complete"
                    ),
                    Err(error) => warn!(%error, "Gaia readiness scheduled scan skipped"),
                }
            }
        }
    }
}

async fn process_nats_message(
    processor: &mut ReadinessProcessor,
    msg_result: Result<
        async_nats::jetstream::Message,
        async_nats::jetstream::consumer::pull::MessagesError,
    >,
) {
    let msg = match msg_result {
        Ok(msg) => msg,
        Err(error) => {
            warn!(%error, "Gaia readiness NATS message error");
            return;
        }
    };
    let source_event_id = nats_header(&msg, "X-Event-ID")
        .unwrap_or_else(|| format!("nats:{}:{}", msg.subject, now_ms()));
    let tick = nats_header(&msg, "X-Tick")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let timestamp_ms = now_ms();

    match processor.process_nats_platform_analysis(
        &source_event_id,
        tick,
        timestamp_ms,
        msg.payload.as_ref(),
    ) {
        Ok(ReadinessOutcome::AlertCreated(alert)) => info!(
            source_event_id = %alert.source_event_id,
            trigger = %alert.trigger,
            target = %alert.target,
            "Gaia readiness alert persisted"
        ),
        Ok(ReadinessOutcome::DuplicateSkipped) => {}
        Ok(ReadinessOutcome::Ignored) => {}
        Err(error) => warn!(%error, "Gaia readiness event ignored"),
    }

    if let Err(error) = msg.ack().await {
        warn!(%error, "Gaia readiness NATS ack failed");
    }
}

fn nats_header(msg: &async_nats::jetstream::Message, key: &str) -> Option<String> {
    msg.headers
        .as_ref()
        .and_then(|headers| headers.get(key))
        .map(|value| value.as_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_limbo::EventStore;
    use tempfile::TempDir;

    fn cfg(dir: &TempDir) -> GaiaLoopConfig {
        GaiaLoopConfig {
            console_dir: dir.path().join("gaia-console"),
            events_db: dir.path().join("events.db"),
            nats_url: crate::DEFAULT_NATS_URL.to_string(),
            http_bind: crate::DEFAULT_HTTP_BIND.to_string(),
            claude_bin: crate::DEFAULT_CLAUDE_BIN.into(),
            sentinel_ctl_bin: crate::DEFAULT_SENTINEL_CTL_BIN.into(),
            sentinel_gaia_bin: crate::DEFAULT_SENTINEL_GAIA_BIN.into(),
            model: None,
            max_budget_usd: crate::DEFAULT_MAX_BUDGET_USD,
            session_timeout_secs: crate::DEFAULT_SESSION_TIMEOUT_SECS,
            max_turns: crate::DEFAULT_MAX_TURNS,
            readiness_scan_interval_secs: crate::DEFAULT_READINESS_SCAN_INTERVAL_SECS,
        }
    }

    fn platform_event(suggested_action: Option<&str>, trigger: &str) -> DomainEvent {
        let payload = DomainEventPayload::PlatformAnalysis {
            trigger: trigger.to_string(),
            severity: "warning".to_string(),
            summary: "Projection lag requires operator attention".to_string(),
            recommendation: "Review projection lag and decide next action".to_string(),
            suggested_action: suggested_action.map(str::to_string),
            target: "system".to_string(),
            provider: Some("claude-code".to_string()),
            model: Some("test".to_string()),
            unresolved_keys: vec!["projection_lag".to_string()],
            parameters: BTreeMap::new(),
        };
        DomainEvent::new(
            payload.event_type_str(),
            "system",
            &payload.to_json(),
            "test-correlation",
            7,
        )
    }

    #[test]
    fn scan_creates_alert_for_escalate_to_operator_without_eventstore_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(&dir);
        let event_store = EventStore::open(cfg.events_db.to_str().unwrap()).unwrap();
        event_store
            .append_with_outbox(
                &platform_event(Some(ESCALATE_TO_OPERATOR), "manual"),
                "sentinel/events/platform_analysis/system",
            )
            .unwrap();
        let before = event_store.get_latest_event_id().unwrap();

        let summary = scan_once(&cfg, 100).unwrap();
        let after = event_store.get_latest_event_id().unwrap();

        assert_eq!(summary.platform_analysis_seen, 1);
        assert_eq!(summary.alerts_created, 1);
        assert_eq!(before, after, "readiness scan must not append events");
        let alert_lines = std::fs::read_to_string(cfg.alerts_path()).unwrap();
        assert!(alert_lines.contains("Projection lag requires operator attention"));
    }

    #[test]
    fn scan_ignores_non_escalating_platform_analysis() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(&dir);
        let event_store = EventStore::open(cfg.events_db.to_str().unwrap()).unwrap();
        event_store
            .append_with_outbox(
                &platform_event(Some("force_profile"), "manual"),
                "sentinel/events/platform_analysis/system",
            )
            .unwrap();

        let summary = scan_once(&cfg, 100).unwrap();

        assert_eq!(summary.platform_analysis_seen, 1);
        assert_eq!(summary.alerts_created, 0);
        assert!(!cfg.alerts_path().exists());
    }

    #[test]
    fn scan_dedupes_existing_alerts() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(&dir);
        let event_store = EventStore::open(cfg.events_db.to_str().unwrap()).unwrap();
        event_store
            .append_with_outbox(
                &platform_event(Some(ESCALATE_TO_OPERATOR), "manual"),
                "sentinel/events/platform_analysis/system",
            )
            .unwrap();

        let first = scan_once(&cfg, 100).unwrap();
        let second = scan_once(&cfg, 100).unwrap();

        assert_eq!(first.alerts_created, 1);
        assert_eq!(second.events_seen, 0);
        assert_eq!(second.alerts_created, 0);
        let alert_lines = std::fs::read_to_string(cfg.alerts_path()).unwrap();
        assert_eq!(alert_lines.lines().count(), 1);
    }

    #[test]
    fn nats_payload_processing_uses_event_id_for_dedupe() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(&dir);
        let store = AlertStore::from_config(&cfg);
        let mut processor = ReadinessProcessor::new(store).unwrap();
        let event = platform_event(Some(ESCALATE_TO_OPERATOR), "manual");

        let first = processor
            .process_nats_platform_analysis("event-1", 7, 99, event.payload.as_bytes())
            .unwrap();
        let second = processor
            .process_nats_platform_analysis("event-1", 7, 100, event.payload.as_bytes())
            .unwrap();

        assert!(matches!(first, ReadinessOutcome::AlertCreated(_)));
        assert_eq!(second, ReadinessOutcome::DuplicateSkipped);
    }
}
