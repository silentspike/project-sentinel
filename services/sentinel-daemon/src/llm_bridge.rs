//! LLM Bridge — Async Perception→Cortex Gateway→Action Pipeline.
//!
//! Verbindet den ECS Tick-Loop (deterministisch) mit dem Cortex Gateway (probabilistisch).
//! Laeuft auf dem Tokio Runtime und kommuniziert via mpsc Channels mit dem ECS Thread.
//!
//! Enterprise Features:
//! - Circuit Breaker (3 Failures → Open, 30s Reset)
//! - Rate Limiting pro Agent (min 5 Ticks zwischen Calls)
//! - Concurrency Limiter (max 4 parallele LLM Calls)
//! - Graceful Degradation (autonomy_system uebernimmt bei Gateway-Ausfall)
//! - Structured Logging mit Tracing Spans

#[cfg(feature = "llm")]
pub mod bridge {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use serde::{Deserialize, Serialize};
    use tokio::sync::Semaphore;
    use tracing::{debug, error, info, instrument, warn};

    use sentinel_common::{ActionType, AgentAction, AgentId, Perception, RoomId, Tick, Timestamp};
    use sentinel_redb::StateStore;

    /// LLM Bridge Konfiguration.
    #[derive(Debug, Clone)]
    pub struct LlmBridgeConfig {
        /// Cortex Gateway Base URL (default: http://localhost:8080)
        pub gateway_url: String,
        /// Max parallele LLM Calls
        pub max_concurrent: usize,
        /// Min Ticks zwischen LLM Calls pro Agent
        pub min_ticks_between_calls: u64,
        /// HTTP Request Timeout
        pub request_timeout: Duration,
        /// Circuit Breaker: Failures bis Open
        pub circuit_breaker_threshold: u32,
        /// Circuit Breaker: Reset-Zeit nach Open
        pub circuit_breaker_reset: Duration,
    }

    impl Default for LlmBridgeConfig {
        fn default() -> Self {
            Self {
                gateway_url: "http://localhost:8080".to_string(),
                max_concurrent: 4,
                min_ticks_between_calls: 5,
                request_timeout: Duration::from_secs(25),
                circuit_breaker_threshold: 3,
                circuit_breaker_reset: Duration::from_secs(30),
            }
        }
    }

    /// Circuit Breaker State.
    #[derive(Debug)]
    struct CircuitBreaker {
        failure_count: u32,
        threshold: u32,
        last_failure: Option<Instant>,
        reset_duration: Duration,
    }

    impl CircuitBreaker {
        fn new(threshold: u32, reset_duration: Duration) -> Self {
            Self {
                failure_count: 0,
                threshold,
                last_failure: None,
                reset_duration,
            }
        }

        fn is_open(&self) -> bool {
            if self.failure_count >= self.threshold {
                // Pruefen ob Reset-Zeit abgelaufen
                if let Some(last) = self.last_failure {
                    if last.elapsed() < self.reset_duration {
                        return true;
                    }
                }
            }
            false
        }

        fn record_success(&mut self) {
            self.failure_count = 0;
            self.last_failure = None;
        }

        fn record_failure(&mut self) {
            self.failure_count += 1;
            self.last_failure = Some(Instant::now());
        }
    }

