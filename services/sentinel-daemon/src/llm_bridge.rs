//! LLM Bridge — Async Perception→Cortex Gateway→Action Pipeline.
//!
//! Verbindet den ECS Tick-Loop (deterministisch) mit dem Cortex Gateway (probabilistisch).
//! Laeuft auf dem Tokio Runtime und kommuniziert via mpsc Channels mit dem ECS Thread.
//!
//! Enterprise Features:
//! - Circuit Breaker (3 Failures → Open, 30s Reset)
//! - Rate Limiting pro Agent (min 5 Ticks zwischen Calls)
//! - Concurrency Limiter (shared Slots: urgent wartet, normal nutzt try_acquire)
//! - Graceful Degradation (autonomy_system uebernimmt bei Gateway-Ausfall)
//! - Structured Logging mit Tracing Spans

#[cfg(feature = "llm")]
pub mod bridge {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use serde::{Deserialize, Serialize};
    use tokio::sync::{Mutex as AsyncMutex, Semaphore};
    use tracing::{debug, error, info, instrument, warn};

    use sentinel_common::{ActionType, AgentAction, AgentId, Perception, Tick, Timestamp};
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
                max_concurrent: 8,
                min_ticks_between_calls: 5,
                request_timeout: Duration::from_secs(35),
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
        open_signal: Arc<AtomicBool>,
    }

    impl CircuitBreaker {
        fn new(threshold: u32, reset_duration: Duration, open_signal: Arc<AtomicBool>) -> Self {
            Self {
                failure_count: 0,
                threshold,
                last_failure: None,
                reset_duration,
                open_signal,
            }
        }

        fn is_open(&self) -> bool {
            if self.failure_count >= self.threshold {
                // Pruefen ob Reset-Zeit abgelaufen
                if let Some(last) = self.last_failure {
                    if last.elapsed() < self.reset_duration {
                        self.open_signal.store(true, Ordering::Relaxed);
                        return true;
                    }
                }
            }
            self.open_signal.store(false, Ordering::Relaxed);
            false
        }

        fn record_success(&mut self) {
            self.failure_count = 0;
            self.last_failure = None;
            self.open_signal.store(false, Ordering::Relaxed);
        }

        fn record_failure(&mut self) {
            self.failure_count += 1;
            self.last_failure = Some(Instant::now());
            let _ = self.is_open();
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
        #[allow(dead_code)]
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
        llm_unavailable: Arc<AtomicBool>,
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

        // Ein geteiltes Semaphore fuer alle echten Gateway-Calls.
        // Urgent Calls warten auf einen Slot, normale Calls droppen bei Ueberlast.
        // Die Kapazitaet richtet sich an der realen Gateway-Forward-Kapazitaet aus.
        let llm_semaphore = Arc::new(Semaphore::new(config.max_concurrent.max(1)));
        let circuit_breaker = Arc::new(std::sync::Mutex::new(CircuitBreaker::new(
            config.circuit_breaker_threshold,
            config.circuit_breaker_reset,
            Arc::clone(&llm_unavailable),
        )));
        llm_unavailable.store(false, Ordering::Relaxed);
        let pending_retries = Arc::new(AsyncMutex::new(HashMap::<AgentId, Perception>::new()));
        let mut last_call_tick: HashMap<AgentId, u64> = HashMap::new();
        // Debounce: Operator-Impulse (Gaia/Broadcast) nur beim ERSTEN Tick urgent,
        // danach 60 Ticks Cooldown. Verhindert Semaphore-Starvation bei 300-Tick TTL.
        let mut impulse_acked: HashMap<AgentId, u64> = HashMap::new();

        // Blocking receive in eigenem Thread, forward an async channel
        let (async_tx, mut async_rx) = tokio::sync::mpsc::channel::<Perception>(256);
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

        while let Some(first) = async_rx.recv().await {
            // Drain: Alle sofort verfuegbaren Perceptions lesen.
            // Pro Agent: neueste behalten, heard_text bevorzugen.
            let mut batch: HashMap<AgentId, Perception> = {
                let mut pending = pending_retries.lock().await;
                std::mem::take(&mut *pending)
            };
            insert_prefer_heard(&mut batch, first);
            while let Ok(p) = async_rx.try_recv() {
                insert_prefer_heard(&mut batch, p);
            }

            // Batch verarbeiten — jede Perception durch Rate-Limit/Filter/Call
            for perception in batch.into_values() {
                let agent_id = perception.agent_id;
                let current_tick = perception.tick.0;

                // heard_text oder direkt angesprochen → Rate-Limit bypass
                let has_heard = !perception.heard_text.is_empty();
                let mut is_urgent = perception.is_directly_addressed
                    || has_heard
                    || perception.has_operator_impulse;

                // Debounce: Operator-Impulse (Gaia/Broadcast) nur beim ERSTEN Tick urgent.
                // IM:1 bleibt im Fingerprint → Synthesis bypassed im Gateway.
                // Aber nur 1 urgent Call pro 60 Ticks pro Agent.
                // Debounce: Operator-Impulse max 1x pro 5 Ticks pro Agent.
                // 60 Ticks war zu aggressiv — bei Gateway-Fehler kein Retry fuer 1 Minute.
                // 5 Ticks gibt dem LLM-Call genug Zeit (12-20s) und verhindert trotzdem Spam.
                if is_urgent
                    && perception.has_operator_impulse
                    && !has_heard
                    && !perception.is_directly_addressed
                {
                    let last_ack = impulse_acked.get(&agent_id).copied().unwrap_or(0);
                    if current_tick.saturating_sub(last_ack) < 5 {
                        is_urgent = false;
                    } else {
                        impulse_acked.insert(agent_id, current_tick);
                    }
                }

                // Rate Limiting pro Agent (urgent bypass)
                if !is_urgent {
                    if let Some(&last_tick) = last_call_tick.get(&agent_id) {
                        if current_tick.saturating_sub(last_tick) < config.min_ticks_between_calls {
                            telemetry
                                .calls_skipped_rate_limit
                                .fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    }
                }

                // Circuit Breaker
                if circuit_breaker.lock().unwrap().is_open() {
                    telemetry
                        .calls_skipped_circuit_open
                        .fetch_add(1, Ordering::Relaxed);
                    if should_retry_perception(&perception) {
                        queue_retry(&pending_retries, perception).await;
                    }
                    continue;
                }

                // Nur Calls mit nicht-leerem Inhalt
                if perception.impulse_text.is_empty()
                    && perception.body_text.is_empty()
                    && perception.heard_text.is_empty()
                {
                    continue;
                }

                last_call_tick.insert(agent_id, current_tick);

                info!(agent = %agent_id,
                    priority = if perception.is_directly_addressed { "P1" } else { "normal" },
                    has_heard = !perception.heard_text.is_empty(),
                    "LLM call triggered");

                let client = client.clone();
                let url = format!("{}/internal/llm", config.gateway_url);
                let action_tx = action_tx.clone();
                let telemetry = Arc::clone(&telemetry);
                let cb = Arc::clone(&circuit_breaker);
                let retry_queue = Arc::clone(&pending_retries);
                let retry_perception = perception.clone();
                let request = build_gateway_request(&perception, &state_store);

                telemetry.calls_total.fetch_add(1, Ordering::Relaxed);

                if is_urgent {
                    // Urgent (heard_text/P1): acquire_owned().await INNERHALB tokio::spawn.
                    // Wartet auf Permit im eigenen Task — Drain-Loop blockiert NICHT,
                    // urgent Calls werden NIEMALS gedroppt.
                    let sem = llm_semaphore.clone();
                    tokio::spawn(async move {
                        // Urgent Calls duerfen auf Semaphore und Gateway warten, aber nicht ewig.
                        let acquire_timeout = config.request_timeout;
                        let permit = match tokio::time::timeout(
                            acquire_timeout,
                            sem.acquire_owned(),
                        )
                        .await
                        {
                            Ok(Ok(permit)) => permit,
                            Ok(Err(_)) => {
                                warn!(agent = %agent_id, "URGENT Semaphore closed");
                                queue_retry(&retry_queue, retry_perception.clone()).await;
                                return;
                            }
                            Err(_) => {
                                warn!(
                                    agent = %agent_id,
                                    timeout_ms = acquire_timeout.as_millis(),
                                    "URGENT Semaphore timeout"
                                );
                                queue_retry(&retry_queue, retry_perception.clone()).await;
                                return;
                            }
                        };
                        let call_start = Instant::now();
                        match client.post(&url).json(&request).send().await {
                            Ok(response) => {
                                let status = response.status();
                                if status.is_success() {
                                    match response.json::<GatewayResponse>().await {
                                        Ok(gateway_resp) => {
                                            let latency_ms = call_start.elapsed().as_millis();
                                            telemetry.calls_success.fetch_add(1, Ordering::Relaxed);
                                            telemetry.tokens_total.fetch_add(
                                                gateway_resp.tokens_used as u64,
                                                Ordering::Relaxed,
                                            );
                                            cb.lock().unwrap().record_success();

                                            info!(
                                                agent = %agent_id,
                                                request_id = %gateway_resp.request_id,
                                                tokens = gateway_resp.tokens_used,
                                                actions = gateway_resp.actions.len(),
                                                latency_ms = latency_ms,
                                                "URGENT LLM Response erhalten"
                                            );

                                            let is_synthesis = gateway_resp.tokens_used == 0;
                                            for extracted in &gateway_resp.actions {
                                                if let Some(agent_action) = map_extracted_to_action(
                                                    agent_id,
                                                    extracted,
                                                    current_tick,
                                                    is_synthesis,
                                                ) {
                                                    let _ = action_tx.send(agent_action);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            warn!(agent = %agent_id, error = %e, "Gateway Response Parse-Fehler");
                                            telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                                            cb.lock().unwrap().record_failure();
                                            queue_retry(&retry_queue, retry_perception.clone())
                                                .await;
                                        }
                                    }
                                } else {
                                    warn!(agent = %agent_id, status = status.as_u16(), "Gateway HTTP Fehler");
                                    telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                                    cb.lock().unwrap().record_failure();
                                    queue_retry(&retry_queue, retry_perception.clone()).await;
                                }
                            }
                            Err(e) => {
                                let is_timeout = e.is_timeout();
                                warn!(agent = %agent_id, error = %e, is_timeout = is_timeout, "Gateway Request fehlgeschlagen");
                                telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                                cb.lock().unwrap().record_failure();
                                queue_retry(&retry_queue, retry_perception.clone()).await;
                            }
                        }
                        drop(permit);
                    });
                } else {
                    // Normal (Heartbeats): try_acquire — droppen OK, Heartbeats sind nicht kritisch.
                    let permit = match llm_semaphore.clone().try_acquire_owned() {
                        Ok(p) => p,
                        Err(_) => {
                            debug!(agent = %agent_id, "Heartbeat LLM Call uebersprungen: max concurrent erreicht");
                            continue;
                        }
                    };
                    tokio::spawn(async move {
                        let call_start = Instant::now();
                        match client.post(&url).json(&request).send().await {
                            Ok(response) => {
                                let status = response.status();
                                if status.is_success() {
                                    match response.json::<GatewayResponse>().await {
                                        Ok(gateway_resp) => {
                                            let latency_ms = call_start.elapsed().as_millis();
                                            telemetry.calls_success.fetch_add(1, Ordering::Relaxed);
                                            telemetry.tokens_total.fetch_add(
                                                gateway_resp.tokens_used as u64,
                                                Ordering::Relaxed,
                                            );
                                            cb.lock().unwrap().record_success();

                                            info!(
                                                agent = %agent_id,
                                                request_id = %gateway_resp.request_id,
                                                tokens = gateway_resp.tokens_used,
                                                actions = gateway_resp.actions.len(),
                                                latency_ms = latency_ms,
                                                "LLM Response erhalten"
                                            );

                                            let is_synthesis = gateway_resp.tokens_used == 0;
                                            for extracted in &gateway_resp.actions {
                                                if let Some(agent_action) = map_extracted_to_action(
                                                    agent_id,
                                                    extracted,
                                                    current_tick,
                                                    is_synthesis,
                                                ) {
                                                    let _ = action_tx.send(agent_action);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            warn!(agent = %agent_id, error = %e, "Gateway Response Parse-Fehler");
                                            telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                                            cb.lock().unwrap().record_failure();
                                        }
                                    }
                                } else {
                                    warn!(agent = %agent_id, status = status.as_u16(), "Gateway HTTP Fehler");
                                    telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                                    cb.lock().unwrap().record_failure();
                                }
                            }
                            Err(e) => {
                                let is_timeout = e.is_timeout();
                                warn!(agent = %agent_id, error = %e, is_timeout = is_timeout, "Gateway Request fehlgeschlagen");
                                telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                                cb.lock().unwrap().record_failure();
                            }
                        }
                        drop(permit);
                    });
                }
            }
        }

        info!("LLM Bridge beendet");
    }

    /// Fuegt Perception in Batch ein. Bevorzugt Versionen MIT heard_text.
    /// #295 Fix: Bewahrt has_operator_impulse (IM-Flag) beim Merge,
    /// damit Gaia/Broadcast-Bypass nicht verloren geht wenn heard_text-Version gewinnt.
    fn insert_prefer_heard(batch: &mut HashMap<AgentId, Perception>, p: Perception) {
        batch
            .entry(p.agent_id)
            .and_modify(|existing| {
                // Behalte Version MIT heard_text, sonst neueste
                if !p.heard_text.is_empty() || existing.heard_text.is_empty() {
                    let preserve_impulse = existing.has_operator_impulse;
                    *existing = p.clone();
                    // IM-Flag aus alter Perception bewahren (Gaia/Broadcast darf nicht verloren gehen)
                    if preserve_impulse && !existing.has_operator_impulse {
                        existing.has_operator_impulse = true;
                        existing.synth_fingerprint =
                            existing.synth_fingerprint.replace("|IM:0", "|IM:1");
                    }
                }
            })
            .or_insert(p);
    }

    fn should_retry_perception(perception: &Perception) -> bool {
        !perception.heard_text.is_empty()
            || perception.is_directly_addressed
            || perception.has_operator_impulse
    }

    async fn queue_retry(
        queue: &Arc<AsyncMutex<HashMap<AgentId, Perception>>>,
        perception: Perception,
    ) {
        if !should_retry_perception(&perception) {
            return;
        }

        let mut pending = queue.lock().await;
        insert_prefer_heard(&mut pending, perception);
    }

    /// Baut den Gateway-Request aus einer Perception + Evolution-Daten aus redb.
    fn build_gateway_request(perception: &Perception, store: &StateStore) -> GatewayRequest {
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

        let formatted_perception = format_perception_metadata(perception);
        let mut metadata = HashMap::new();
        metadata.insert("agent_id".to_string(), perception.agent_id.0.to_string());
        metadata.insert("circadian".to_string(), perception.circadian_text.clone());
        metadata.insert("body".to_string(), perception.body_text.clone());
        metadata.insert(
            "environment".to_string(),
            perception.environment_text.clone(),
        );
        metadata.insert("acoustic".to_string(), perception.acoustic_text.clone());
        metadata.insert("heard".to_string(), perception.heard_text.clone());
        metadata.insert("presence".to_string(), perception.presence_text.clone());
        metadata.insert("impulse".to_string(), perception.impulse_text.clone());
        metadata.insert("perception".to_string(), formatted_perception);
        metadata.insert("tick".to_string(), perception.tick.0.to_string());
        metadata.insert("request_id".to_string(), uuid::Uuid::new_v4().to_string());

        // Traffic Control Metadata (Synthesis, Chat-Sequencing)
        metadata.insert("room_id".to_string(), perception.room_id.clone());
        metadata.insert("max_priority".to_string(), perception.max_priority.clone());
        metadata.insert("synth_fp".to_string(), perception.synth_fingerprint.clone());
        metadata.insert(
            "is_directly_addressed".to_string(),
            perception.is_directly_addressed.to_string(),
        );
        metadata.insert(
            "personality_type".to_string(),
            perception.personality_type.clone(),
        );

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
            messages: vec![GatewayMessage {
                role: "user".to_string(),
                content: user_prompt,
            }],
            temperature: 0.7,
            max_tokens: 1024,
            model: String::new(), // Gateway waehlt default
            metadata,
        }
    }

    fn format_perception_metadata(perception: &Perception) -> String {
        let mut lines = Vec::new();

        if !perception.circadian_text.is_empty() {
            lines.push(format!("CIRCADIAN: {}", perception.circadian_text));
        }
        if !perception.body_text.is_empty() {
            lines.push(format!("KOERPER: {}", perception.body_text));
        }
        if !perception.environment_text.is_empty() {
            lines.push(format!("ENVIRONMENT: {}", perception.environment_text));
        }
        if !perception.acoustic_text.is_empty() {
            lines.push(format!("AKUSTIK: {}", perception.acoustic_text));
        }
        if !perception.heard_text.is_empty() {
            lines.push(format!("GEHOERT: {}", perception.heard_text));
        }
        if !perception.presence_text.is_empty() {
            lines.push(format!("ANWESEND: {}", perception.presence_text));
        }
        if !perception.impulse_text.is_empty() {
            lines.push(format!("IMPULS: {}", perception.impulse_text));
        }

        lines.join("\n")
    }

    /// Mappt eine ExtractedAction (Gateway) auf eine AgentAction (ECS).
    /// `is_synthesis`: true wenn Gateway Synthesis-Template geliefert hat (tokens=0).
    /// Synthesis-Chat wird zu Emote remappt um Chat-Kaskaden zu vermeiden.
    fn map_extracted_to_action(
        agent_id: AgentId,
        extracted: &ExtractedAction,
        tick: u64,
        is_synthesis: bool,
    ) -> Option<AgentAction> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let action_type = match extracted.action_type.as_str() {
            "move" => ActionType::Move,
            "chat" | "work" | "break" | "think" => {
                // Synthesis-generierte Chat-Aktionen als Emote behandeln,
                // damit sie NICHT in den RoomChatBuffer fliessen und
                // keine Chat-Kaskade ausloesen (P3 Fix)
                if is_synthesis {
                    ActionType::Emote
                } else {
                    ActionType::Chat
                }
            }
            "tool_use" => ActionType::ToolUse,
            "emote" => ActionType::Emote,
            "phone_call" => ActionType::PhoneCall,
            other => {
                debug!(
                    action_type = other,
                    "Unbekannter Action-Typ als Chat gemappt"
                );
                ActionType::Chat
            }
        };

        let target_room = if !extracted.target.is_empty() {
            Some(extracted.target.clone())
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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn build_gateway_request_formats_perception_for_gateway_compiler() {
            let dir = tempfile::tempdir().unwrap();
            let store_path = dir.path().join("state.redb");
            let store = StateStore::open(store_path.to_str().unwrap()).unwrap();
            let perception = Perception {
                agent_id: AgentId(7),
                circadian_text: "10:00 Uhr".to_string(),
                body_text: "Du fuehlst dich wach.".to_string(),
                environment_text: "Du bist im Designbuero. Es ist deutlich zu warm (27.5 °C). Die Luft ist sehr stickig (1600 ppm CO2).".to_string(),
                acoustic_text: "Es ist laut (72 dB). Konzentration faellt schwer.".to_string(),
                heard_text: String::new(),
                presence_text: "Lisa (Konzept), Thomas (Review)".to_string(),
                impulse_text: "Du willst kurz frische Luft.".to_string(),
                is_directly_addressed: false,
                timestamp: Timestamp(1234),
                tick: Tick(55),
                room_id: "buero-design-1".to_string(),
                max_priority: "P2".to_string(),
                synth_fingerprint: "H3|E7|B2|S4|C1|SN5|R:buero-design-1|P:2|CH:0|HR:0|T:10|TMP:1|PE:E|IM:0".to_string(),
                personality_type: "E".to_string(),
                has_operator_impulse: false,
            };

            let request = build_gateway_request(&perception, &store);

            assert_eq!(request.messages.len(), 1);
            assert_eq!(request.messages[0].role, "user");
            assert!(request.messages[0]
                .content
                .contains("Folgende Impulse sind gerade wichtig"),);
            assert!(request.messages[0]
                .content
                .contains("Was machst du als naechstes? Reagiere natuerlich."),);

            let metadata = &request.metadata;
            let formatted = metadata.get("perception").unwrap();
            assert!(formatted.contains("CIRCADIAN: 10:00 Uhr"));
            assert!(
                formatted.contains("ENVIRONMENT: Du bist im Designbuero. Es ist deutlich zu warm")
            );
            assert!(formatted.contains("AKUSTIK: Es ist laut (72 dB)."));
            assert!(formatted.contains("ANWESEND: Lisa (Konzept), Thomas (Review)"));
            assert!(!formatted.trim_start().starts_with('{'));
            assert_eq!(
                metadata.get("environment").unwrap(),
                &perception.environment_text
            );
            assert_eq!(metadata.get("acoustic").unwrap(), &perception.acoustic_text);

            // Traffic Control Metadata
            assert_eq!(metadata.get("room_id").unwrap(), "buero-design-1");
            assert_eq!(metadata.get("max_priority").unwrap(), "P2");
            assert!(metadata.get("synth_fp").unwrap().starts_with("H3|E7|"));
            assert_eq!(metadata.get("is_directly_addressed").unwrap(), "false");
            assert_eq!(metadata.get("personality_type").unwrap(), "E");
        }

        #[test]
        fn circuit_breaker_updates_open_signal() {
            let open_signal = Arc::new(AtomicBool::new(false));
            let mut breaker =
                CircuitBreaker::new(2, Duration::from_millis(1), Arc::clone(&open_signal));

            assert!(!breaker.is_open());
            assert!(!open_signal.load(Ordering::Relaxed));

            breaker.record_failure();
            assert!(!breaker.is_open());
            assert!(!open_signal.load(Ordering::Relaxed));

            breaker.record_failure();
            assert!(breaker.is_open());
            assert!(open_signal.load(Ordering::Relaxed));

            std::thread::sleep(Duration::from_millis(2));
            assert!(!breaker.is_open());
            assert!(!open_signal.load(Ordering::Relaxed));

            breaker.record_success();
            assert!(!breaker.is_open());
            assert!(!open_signal.load(Ordering::Relaxed));
        }

        fn make_perception(agent_id: u16, heard: &str, impulse: bool) -> Perception {
            let im = if impulse { 1 } else { 0 };
            let hr = if heard.is_empty() { 0 } else { 1 };
            Perception {
                agent_id: AgentId(agent_id),
                circadian_text: String::new(),
                body_text: "wach".to_string(),
                environment_text: String::new(),
                acoustic_text: String::new(),
                heard_text: heard.to_string(),
                presence_text: String::new(),
                impulse_text: String::new(),
                is_directly_addressed: false,
                timestamp: Timestamp(100),
                tick: Tick(100),
                room_id: "buero-dev-1".to_string(),
                max_priority: "NONE".to_string(),
                synth_fingerprint: format!(
                    "H5|E5|B3|S3|C5|SN5|R:buero-dev-1|P:2|CH:0|HR:{}|T:10|TMP:0|PE:E|IM:{}",
                    hr, im
                ),
                personality_type: "E".to_string(),
                has_operator_impulse: impulse,
            }
        }

        #[test]
        fn insert_prefer_heard_preserves_im_flag_on_merge() {
            // #295: Wenn Perception A (IM:1, Gaia) und B (HR:1, Chat) gemerged werden,
            // darf der IM-Flag NICHT verloren gehen.
            let mut batch: HashMap<AgentId, Perception> = HashMap::new();

            // Erst: Gaia-Perception (IM:1, kein heard_text)
            let gaia = make_perception(16, "", true);
            assert!(gaia.has_operator_impulse);
            assert!(gaia.synth_fingerprint.contains("|IM:1"));
            insert_prefer_heard(&mut batch, gaia);

            // Dann: Chat-Perception (HR:1, kein IM)
            let chat = make_perception(16, "Thomas sagte: Hallo", false);
            assert!(!chat.has_operator_impulse);
            assert!(chat.synth_fingerprint.contains("|IM:0"));
            insert_prefer_heard(&mut batch, chat);

            // Resultat: BEIDE Flags muessen gesetzt sein
            let merged = batch.get(&AgentId(16)).unwrap();
            assert!(
                !merged.heard_text.is_empty(),
                "heard_text muss aus Chat-Perception uebernommen werden"
            );
            assert!(
                merged.has_operator_impulse,
                "has_operator_impulse muss aus Gaia-Perception bewahrt werden"
            );
            assert!(
                merged.synth_fingerprint.contains("|IM:1"),
                "Fingerprint IM-Flag muss auf 1 korrigiert werden, got: {}",
                merged.synth_fingerprint
            );
        }

        #[test]
        fn insert_prefer_heard_keeps_heard_text_over_empty() {
            let mut batch: HashMap<AgentId, Perception> = HashMap::new();

            // Erst: leere Perception (heartbeat)
            let heartbeat = make_perception(20, "", false);
            insert_prefer_heard(&mut batch, heartbeat);

            // Dann: Perception mit heard_text (Chat)
            let chat = make_perception(20, "Besucher sagte: Hallo", false);
            insert_prefer_heard(&mut batch, chat);

            let merged = batch.get(&AgentId(20)).unwrap();
            assert_eq!(merged.heard_text, "Besucher sagte: Hallo");
            assert!(merged.synth_fingerprint.contains("|HR:1"));
        }

        #[test]
        fn insert_prefer_heard_does_not_replace_heard_with_empty() {
            let mut batch: HashMap<AgentId, Perception> = HashMap::new();

            // Erst: Perception mit heard_text
            let chat = make_perception(20, "Besucher sagte: Hallo", false);
            insert_prefer_heard(&mut batch, chat);

            // Dann: leere heartbeat Perception
            let heartbeat = make_perception(20, "", false);
            insert_prefer_heard(&mut batch, heartbeat);

            let merged = batch.get(&AgentId(20)).unwrap();
            assert_eq!(
                merged.heard_text, "Besucher sagte: Hallo",
                "heard_text darf nicht durch leere Perception ueberschrieben werden"
            );
        }

        #[test]
        fn should_retry_perception_for_room_chat() {
            let perception = make_perception(21, "Besucher sagte: Hallo", false);
            assert!(should_retry_perception(&perception));
        }

        #[test]
        fn should_not_retry_plain_heartbeat() {
            let perception = make_perception(22, "", false);
            assert!(!should_retry_perception(&perception));
        }
    }
}