    // -- Gateway Request/Response Types --

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
        actions: Vec<ExtractedAction>,
        #[serde(default)]
        tokens_used: i32,
        #[serde(default)]
        request_id: String,
    }

    #[derive(Debug, Deserialize)]
    struct ExtractedAction {
        #[serde(rename = "type", default)]
        action_type: String,
        #[serde(default)]
        content: String,
        #[serde(default)]
        target: String,
        #[serde(default)]
        emotion: String,
    }

    /// Telemetrie-Zaehler fuer LLM Bridge.
    #[derive(Debug)]
    pub struct BridgeTelemetry {
        pub calls_total: AtomicU64,
        pub calls_success: AtomicU64,
        pub calls_failed: AtomicU64,
        pub calls_skipped_rate_limit: AtomicU64,
        pub calls_skipped_circuit_open: AtomicU64,
        pub tokens_total: AtomicU64,
    }

    impl Default for BridgeTelemetry {
        fn default() -> Self {
            Self {
                calls_total: AtomicU64::new(0),
                calls_success: AtomicU64::new(0),
                calls_failed: AtomicU64::new(0),
                calls_skipped_rate_limit: AtomicU64::new(0),
                calls_skipped_circuit_open: AtomicU64::new(0),
                tokens_total: AtomicU64::new(0),
            }
        }
    }

    /// Startet die LLM Bridge auf dem Tokio Runtime.
    ///
    /// Empfaengt Perceptions vom ECS Thread, ruft Cortex Gateway auf,
    /// und sendet resultierende AgentActions zurueck.
    #[instrument(skip_all, fields(gateway = %config.gateway_url))]
    pub async fn run_llm_bridge(
        config: LlmBridgeConfig,
        perception_rx: mpsc::Receiver<Perception>,
        action_tx: mpsc::Sender<AgentAction>,
        telemetry: Arc<BridgeTelemetry>,
        state_store: Arc<StateStore>,
    ) {
        info!(
            max_concurrent = config.max_concurrent,
            min_ticks = config.min_ticks_between_calls,
            timeout_secs = config.request_timeout.as_secs(),
            "LLM Bridge gestartet"
        );

        let client = match reqwest::Client::builder()
            .timeout(config.request_timeout)
            .pool_max_idle_per_host(config.max_concurrent)
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                error!(error = %e, "HTTP Client erstellen fehlgeschlagen");
                return;
            }
        };

        let semaphore = Arc::new(Semaphore::new(config.max_concurrent));
        let mut circuit_breaker = CircuitBreaker::new(
            config.circuit_breaker_threshold,
            config.circuit_breaker_reset,
        );
        let mut last_call_tick: HashMap<AgentId, u64> = HashMap::new();

        // Blocking receive in eigenem Thread, forward an async channel
        let (async_tx, mut async_rx) = tokio::sync::mpsc::channel::<Perception>(64);
        std::thread::Builder::new()
            .name("llm-bridge-recv".into())
            .spawn(move || {
                while let Ok(perception) = perception_rx.recv() {
                    if async_tx.blocking_send(perception).is_err() {
                        break;
                    }
                }
                debug!("LLM Bridge Receiver Thread beendet");
            })
            .expect("LLM Bridge Receiver Thread spawnen");

        while let Some(perception) = async_rx.recv().await {
            let agent_id = perception.agent_id;
            let current_tick = perception.tick.0;

            // Rate Limiting pro Agent
            if let Some(&last_tick) = last_call_tick.get(&agent_id) {
                if current_tick.saturating_sub(last_tick) < config.min_ticks_between_calls {
                    telemetry
                        .calls_skipped_rate_limit
                        .fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }

            // Circuit Breaker
            if circuit_breaker.is_open() {
                telemetry
                    .calls_skipped_circuit_open
                    .fetch_add(1, Ordering::Relaxed);
                debug!(
                    agent = %agent_id,
                    "LLM Call uebersprungen: Circuit Breaker offen"
                );
                continue;
            }

            // Nur Calls mit nicht-leerem impulse_text (Agent hat etwas zu tun)
            if perception.impulse_text.is_empty() && perception.body_text.is_empty() {
                continue;
            }

            last_call_tick.insert(agent_id, current_tick);

            // Semaphore fuer Concurrency Limiting
            let permit = match semaphore.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    debug!(agent = %agent_id, "LLM Call uebersprungen: max concurrent erreicht");
                    continue;
                }
            };

            let client = client.clone();
            let url = format!("{}/v1/chat/completions", config.gateway_url);
            let action_tx = action_tx.clone();
            let telemetry = Arc::clone(&telemetry);

            // Gateway Request bauen (mit Evolution-Daten aus redb)
            let request = build_gateway_request(&perception, &state_store);

            telemetry.calls_total.fetch_add(1, Ordering::Relaxed);

            // Async HTTP Call
            let call_start = Instant::now();
            match client.post(&url).json(&request).send().await {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        match response.json::<GatewayResponse>().await {
                            Ok(gateway_resp) => {
                                let latency_ms = call_start.elapsed().as_millis();
                                telemetry.calls_success.fetch_add(1, Ordering::Relaxed);
                                telemetry
                                    .tokens_total
                                    .fetch_add(gateway_resp.tokens_used as u64, Ordering::Relaxed);
                                circuit_breaker.record_success();

                                debug!(
                                    agent = %agent_id,
                                    request_id = %gateway_resp.request_id,
                                    tokens = gateway_resp.tokens_used,
                                    actions = gateway_resp.actions.len(),
                                    content_len = gateway_resp.content.len(),
                                    latency_ms = latency_ms,
                                    "LLM Response erhalten"
                                );

                                // Actions in AgentActions umwandeln und senden
                                for extracted in &gateway_resp.actions {
                                    if let Some(agent_action) =
                                        map_extracted_to_action(agent_id, extracted, current_tick)
                                    {
                                        if action_tx.send(agent_action).is_err() {
                                            debug!("Action Channel geschlossen, Bridge beendet");
                                            drop(permit);
                                            return;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(
                                    agent = %agent_id,
                                    error = %e,
                                    "Gateway Response Parse-Fehler"
                                );
                                telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                                circuit_breaker.record_failure();
                            }
                        }
                    } else {
                        warn!(
                            agent = %agent_id,
                            status = status.as_u16(),
                            "Gateway HTTP Fehler"
                        );
                        telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                        circuit_breaker.record_failure();
                    }
                }
                Err(e) => {
                    let is_timeout = e.is_timeout();
                    warn!(
                        agent = %agent_id,
                        error = %e,
                        is_timeout = is_timeout,
                        "Gateway Request fehlgeschlagen"
                    );
                    telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                    circuit_breaker.record_failure();
                }
            }

            drop(permit);
        }

        info!("LLM Bridge beendet");
    }

    /// Baut den Gateway-Request aus einer Perception + Evolution-Daten aus redb.
    fn build_gateway_request(perception: &Perception, store: &StateStore) -> GatewayRequest {
        // System-Injection Block (wie in architecture.md definiert)
        let system_injection = format!(
            "[SYSTEM_INJECTION]\n\
             Koerperwahrnehmung: {}\n\
             Umgebungswahrnehmung: {}\n\
             Soziale Wahrnehmung: {}\n\
             Tageszeit: {}\n\
             [/SYSTEM_INJECTION]",
            perception.body_text,
            perception.environment_text,
            perception.presence_text,
            perception.circadian_text,
        );

        let user_prompt = if perception.impulse_text.is_empty() {
            "Was machst du als naechstes? Reagiere natuerlich auf deine aktuelle Situation."
                .to_string()
        } else {
            format!(
                "Folgende Impulse sind gerade wichtig:\n{}\n\n\
                 Was machst du als naechstes? Reagiere natuerlich.",
                perception.impulse_text
            )
        };

        let mut metadata = HashMap::new();
        metadata.insert("agent_id".to_string(), perception.agent_id.0.to_string());
        metadata.insert(
            "perception".to_string(),
            serde_json::to_string(perception).unwrap_or_default(),
        );
        metadata.insert("tick".to_string(), perception.tick.0.to_string());
        metadata.insert("request_id".to_string(), uuid::Uuid::new_v4().to_string());

        // Evolution-Daten aus redb lesen und als Metadata-Keys hinzufuegen.
        // Gateway parst diese via EvolutionFromMetadata() fuer 3-Source Assembly.
        let agent_id = perception.agent_id;
        if let Ok(Some(voice)) = store.get_voice_style(agent_id) {
            if let Ok(voice_str) = String::from_utf8(voice) {
                if !voice_str.is_empty() {
                    metadata.insert("evolution_voice".to_string(), voice_str);
                }
            }
        }
        if let Ok(Some(notes)) = store.get_behavioral_notes(agent_id) {
            if let Ok(notes_str) = String::from_utf8(notes) {
                if !notes_str.is_empty() {
                    metadata.insert("evolution_notes".to_string(), notes_str);
                }
            }
        }
        if let Ok(Some(narrative)) = store.get_narrative_summary(agent_id) {
            if let Ok(narrative_str) = String::from_utf8(narrative) {
                if !narrative_str.is_empty() {
                    metadata.insert("evolution_narrative".to_string(), narrative_str);
                }
            }
        }
        if let Ok(Some(facts)) = store.get_agent_facts(agent_id) {
            if let Ok(facts_str) = String::from_utf8(facts) {
                if !facts_str.is_empty() {
                    tracing::debug!(
                        agent_id = %agent_id,
                        len = facts_str.len(),
                        "evolution_facts in Metadata eingefuegt"
                    );
                    metadata.insert("evolution_facts".to_string(), facts_str);
                }
            }
        }
        let version = store.get_evolution_version(agent_id).unwrap_or(0);
        if version > 0 {
            metadata.insert("evolution_version".to_string(), version.to_string());
        }

        GatewayRequest {
            messages: vec![
                GatewayMessage {
                    role: "system".to_string(),
                    content: system_injection,
                },
                GatewayMessage {
                    role: "user".to_string(),
                    content: user_prompt,
                },
            ],
            temperature: 0.7,
            max_tokens: 1024,
            model: String::new(), // Gateway waehlt default
            metadata,
        }
    }

    /// Mappt eine ExtractedAction (Gateway) auf eine AgentAction (ECS).
    fn map_extracted_to_action(
        agent_id: AgentId,
        extracted: &ExtractedAction,
        tick: u64,
    ) -> Option<AgentAction> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let action_type = match extracted.action_type.as_str() {
            "move" => ActionType::Move,
            "chat" => ActionType::Chat,
            "tool_use" => ActionType::ToolUse,
            "emote" => ActionType::Emote,
            "phone_call" => ActionType::PhoneCall,
            other => {
                debug!(action_type = other, "Unbekannter Action-Typ, uebersprungen");
                return None;
            }
        };

        // Target Room: Versuche aus dem target-Feld eine RoomId zu parsen
        let target_room = if !extracted.target.is_empty() {
            // Gateway liefert Room-Name (z.B. "kueche"), RoomId braucht u16
            // Fuer jetzt: Dummy-RoomId 1 — rooms.toml Mapping kommt spaeter
            Some(RoomId(1))
        } else {
            None
        };

        // Bei Emote-Actions: emotion als Content nutzen wenn kein expliziter Content
        let content = if !extracted.content.is_empty() {
            Some(extracted.content.clone())
        } else if action_type == ActionType::Emote && !extracted.emotion.is_empty() {
            Some(extracted.emotion.clone())
        } else {
            None
        };

        Some(AgentAction {
            agent_id,
            action_type,
            target_room,
            target_agent: None,
            content,
            timestamp: Timestamp(now_ms),
            tick: Tick(tick),
        })
    }
}
