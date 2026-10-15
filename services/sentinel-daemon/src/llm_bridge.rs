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
#[path = "llm_bridge/completion_recovery.rs"]
mod completion_recovery;
#[cfg(feature = "llm")]
#[path = "llm_bridge/provider_blocking_lanes.rs"]
mod provider_blocking_lanes;
#[cfg(feature = "llm")]
#[path = "llm_bridge/provider_preparation_controller.rs"]
mod provider_preparation_controller;

#[cfg(feature = "llm")]
pub mod bridge {
    use super::completion_recovery::CompletionRecovery;
    use super::provider_blocking_lanes::{
        BlockingExecutor, JobCancellation, JobError, Lane, OwnedBlockingLanes, SubmitError,
    };
    use super::provider_preparation_controller::ProviderPreparationController;
    use std::collections::{BTreeMap, HashMap};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex, OnceLock, RwLock};
    use std::time::{Duration, Instant};

    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};
    use tokio::sync::{watch, Mutex as AsyncMutex, Semaphore};
    use tokio::task::JoinSet;
    use tracing::{debug, error, info, instrument, warn};

    use crate::workflow_api::model_execution::{
        ModelExecutionCompletion as ModelWorkCompletion, ModelExecutionContext as ModelWorkContext,
        ProviderExecutionAuthority,
    };
    use crate::workflow_api::model_work::MAX_MODEL_WORK_BYTES;
    use sentinel_common::{
        ActionType, AgentAction, AgentId, CostSource, DomainEvent, DomainEventPayload,
        HierarchyTier, Perception, Tick, Timestamp,
    };
    use sentinel_limbo::{
        EventStore, LlmCompletionEntry, LlmModelReservationV1, LlmModelSubjectV1,
        LlmModelUsageBindingV1, LlmRetrospectiveUnknownModelEvidenceV1,
        LlmSealedUnknownModelEvidenceV1,
    };
    use sentinel_redb::StateStore;

    pub type SharedLlmActivityTicks = Arc<Mutex<HashMap<AgentId, u64>>>;

    /// Durable company authority attached to exactly one provider effect.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct ProviderUsageAuthority {
        pub tenant_id: String,
        pub project_id: String,
        pub work_item_id: String,
        pub reservation_id: String,
        pub assignment_id: String,
        pub assignment_version: u64,
        pub agent_id: AgentId,
        pub provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub subscription_grant: Option<sentinel_workflow::SubscriptionCallGrantV1>,
    }

    /// Bounded, effect-local perception state. It is model input, never authority;
    /// the serialized value is retained in the request digest for retry identity.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AgentPerceptionSnapshotV1 {
        schema_version: u16,
        agent_id: AgentId,
        tick: u64,
        room_id: String,
        circadian: String,
        body: String,
        environment: String,
        acoustic: String,
        heard: String,
        presence: String,
        impulse: String,
        max_priority: String,
        synth_fingerprint: String,
        personality_type: String,
        is_directly_addressed: bool,
        has_operator_impulse: bool,
        evolution_version: u64,
        evolution_voice: String,
        evolution_notes: String,
        evolution_narrative: String,
        evolution_facts: String,
    }

    const PERCEPTION_FIELD_LIMIT: usize = 4096;

    fn bounded_perception_text(value: &str) -> String {
        value.chars().take(PERCEPTION_FIELD_LIMIT).collect()
    }

    fn perception_snapshot_json(
        perception: &Perception,
        metadata: &BTreeMap<String, String>,
    ) -> Result<String, serde_json::Error> {
        serde_json::to_string(&AgentPerceptionSnapshotV1 {
            schema_version: 1,
            agent_id: perception.agent_id,
            tick: perception.tick.0,
            room_id: bounded_perception_text(&perception.room_id),
            circadian: bounded_perception_text(&perception.circadian_text),
            body: bounded_perception_text(&perception.body_text),
            environment: bounded_perception_text(&perception.environment_text),
            acoustic: bounded_perception_text(&perception.acoustic_text),
            heard: bounded_perception_text(&perception.heard_text),
            presence: bounded_perception_text(&perception.presence_text),
            impulse: bounded_perception_text(&perception.impulse_text),
            max_priority: bounded_perception_text(&perception.max_priority),
            synth_fingerprint: bounded_perception_text(&perception.synth_fingerprint),
            personality_type: bounded_perception_text(&perception.personality_type),
            is_directly_addressed: perception.is_directly_addressed,
            has_operator_impulse: perception.has_operator_impulse,
            evolution_version: metadata
                .get("evolution_version")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            evolution_voice: metadata
                .get("evolution_voice")
                .map_or_else(String::new, |value| bounded_perception_text(value)),
            evolution_notes: metadata
                .get("evolution_notes")
                .map_or_else(String::new, |value| bounded_perception_text(value)),
            evolution_narrative: metadata
                .get("evolution_narrative")
                .map_or_else(String::new, |value| bounded_perception_text(value)),
            evolution_facts: metadata
                .get("evolution_facts")
                .map_or_else(String::new, |value| bounded_perception_text(value)),
        })
    }

    #[derive(Debug)]
    pub struct PreparedProviderCandidate {
        pub authority: Option<ProviderExecutionAuthority>,
        pub prepared_context: Option<ModelWorkContext>,
    }

    pub trait ProviderUsageAuthorityResolver: Send + Sync {
        fn allows_unbound_provider_usage(&self) -> bool {
            true
        }

        fn is_provider_usage_candidate(&self, _agent_id: AgentId) -> Result<bool, &'static str> {
            Ok(true)
        }

        fn resolve_provider_usage_authority(
            &self,
            agent_id: AgentId,
        ) -> Result<Option<ProviderExecutionAuthority>, &'static str>;

        /// Contexts not already required by resolution stay deferred until recovery.
        fn prepare_provider_usage_candidate(
            &self,
            agent_id: AgentId,
        ) -> Result<Option<PreparedProviderCandidate>, &'static str> {
            if !self.is_provider_usage_candidate(agent_id)? {
                return Ok(None);
            }
            Ok(Some(PreparedProviderCandidate {
                authority: self.resolve_provider_usage_authority(agent_id)?,
                prepared_context: None,
            }))
        }

        /// Validate a chosen authority; adapters may separate this from scheduling.
        fn validate_provider_usage_authority(
            &self,
            expected: &ProviderExecutionAuthority,
        ) -> Result<bool, &'static str> {
            self.resolve_provider_usage_authority(expected.agent_id())
                .map(|current| current.as_ref() == Some(expected))
        }

        fn model_work_context(
            &self,
            _authority: &ProviderExecutionAuthority,
        ) -> Result<Option<ModelWorkContext>, &'static str> {
            Ok(None)
        }

        /// One fresh proof may certify both bindings within this gate, never across gates.
        fn validate_prepared_provider_model_context(
            &self,
            _expected: &ProviderExecutionAuthority,
            _context: &ModelWorkContext,
        ) -> Result<Option<()>, String> {
            Ok(None)
        }

        /// Returns true only when durable workflow authority proves that the
        /// provider dispatch for this exact request was never committed.
        fn provider_dispatch_is_definitively_absent(
            &self,
            _authority: &ProviderExecutionAuthority,
            _request_id: &str,
            _request_digest: &str,
        ) -> Result<bool, &'static str> {
            Ok(false)
        }

        fn admit_model_work(
            &self,
            _completion: &ModelWorkCompletion,
            _request_id: &str,
            _request_digest: &str,
        ) -> Result<(), &'static str> {
            Err("model work adapter is unavailable")
        }
    }

    /// LLM Bridge Konfiguration.
    #[derive(Clone)]
    pub struct LlmBridgeConfig {
        /// Cortex Gateway Base URL (default: http://localhost:8080)
        pub gateway_url: String,
        /// Max parallele LLM Calls
        pub max_concurrent: usize,
        /// Min Ticks zwischen LLM Calls pro Agent
        pub min_ticks_between_calls: u64,
        /// HTTP Request Timeout
        pub request_timeout: Duration,
        /// Maximum time to drain already reserved provider calls during shutdown.
        pub shutdown_drain_timeout: Duration,
        /// Circuit Breaker: Failures bis Open
        pub circuit_breaker_threshold: u32,
        /// Circuit Breaker: Reset-Zeit nach Open
        pub circuit_breaker_reset: Duration,
        /// Dedicated agent-runtime credential. Never log this value.
        pub credential: String,
        /// Producer cutover flag. Disabled until the hierarchy projection has backfilled.
        pub usage_v2_enabled: bool,
        /// Delay between durable completed-response recovery passes.
        pub completion_retry_interval: Duration,
        /// Finite local usage-persistence attempts before fail-closed quarantine.
        pub completion_max_attempts: u32,
        /// Optional productive company-workflow authority. Ambient agent calls
        /// remain valid without a binding, but cannot be committed as project cost.
        pub provider_usage_authority: Option<Arc<dyn ProviderUsageAuthorityResolver>>,
        /// Shared with the authenticated recovery boundary; never exported to a client.
        pub active_provider_requests: Option<Arc<Mutex<HashMap<String, usize>>>>,
    }

    impl Default for LlmBridgeConfig {
        fn default() -> Self {
            Self {
                gateway_url: "http://localhost:8080".to_string(),
                max_concurrent: 8,
                min_ticks_between_calls: 5,
                request_timeout: Duration::from_secs(35),
                shutdown_drain_timeout: Duration::from_secs(40),
                circuit_breaker_threshold: 3,
                circuit_breaker_reset: Duration::from_secs(30),
                credential: String::new(),
                usage_v2_enabled: false,
                completion_retry_interval: Duration::from_secs(1),
                completion_max_attempts: 5,
                provider_usage_authority: None,
                active_provider_requests: None,
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
        terminal: Arc<Mutex<bool>>,
    }

    impl CircuitBreaker {
        #[cfg(test)]
        fn new(threshold: u32, reset_duration: Duration, open_signal: Arc<AtomicBool>) -> Self {
            Self::with_terminal(
                threshold,
                reset_duration,
                open_signal,
                Arc::new(Mutex::new(false)),
            )
        }

        fn with_terminal(
            threshold: u32,
            reset_duration: Duration,
            open_signal: Arc<AtomicBool>,
            terminal: Arc<Mutex<bool>>,
        ) -> Self {
            Self {
                failure_count: 0,
                threshold,
                last_failure: None,
                reset_duration,
                open_signal,
                terminal,
            }
        }

        fn is_open(&self) -> bool {
            let terminal = self
                .terminal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *terminal {
                self.open_signal.store(true, Ordering::Release);
                return true;
            }
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
            let terminal = self
                .terminal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.failure_count = 0;
            self.last_failure = None;
            self.open_signal.store(*terminal, Ordering::Release);
        }

        fn record_failure(&mut self) {
            self.failure_count += 1;
            self.last_failure = Some(Instant::now());
            let _ = self.is_open();
        }

        fn mark_terminal(&self) {
            publish_terminal_unavailability(&self.open_signal, &self.terminal);
        }
    }

    fn publish_terminal_unavailability(signal: &AtomicBool, terminal: &Mutex<bool>) {
        let mut terminal = terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *terminal = true;
        signal.store(true, Ordering::Release);
    }

    // -- Gateway Request/Response Types --

    #[derive(Debug, Serialize)]
    struct GatewayRequest {
        messages: Vec<GatewayMessage>,
        temperature: f64,
        max_tokens: i32,
        model: String,
        metadata: BTreeMap<String, String>,
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
        decision: String,
        #[serde(default)]
        actions: Vec<ExtractedAction>,
        #[serde(default)]
        tokens_used: i32,
        #[serde(default)]
        request_id: String,
        #[serde(default)]
        provider: String,
        // #427: cache-aware breakdown + gateway-resolved tier + per-call cost. input_tokens
        // is the FOLDED input (fresh + cache); the fresh input for the event is recovered as
        // input_tokens - cache_read - cache_creation (matches the gateway per-agent counter).
        #[serde(default)]
        input_tokens: u32,
        #[serde(default)]
        output_tokens: u32,
        #[serde(default)]
        cache_read: u32,
        #[serde(default)]
        cache_creation: u32,
        #[serde(default)]
        tier: String,
        #[serde(default)]
        cost_usd: f64,
        hierarchy_tier: Option<HierarchyTier>,
        cost_source: Option<CostSource>,
        #[serde(default)]
        effective_model: String,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct CompletedLlmResponse {
        version: u32,
        request_id: String,
        request_digest: String,
        usage_event: DomainEvent,
        actions: Vec<AgentAction>,
        tokens_used: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_work: Option<ModelWorkCompletion>,
        // Digest of raw response content, even when oversized content is discarded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_response_digest: Option<String>,
    }

    trait CompletionStore: Send + Sync {
        fn seal_output_schema_rejection(
            &self,
            _reservation: &LlmModelReservationV1,
            _tick: u64,
        ) -> anyhow::Result<bool> {
            anyhow::bail!("store does not support durable provider diagnostics")
        }
        fn bind_model_reservation(&self, reservation: &LlmModelReservationV1)
            -> anyhow::Result<()>;
        fn sealed_model_evidence(
            &self,
            request_id: &str,
            request_digest: &str,
            owner: &sentinel_common::StateTransferScope,
        ) -> anyhow::Result<Option<LlmSealedUnknownModelEvidenceV1>>;
        fn persist_sealed_model_usage(
            &self,
            evidence: &LlmSealedUnknownModelEvidenceV1,
            event: &DomainEvent,
        ) -> anyhow::Result<bool>;
        fn reserve_request(
            &self,
            request_id: &str,
            request_digest: &str,
            agent_id: &str,
        ) -> anyhow::Result<bool>;
        fn release_undispatched_request(
            &self,
            request_id: &str,
            request_digest: &str,
        ) -> anyhow::Result<bool>;
        fn enqueue_completion(
            &self,
            request_id: &str,
            request_digest: &str,
            payload: &str,
        ) -> anyhow::Result<()>;
        fn get_completion(&self, request_id: &str) -> anyhow::Result<Option<LlmCompletionEntry>>;
        fn poll_completions(&self, limit: usize) -> anyhow::Result<Vec<LlmCompletionEntry>>;
        fn poll_provider_in_flight(&self, limit: usize) -> anyhow::Result<Vec<LlmCompletionEntry>>;
        fn mark_provider_unknown(
            &self,
            request_id: &str,
            request_digest: &str,
            reason: &str,
        ) -> anyhow::Result<bool>;
        fn persist_usage(
            &self,
            request_id: &str,
            request_digest: &str,
            event: &DomainEvent,
        ) -> anyhow::Result<()>;
        fn record_failure(
            &self,
            request_id: &str,
            request_digest: &str,
            error: &str,
            max_attempts: u32,
        ) -> anyhow::Result<(u32, bool)>;
        fn claim_actions(&self, request_id: &str, request_digest: &str) -> anyhow::Result<bool>;
        fn complete_actions(&self, request_id: &str, request_digest: &str) -> anyhow::Result<bool>;
        fn has_operation(&self, operation_id: &str) -> anyhow::Result<bool>;
    }

    impl CompletionStore for EventStore {
        fn seal_output_schema_rejection(
            &self,
            reservation: &LlmModelReservationV1,
            tick: u64,
        ) -> anyhow::Result<bool> {
            self.seal_llm_model_output_schema_rejection(reservation, tick)
        }
        fn bind_model_reservation(
            &self,
            reservation: &LlmModelReservationV1,
        ) -> anyhow::Result<()> {
            self.bind_llm_model_reservation(reservation)
        }
        fn sealed_model_evidence(
            &self,
            request_id: &str,
            request_digest: &str,
            owner: &sentinel_common::StateTransferScope,
        ) -> anyhow::Result<Option<LlmSealedUnknownModelEvidenceV1>> {
            self.sealed_unknown_llm_model_evidence(request_id, request_digest, owner)
        }
        fn persist_sealed_model_usage(
            &self,
            evidence: &LlmSealedUnknownModelEvidenceV1,
            event: &DomainEvent,
        ) -> anyhow::Result<bool> {
            self.persist_sealed_unknown_llm_model_usage(evidence, event)
        }
        fn poll_provider_in_flight(&self, limit: usize) -> anyhow::Result<Vec<LlmCompletionEntry>> {
            self.poll_llm_provider_in_flight(limit)
        }

        fn mark_provider_unknown(
            &self,
            request_id: &str,
            request_digest: &str,
            reason: &str,
        ) -> anyhow::Result<bool> {
            self.mark_llm_provider_outcome_unknown(request_id, request_digest, reason)
        }

        fn reserve_request(
            &self,
            request_id: &str,
            request_digest: &str,
            agent_id: &str,
        ) -> anyhow::Result<bool> {
            self.reserve_llm_request(request_id, request_digest, agent_id)
        }

        fn release_undispatched_request(
            &self,
            request_id: &str,
            request_digest: &str,
        ) -> anyhow::Result<bool> {
            self.release_undispatched_llm_request(request_id, request_digest)
        }

        fn enqueue_completion(
            &self,
            request_id: &str,
            request_digest: &str,
            payload: &str,
        ) -> anyhow::Result<()> {
            self.enqueue_llm_completion(request_id, request_digest, payload)
        }

        fn get_completion(&self, request_id: &str) -> anyhow::Result<Option<LlmCompletionEntry>> {
            self.get_llm_completion(request_id)
        }

        fn poll_completions(&self, limit: usize) -> anyhow::Result<Vec<LlmCompletionEntry>> {
            self.poll_llm_completions(limit)
        }

        fn persist_usage(
            &self,
            request_id: &str,
            request_digest: &str,
            event: &DomainEvent,
        ) -> anyhow::Result<()> {
            self.persist_llm_completion_usage(request_id, request_digest, event)
        }

        fn record_failure(
            &self,
            request_id: &str,
            request_digest: &str,
            error: &str,
            max_attempts: u32,
        ) -> anyhow::Result<(u32, bool)> {
            self.record_llm_completion_failure(request_id, request_digest, error, max_attempts)
        }

        fn claim_actions(&self, request_id: &str, request_digest: &str) -> anyhow::Result<bool> {
            self.claim_llm_completion_actions(request_id, request_digest)
        }

        fn complete_actions(&self, request_id: &str, request_digest: &str) -> anyhow::Result<bool> {
            self.complete_llm_completion_actions(request_id, request_digest)
        }

        fn has_operation(&self, operation_id: &str) -> anyhow::Result<bool> {
            self.has_event_operation_id(operation_id)
        }
    }

    type ActiveProviderRequests = Arc<Mutex<HashMap<String, usize>>>;

    // Scheduling ownership is not provider execution and must not affect orphan recovery.
    type PreparingProviderRequests = Arc<Mutex<HashMap<String, ()>>>;

    struct ProviderPreparationGuard {
        requests: PreparingProviderRequests,
        request_id: String,
    }

    impl ProviderPreparationGuard {
        fn try_acquire(requests: &PreparingProviderRequests, request_id: &str) -> Option<Self> {
            let mut pending = requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if pending.contains_key(request_id) {
                return None;
            }
            pending.insert(request_id.to_owned(), ());
            Some(Self {
                requests: Arc::clone(requests),
                request_id: request_id.to_owned(),
            })
        }
    }

    impl Drop for ProviderPreparationGuard {
        fn drop(&mut self) {
            self.requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&self.request_id);
        }
    }

    // Register before reserving, so the orphan watcher cannot race live dispatch.
    // Counts also protect the original task when a duplicate reservation is denied.
    struct ProviderOutcomeGuard<S: CompletionStore> {
        store: Arc<S>,
        request_id: String,
        request_digest: String,
        active: ActiveProviderRequests,
        armed: bool,
    }

    impl<S: CompletionStore> ProviderOutcomeGuard<S> {
        fn new(
            store: Arc<S>,
            active: ActiveProviderRequests,
            request_id: &str,
            request_digest: &str,
        ) -> Self {
            *active
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(request_id.to_owned())
                .or_default() += 1;
            Self {
                store,
                active,
                request_id: request_id.to_owned(),
                request_digest: request_digest.to_owned(),
                armed: false,
            }
        }

        fn disarm_if_resolved(&mut self) {
            match self.store.get_completion(&self.request_id) {
                Ok(None) => self.armed = false,
                Ok(Some(entry))
                    if entry.request_digest == self.request_digest
                        && (entry.status != "provider_in_flight" || !entry.payload.is_empty()) =>
                {
                    self.armed = false;
                }
                _ => {}
            }
        }
    }

    impl<S: CompletionStore> Drop for ProviderOutcomeGuard<S> {
        fn drop(&mut self) {
            self.disarm_if_resolved();
            if self.armed {
                if let Err(error) = self.store.mark_provider_unknown(
                    &self.request_id,
                    &self.request_digest,
                    "UnknownOutcome: bridge_task_ended_without_durable_response",
                ) {
                    // Completion/release may remove the row between lookup and mark.
                    self.disarm_if_resolved();
                    if self.armed {
                        error!(request_id = %self.request_id, error = %error, "Provider outcome classification failed closed");
                    }
                }
            }
            let mut active = self
                .active
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(count) = active.get_mut(&self.request_id) {
                *count -= 1;
                if *count == 0 {
                    active.remove(&self.request_id);
                }
            }
        }
    }

    fn unix_now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    fn classify_stale_provider_requests<S: CompletionStore>(
        store: &S,
        active: &ActiveProviderRequests,
        now_ms: u64,
        request_timeout: Duration,
    ) {
        // Age bounds orphaned transport only. Live tasks have the reqwest deadline
        // and their drop guard; local durable persistence must not be interrupted.
        let active = active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let age = u64::try_from(request_timeout.as_millis())
            .unwrap_or(u64::MAX)
            .saturating_add(10_000);
        match store.poll_provider_in_flight(64) {
            Ok(entries) => {
                for entry in entries {
                    if entry.status == "provider_in_flight"
                        && entry.payload.is_empty()
                        && !active.contains_key(&entry.request_id)
                        && now_ms >= entry.created_at.saturating_add(age)
                    {
                        if let Err(error) = store.mark_provider_unknown(
                            &entry.request_id,
                            &entry.request_digest,
                            "UnknownOutcome: provider_transport_deadline_elapsed",
                        ) {
                            let unresolved = store
                                .get_completion(&entry.request_id)
                                .map(|current| {
                                    current.is_some_and(|current| {
                                        current.status == "provider_in_flight"
                                            && current.payload.is_empty()
                                    })
                                })
                                .unwrap_or(true);
                            if unresolved {
                                error!(request_id = %entry.request_id, error = %error, "Stale provider classification failed closed");
                            }
                        }
                    }
                }
            }
            Err(error) => error!(error = %error, "Provider reservation scan failed closed"),
        }
    }

    pub fn stop_provider_admission(admission: &RwLock<bool>) {
        let mut open = admission
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *open = false;
    }

    struct CloseProviderAdmissionOnExit(Arc<RwLock<bool>>);

    impl Drop for CloseProviderAdmissionOnExit {
        fn drop(&mut self) {
            stop_provider_admission(self.0.as_ref());
        }
    }

    fn reserve_provider_request_until<S: CompletionStore>(
        admission: &RwLock<bool>,
        store: &S,
        request_id: &str,
        request_digest: &str,
        agent_id: &str,
        cancelled: &impl Fn() -> bool,
    ) -> anyhow::Result<Option<bool>> {
        let open = admission
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*open || cancelled() {
            return Ok(None);
        }
        store
            .reserve_request(request_id, request_digest, agent_id)
            .map(Some)
    }

    fn release_connect_failed_request<S: CompletionStore>(
        store: &S,
        request_id: &str,
        request_digest: &str,
        error: &reqwest::Error,
    ) -> bool {
        if !error.is_connect() {
            return false;
        }
        match store.release_undispatched_request(request_id, request_digest) {
            Ok(true) => true,
            Ok(false) => {
                warn!(request_id, "Undispatched LLM reservation was not released");
                false
            }
            Err(release_error) => {
                error!(request_id, error = %release_error, "Undispatched LLM reservation release failed closed");
                false
            }
        }
    }

    fn release_pre_provider_rejection<S: CompletionStore>(
        store: &S,
        request_id: &str,
        request_digest: &str,
        provider_io: Option<&str>,
    ) -> bool {
        if provider_io != Some("not-started") {
            return false;
        }
        match store.release_undispatched_request(request_id, request_digest) {
            Ok(true) => true,
            Ok(false) => {
                warn!(request_id, "Pre-provider LLM reservation was not released");
                false
            }
            Err(release_error) => {
                error!(request_id, error = %release_error, "Pre-provider LLM reservation release failed closed");
                false
            }
        }
    }

    fn release_stale_undispatched_subscription<S: CompletionStore>(
        store: &S,
        resolver: Option<&dyn ProviderUsageAuthorityResolver>,
        authority: Option<&ProviderExecutionAuthority>,
        entry: &LlmCompletionEntry,
        now_ms: u64,
        request_timeout: Duration,
    ) -> bool {
        const PRE_PROVIDER_GRACE_MS: u64 = 10_000;
        let request_timeout_ms = request_timeout.as_millis().try_into().unwrap_or(u64::MAX);
        let recovery_age_ms = request_timeout_ms.saturating_add(PRE_PROVIDER_GRACE_MS);
        if entry.status != "provider_in_flight"
            || now_ms < entry.created_at.saturating_add(recovery_age_ms)
        {
            return false;
        }
        let (Some(resolver), Some(authority)) = (resolver, authority) else {
            return false;
        };
        match resolver.provider_dispatch_is_definitively_absent(
            authority,
            &entry.request_id,
            &entry.request_digest,
        ) {
            Ok(true) => release_pre_provider_rejection(
                store,
                &entry.request_id,
                &entry.request_digest,
                Some("not-started"),
            ),
            Ok(false) => false,
            Err(reason) => {
                warn!(request_id = %entry.request_id, reason, "Undispatched provider recovery proof unavailable");
                false
            }
        }
    }

    fn is_output_schema_rejection(
        status: reqwest::StatusCode,
        headers: &reqwest::header::HeaderMap,
    ) -> bool {
        if status != reqwest::StatusCode::BAD_GATEWAY {
            return false;
        }
        let mut diagnostic = headers.get_all("x-sentinel-provider-diagnostic").iter();
        if diagnostic.next().and_then(|value| value.to_str().ok())
            != Some("codex_output_schema_rejected")
            || diagnostic.next().is_some()
        {
            return false;
        }
        // An admission rejection cannot attest a post-dispatch provider failure.
        let mut provider_io = headers.get_all("x-sentinel-provider-io").iter();
        matches!((provider_io.next(), provider_io.next()), (None, None))
    }

    fn persist_output_schema_rejection<S: CompletionStore>(
        store: &S,
        context: Option<&ModelWorkContext>,
        request_id: &str,
        request_digest: &str,
        tick: u64,
        status: reqwest::StatusCode,
        headers: &reqwest::header::HeaderMap,
    ) -> bool {
        if !is_output_schema_rejection(status, headers) {
            return false;
        }
        let Some(context) = context else {
            return false;
        };
        match model_reservation(context, request_id, request_digest) {
            Ok(Some(reservation)) => match store.seal_output_schema_rejection(&reservation, tick) {
                Ok(_) => true,
                Err(error) => {
                    warn!(request_id, error = %error, "Provider diagnostic could not be sealed");
                    false
                }
            },
            _ => false,
        }
    }

    fn handle_gateway_rejection<S: CompletionStore>(
        store: &S,
        context: Option<&ModelWorkContext>,
        request_id: &str,
        request_digest: &str,
        tick: u64,
        status: reqwest::StatusCode,
        headers: &reqwest::header::HeaderMap,
    ) -> bool {
        let mut provider_io = headers.get_all("x-sentinel-provider-io").iter();
        let first = provider_io.next();
        let duplicate = provider_io.next().is_some();
        if !headers.contains_key("x-sentinel-provider-diagnostic")
            && !duplicate
            && release_pre_provider_rejection(
                store,
                request_id,
                request_digest,
                first.and_then(|value| value.to_str().ok()),
            )
        {
            return true;
        }
        persist_output_schema_rejection(
            store,
            context,
            request_id,
            request_digest,
            tick,
            status,
            headers,
        )
    }

    pub(crate) fn model_reservation(
        context: &ModelWorkContext,
        request_id: &str,
        request_digest: &str,
    ) -> Result<Option<LlmModelReservationV1>, String> {
        let context_digest = match context {
            ModelWorkContext::AdaptiveLeadershipReview(value) => value.context_digest.clone(),
            _ => format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(context).map_err(|error| error.to_string())?)
            ),
        };
        model_reservation_for_authority(
            &context.binding(),
            request_id,
            request_digest,
            context_digest,
        )
    }

    fn model_reservation_for_authority(
        authority: &ProviderExecutionAuthority,
        request_id: &str,
        request_digest: &str,
        context_digest: String,
    ) -> Result<Option<LlmModelReservationV1>, String> {
        let (subject, allowance_id, usage_binding) = match authority {
            ProviderExecutionAuthority::Adaptive(value) => {
                let grant = &value.grant;
                (
                    LlmModelSubjectV1::Adaptive {
                        session_id: grant.session_id,
                        effect_id: value.effect_id,
                        session_version: value.session_version,
                    },
                    grant.provider_allowance_id.clone(),
                    LlmModelUsageBindingV1 {
                        agent_id: grant.authority.agent_id,
                        tenant_id: grant.authority.tenant_id.0.clone(),
                        project_id: grant.authority.project_id.0.clone(),
                        work_item_id: grant.authority.work_item_id.0.clone(),
                        reservation_id: grant.provider_allowance_id.clone(),
                        assignment_id: value.assignment_id.clone(),
                        assignment_version: grant.authority.assignment_version,
                        provider: grant.provider.clone(),
                        model: grant.model.clone(),
                    },
                )
            }
            ProviderExecutionAuthority::AdaptiveLeadershipReview(value) => {
                let grant = &value.grant;
                (
                    LlmModelSubjectV1::AdaptiveLeadershipReview {
                        review_id: grant.review_id,
                    },
                    value.allowance_id.clone(),
                    LlmModelUsageBindingV1 {
                        agent_id: authority.agent_id(),
                        tenant_id: authority.tenant_id().to_owned(),
                        project_id: grant.project_id.0.clone(),
                        work_item_id: grant.work_item_id.0.clone(),
                        reservation_id: value.reservation_id.clone(),
                        assignment_id: grant.assignment_id.clone(),
                        assignment_version: grant.assignee_authority.assignment_version,
                        provider: grant.provider.clone(),
                        model: grant.model.clone(),
                    },
                )
            }
            _ => return Ok(None),
        };
        if authority.request_id() != request_id {
            return Err("model reservation request identity mismatch".to_owned());
        }
        Ok(Some(LlmModelReservationV1 {
            schema_version: 1,
            request_id: request_id.to_owned(),
            request_digest: request_digest.to_owned(),
            owner_scope: sentinel_common::StateTransferScope::for_agent(
                authority.agent_id().to_string(),
            ),
            subject,
            allowance_id,
            context_digest,
            authority_digest: format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(authority).map_err(|error| error.to_string())?)
            ),
            usage_binding,
        }))
    }

    fn exact_sealed_model_evidence<S: CompletionStore>(
        store: &S,
        context: &ModelWorkContext,
        request_id: &str,
        request_digest: &str,
    ) -> anyhow::Result<Option<LlmSealedUnknownModelEvidenceV1>> {
        let expected = model_reservation(context, request_id, request_digest)
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow::anyhow!("unsupported unknown model subject"))?;
        let evidence =
            store.sealed_model_evidence(request_id, request_digest, &expected.owner_scope)?;
        if let Some(evidence) = &evidence {
            anyhow::ensure!(
                evidence.reservation == expected,
                "sealed model authority or context changed"
            );
        }
        Ok(evidence)
    }

    /// Exact bridge evidence, not authorization to abandon a domain effect.
    /// Reads the sealed context digest without reconstructing historical prompts.
    pub fn sealed_unknown_model_evidence(
        store: &EventStore,
        authority: &ProviderExecutionAuthority,
        request_id: &str,
        request_digest: &str,
    ) -> anyhow::Result<Option<LlmSealedUnknownModelEvidenceV1>> {
        anyhow::ensure!(
            matches!(
                authority,
                ProviderExecutionAuthority::Adaptive(_)
                    | ProviderExecutionAuthority::AdaptiveLeadershipReview(_)
            ) && authority.request_id() == request_id,
            "unsupported or changed model subject"
        );
        let owner =
            sentinel_common::StateTransferScope::for_agent(authority.agent_id().to_string());
        let evidence =
            store.sealed_unknown_llm_model_evidence(request_id, request_digest, &owner)?;
        if let Some(evidence) = &evidence {
            let expected = model_reservation_for_authority(
                authority,
                request_id,
                request_digest,
                evidence.reservation.context_digest.clone(),
            )
            .map_err(anyhow::Error::msg)?;
            anyhow::ensure!(
                expected.as_ref() == Some(&evidence.reservation),
                "sealed model authority changed"
            );
        }
        Ok(evidence)
    }

    /// Internal authenticated Gateway evidence only; never admits a model result.
    /// Historical usage validation deliberately does not renew current authority.
    pub fn retain_sealed_unknown_model_usage(
        store: &EventStore,
        context: &ModelWorkContext,
        request_id: &str,
        request_digest: &str,
        event: &DomainEvent,
    ) -> anyhow::Result<bool> {
        let evidence = exact_sealed_model_evidence(store, context, request_id, request_digest)?
            .ok_or_else(|| anyhow::anyhow!("no sealed unknown model reservation"))?;
        validate_sealed_model_usage(context, event).map_err(anyhow::Error::msg)?;
        store.persist_sealed_unknown_llm_model_usage(&evidence, event)
    }

    /// Trusted historical verifier supplied the provenance at import. This
    /// matches the original authority, without reconstructing prompts or grants.
    /// It does not authorize continuation or assert no provider-private activity.
    pub fn retrospective_unknown_model_evidence(
        store: &EventStore,
        authority: &ProviderExecutionAuthority,
        request_id: &str,
        request_digest: &str,
    ) -> anyhow::Result<Option<LlmRetrospectiveUnknownModelEvidenceV1>> {
        let ProviderExecutionAuthority::Adaptive(value) = authority else {
            anyhow::bail!("unsupported retrospective model authority");
        };
        anyhow::ensure!(
            authority.request_id() == request_id,
            "historical model request changed"
        );
        let grant = &value.grant;
        let owner =
            sentinel_common::StateTransferScope::for_agent(grant.authority.agent_id.to_string());
        let evidence =
            store.retrospective_unknown_llm_model_evidence(request_id, request_digest, &owner)?;
        if let Some(evidence) = &evidence {
            let expected_subject = LlmModelSubjectV1::Adaptive {
                session_id: grant.session_id,
                effect_id: value.effect_id,
                session_version: value.session_version,
            };
            let expected_usage = LlmModelUsageBindingV1 {
                agent_id: grant.authority.agent_id,
                tenant_id: grant.authority.tenant_id.0.clone(),
                project_id: grant.authority.project_id.0.clone(),
                work_item_id: grant.authority.work_item_id.0.clone(),
                reservation_id: grant.provider_allowance_id.clone(),
                assignment_id: value.assignment_id.clone(),
                assignment_version: grant.authority.assignment_version,
                provider: grant.provider.clone(),
                model: grant.model.clone(),
            };
            let authority_digest = format!("{:x}", Sha256::digest(serde_json::to_vec(authority)?));
            let sentinel_limbo::LlmRetrospectiveModelProvenanceV1::JournalAndPinnedInferenceBoundary {
                original_allowance_digest, ..
            } = &evidence.binding.provenance;
            anyhow::ensure!(
                evidence.binding.subject == expected_subject
                    && evidence.binding.allowance_id == grant.provider_allowance_id
                    && evidence.binding.usage_binding == expected_usage
                    && evidence.binding.authority_digest == authority_digest
                    && original_allowance_digest == &grant.provider_authority_digest,
                "retrospective model authority changed"
            );
        }
        Ok(evidence)
    }

    /// Authenticated accounting-only evidence. No historical context is invented
    /// or required; Limbo rechecks the exact imported seal in the append transaction.
    pub fn retain_retrospective_unknown_model_usage(
        store: &EventStore,
        authority: &ProviderExecutionAuthority,
        request_id: &str,
        request_digest: &str,
        event: &DomainEvent,
    ) -> anyhow::Result<bool> {
        let evidence =
            retrospective_unknown_model_evidence(store, authority, request_id, request_digest)?
                .ok_or_else(|| anyhow::anyhow!("no retrospective unknown model evidence"))?;
        store.persist_retrospective_unknown_llm_model_usage(&evidence, event)
    }

    fn validate_sealed_model_usage(
        context: &ModelWorkContext,
        event: &DomainEvent,
    ) -> Result<(), &'static str> {
        ModelWorkCompletion {
            context: context.clone(),
            content: String::new(),
            admissible: false,
        }
        .validate_usage(event)
    }

    fn recover_reserved_model_request<S: CompletionStore>(
        store: &S,
        resolver: Option<&dyn ProviderUsageAuthorityResolver>,
        authority: &ProviderExecutionAuthority,
        action_tx: &mpsc::Sender<AgentAction>,
        max_attempts: u32,
        now_ms: u64,
        request_timeout: Duration,
    ) -> anyhow::Result<bool> {
        let request_id = authority.request_id();
        let Some(entry) = store.get_completion(&request_id)? else {
            return Ok(false);
        };
        anyhow::ensure!(
            entry.request_id == request_id
                && entry.owner_scope
                    == sentinel_common::StateTransferScope::for_agent(
                        authority.agent_id().to_string(),
                    ),
            "reserved model request owner changed"
        );
        // A later tick is new perception, not a new version of the reserved
        // effect. Recovery uses only its original digest and stored completion.
        if entry.status == "provider_in_flight" {
            if release_stale_undispatched_subscription(
                store,
                resolver,
                Some(authority),
                &entry,
                now_ms,
                request_timeout,
            ) {
                info!(
                    request_id,
                    "Definitively undispatched model reservation released"
                );
            } else {
                debug!(request_id, "Prior model execution remains fail-closed");
            }
        } else {
            recover_completion(store, entry, action_tx, max_attempts, resolver);
        }
        Ok(true)
    }

    fn agent_runtime_request(
        client: &reqwest::Client,
        url: &str,
        credential: &str,
        request_id: &str,
        request_digest: &str,
        request: &GatewayRequest,
    ) -> reqwest::RequestBuilder {
        client
            .post(url)
            .bearer_auth(credential)
            .header("X-Request-ID", request_id)
            .header("X-Request-Digest", request_digest)
            .json(request)
    }

    fn agent_runtime_request_id(
        perception: &Perception,
        authority: Option<&ProviderExecutionAuthority>,
    ) -> String {
        authority.map_or_else(
            || {
                format!(
                    "agent-runtime-{:02}-{}",
                    perception.agent_id.0, perception.tick.0
                )
            },
            ProviderExecutionAuthority::request_id,
        )
    }

    fn gateway_request_digest(request: &GatewayRequest) -> Result<String, serde_json::Error> {
        let bytes = serde_json::to_vec(request)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    /// #427: baut das `AgentLlmUsage`-Event aus einer Gateway-Response. Die frischen
    /// (nicht gecachten) Input-Tokens werden aus dem gefoldeten `input_tokens` rekonstruiert,
    /// damit Event-Aggregation und Gateway-Counter exakt rekonziliieren. `operation_id` ist
    /// deterministisch aus der `request_id` abgeleitet (Idempotenz, kein Doppel-Append).
    fn build_usage_event(
        agent_id: AgentId,
        tick: u64,
        requested_model: &str,
        authority: Option<&ProviderExecutionAuthority>,
        resp: &GatewayResponse,
        usage_v2_enabled: bool,
    ) -> Result<DomainEvent, String> {
        if usage_v2_enabled {
            if resp.effective_model.trim().is_empty() {
                return Err("v2 response missing effective_model".to_string());
            }
            if resp.provider.trim().is_empty() {
                return Err("v2 response missing provider".to_string());
            }
            if resp.tier.trim().is_empty() {
                return Err("v2 response missing model tier".to_string());
            }
            if !resp.cost_usd.is_finite() || resp.cost_usd < 0.0 {
                return Err("v2 response has invalid cost_usd".to_string());
            }
        }
        let fresh_input = resp
            .input_tokens
            .saturating_sub(resp.cache_read)
            .saturating_sub(resp.cache_creation);
        let payload = DomainEventPayload::AgentLlmUsage {
            agent_id,
            tenant_id: authority.map(|value| value.tenant_id().to_owned()),
            project_id: authority.and_then(|value| match value {
                ProviderExecutionAuthority::AdaptiveLeadershipReview(review) => {
                    Some(review.grant.project_id.0.clone())
                }
                ProviderExecutionAuthority::Project(project) => Some(project.project_id.clone()),
                ProviderExecutionAuthority::Adaptive(adaptive) => {
                    Some(adaptive.grant.authority.project_id.0.clone())
                }
                ProviderExecutionAuthority::ProjectPlanning(planning) => {
                    Some(planning.grant.project_id.0.clone())
                }
                ProviderExecutionAuthority::RequestSales(_) => None,
            }),
            work_item_id: authority.and_then(|value| match value {
                ProviderExecutionAuthority::AdaptiveLeadershipReview(review) => {
                    Some(review.grant.work_item_id.0.clone())
                }
                ProviderExecutionAuthority::Project(project) => Some(project.work_item_id.clone()),
                ProviderExecutionAuthority::Adaptive(adaptive) => {
                    Some(adaptive.grant.authority.work_item_id.0.clone())
                }
                ProviderExecutionAuthority::RequestSales(_)
                | ProviderExecutionAuthority::ProjectPlanning(_) => None,
            }),
            reservation_id: authority.map(|value| value.reservation_id().to_owned()),
            assignment_id: authority.and_then(|value| match value {
                ProviderExecutionAuthority::AdaptiveLeadershipReview(review) => {
                    Some(review.grant.assignment_id.clone())
                }
                ProviderExecutionAuthority::Project(project) => Some(project.assignment_id.clone()),
                ProviderExecutionAuthority::Adaptive(adaptive) => {
                    Some(adaptive.assignment_id.clone())
                }
                ProviderExecutionAuthority::RequestSales(_)
                | ProviderExecutionAuthority::ProjectPlanning(_) => None,
            }),
            assignment_version: authority.and_then(|value| match value {
                ProviderExecutionAuthority::AdaptiveLeadershipReview(review) => {
                    Some(review.grant.assignee_authority.assignment_version)
                }
                ProviderExecutionAuthority::Project(project) => Some(project.assignment_version),
                ProviderExecutionAuthority::Adaptive(adaptive) => {
                    Some(adaptive.grant.authority.assignment_version)
                }
                ProviderExecutionAuthority::RequestSales(_)
                | ProviderExecutionAuthority::ProjectPlanning(_) => None,
            }),
            provider: usage_v2_enabled.then(|| resp.provider.clone()),
            requested_model: usage_v2_enabled.then(|| {
                if requested_model.trim().is_empty() {
                    "gateway-policy-default".to_string()
                } else {
                    requested_model.to_string()
                }
            }),
            caller_role: usage_v2_enabled.then(|| "agent_runtime".to_string()),
            tier: resp.tier.clone(),
            hierarchy_tier: if usage_v2_enabled {
                Some(
                    resp.hierarchy_tier
                        .ok_or("v2 response missing hierarchy_tier")?,
                )
            } else {
                None
            },
            cost_source: if usage_v2_enabled {
                Some(resp.cost_source.ok_or("v2 response missing cost_source")?)
            } else {
                None
            },
            effective_model: usage_v2_enabled.then(|| resp.effective_model.clone()),
            input_tokens: fresh_input,
            output_tokens: resp.output_tokens,
            cache_read: resp.cache_read,
            cache_creation: resp.cache_creation,
            cost_usd: resp.cost_usd,
        };
        let aggregate_id = format!("AGENT-{:02}", agent_id.0);
        let mut event = DomainEvent::new(
            payload.event_type_str(),
            &aggregate_id,
            &payload.to_json(),
            &resp.request_id,
            tick,
        );
        if !resp.request_id.is_empty() {
            event = event.with_operation_id(&format!("llm_usage_{}", resp.request_id));
        }
        if usage_v2_enabled {
            event = event.with_schema_version(match authority {
                Some(ProviderExecutionAuthority::AdaptiveLeadershipReview(_)) => 6,
                Some(ProviderExecutionAuthority::RequestSales(_)) => 4,
                Some(ProviderExecutionAuthority::ProjectPlanning(_)) => 5,
                Some(
                    ProviderExecutionAuthority::Project(_)
                    | ProviderExecutionAuthority::Adaptive(_),
                ) => 3,
                None => 2,
            });
        }
        Ok(event)
    }

    fn leadership_response_digest_matches(
        model_work: &ModelWorkCompletion,
        digest: Option<&str>,
    ) -> bool {
        let Some(digest) = digest else {
            return false;
        };
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return false;
        }
        // Oversized raw content is discarded, but its usage and raw digest remain
        // durable. Discarded content can never authorize model-work admission.
        (!model_work.admissible && model_work.content.is_empty())
            || digest == format!("{:x}", Sha256::digest(model_work.content.as_bytes()))
    }

    pub(crate) fn observe_completion_stage<T, E>(
        request_id: &str,
        stage: &'static str,
        action: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let started = Instant::now();
        let result = action();
        let elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        info!(
            request_id,
            stage,
            elapsed_us,
            succeeded = result.is_ok(),
            "Owned completion stage finished"
        );
        result
    }

    fn recover_completion<S: CompletionStore>(
        store: &S,
        entry: LlmCompletionEntry,
        action_tx: &mpsc::Sender<AgentAction>,
        max_attempts: u32,
        resolver: Option<&dyn ProviderUsageAuthorityResolver>,
    ) {
        if !matches!(entry.status.as_str(), "pending_usage" | "ready_for_action") {
            return;
        }
        let Some(_recovery) = CompletionRecovery::try_acquire(store, &entry.request_id) else {
            info!(request_id = %entry.request_id, "Durable completion recovery already active");
            return;
        };
        recover_completion_owned(store, entry, action_tx, max_attempts, resolver, _recovery);
    }

    fn recover_completion_owned<S: CompletionStore>(
        store: &S,
        entry: LlmCompletionEntry,
        action_tx: &mpsc::Sender<AgentAction>,
        max_attempts: u32,
        resolver: Option<&dyn ProviderUsageAuthorityResolver>,
        _recovery: CompletionRecovery<'_, S>,
    ) {
        let current = match observe_completion_stage(&entry.request_id, "recovery_refresh", || {
            store.get_completion(&entry.request_id)
        }) {
            Ok(Some(current)) => current,
            Ok(None) => return,
            Err(error) => {
                error!(request_id = %entry.request_id, error = %error, "Failed to refresh durable completion recovery");
                return;
            }
        };
        if current.request_id != entry.request_id
            || current.request_digest != entry.request_digest
            || current.owner_scope != entry.owner_scope
            || current.payload != entry.payload
            || current.created_at != entry.created_at
        {
            error!(request_id = %entry.request_id, "Durable completion recovery binding changed");
            return;
        }
        if !matches!(
            current.status.as_str(),
            "pending_usage" | "ready_for_action"
        ) {
            return;
        }
        let entry = current;
        let completed: CompletedLlmResponse = match serde_json::from_str(&entry.payload) {
            Ok(completed) => completed,
            Err(error) => {
                error!(request_id = %entry.request_id, error = %error, "Durable LLM completion payload is invalid");
                let _ = store.record_failure(
                    &entry.request_id,
                    &entry.request_digest,
                    &format!("invalid completion payload: {error}"),
                    max_attempts,
                );
                return;
            }
        };
        if !matches!(
            (completed.version, completed.model_work.is_some()),
            (1, false) | (2, true)
        ) || completed.request_id != entry.request_id
            || completed.request_digest != entry.request_digest
            || completed.model_work.as_ref().is_some_and(|model_work| {
                !completed.actions.is_empty()
                    || (matches!(
                        model_work.context,
                        ModelWorkContext::AdaptiveLeadershipReview(_)
                    ) && !leadership_response_digest_matches(
                        model_work,
                        completed.model_response_digest.as_deref(),
                    ))
                    || entry.owner_scope
                        != sentinel_common::StateTransferScope::for_agent(
                            model_work.context.binding().agent_id().to_string(),
                        )
                    || model_work.validate_usage(&completed.usage_event).is_err()
            })
        {
            error!(request_id = %entry.request_id, "Durable LLM completion identity mismatch");
            let _ = store.record_failure(
                &entry.request_id,
                &entry.request_digest,
                "completion identity mismatch",
                max_attempts,
            );
            return;
        }

        if entry.status == "pending_usage" {
            if let Err(error) = observe_completion_stage(&entry.request_id, "usage", || {
                store.persist_usage(
                    &entry.request_id,
                    &entry.request_digest,
                    &completed.usage_event,
                )
            }) {
                match store.record_failure(
                    &entry.request_id,
                    &entry.request_digest,
                    &error.to_string(),
                    max_attempts,
                ) {
                    Ok((attempt, terminal)) => warn!(
                        request_id = %entry.request_id,
                        attempt,
                        max_attempts,
                        terminal,
                        error = %error,
                        "AgentLlmUsage local append failed"
                    ),
                    Err(record_error) => error!(
                        request_id = %entry.request_id,
                        error = %record_error,
                        "Failed to record LLM completion append failure"
                    ),
                }
                return;
            }
        }

        // Admission is durable and idempotent by provider request. Retry it before
        // claiming the legacy channel actions, without repeating provider I/O.
        if let Some(model_work) = &completed.model_work {
            let result = observe_completion_stage(&entry.request_id, "admission", || {
                resolver
                    .ok_or("model work resolver unavailable")
                    .and_then(|resolver| {
                        resolver.admit_model_work(
                            model_work,
                            &entry.request_id,
                            &entry.request_digest,
                        )
                    })
            });
            if let Err(reason) = result {
                let _ = store.record_failure(
                    &entry.request_id,
                    &entry.request_digest,
                    reason,
                    max_attempts,
                );
                warn!(request_id = %entry.request_id, reason, "Model work admission retained fail-closed");
                return;
            }
        }

        let claimed = match observe_completion_stage(&entry.request_id, "claim", || {
            store.claim_actions(&entry.request_id, &entry.request_digest)
        }) {
            Ok(claimed) => claimed,
            Err(error) => {
                error!(request_id = %entry.request_id, error = %error, "Failed to claim completed LLM actions");
                let _ = store.record_failure(
                    &entry.request_id,
                    &entry.request_digest,
                    &format!("action claim failed: {error}"),
                    max_attempts,
                );
                return;
            }
        };
        if !claimed {
            return;
        }

        for action in completed.actions {
            if let Err(error) = action_tx.send(action) {
                error!(
                    request_id = %entry.request_id,
                    error = %error,
                    "Claimed LLM action could not be delivered; left fail-closed"
                );
                return;
            }
        }
        if let Err(error) = observe_completion_stage(&entry.request_id, "complete", || {
            store.complete_actions(&entry.request_id, &entry.request_digest)
        }) {
            error!(
                request_id = %entry.request_id,
                error = %error,
                "Completed LLM action cleanup failed; claim remains non-replayable"
            );
        }
    }

    fn recover_completion_batch<S: CompletionStore>(
        store: &S,
        action_tx: &mpsc::Sender<AgentAction>,
        max_attempts: u32,
        resolver: Option<&dyn ProviderUsageAuthorityResolver>,
    ) {
        match store.poll_completions(64) {
            Ok(entries) => {
                for entry in entries {
                    recover_completion(store, entry, action_tx, max_attempts, resolver);
                }
            }
            Err(error) => error!(error = %error, "Failed to poll durable LLM completions"),
        }
    }

    struct GatewayCompletionContext<'a> {
        request_id: &'a str,
        request_digest: &'a str,
        agent_id: AgentId,
        tick: u64,
        requested_model: &'a str,
        authority: Option<&'a ProviderExecutionAuthority>,
        authority_resolver: Option<&'a dyn ProviderUsageAuthorityResolver>,
        gateway_response: &'a GatewayResponse,
        usage_v2_enabled: bool,
        model_work: Option<&'a ModelWorkContext>,
    }

    fn validate_current_provider_usage_authority(
        resolver: Option<&dyn ProviderUsageAuthorityResolver>,
        expected: Option<&ProviderExecutionAuthority>,
        agent_id: AgentId,
    ) -> Result<(), String> {
        match resolver {
            Some(resolver) => {
                if expected.is_none() && !resolver.allows_unbound_provider_usage() {
                    return Err("provider work requires explicit authority".to_owned());
                }
                let valid = match expected {
                    Some(expected) => {
                        if expected.agent_id() != agent_id {
                            return Err(
                                "provider usage authority returned another agent".to_owned()
                            );
                        }
                        resolver.validate_provider_usage_authority(expected)
                    }
                    None => resolver
                        .resolve_provider_usage_authority(agent_id)
                        .map(|current| current.is_none()),
                }
                .map_err(|reason| format!("provider usage reauthorization failed: {reason}"))?;
                if !valid {
                    return Err("provider usage authority changed during provider I/O".to_string());
                }
            }
            None if expected.is_some() => {
                return Err("provider usage authority resolver is unavailable".to_string());
            }
            None => {}
        }
        Ok(())
    }

    fn validate_provider_usage_mode(
        authority: Option<&ProviderExecutionAuthority>,
        usage_v2_enabled: bool,
        unbound_allowed: bool,
    ) -> Result<(), &'static str> {
        if authority.is_none() && !unbound_allowed {
            return Err("provider work requires explicit authority");
        }
        if authority.is_some() && !usage_v2_enabled {
            return Err("project provider usage requires schema-v3 accounting");
        }
        Ok(())
    }

    fn validate_unreserved_provider_authority(
        resolver: Option<&dyn ProviderUsageAuthorityResolver>,
        expected: Option<&ProviderExecutionAuthority>,
        agent_id: AgentId,
        model_work: Option<&ModelWorkContext>,
    ) -> Result<(), String> {
        if let (Some(resolver), Some(expected), Some(context)) = (resolver, expected, model_work) {
            if expected.agent_id() != agent_id {
                return Err("provider usage authority returned another agent".to_owned());
            }
            if context.binding() == *expected
                && resolver
                    .validate_prepared_provider_model_context(expected, context)?
                    .is_some()
            {
                return Ok(());
            }
        }
        validate_current_provider_usage_authority(resolver, expected, agent_id)
            .and_then(|()| validate_model_work_context(resolver, model_work))
    }

    #[cfg(test)]
    fn validate_pre_dispatch_provider_authority<S: CompletionStore>(
        store: &S,
        resolver: Option<&dyn ProviderUsageAuthorityResolver>,
        expected: Option<&ProviderExecutionAuthority>,
        agent_id: AgentId,
        request_id: &str,
        request_digest: &str,
        model_work: Option<&ModelWorkContext>,
    ) -> Result<(), String> {
        validate_pre_dispatch_provider_authority_until(
            store,
            resolver,
            expected,
            agent_id,
            request_id,
            request_digest,
            model_work,
            &|| false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_pre_dispatch_provider_authority_until<S: CompletionStore>(
        store: &S,
        resolver: Option<&dyn ProviderUsageAuthorityResolver>,
        expected: Option<&ProviderExecutionAuthority>,
        agent_id: AgentId,
        request_id: &str,
        request_digest: &str,
        model_work: Option<&ModelWorkContext>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<(), String> {
        let validation =
            validate_unreserved_provider_authority(resolver, expected, agent_id, model_work)
                .and_then(|()| {
                    if cancelled() {
                        return Err("provider dispatch cancelled before binding".to_owned());
                    }
                    if let Some(context) = model_work {
                        context.validate_dispatch(unix_now_ms())?;
                        if let Some(reservation) =
                            model_reservation(context, request_id, request_digest)?
                        {
                            store
                                .bind_model_reservation(&reservation)
                                .map_err(|error| error.to_string())?;
                        }
                    }
                    Ok(())
                });
        if let Err(reason) = validation {
            return match store.release_undispatched_request(request_id, request_digest) {
                Ok(true) => Err(reason),
                Ok(false) => Err(format!(
                    "{reason}; undispatched provider reservation was not released"
                )),
                Err(error) => Err(format!(
                    "{reason}; undispatched provider reservation release failed: {error}"
                )),
            };
        }
        Ok(())
    }

    fn validate_model_work_context(
        resolver: Option<&dyn ProviderUsageAuthorityResolver>,
        expected: Option<&ModelWorkContext>,
    ) -> Result<(), String> {
        if let Some(expected) = expected {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| "model work clock is unavailable")?
                .as_millis();
            expected.validate_dispatch(
                u64::try_from(now_ms).map_err(|_| "model work clock overflow")?,
            )?;
            let current = resolver
                .ok_or("model work resolver unavailable")?
                .model_work_context(&expected.binding())?;
            if current.as_ref() != Some(expected) {
                return Err("model work authority changed during provider I/O".to_owned());
            }
            expected.validate_dispatch(unix_now_ms())?;
        }
        Ok(())
    }

    fn initial_model_work_context(
        resolver: Option<&dyn ProviderUsageAuthorityResolver>,
        authority: Option<&ProviderExecutionAuthority>,
        prepared_context: Option<ModelWorkContext>,
    ) -> Result<Option<ModelWorkContext>, &'static str> {
        if let Some(context) = prepared_context {
            if authority != Some(&context.binding()) {
                return Err("prepared model work authority changed");
            }
            return Ok(Some(context));
        }
        match (resolver, authority) {
            (Some(resolver), Some(authority)) => resolver.model_work_context(authority),
            _ => Ok(None),
        }
    }

    fn log_provider_preparation_duration(
        agent_id: AgentId,
        request_id: Option<&str>,
        phase: &'static str,
        started: Instant,
        bound: bool,
        accepted: bool,
    ) {
        const SLOW_PREPARATION_MS: u128 = 1_000;
        let elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let elapsed_ms = elapsed_us / 1_000;
        if bound && u128::from(elapsed_ms) >= SLOW_PREPARATION_MS {
            warn!(agent = %agent_id, request_id, phase, elapsed_us, elapsed_ms, accepted,
                "Slow initial provider preparation");
        } else if bound {
            info!(agent = %agent_id, request_id, phase, elapsed_us, elapsed_ms, accepted,
                "Initial provider preparation finished");
        } else {
            debug!(agent = %agent_id, phase, elapsed_ms, bound, accepted,
                "Initial provider preparation finished");
        }
    }

    fn validate_gateway_completion_authority(
        resolver: Option<&dyn ProviderUsageAuthorityResolver>,
        expected: Option<&ProviderExecutionAuthority>,
        gateway_response: &GatewayResponse,
        agent_id: AgentId,
    ) -> Result<(), String> {
        if let Some(expected) = expected {
            if gateway_response.provider != expected.provider() {
                return Err("gateway provider does not match the reserved provider".to_string());
            }
        }
        validate_current_provider_usage_authority(resolver, expected, agent_id)
    }

    fn store_gateway_completion<S: CompletionStore>(
        store: &S,
        action_tx: &mpsc::Sender<AgentAction>,
        context: GatewayCompletionContext<'_>,
        max_attempts: u32,
    ) -> Result<(), String> {
        let GatewayCompletionContext {
            request_id,
            request_digest,
            agent_id,
            tick,
            requested_model,
            authority,
            authority_resolver,
            gateway_response,
            usage_v2_enabled,
            model_work,
        } = context;
        let gateway_resp = gateway_response;
        if gateway_resp.request_id != request_id {
            return Err(format!(
                "gateway request_id mismatch: sent {request_id}, received {}",
                gateway_resp.request_id
            ));
        }
        if let Some(work) = model_work {
            // Usage belongs to the dispatched authority even when that authority
            // was revoked while the provider ran. Admission rechecks currentness
            // only after the completed result is durable.
            if authority != Some(&work.binding())
                || gateway_resp.provider != work.binding().provider()
                || agent_id != work.binding().agent_id()
            {
                return Err("model work response authority mismatch".to_owned());
            }
        } else {
            validate_gateway_completion_authority(
                authority_resolver,
                authority,
                gateway_resp,
                agent_id,
            )?;
        }
        let usage_event = build_usage_event(
            agent_id,
            tick,
            requested_model,
            authority,
            gateway_resp,
            usage_v2_enabled,
        )?;
        let model_response_digest = model_work.and_then(|context| {
            matches!(context, ModelWorkContext::AdaptiveLeadershipReview(_))
                .then(|| format!("{:x}", Sha256::digest(gateway_resp.content.as_bytes())))
        });
        let model_completion = if let Some(context) = model_work {
            if authority != Some(&context.binding()) {
                return Err("model work response authority mismatch".to_owned());
            }
            let bounded = gateway_resp.content.len() <= MAX_MODEL_WORK_BYTES;
            Some(ModelWorkCompletion {
                context: context.clone(),
                content: if bounded {
                    gateway_resp.content.clone()
                } else {
                    String::new()
                },
                admissible: bounded
                    && gateway_resp.decision == "forward"
                    && gateway_resp.output_tokens > 0,
            })
        } else {
            None
        };
        let is_synthesis = gateway_resp.tokens_used == 0;
        let actions = if model_completion.is_some() {
            Vec::new()
        } else {
            gateway_resp
                .actions
                .iter()
                .filter_map(|action| map_extracted_to_action(agent_id, action, tick, is_synthesis))
                .collect()
        };
        let completed = CompletedLlmResponse {
            version: if model_completion.is_some() { 2 } else { 1 },
            request_id: request_id.to_string(),
            request_digest: request_digest.to_string(),
            usage_event,
            actions,
            tokens_used: gateway_resp.tokens_used.max(0) as u64,
            model_response_digest,
            model_work: model_completion,
        };
        let payload =
            observe_completion_stage(request_id, "encode", || serde_json::to_string(&completed))
                .map_err(|error| error.to_string())?;
        // Own the enqueue-to-load window too: a recovery consumer must not
        // finish/delete this row between its successful enqueue and first read.
        let recovery = CompletionRecovery::try_acquire(store, request_id);
        if let Err(error) = observe_completion_stage(request_id, "enqueue", || {
            store.enqueue_completion(request_id, request_digest, &payload)
        }) {
            if let Some(context) = model_work {
                if matches!(
                    context,
                    ModelWorkContext::Adaptive(_) | ModelWorkContext::AdaptiveLeadershipReview(_)
                ) {
                    if let Some(evidence) =
                        exact_sealed_model_evidence(store, context, request_id, request_digest)
                            .map_err(|error| error.to_string())?
                    {
                        validate_sealed_model_usage(context, &completed.usage_event)
                            .map_err(str::to_owned)?;
                        store
                            .persist_sealed_model_usage(&evidence, &completed.usage_event)
                            .map_err(|error| error.to_string())?;
                        // Enqueue and unknown sealing race under the same fence.
                        // If sealing won, only accounting survives, never admission.
                        return Ok(());
                    }
                }
            }
            return Err(error.to_string());
        }
        let Some(recovery) = recovery else {
            info!(request_id, "Durable completion handed to active recovery");
            return Ok(());
        };
        let entry =
            observe_completion_stage(request_id, "load", || store.get_completion(request_id))
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("durable LLM completion {request_id} disappeared"))?;
        recover_completion_owned(
            store,
            entry,
            action_tx,
            max_attempts,
            authority_resolver,
            recovery,
        );
        Ok(())
    }

    /// Synthetic test input through the real serializer, usage append and admission;
    /// this is not evidence of live provider execution.
    #[cfg(test)]
    pub(crate) fn retain_adaptive_gateway_completion_for_test(
        store: &EventStore,
        resolver: &crate::workflow_api::WorkflowApi,
        completion: &ModelWorkCompletion,
        request_id: &str,
        request_digest: &str,
    ) -> Result<(), String> {
        let ModelWorkContext::Adaptive(context) = &completion.context else {
            return Err("producer fixture requires an Adaptive completion".to_owned());
        };
        if !completion.admissible {
            return Err("producer fixture requires an admissible completion".to_owned());
        }
        let authority = completion.context.binding();
        let response = GatewayResponse {
            content: completion.content.clone(),
            decision: "forward".to_owned(),
            actions: Vec::new(),
            tokens_used: 18,
            request_id: request_id.to_owned(),
            provider: authority.provider().to_owned(),
            input_tokens: 11,
            output_tokens: 7,
            cache_read: 0,
            cache_creation: 0,
            tier: "mid".to_owned(),
            cost_usd: 0.0,
            hierarchy_tier: Some(HierarchyTier::TIER_2),
            cost_source: Some(CostSource::ProviderReported),
            effective_model: context.binding.grant.model.clone(),
        };
        let (action_tx, action_rx) = mpsc::channel();
        let result = store_gateway_completion(
            store,
            &action_tx,
            GatewayCompletionContext {
                request_id,
                request_digest,
                agent_id: authority.agent_id(),
                tick: 1,
                requested_model: &context.binding.grant.model,
                authority: Some(&authority),
                authority_resolver: Some(resolver),
                gateway_response: &response,
                usage_v2_enabled: true,
                model_work: Some(&completion.context),
            },
            5,
        );
        assert!(
            action_rx.try_recv().is_err(),
            "model completion emitted a legacy action"
        );
        result
    }

    #[derive(Clone)]
    struct AgentRoutingClaim {
        role: String,
        tier: HierarchyTier,
    }

    static AGENT_ROUTING: OnceLock<RwLock<HashMap<AgentId, AgentRoutingClaim>>> = OnceLock::new();

    /// Replace the node-local derived routing cache after startup or a committed
    /// config apply. Agent TOML remains the source of truth.
    pub fn replace_agent_routing(agents: &[sentinel_common::agent_config::AgentConfig]) {
        let claims = agents
            .iter()
            .map(|agent| {
                let tier = agent.identity.tier.unwrap_or_else(|| {
                    sentinel_common::legacy_hierarchy_tier_from_role(&agent.identity.role)
                });
                (
                    AgentId(agent.identity.id),
                    AgentRoutingClaim {
                        role: agent.identity.role.clone(),
                        tier,
                    },
                )
            })
            .collect();
        let cache = AGENT_ROUTING.get_or_init(|| RwLock::new(HashMap::new()));
        *cache.write().expect("agent routing cache poisoned") = claims;
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
        #[cfg(test)]
        preparations_processed: AtomicU64,
        #[cfg(test)]
        providers_retired: AtomicU64,
        #[cfg(test)]
        perceptions_offered: AtomicU64,
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
                #[cfg(test)]
                preparations_processed: AtomicU64::new(0),
                #[cfg(test)]
                providers_retired: AtomicU64::new(0),
                #[cfg(test)]
                perceptions_offered: AtomicU64::new(0),
            }
        }
    }

    #[cfg(test)]
    struct PreparationProcessed<'a>(&'a AtomicU64);

    #[cfg(test)]
    impl Drop for PreparationProcessed<'_> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
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
        event_store: Arc<EventStore>,
        llm_unavailable: Arc<AtomicBool>,
        llm_activity_ticks: SharedLlmActivityTicks,
    ) {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let provider_admission = Arc::new(RwLock::new(true));
        let result = run_llm_bridge_with_store(
            config,
            perception_rx,
            action_tx,
            telemetry,
            state_store,
            event_store,
            llm_unavailable,
            llm_activity_ticks,
            shutdown_rx,
            provider_admission,
        )
        .await;
        drop(shutdown_tx);
        if let Err(reason) = result {
            error!(reason, "LLM Bridge shutdown failed closed");
        }
    }

    pub async fn run_llm_bridge_with_shutdown(
        config: LlmBridgeConfig,
        perception_rx: mpsc::Receiver<Perception>,
        action_tx: mpsc::Sender<AgentAction>,
        telemetry: Arc<BridgeTelemetry>,
        state_store: Arc<StateStore>,
        event_store: Arc<EventStore>,
        llm_unavailable: Arc<AtomicBool>,
        llm_activity_ticks: SharedLlmActivityTicks,
        shutdown_rx: watch::Receiver<bool>,
        provider_admission: Arc<RwLock<bool>>,
    ) -> std::result::Result<(), &'static str> {
        run_llm_bridge_with_store(
            config,
            perception_rx,
            action_tx,
            telemetry,
            state_store,
            event_store,
            llm_unavailable,
            llm_activity_ticks,
            shutdown_rx,
            provider_admission,
        )
        .await
    }

    struct ProviderPreparationDependencies<S: CompletionStore> {
        config: LlmBridgeConfig,
        state_store: Arc<StateStore>,
        event_store: Arc<S>,
        action_tx: mpsc::Sender<AgentAction>,
        active_requests: ActiveProviderRequests,
        preparing_requests: PreparingProviderRequests,
        completion_max_attempts: u32,
        executor: BlockingExecutor,
        runtime: tokio::runtime::Handle,
        last_call_tick: Arc<Mutex<HashMap<AgentId, u64>>>,
        activity: SharedLlmActivityTicks,
        telemetry: Arc<BridgeTelemetry>,
    }

    struct PreparationWindow {
        enqueued_at: Instant,
        tick: u64,
        urgent: bool,
        heard: bool,
        retained_heard: Option<(u64, Instant)>,
    }

    impl PreparationWindow {
        fn enqueued_at_for(&self, tick: u64) -> Instant {
            if self.tick == tick {
                return self.enqueued_at;
            }
            self.retained_heard
                .filter(|(heard_tick, _)| *heard_tick == tick)
                .map_or(self.enqueued_at, |(_, enqueued_at)| enqueued_at)
        }

        fn contains_tick(&self, tick: u64) -> bool {
            self.tick == tick
                || self
                    .retained_heard
                    .is_some_and(|(heard_tick, _)| heard_tick == tick)
        }

        fn urgent_for(&self, tick: u64) -> bool {
            (self.tick == tick && self.urgent)
                || self
                    .retained_heard
                    .is_some_and(|(heard_tick, _)| heard_tick == tick)
        }
    }

    fn intake_preparation_window(
        perception: &Perception,
        previous: Option<&Arc<PreparationWindow>>,
        impulse_acked: &mut HashMap<AgentId, u64>,
        now: Instant,
    ) -> Arc<PreparationWindow> {
        let tick = perception.tick.0;
        let heard = !perception.heard_text.is_empty();
        let preserved_urgent = previous.is_some_and(|window| window.urgent_for(tick));
        let mut urgent = preserved_urgent
            || heard
            || perception.is_directly_addressed
            || perception.has_operator_impulse;
        if !preserved_urgent
            && perception.has_operator_impulse
            && !heard
            && !perception.is_directly_addressed
        {
            let last = impulse_acked
                .get(&perception.agent_id)
                .copied()
                .unwrap_or(0);
            if tick.saturating_sub(last) < 5 {
                urgent = false;
            } else {
                impulse_acked.insert(perception.agent_id, tick);
            }
        }
        let enqueued_at = previous
            .filter(|window| window.contains_tick(tick))
            .map_or(now, |window| window.enqueued_at_for(tick));
        // Only the actually selected heard tick can use this bounded fallback.
        let retained_heard = if heard {
            None
        } else {
            previous.and_then(|window| {
                if window.heard {
                    Some((window.tick, window.enqueued_at))
                } else {
                    window.retained_heard
                }
            })
        };
        Arc::new(PreparationWindow {
            enqueued_at,
            tick,
            urgent,
            heard,
            retained_heard,
        })
    }

    struct PreparedProviderCall {
        perception: Perception,
        request_id: String,
        request_digest: String,
        request: GatewayRequest,
        usage_authority: Option<ProviderExecutionAuthority>,
        model_work: Option<ModelWorkContext>,
        preparation_guard: Option<ProviderPreparationGuard>,
        window: Arc<PreparationWindow>,
        deadline: Option<Instant>,
    }

    struct ProviderPreparationOutcome {
        perception: Perception,
        attempt_started: bool,
        call: Option<PreparedProviderCall>,
        window: Arc<PreparationWindow>,
        recovery_failure: Option<&'static str>,
    }

    fn prepare_provider_call<S: CompletionStore + 'static>(
        dependencies: &ProviderPreparationDependencies<S>,
        perception: Perception,
        cancellation: &JobCancellation,
        window: Arc<PreparationWindow>,
    ) -> ProviderPreparationOutcome {
        let mut outcome = ProviderPreparationOutcome {
            perception,
            attempt_started: false,
            call: None,
            window: Arc::clone(&window),
            recovery_failure: None,
        };
        if cancellation.is_cancelled() {
            return outcome;
        }
        let config = &dependencies.config;
        let agent_id = outcome.perception.agent_id;
        // Intake may precede a slow earlier preparation. Recheck before selecting
        // any new work; recovery-only passes never record an attempt or extend this clock.
        if !window.urgent_for(outcome.perception.tick.0)
            && outcome.perception.heard_text.is_empty()
            && !outcome.perception.is_directly_addressed
            && dependencies
                .last_call_tick
                .lock()
                .unwrap()
                .get(&agent_id)
                .is_some_and(|last| {
                    outcome.perception.tick.0.saturating_sub(*last) < config.min_ticks_between_calls
                })
        {
            dependencies
                .telemetry
                .calls_skipped_rate_limit
                .fetch_add(1, Ordering::Relaxed);
            return outcome;
        }
        let enqueued_at = window.enqueued_at_for(outcome.perception.tick.0);
        let started = Instant::now();
        let candidate = match config.provider_usage_authority.as_ref() {
            Some(resolver) => match resolver.prepare_provider_usage_candidate(agent_id) {
                Ok(Some(candidate)) => candidate,
                Ok(None) => return outcome,
                Err(reason) => {
                    error!(agent = %agent_id, reason, "Provider candidate preparation failed closed");
                    return outcome;
                }
            },
            None => PreparedProviderCandidate {
                authority: None,
                prepared_context: None,
            },
        };
        let request_id = candidate
            .authority
            .as_ref()
            .map(ProviderExecutionAuthority::request_id);
        log_provider_preparation_duration(
            agent_id,
            request_id.as_deref(),
            "selection",
            started,
            candidate.authority.is_some(),
            true,
        );
        if cancellation.is_cancelled() {
            return outcome;
        }
        let usage_authority = candidate.authority;
        if usage_authority
            .as_ref()
            .is_some_and(|authority| authority.agent_id() != agent_id)
        {
            error!(agent = %agent_id, "Provider usage authority returned another agent");
            return outcome;
        }
        let unbound_allowed = config
            .provider_usage_authority
            .as_ref()
            .is_none_or(|resolver| resolver.allows_unbound_provider_usage());
        if usage_authority.is_none() && !unbound_allowed {
            return outcome;
        }
        if let Err(reason) = validate_provider_usage_mode(
            usage_authority.as_ref(),
            config.usage_v2_enabled,
            unbound_allowed,
        ) {
            error!(agent = %agent_id, reason, "Provider usage accounting is unavailable");
            return outcome;
        }
        outcome.attempt_started = usage_authority.is_none();
        if outcome.attempt_started {
            record_provider_preparation_attempt(
                &mut dependencies.last_call_tick.lock().unwrap(),
                &dependencies.activity,
                agent_id,
                outcome.perception.tick.0,
                config.min_ticks_between_calls,
            );
        }
        let request_id = agent_runtime_request_id(&outcome.perception, usage_authority.as_ref());
        if dependencies
            .active_requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&request_id)
        {
            return outcome;
        }
        if let Some(authority) = usage_authority.as_ref() {
            let store = Arc::clone(&dependencies.event_store);
            let resolver = config.provider_usage_authority.clone();
            let authority = authority.clone();
            let action_tx = dependencies.action_tx.clone();
            let attempts = dependencies.completion_max_attempts;
            let timeout = config.request_timeout;
            let recovery = recover_on_owned_lane(dependencies, move |_| {
                recover_reserved_model_request(
                    store.as_ref(),
                    resolver.as_deref(),
                    &authority,
                    &action_tx,
                    attempts,
                    unix_now_ms(),
                    timeout,
                )
            });
            match recovery {
                Ok(Ok(true)) => return outcome,
                Ok(Ok(false)) => {}
                Ok(Err(error)) => {
                    error!(request_id, error = %error, "Reserved model recovery failed closed");
                    return outcome;
                }
                Err(reason) => {
                    outcome.recovery_failure = Some(reason);
                    error!(
                        request_id,
                        reason, "Reserved model recovery lane failed closed"
                    );
                    return outcome;
                }
            }
        }
        if cancellation.is_cancelled() {
            return outcome;
        }
        let preparation_guard = if usage_authority.is_some() {
            let Some(guard) = ProviderPreparationGuard::try_acquire(
                &dependencies.preparing_requests,
                &request_id,
            ) else {
                return outcome;
            };
            outcome.attempt_started = true;
            record_provider_preparation_attempt(
                &mut dependencies.last_call_tick.lock().unwrap(),
                &dependencies.activity,
                agent_id,
                outcome.perception.tick.0,
                config.min_ticks_between_calls,
            );
            Some(guard)
        } else {
            None
        };
        let mut request = build_gateway_request(
            &outcome.perception,
            &dependencies.state_store,
            &request_id,
            usage_authority.as_ref(),
        );
        if cancellation.is_cancelled()
            || (usage_authority.is_none() && enqueued_at.elapsed() >= UNBOUND_ADMISSION_MAX_AGE)
        {
            return outcome;
        }
        let started = Instant::now();
        let preparation = initial_model_work_context(
            config.provider_usage_authority.as_deref(),
            usage_authority.as_ref(),
            candidate.prepared_context,
        );
        log_provider_preparation_duration(
            agent_id,
            Some(&request_id),
            "context",
            started,
            usage_authority.is_some(),
            preparation.is_ok(),
        );
        let model_work = match preparation {
            Ok(context) => context,
            Err(reason) => {
                warn!(agent = %agent_id, reason, "Model work context unavailable");
                return outcome;
            }
        };
        if let Some(context) = &model_work {
            if let Err(reason) = bind_model_work_request(&mut request, context) {
                warn!(agent = %agent_id, reason, "Model work request rejected");
                return outcome;
            }
        }
        let request_digest = match gateway_request_digest(&request) {
            Ok(digest) => digest,
            Err(error) => {
                error!(agent = %agent_id, error = %error, "Gateway request digest failed");
                return outcome;
            }
        };
        match dependencies.event_store.get_completion(&request_id) {
            Ok(Some(entry))
                if entry.request_digest == request_digest
                    && entry.status == "provider_in_flight" =>
            {
                let store = Arc::clone(&dependencies.event_store);
                let resolver = config.provider_usage_authority.clone();
                let authority = usage_authority.clone();
                let timeout = config.request_timeout;
                if let Err(reason) = recover_on_owned_lane(dependencies, move |_| {
                    release_stale_undispatched_subscription(
                        store.as_ref(),
                        resolver.as_deref(),
                        authority.as_ref(),
                        &entry,
                        unix_now_ms(),
                        timeout,
                    );
                }) {
                    outcome.recovery_failure = Some(reason);
                    error!(
                        request_id,
                        reason, "Undispatched recovery lane failed closed"
                    );
                }
                return outcome;
            }
            Ok(Some(entry)) if entry.request_digest == request_digest => {
                let store = Arc::clone(&dependencies.event_store);
                let resolver = config.provider_usage_authority.clone();
                let action_tx = dependencies.action_tx.clone();
                let attempts = dependencies.completion_max_attempts;
                if let Err(reason) = recover_on_owned_lane(dependencies, move |_| {
                    recover_completion(
                        store.as_ref(),
                        entry,
                        &action_tx,
                        attempts,
                        resolver.as_deref(),
                    );
                }) {
                    outcome.recovery_failure = Some(reason);
                    error!(request_id, reason, "Completion recovery lane failed closed");
                }
                return outcome;
            }
            Ok(Some(_)) => {
                error!(request_id = %request_id, "Stable LLM request ID reused with different request digest");
                return outcome;
            }
            Ok(None) => {}
            Err(error) => {
                error!(request_id = %request_id, error = %error, "LLM completion lookup failed closed");
                return outcome;
            }
        }
        match dependencies
            .event_store
            .has_operation(&format!("llm_usage_{request_id}"))
        {
            Ok(true) => return outcome,
            Ok(false) => {}
            Err(error) => {
                error!(request_id = %request_id, error = %error, "LLM usage lookup failed closed");
                return outcome;
            }
        }
        if !cancellation.is_cancelled()
            && model_work
                .as_ref()
                .is_none_or(|context| context.validate_dispatch(unix_now_ms()).is_ok())
        {
            let deadline = model_work
                .as_ref()
                .and_then(model_context_deadline)
                .or_else(|| {
                    usage_authority
                        .is_none()
                        .then(|| enqueued_at + UNBOUND_ADMISSION_MAX_AGE)
                });
            outcome.call = Some(PreparedProviderCall {
                perception: outcome.perception.clone(),
                request_id,
                request_digest,
                request,
                usage_authority,
                model_work,
                preparation_guard,
                window,
                deadline,
            });
        }
        outcome
    }

    // Only ambient calls use this scheduler age bound. HTTP retains config.request_timeout;
    // bound work uses its original model context expiry, never this unrelated queue budget.
    const UNBOUND_ADMISSION_MAX_AGE: Duration = Duration::from_secs(60);

    fn model_context_deadline(context: &ModelWorkContext) -> Option<Instant> {
        let deadline_ms = match context {
            ModelWorkContext::Project(value) => value.deadline_unix_ms,
            ModelWorkContext::Adaptive(value) => value.binding.grant.deadline_ms,
            ModelWorkContext::AdaptiveLeadershipReview(value) => {
                value.binding.grant.expires_at_unix_ms
            }
            ModelWorkContext::RequestSales(value) => value.binding.grant.expires_at_unix_ms,
            ModelWorkContext::ProjectPlanning(value) => value.binding.grant.expires_at_unix_ms,
        };
        Instant::now().checked_add(Duration::from_millis(
            deadline_ms.saturating_sub(unix_now_ms()),
        ))
    }

    fn recover_on_owned_lane<S: CompletionStore + 'static, T: Send + 'static>(
        dependencies: &ProviderPreparationDependencies<S>,
        job: impl FnOnce(JobCancellation) -> T + Send + 'static,
    ) -> Result<T, &'static str> {
        let job = dependencies
            .executor
            .submit_required(Lane::Recovery, job)
            .map_err(|error| match error {
                SubmitError::Full => "llm_recovery_lane_full",
                SubmitError::Closed => "llm_recovery_lane_closed",
            })?;
        dependencies
            .runtime
            .block_on(job.wait())
            .map_err(|error| match error {
                JobError::Panicked => "llm_recovery_job_panicked",
                _ => "llm_recovery_job_failed",
            })
    }

    fn log_owned_provider_phase(
        agent_id: AgentId,
        request_id: &str,
        phase: &'static str,
        elapsed: Duration,
        accepted: bool,
    ) {
        let elapsed_us = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        let elapsed_ms = elapsed_us / 1_000;
        info!(agent = %agent_id, request_id = %request_id, phase, elapsed_us, elapsed_ms, accepted,
            "Owned provider phase finished");
    }

    struct BridgeInput<S: CompletionStore> {
        config: LlmBridgeConfig,
        perception_rx: mpsc::Receiver<Perception>,
        action_tx: mpsc::Sender<AgentAction>,
        telemetry: Arc<BridgeTelemetry>,
        state_store: Arc<StateStore>,
        event_store: Arc<S>,
        llm_unavailable: Arc<AtomicBool>,
        activity: SharedLlmActivityTicks,
        shutdown_rx: watch::Receiver<bool>,
        admission: Arc<RwLock<bool>>,
        stopping: Arc<AtomicBool>,
        terminal: Arc<Mutex<bool>>,
    }

    enum SupervisorMessage {
        Receiver(std::thread::JoinHandle<()>),
        Stop,
    }

    struct ProviderExecutionDependencies<S: CompletionStore> {
        preparation: Arc<ProviderPreparationDependencies<S>>,
        client: reqwest::Client,
        semaphore: Arc<Semaphore>,
        circuit: Arc<Mutex<CircuitBreaker>>,
        telemetry: Arc<BridgeTelemetry>,
        admission: Arc<RwLock<bool>>,
        stopping: Arc<AtomicBool>,
        retries: Arc<AsyncMutex<HashMap<AgentId, QueuedProviderRetry>>>,
        retry_overflow: Arc<AtomicBool>,
    }

    struct QueuedProviderRetry {
        perception: Perception,
        window: Option<Arc<PreparationWindow>>,
    }

    struct ProviderLifetime<S: CompletionStore> {
        outcome: Option<ProviderOutcomeGuard<S>>,
        preparation: Option<ProviderPreparationGuard>,
        agent: AgentId,
        request_id: String,
        started: Instant,
        accepted: bool,
    }

    impl<S: CompletionStore> Drop for ProviderLifetime<S> {
        fn drop(&mut self) {
            let cleanup = Instant::now();
            drop(self.outcome.take());
            drop(self.preparation.take());
            log_owned_provider_phase(
                self.agent,
                &self.request_id,
                "cleanup",
                cleanup.elapsed(),
                true,
            );
            log_owned_provider_phase(
                self.agent,
                &self.request_id,
                "total",
                self.started.elapsed(),
                self.accepted,
            );
        }
    }

    async fn await_owned_transport<F: std::future::Future>(
        cancellation: &JobCancellation,
        transport: F,
        first_poll: &AtomicBool,
    ) -> Option<F::Output> {
        tokio::pin!(transport);
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => None,
            result = std::future::poll_fn(|cx| {
                first_poll.store(true, Ordering::Release);
                std::future::Future::poll(transport.as_mut(), cx)
            }) => Some(result),
        }
    }

    fn classify_cancelled_transport<S: CompletionStore>(
        guard: &mut ProviderOutcomeGuard<S>,
        polled: bool,
    ) {
        if !polled {
            if let Err(error) = guard
                .store
                .release_undispatched_request(&guard.request_id, &guard.request_digest)
            {
                warn!(request_id = %guard.request_id, error = %error, "Undispatched provider cancellation release failed closed");
            }
            guard.disarm_if_resolved();
        }
    }

    fn execute_owned_provider<S: CompletionStore + 'static>(
        dependencies: Arc<ProviderExecutionDependencies<S>>,
        mut call: PreparedProviderCall,
        cancellation: JobCancellation,
        submitted_at: Instant,
    ) {
        let agent = call.perception.agent_id;
        log_owned_provider_phase(
            agent,
            &call.request_id,
            "enqueue_wait",
            submitted_at.elapsed(),
            true,
        );
        let mut lifetime = ProviderLifetime {
            outcome: None,
            preparation: call.preparation_guard.take(),
            agent,
            request_id: call.request_id.clone(),
            started: call.window.enqueued_at_for(call.perception.tick.0),
            accepted: false,
        };
        dependencies.preparation.runtime.block_on(async {
            let cancelled = || cancellation.is_cancelled()
                || dependencies.stopping.load(Ordering::Acquire)
                || call.deadline.is_some_and(|deadline| Instant::now() >= deadline);
            let started = Instant::now();
            let urgent = call.window.urgent_for(call.perception.tick.0) || !call.perception.heard_text.is_empty()
                || call.perception.is_directly_addressed;
            let permit_deadline = Instant::now() + dependencies.preparation.config.request_timeout;
            let permit_deadline = call.deadline.map_or(permit_deadline, |deadline| deadline.min(permit_deadline));
            let permit = if urgent {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => None,
                    result = tokio::time::timeout_at(
                        tokio::time::Instant::from_std(permit_deadline),
                        Arc::clone(&dependencies.semaphore).acquire_owned(),
                    ) => result.ok().and_then(Result::ok),
                }
            } else {
                Arc::clone(&dependencies.semaphore).try_acquire_owned().ok()
            };
            log_owned_provider_phase(agent, &call.request_id, "permit_wait", started.elapsed(), permit.is_some());
            let Some(permit) = permit else {
                if urgent && !cancelled()
                    && !queue_bounded_retry(&dependencies.retries, call.perception.clone(),
                        Some(Arc::clone(&call.window))).await {
                    dependencies.retry_overflow.store(true, Ordering::Release);
                    dependencies.circuit.lock().unwrap().mark_terminal();
                }
                return;
            };
            if cancelled() { return; }
            let resolver = dependencies.preparation.config.provider_usage_authority.as_deref();
            let started = Instant::now();
            let authority = validate_unreserved_provider_authority(
                resolver, call.usage_authority.as_ref(), agent, call.model_work.as_ref(),
            );
            let fresh = authority.is_ok() && !cancelled()
                && call.model_work.as_ref().is_none_or(|context| context.validate_dispatch(unix_now_ms()).is_ok());
            log_owned_provider_phase(agent, &call.request_id, "unreserved_authority", started.elapsed(), fresh);
            if !fresh {
                if let Err(error) = authority {
                    warn!(request_id = %call.request_id, error, "Provider request reauthorization failed before reservation");
                }
                return;
            }
            lifetime.outcome = Some(ProviderOutcomeGuard::new(
                Arc::clone(&dependencies.preparation.event_store),
                Arc::clone(&dependencies.preparation.active_requests),
                &call.request_id, &call.request_digest,
            ));
            let started = Instant::now();
            let reservation = reserve_provider_request_until(
                dependencies.admission.as_ref(), dependencies.preparation.event_store.as_ref(),
                &call.request_id, &call.request_digest, &agent.to_string(), &cancelled,
            );
            let reserved = matches!(reservation, Ok(Some(true)));
            log_owned_provider_phase(agent, &call.request_id, "reservation", started.elapsed(), reserved);
            match reservation {
                Ok(Some(true)) => lifetime.outcome.as_mut().unwrap().armed = true,
                Ok(Some(false)) | Ok(None) => return,
                Err(error) => {
                    error!(request_id = %call.request_id, error = %error, "LLM provider reservation failed closed");
                    return;
                }
            }
            let started = Instant::now();
            let validation = validate_pre_dispatch_provider_authority_until(
                dependencies.preparation.event_store.as_ref(), resolver,
                call.usage_authority.as_ref(), agent, &call.request_id, &call.request_digest,
                call.model_work.as_ref(), &cancelled,
            );
            log_owned_provider_phase(agent, &call.request_id, "predispatch_authority", started.elapsed(), validation.is_ok());
            if let Err(error) = validation {
                warn!(request_id = %call.request_id, error, "Provider request reauthorization failed before dispatch");
                lifetime.outcome.as_mut().unwrap().disarm_if_resolved();
                return;
            }
            if cancelled() {
                let _ = dependencies.preparation.event_store.release_undispatched_request(&call.request_id, &call.request_digest);
                lifetime.outcome.as_mut().unwrap().disarm_if_resolved();
                return;
            }
            let started = Instant::now();
            let url = format!("{}/internal/agent-runtime", dependencies.preparation.config.gateway_url);
            let first_poll = AtomicBool::new(false);
            let response = await_owned_transport(&cancellation, agent_runtime_request(
                    &dependencies.client, &url, &dependencies.preparation.config.credential,
                    &call.request_id, &call.request_digest, &call.request,
                ).send(), &first_poll).await;
            let Some(response) = response else {
                classify_cancelled_transport(lifetime.outcome.as_mut().unwrap(), first_poll.load(Ordering::Acquire));
                log_owned_provider_phase(agent, &call.request_id, "http", started.elapsed(), false);
                return;
            };
            log_owned_provider_phase(agent, &call.request_id, "http", started.elapsed(), response.as_ref().is_ok_and(|response| response.status().is_success()));
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    let released = release_connect_failed_request(
                        dependencies.preparation.event_store.as_ref(),
                        &call.request_id, &call.request_digest, &error,
                    );
                    if released { lifetime.outcome.as_mut().unwrap().armed = false; }
                    warn!(agent = %agent, error = %error, reservation_released = released, "Gateway Request fehlgeschlagen");
                    dependencies.telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                    dependencies.circuit.lock().unwrap().record_failure();
                    return;
                }
            };
            if !response.status().is_success() {
                let resolved = handle_gateway_rejection(
                    dependencies.preparation.event_store.as_ref(), call.model_work.as_ref(),
                    &call.request_id, &call.request_digest, call.perception.tick.0,
                    response.status(), response.headers(),
                );
                if resolved { lifetime.outcome.as_mut().unwrap().armed = false; }
                dependencies.telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                dependencies.circuit.lock().unwrap().record_failure();
                return;
            }
            let started = Instant::now();
            let decoded = tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    log_owned_provider_phase(agent, &call.request_id, "response_decode", started.elapsed(), false);
                    return;
                }
                decoded = response.json::<GatewayResponse>() => decoded,
            };
            log_owned_provider_phase(agent, &call.request_id, "response_decode", started.elapsed(), decoded.is_ok());
            let gateway_response = match decoded {
                Ok(response) => response,
                Err(error) => {
                    warn!(agent = %agent, error = %error, "Gateway Response Parse-Fehler");
                    dependencies.telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                    dependencies.circuit.lock().unwrap().record_failure();
                    return;
                }
            };
            drop(permit);
            // A fully decoded response must be durably classified even after cancellation.
            let started = Instant::now();
            let persisted = store_gateway_completion(
                dependencies.preparation.event_store.as_ref(), &dependencies.preparation.action_tx,
                GatewayCompletionContext {
                    request_id: &call.request_id, request_digest: &call.request_digest, agent_id: agent,
                    tick: call.perception.tick.0, requested_model: &call.request.model,
                    authority: call.usage_authority.as_ref(), authority_resolver: resolver,
                    gateway_response: &gateway_response,
                    usage_v2_enabled: dependencies.preparation.config.usage_v2_enabled,
                    model_work: call.model_work.as_ref(),
                }, dependencies.preparation.completion_max_attempts,
            );
            log_owned_provider_phase(agent, &call.request_id, "persistence", started.elapsed(), persisted.is_ok());
            if let Err(error) = persisted {
                warn!(agent = %agent, error = %error, "Completed gateway response rejected fail-closed");
                lifetime.outcome.as_mut().unwrap().disarm_if_resolved();
                dependencies.telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                dependencies.circuit.lock().unwrap().record_failure();
                return;
            }
            lifetime.outcome.as_mut().unwrap().armed = false;
            lifetime.accepted = true;
            dependencies.telemetry.calls_success.fetch_add(1, Ordering::Relaxed);
            dependencies.telemetry.tokens_total.fetch_add(gateway_response.tokens_used.max(0) as u64, Ordering::Relaxed);
            dependencies.circuit.lock().unwrap().record_success();
        });
    }

    async fn run_llm_bridge_with_store<S: CompletionStore + 'static>(
        config: LlmBridgeConfig,
        perception_rx: mpsc::Receiver<Perception>,
        action_tx: mpsc::Sender<AgentAction>,
        telemetry: Arc<BridgeTelemetry>,
        state_store: Arc<StateStore>,
        event_store: Arc<S>,
        llm_unavailable: Arc<AtomicBool>,
        activity: SharedLlmActivityTicks,
        shutdown_rx: watch::Receiver<bool>,
        admission: Arc<RwLock<bool>>,
    ) -> Result<(), &'static str> {
        info!(
            max_concurrent = config.max_concurrent,
            min_ticks = config.min_ticks_between_calls,
            timeout_secs = config.request_timeout.as_secs(),
            "LLM Bridge gestartet"
        );
        let stopping = Arc::new(AtomicBool::new(false));
        let terminal = Arc::new(Mutex::new(false));
        let terminal_signal = Arc::clone(&llm_unavailable);
        let owner_stopping = Arc::clone(&stopping);
        let owner_admission = Arc::clone(&admission);
        let workers = config.max_concurrent.max(1);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (owner_tx, owner_rx) = mpsc::channel();
        // The lane owner never lives on a Tokio thread, including initialization failures/unwind.
        let supervisor = tokio::task::spawn_blocking(move || {
            // The off-runtime owner closes admission even during startup failure/unwind.
            let _close_admission = CloseProviderAdmissionOnExit(Arc::clone(&owner_admission));
            let mut lanes = match OwnedBlockingLanes::spawn(workers, 32) {
                Ok(lanes) => lanes,
                Err(_) => {
                    let _ = ready_tx.send(Err("llm_blocking_lanes_spawn_failed"));
                    return Err("llm_blocking_lanes_spawn_failed");
                }
            };
            let mut receiver = None;
            if ready_tx.send(Ok(lanes.executor())).is_ok() {
                while let Ok(message) = owner_rx.recv() {
                    match message {
                        SupervisorMessage::Receiver(handle) => receiver = Some(handle),
                        SupervisorMessage::Stop => break,
                    }
                }
            }
            owner_stopping.store(true, Ordering::Release);
            stop_provider_admission(owner_admission.as_ref());
            let joined = lanes.stop_and_join();
            if receiver.is_some_and(|receiver| receiver.join().is_err()) {
                return Err("llm_receiver_thread_failed");
            }
            joined
        });
        let executor = match ready_rx.await {
            Ok(Ok(executor)) => executor,
            _ => {
                publish_terminal_unavailability(&terminal_signal, &terminal);
                drop(owner_tx);
                let _ = supervisor.await;
                return Err("llm_blocking_lanes_spawn_failed");
            }
        };
        let loop_owner_tx = owner_tx.clone();
        let body = tokio::spawn(run_llm_bridge_on_lanes(
            BridgeInput {
                config,
                perception_rx,
                action_tx,
                telemetry,
                state_store,
                event_store,
                llm_unavailable,
                activity,
                shutdown_rx,
                admission,
                stopping,
                terminal: Arc::clone(&terminal),
            },
            executor,
            loop_owner_tx,
        ));
        let result = body.await.unwrap_or(Err("llm_bridge_loop_failed"));
        if result.is_err() {
            publish_terminal_unavailability(&terminal_signal, &terminal);
        }
        let _ = owner_tx.send(SupervisorMessage::Stop);
        drop(owner_tx);
        let joined = supervisor
            .await
            .unwrap_or(Err("llm_blocking_supervisor_failed"));
        if joined.is_err() {
            publish_terminal_unavailability(&terminal_signal, &terminal);
        }
        joined?;
        result
    }

    async fn run_llm_bridge_on_lanes<S: CompletionStore + 'static>(
        input: BridgeInput<S>,
        executor: BlockingExecutor,
        owner_tx: mpsc::Sender<SupervisorMessage>,
    ) -> Result<(), &'static str> {
        let BridgeInput {
            config,
            perception_rx,
            action_tx,
            telemetry,
            state_store,
            event_store,
            llm_unavailable,
            activity,
            mut shutdown_rx,
            admission,
            stopping,
            terminal,
        } = input;
        let client = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .pool_max_idle_per_host(config.max_concurrent)
            .build()
            .map_err(|_| "llm_http_client_failed")?;
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent.max(1)));
        let circuit = Arc::new(Mutex::new(CircuitBreaker::with_terminal(
            config.circuit_breaker_threshold,
            config.circuit_breaker_reset,
            Arc::clone(&llm_unavailable),
            terminal,
        )));
        llm_unavailable.store(false, Ordering::Relaxed);
        let pending_retries = Arc::new(AsyncMutex::new(HashMap::new()));
        let retry_overflow = Arc::new(AtomicBool::new(false));
        let last_call_tick = Arc::new(Mutex::new(HashMap::new()));
        let preparation = Arc::new(ProviderPreparationDependencies {
            config: config.clone(),
            state_store,
            event_store: Arc::clone(&event_store),
            action_tx: action_tx.clone(),
            active_requests: config
                .active_provider_requests
                .clone()
                .unwrap_or_else(|| Arc::new(Mutex::new(HashMap::new()))),
            preparing_requests: Arc::new(Mutex::new(HashMap::new())),
            completion_max_attempts: config.completion_max_attempts.max(1),
            executor: executor.clone(),
            runtime: tokio::runtime::Handle::current(),
            last_call_tick: Arc::clone(&last_call_tick),
            activity,
            telemetry: Arc::clone(&telemetry),
        });
        let execution = Arc::new(ProviderExecutionDependencies {
            preparation: Arc::clone(&preparation),
            client,
            semaphore: Arc::clone(&semaphore),
            circuit: Arc::clone(&circuit),
            telemetry: Arc::clone(&telemetry),
            admission: Arc::clone(&admission),
            stopping: Arc::clone(&stopping),
            retries: Arc::clone(&pending_retries),
            retry_overflow: Arc::clone(&retry_overflow),
        });
        let receiver_stopping = Arc::clone(&stopping);
        let (async_tx, mut async_rx) = tokio::sync::mpsc::channel::<Perception>(256);
        let receiver = std::thread::Builder::new()
            .name("llm-bridge-recv".into())
            .spawn(move || {
                while !receiver_stopping.load(Ordering::Acquire) {
                    match perception_rx.recv_timeout(Duration::from_millis(50)) {
                        Ok(perception) => {
                            if async_tx.blocking_send(perception).is_err() {
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .map_err(|_| "llm_receiver_spawn_failed")?;
        if let Err(error) = owner_tx.send(SupervisorMessage::Receiver(receiver)) {
            stopping.store(true, Ordering::Release);
            drop(async_rx);
            if let SupervisorMessage::Receiver(receiver) = error.0 {
                let _ = tokio::task::spawn_blocking(move || receiver.join()).await;
            }
            return Err("llm_supervisor_closed");
        }
        // This sender is not retained by the body; dropping the wrapper always wakes the supervisor.
        drop(owner_tx);
        let mut controller = ProviderPreparationController::new(32, 1, insert_prefer_heard);
        let mut windows: HashMap<AgentId, Arc<PreparationWindow>> = HashMap::new();
        let mut impulse_acked: HashMap<AgentId, u64> = HashMap::new();
        let mut provider_tasks = JoinSet::new();
        let mut provider_cancellations = HashMap::new();
        let mut next_provider_id = 0_u64;
        let mut recovery_tasks = JoinSet::new();
        let mut startup_recovered = false;
        let mut intake_closed = false;
        let mut failed = None;
        let retry_interval = if config.completion_retry_interval.is_zero() {
            Duration::from_secs(1)
        } else {
            config.completion_retry_interval
        };
        let mut recovery_interval = tokio::time::interval(retry_interval);
        recovery_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // The explicit startup recovery owns the interval's initial tick.
        recovery_interval.tick().await;
        let mut retry_tick = tokio::time::interval(Duration::from_millis(50));
        let schedule_recovery = |tasks: &mut JoinSet<Result<(), JobError>>| {
            let dependencies = Arc::clone(&preparation);
            match executor.submit_required(Lane::Recovery, move |_| {
                classify_stale_provider_requests(
                    dependencies.event_store.as_ref(),
                    &dependencies.active_requests,
                    unix_now_ms(),
                    dependencies.config.request_timeout,
                );
                recover_completion_batch(
                    dependencies.event_store.as_ref(),
                    &dependencies.action_tx,
                    dependencies.completion_max_attempts,
                    dependencies.config.provider_usage_authority.as_deref(),
                );
            }) {
                Ok(job) => {
                    tasks.spawn(async move { job.wait().await });
                    true
                }
                Err(_) => false,
            }
        };
        if !schedule_recovery(&mut recovery_tasks) {
            circuit.lock().unwrap().mark_terminal();
            return Err("llm_recovery_admission_failed");
        }
        loop {
            if failed.is_some() {
                break;
            }
            if retry_overflow.load(Ordering::Acquire) {
                failed = Some("llm_retry_capacity_exhausted");
                break;
            }
            if stopping.load(Ordering::Acquire) || *shutdown_rx.borrow() {
                break;
            }
            if intake_closed
                && !controller.has_work()
                && provider_tasks.is_empty()
                && recovery_tasks.is_empty()
            {
                break;
            }
            if startup_recovered && controller.has_work() {
                let snapshot = Arc::new(windows.clone());
                let dependencies = Arc::clone(&preparation);
                controller.pump(&executor, None, move |perception, cancellation| {
                    let window = snapshot
                        .get(&perception.agent_id)
                        .cloned()
                        .unwrap_or_else(|| {
                            let now = Instant::now();
                            Arc::new(PreparationWindow {
                                enqueued_at: now,
                                tick: perception.tick.0,
                                urgent: should_retry_perception(&perception),
                                heard: !perception.heard_text.is_empty(),
                                retained_heard: None,
                            })
                        });
                    let started = Instant::now();
                    let queue_elapsed = started
                        .saturating_duration_since(window.enqueued_at_for(perception.tick.0));
                    let outcome =
                        prepare_provider_call(&dependencies, perception, &cancellation, window);
                    let request_id = outcome
                        .call
                        .as_ref()
                        .map_or("", |call| call.request_id.as_str());
                    log_owned_provider_phase(
                        outcome.perception.agent_id,
                        request_id,
                        "enqueue_wait",
                        queue_elapsed,
                        outcome.call.is_some(),
                    );
                    outcome
                });
            }
            let mut batch = HashMap::new();
            let mut retry_windows = HashMap::new();
            tokio::select! {
                biased;
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() { break; }
                }
                result = controller.next_result(), if controller.inflight_count() > 0 => {
                    match result {
                        Some((agent, Ok(outcome))) => {
                            #[cfg(test)]
                            let _processed = PreparationProcessed(&telemetry.preparations_processed);
                            if let Some(reason) = outcome.recovery_failure {
                                failed = Some(reason);
                                break;
                            }
                            debug!(agent = %agent, attempt_started = outcome.attempt_started,
                                prepared = outcome.call.is_some(), "Provider preparation result received");
                            if outcome.call.is_none()
                                && windows.get(&agent).is_some_and(|current| Arc::ptr_eq(current, &outcome.window)) {
                                windows.remove(&agent);
                            }
                            if let Some(call) = outcome.call {
                                telemetry.calls_total.fetch_add(1, Ordering::Relaxed);
                                let urgent = call.window.urgent_for(call.perception.tick.0) || !call.perception.heard_text.is_empty()
                                    || call.perception.is_directly_addressed;
                                if !urgent && semaphore.available_permits() == 0 {
                                    if windows.get(&agent).is_some_and(|current| Arc::ptr_eq(current, &call.window)) {
                                        windows.remove(&agent);
                                    }
                                    continue;
                                }
                                let Some(id) = next_provider_id.checked_add(1) else {
                                    failed = Some("llm_provider_identifier_exhausted"); break;
                                };
                                next_provider_id = id;
                                let submitted_at = Instant::now();
                                let deadline = call.deadline;
                                let retry = urgent.then(|| call.perception.clone());
                                let request_id = call.request_id.clone();
                                let window = Arc::clone(&call.window);
                                let dependencies = Arc::clone(&execution);
                                match executor.submit(Lane::Provider, deadline, move |cancellation| {
                                    execute_owned_provider(dependencies, call, cancellation, submitted_at);
                                }) {
                                    Ok(job) => {
                                        provider_cancellations.insert(id, job.cancellation());
                                        provider_tasks.spawn(async move {
                                            (id, agent, request_id, window, submitted_at, job.wait().await)
                                        });
                                    }
                                    Err(_) => {
                                        // Only unarmed preparation data was captured by the rejected closure.
                                        if let Some(perception) = retry {
                                            if !queue_bounded_retry(&pending_retries, perception,
                                                Some(Arc::clone(&window))).await {
                                                failed = Some("llm_retry_capacity_exhausted");
                                                break;
                                            }
                                        }
                                        if windows.get(&agent).is_some_and(|current| Arc::ptr_eq(current, &window)) {
                                            windows.remove(&agent);
                                        }
                                    }
                                }
                            }
                        }
                        Some((agent, Err(JobError::Cancelled | JobError::DeadlineExceeded))) => {
                            // No selected request or armed guard exists for a pre-start skip.
                            if controller.pending_count() == 0 { windows.remove(&agent); }
                        }
                        Some((_, Err(JobError::Panicked | JobError::Stopped))) => {
                            failed = Some("llm_preparation_job_failed"); break;
                        }
                        None => {}
                    }
                }
                result = provider_tasks.join_next(), if !provider_tasks.is_empty() => {
                    match result {
                        Some(Ok((id, agent, request_id, window, queued, result))) => {
                            #[cfg(test)]
                            telemetry.providers_retired.fetch_add(1, Ordering::SeqCst);
                            provider_cancellations.remove(&id);
                            if windows.get(&agent).is_some_and(|current| Arc::ptr_eq(current, &window)) {
                                windows.remove(&agent);
                            }
                            if let Err(error) = result {
                                log_owned_provider_phase(agent, &request_id, "enqueue_wait", queued.elapsed(), false);
                                if matches!(error, JobError::Panicked | JobError::Stopped) {
                                    failed = Some("llm_provider_task_failed");
                                    circuit.lock().unwrap().mark_terminal();
                                    break;
                                }
                            }
                        }
                        Some(Err(_)) => { failed = Some("llm_provider_adapter_failed"); break; }
                        None => {}
                    }
                }
                result = recovery_tasks.join_next(), if !recovery_tasks.is_empty() => {
                    match result {
                        Some(Ok(Ok(()))) => startup_recovered = true,
                        _ => { failed = Some("llm_recovery_task_failed"); break; }
                    }
                }
                _ = recovery_interval.tick(), if startup_recovered && recovery_tasks.is_empty() => {
                    if !schedule_recovery(&mut recovery_tasks) {
                        failed = Some("llm_recovery_admission_failed"); break;
                    }
                }
                _ = retry_tick.tick() => {
                    let retries = std::mem::take(&mut *pending_retries.lock().await);
                    for (agent, retry) in retries {
                        if let Some(window) = retry.window { retry_windows.insert(agent, window); }
                        insert_prefer_heard(&mut batch, retry.perception);
                    }
                }
                perception = async_rx.recv(), if !intake_closed => match perception {
                    Some(perception) => insert_prefer_heard(&mut batch, perception),
                    None => intake_closed = true,
                }
            }
            for _ in 0..255 {
                let Ok(perception) = async_rx.try_recv() else {
                    break;
                };
                insert_prefer_heard(&mut batch, perception);
            }
            for perception in batch.into_values() {
                let agent = perception.agent_id;
                let tick = perception.tick.0;
                let heard = !perception.heard_text.is_empty();
                let previous = retry_windows.get(&agent).or_else(|| windows.get(&agent));
                // Capture original age/normalized urgency before either backpressure path.
                let window = intake_preparation_window(
                    &perception,
                    previous,
                    &mut impulse_acked,
                    Instant::now(),
                );
                let urgent = window.urgent_for(tick);
                if !urgent
                    && last_call_tick
                        .lock()
                        .unwrap()
                        .get(&agent)
                        .is_some_and(|last| {
                            tick.saturating_sub(*last) < config.min_ticks_between_calls
                        })
                {
                    telemetry
                        .calls_skipped_rate_limit
                        .fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                if circuit.lock().unwrap().is_open() {
                    telemetry
                        .calls_skipped_circuit_open
                        .fetch_add(1, Ordering::Relaxed);
                    if !queue_bounded_retry(&pending_retries, perception, Some(window)).await {
                        failed = Some("llm_retry_capacity_exhausted");
                        break;
                    }
                    continue;
                }
                if perception.impulse_text.is_empty() && perception.body_text.is_empty() && !heard {
                    continue;
                }
                match controller.offer(perception) {
                    Ok(()) => {
                        windows.insert(agent, window);
                        #[cfg(test)]
                        telemetry.perceptions_offered.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(perception) => {
                        if !queue_bounded_retry(&pending_retries, *perception, Some(window)).await {
                            failed = Some("llm_retry_capacity_exhausted");
                            break;
                        }
                    }
                }
            }
        }
        if failed.is_some() {
            circuit.lock().unwrap().mark_terminal();
        }
        controller.stop_intake();
        controller.cancel_pending();
        controller.cancel_inflight();
        stopping.store(true, Ordering::Release);
        async_rx.close();
        let drained = tokio::time::timeout(config.shutdown_drain_timeout, async {
            close_provider_admission_async(&admission).await;
            while !provider_tasks.is_empty() {
                if !matches!(
                    provider_tasks.join_next().await,
                    Some(Ok((
                        _,
                        _,
                        _,
                        _,
                        _,
                        Ok(()) | Err(JobError::Cancelled | JobError::DeadlineExceeded)
                    )))
                ) {
                    failed.get_or_insert("llm_provider_task_failed");
                    circuit.lock().unwrap().mark_terminal();
                }
            }
            while controller.inflight_count() > 0 {
                match controller.next_result().await {
                    Some((_, Ok(outcome))) => {
                        if let Some(reason) = outcome.recovery_failure {
                            failed.get_or_insert(reason);
                            circuit.lock().unwrap().mark_terminal();
                        }
                    }
                    Some((_, Err(JobError::Panicked | JobError::Stopped))) => {
                        failed.get_or_insert("llm_preparation_job_failed");
                        circuit.lock().unwrap().mark_terminal();
                    }
                    _ => {}
                }
            }
            while !recovery_tasks.is_empty() {
                if !matches!(recovery_tasks.join_next().await, Some(Ok(Ok(())))) {
                    failed.get_or_insert("llm_recovery_task_failed");
                    circuit.lock().unwrap().mark_terminal();
                }
            }
        })
        .await;
        if drained.is_err() {
            circuit.lock().unwrap().mark_terminal();
            for cancellation in provider_cancellations.values() {
                cancellation.cancel();
            }
            controller.cancel_inflight();
            close_provider_admission_async(&admission).await;
            // Cancellation interrupts transport, not synchronous response persistence or guard cleanup.
            while provider_tasks.join_next().await.is_some() {}
            while controller.inflight_count() > 0 {
                let _ = controller.next_result().await;
            }
            while recovery_tasks.join_next().await.is_some() {}
            return Err("llm_provider_drain_timeout");
        }
        failed.map_or(Ok(()), Err)
    }

    async fn close_provider_admission_async(admission: &RwLock<bool>) {
        loop {
            match admission.try_write() {
                Ok(mut open) => {
                    *open = false;
                    return;
                }
                Err(std::sync::TryLockError::Poisoned(error)) => {
                    *error.into_inner() = false;
                    return;
                }
                Err(std::sync::TryLockError::WouldBlock) => {}
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    /// Fuegt Perception in Batch ein. Bevorzugt Versionen MIT heard_text.
    /// #295 Fix: Bewahrt has_operator_impulse (IM-Flag) beim Merge,
    /// damit Gaia/Broadcast-Bypass nicht verloren geht wenn heard_text-Version gewinnt.
    pub(super) fn insert_prefer_heard(batch: &mut HashMap<AgentId, Perception>, p: Perception) {
        batch
            .entry(p.agent_id)
            .and_modify(|existing| {
                let preserve_impulse = existing.has_operator_impulse || p.has_operator_impulse;
                // Behalte Version MIT heard_text, sonst neueste
                if !p.heard_text.is_empty() || existing.heard_text.is_empty() {
                    *existing = p.clone();
                }
                // Either input may carry Gaia/Broadcast, even when heard text retains the older frame.
                if preserve_impulse && !existing.has_operator_impulse {
                    existing.has_operator_impulse = true;
                    existing.synth_fingerprint =
                        existing.synth_fingerprint.replace("|IM:0", "|IM:1");
                }
            })
            .or_insert(p);
    }

    fn record_provider_preparation_attempt(
        last_call_tick: &mut HashMap<AgentId, u64>,
        llm_activity_ticks: &SharedLlmActivityTicks,
        agent_id: AgentId,
        current_tick: u64,
        min_ticks_between_calls: u64,
    ) {
        last_call_tick.insert(agent_id, current_tick);
        let mut ticks = llm_activity_ticks.lock().unwrap();
        ticks.insert(agent_id, current_tick);
        let retention_ticks = min_ticks_between_calls.saturating_mul(24).max(120);
        ticks.retain(|_, last_tick| current_tick.saturating_sub(*last_tick) <= retention_ticks);
    }

    fn should_retry_perception(perception: &Perception) -> bool {
        !perception.heard_text.is_empty()
            || perception.is_directly_addressed
            || perception.has_operator_impulse
    }

    async fn queue_bounded_retry(
        queue: &Arc<AsyncMutex<HashMap<AgentId, QueuedProviderRetry>>>,
        perception: Perception,
        window: Option<Arc<PreparationWindow>>,
    ) -> bool {
        if !should_retry_perception(&perception) {
            return true;
        }

        let window = window.or_else(|| {
            Some(Arc::new(PreparationWindow {
                enqueued_at: Instant::now(),
                tick: perception.tick.0,
                urgent: should_retry_perception(&perception),
                heard: !perception.heard_text.is_empty(),
                retained_heard: None,
            }))
        });

        let mut pending = queue.lock().await;
        if pending.len() >= 256 && !pending.contains_key(&perception.agent_id) {
            return false;
        }
        let agent = perception.agent_id;
        let previous = pending.remove(&agent);
        let mut merged = HashMap::new();
        if let Some(previous) = &previous {
            insert_prefer_heard(&mut merged, previous.perception.clone());
        }
        insert_prefer_heard(&mut merged, perception);
        let perception = merged.remove(&agent).unwrap();
        let window = previous
            .and_then(|previous| previous.window)
            .filter(|previous| previous.contains_tick(perception.tick.0))
            .or_else(|| window.filter(|window| window.contains_tick(perception.tick.0)));
        pending.insert(agent, QueuedProviderRetry { perception, window });
        true
    }

    /// Baut den Gateway-Request aus einer Perception + Evolution-Daten aus redb.
    fn build_gateway_request(
        perception: &Perception,
        store: &StateStore,
        request_id: &str,
        authority: Option<&ProviderExecutionAuthority>,
    ) -> GatewayRequest {
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
        let mut metadata = BTreeMap::new();
        metadata.insert("agent_id".to_string(), perception.agent_id.0.to_string());
        if let Some(claim) = AGENT_ROUTING
            .get()
            .and_then(|cache| cache.read().ok())
            .and_then(|claims| claims.get(&perception.agent_id).cloned())
        {
            metadata.insert("agent_role".to_string(), claim.role);
            metadata.insert("hierarchy_tier".to_string(), claim.tier.get().to_string());
        }
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
        metadata.insert("request_id".to_string(), request_id.to_string());
        if let Some(binding) = authority {
            metadata.extend(provider_metadata(binding));
        }

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
        if let Ok(snapshot) = perception_snapshot_json(perception, &metadata) {
            metadata.insert("agent_perception_snapshot".to_owned(), snapshot);
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

    fn leadership_review_kind(
        grant: &serde_json::Value,
    ) -> Result<Option<&'static str>, &'static str> {
        // Use the authoritative grant wire shape across additive schema versions.
        // Request metadata cannot select a review subject.
        match grant
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
        {
            Some(1) if grant.get("subject").is_none_or(serde_json::Value::is_null) => Ok(None),
            Some(2) => match grant
                .get("subject")
                .and_then(|subject| subject.get("kind"))
                .and_then(serde_json::Value::as_str)
            {
                Some("unknown_model") => Ok(Some("unknown_model")),
                Some("blocked_continuation") => Ok(Some("blocked_continuation")),
                _ => Err("unsupported leadership review subject"),
            },
            Some(3)
                if grant
                    .get("subject")
                    .and_then(|subject| subject.get("kind"))
                    .and_then(serde_json::Value::as_str)
                    == Some("budget_window_exhausted") =>
            {
                Ok(Some("budget_window_exhausted"))
            }
            Some(4)
                if grant
                    .get("subject")
                    .and_then(|subject| subject.get("kind"))
                    .and_then(serde_json::Value::as_str)
                    == Some("budget_window_exhausted")
                    && grant
                        .get("recovery_epoch")
                        .and_then(|binding| binding.get("schema_version"))
                        .and_then(serde_json::Value::as_u64)
                        == Some(2) =>
            {
                Ok(Some("admission_repair"))
            }
            Some(5)
                if grant
                    .get("subject")
                    .and_then(|subject| subject.get("kind"))
                    .and_then(serde_json::Value::as_str)
                    == Some("budget_window_exhausted")
                    && grant
                        .get("work_funding")
                        .is_some_and(serde_json::Value::is_object)
                    && grant
                        .get("resume_policy")
                        .is_none_or(serde_json::Value::is_null)
                    && grant
                        .get("recovery_epoch")
                        .is_none_or(serde_json::Value::is_null)
                    && grant
                        .get("funded_review_recovery")
                        .is_none_or(serde_json::Value::is_null) =>
            {
                Ok(Some("work_funding"))
            }
            Some(6)
                if grant
                    .get("subject")
                    .and_then(|subject| subject.get("kind"))
                    .and_then(serde_json::Value::as_str)
                    == Some("budget_window_exhausted")
                    && grant
                        .get("work_funding")
                        .is_some_and(serde_json::Value::is_object)
                    && grant
                        .get("funded_review_recovery")
                        .and_then(|binding| binding.get("schema_version"))
                        .and_then(serde_json::Value::as_u64)
                        == Some(1)
                    && grant
                        .get("resume_policy")
                        .is_none_or(serde_json::Value::is_null)
                    && grant
                        .get("recovery_epoch")
                        .is_none_or(serde_json::Value::is_null) =>
            {
                Ok(Some("funded_undispatched_recovery"))
            }
            _ => Err("unsupported leadership review grant schema"),
        }
    }

    fn bind_model_work_request(
        request: &mut GatewayRequest,
        context: &ModelWorkContext,
    ) -> Result<(), &'static str> {
        let binding = context.binding();
        let forbidden = match binding {
            ProviderExecutionAuthority::AdaptiveLeadershipReview(_) => &[
                "customer_request_id",
                "customer_request_version",
                "adaptive_session_id",
                "adaptive_effect_id",
                "adaptive_session_version",
                "project_version",
            ][..],
            ProviderExecutionAuthority::Project(_) | ProviderExecutionAuthority::Adaptive(_) => &[
                "company_execution_subject",
                "customer_request_id",
                "customer_request_version",
                "project_version",
            ][..],
            ProviderExecutionAuthority::RequestSales(_) => &[
                "project_id",
                "project_version",
                "work_item_id",
                "assignment_id",
                "assignment_version",
            ][..],
            ProviderExecutionAuthority::ProjectPlanning(_) => &[
                "customer_request_id",
                "customer_request_version",
                "work_item_id",
                "assignment_id",
                "assignment_version",
            ][..],
        };
        if forbidden
            .iter()
            .any(|key| request.metadata.contains_key(*key))
        {
            return Err("mixed model execution subjects");
        }
        let mut required = provider_metadata(&binding);
        required.insert("agent_id".to_owned(), binding.agent_id().0.to_string());
        required.insert("request_id".to_owned(), binding.request_id());
        for (key, value) in &required {
            if request.metadata.get(key) != Some(value) {
                return Err("model work request authority does not match its context");
            }
        }
        let perception_snapshot = request
            .metadata
            .get("agent_perception_snapshot")
            .cloned()
            .ok_or("model work request is missing its perception snapshot")?;
        if perception_snapshot.len() > MAX_MODEL_WORK_BYTES {
            return Err("model perception snapshot exceeds its bound");
        }
        let _: AgentPerceptionSnapshotV1 = serde_json::from_str(&perception_snapshot)
            .map_err(|_| "model perception snapshot is invalid")?;
        // A reservation names one provider effect. The bounded perception
        // snapshot is the immutable model input for that effect and retry.
        request.metadata.retain(|key, _| {
            matches!(
                key.as_str(),
                "agent_role" | "hierarchy_tier" | "agent_perception_snapshot"
            ) || required.contains_key(key)
        });
        let context_bytes =
            serde_json::to_vec(context).map_err(|_| "model work context encoding failed")?;
        request.metadata.insert(
            "company_execution_schema".to_owned(),
            match binding {
                ProviderExecutionAuthority::AdaptiveLeadershipReview(_) => "5",
                ProviderExecutionAuthority::RequestSales(_) => "2",
                ProviderExecutionAuthority::Adaptive(_) => "3",
                ProviderExecutionAuthority::ProjectPlanning(_) => "4",
                ProviderExecutionAuthority::Project(_) => "1",
            }
            .to_owned(),
        );
        request.metadata.insert(
            "company_execution_context_digest".to_owned(),
            match context {
                ModelWorkContext::AdaptiveLeadershipReview(review) => review.context_digest.clone(),
                _ => format!("{:x}", Sha256::digest(context_bytes)),
            },
        );
        if let ModelWorkContext::Project(work) = context {
            request.metadata.insert(
                "company_execution_output_kind".to_owned(),
                if work.task.required_role == sentinel_workflow::CompanyRoleV1::Qa {
                    "source_review"
                } else {
                    "tool_plan"
                }
                .to_owned(),
            );
        } else if matches!(context, ModelWorkContext::Adaptive(_)) {
            request.metadata.insert(
                "company_execution_output_kind".to_owned(),
                "adaptive_decision".to_owned(),
            );
            if let ModelWorkContext::Adaptive(adaptive) = context {
                if adaptive.fresh_observation_required {
                    request.metadata.insert(
                        "company_execution_fresh_observation_required".to_owned(),
                        "true".to_owned(),
                    );
                }
            }
        } else if matches!(context, ModelWorkContext::AdaptiveLeadershipReview(_)) {
            request.metadata.insert(
                "company_execution_output_kind".to_owned(),
                "leadership_decision".to_owned(),
            );
            if let ModelWorkContext::AdaptiveLeadershipReview(review) = context {
                review
                    .source
                    .validate(&review.binding.grant)
                    .map_err(|_| "leadership generation evidence invalid")?;
                // A generation constraint, not authority: adoption still checks
                // every exact reference against the sealed review context.
                let refs = serde_json::to_string(&review.source.evidence_refs)
                    .map_err(|_| "leadership generation evidence encoding failed")?;
                if refs.len() > sentinel_workflow::ADAPTIVE_LEADERSHIP_MAX_CONTEXT_BYTES {
                    return Err("leadership generation evidence exceeds bound");
                }
                request
                    .metadata
                    .insert("leadership_evidence_refs".to_owned(), refs);
            }
        }
        if let Some(grant) = binding
            .project()
            .and_then(|value| value.subscription_grant.as_ref())
        {
            request.model = grant.model.clone();
            request.metadata.insert(
                "subscription_allowance_id".to_owned(),
                binding.reservation_id().to_owned(),
            );
            request.metadata.insert(
                "subscription_catalog_digest".to_owned(),
                grant.catalog_digest.clone(),
            );
        }
        if let ProviderExecutionAuthority::RequestSales(sales) = &binding {
            request.model = sales.grant.model.clone();
            request.metadata.insert(
                "subscription_allowance_id".to_owned(),
                sales.allowance_id.clone(),
            );
            request.metadata.insert(
                "subscription_catalog_digest".to_owned(),
                sales.grant.catalog_digest.clone(),
            );
        }
        if let ProviderExecutionAuthority::Adaptive(adaptive) = &binding {
            request.model = adaptive.grant.model.clone();
            request.max_tokens = i32::try_from(adaptive.grant.max_output_tokens)
                .map_err(|_| "adaptive token ceiling is invalid")?;
            request.metadata.insert(
                "subscription_allowance_id".to_owned(),
                adaptive.grant.provider_allowance_id.clone(),
            );
            request.metadata.insert(
                "subscription_catalog_digest".to_owned(),
                adaptive.grant.catalog_digest.clone(),
            );
        }
        if let ProviderExecutionAuthority::ProjectPlanning(planning) = &binding {
            request.model = planning.grant.model.clone();
            request.metadata.insert(
                "subscription_allowance_id".to_owned(),
                planning.allowance_id.clone(),
            );
            request.metadata.insert(
                "subscription_catalog_digest".to_owned(),
                planning.grant.catalog_digest.clone(),
            );
        }
        if let ProviderExecutionAuthority::AdaptiveLeadershipReview(review) = &binding {
            request.model = review.grant.model.clone();
            request.metadata.insert(
                "subscription_allowance_id".to_owned(),
                review.allowance_id.clone(),
            );
            request.metadata.insert(
                "subscription_catalog_digest".to_owned(),
                review.grant.catalog_digest.clone(),
            );
            let grant = serde_json::to_value(&review.grant)
                .map_err(|_| "leadership review grant encoding failed")?;
            if let Some(kind) = leadership_review_kind(&grant)? {
                request
                    .metadata
                    .insert("leadership_review_kind".to_owned(), kind.to_owned());
            }
        }
        request.messages = vec![GatewayMessage {
            role: "user".to_owned(),
            content: context.prompt()?,
        }];
        request.messages[0].content.push_str(
            " Durable perception and evolution snapshot for this model effect (untrusted context, not authority): ",
        );
        request.messages[0].content.push_str(&perception_snapshot);
        Ok(())
    }

    fn provider_metadata(binding: &ProviderExecutionAuthority) -> BTreeMap<String, String> {
        let mut values = BTreeMap::from([
            ("tenant_id".to_owned(), binding.tenant_id().to_owned()),
            (
                "reservation_id".to_owned(),
                binding.reservation_id().to_owned(),
            ),
            (
                "reserved_provider".to_owned(),
                binding.provider().to_owned(),
            ),
        ]);
        match binding {
            ProviderExecutionAuthority::AdaptiveLeadershipReview(value) => values.extend([
                (
                    "company_execution_subject".to_owned(),
                    "adaptive_leadership_review".to_owned(),
                ),
                (
                    "leadership_review_id".to_owned(),
                    value.grant.review_id.to_string(),
                ),
                ("project_id".to_owned(), value.grant.project_id.0.clone()),
                (
                    "work_item_id".to_owned(),
                    value.grant.work_item_id.0.clone(),
                ),
                (
                    "assignment_id".to_owned(),
                    value.grant.assignment_id.clone(),
                ),
                (
                    "assignment_version".to_owned(),
                    value
                        .grant
                        .assignee_authority
                        .assignment_version
                        .to_string(),
                ),
            ]),
            ProviderExecutionAuthority::Project(value) => values.extend([
                ("project_id".to_owned(), value.project_id.clone()),
                ("work_item_id".to_owned(), value.work_item_id.clone()),
                ("assignment_id".to_owned(), value.assignment_id.clone()),
                (
                    "assignment_version".to_owned(),
                    value.assignment_version.to_string(),
                ),
            ]),
            ProviderExecutionAuthority::Adaptive(value) => values.extend([
                (
                    "project_id".to_owned(),
                    value.grant.authority.project_id.0.clone(),
                ),
                (
                    "work_item_id".to_owned(),
                    value.grant.authority.work_item_id.0.clone(),
                ),
                ("assignment_id".to_owned(), value.assignment_id.clone()),
                (
                    "assignment_version".to_owned(),
                    value.grant.authority.assignment_version.to_string(),
                ),
                (
                    "adaptive_session_id".to_owned(),
                    value.grant.session_id.to_string(),
                ),
                ("adaptive_effect_id".to_owned(), value.effect_id.to_string()),
                (
                    "adaptive_session_version".to_owned(),
                    value.session_version.to_string(),
                ),
            ]),
            ProviderExecutionAuthority::RequestSales(value) => values.extend([
                (
                    "company_execution_subject".to_owned(),
                    "customer_request".to_owned(),
                ),
                (
                    "customer_request_id".to_owned(),
                    value.grant.request_id.clone(),
                ),
                (
                    "customer_request_version".to_owned(),
                    value.grant.expected_version.to_string(),
                ),
            ]),
            ProviderExecutionAuthority::ProjectPlanning(value) => values.extend([
                (
                    "company_execution_subject".to_owned(),
                    "project_planning".to_owned(),
                ),
                ("project_id".to_owned(), value.grant.project_id.0.clone()),
                (
                    "project_version".to_owned(),
                    value.grant.expected_version.to_string(),
                ),
            ]),
        }
        values
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

        include!("llm_bridge/tests/owned_pipeline.rs");

        mod completion_recovery_tests {
            include!("llm_bridge/tests/completion_recovery.rs");
        }

        #[test]
        fn completion_stage_observation_preserves_results_and_runs_once() {
            let mut calls = 0;
            let value = observe_completion_stage("fixture", "enqueue", || {
                calls += 1;
                Ok::<_, &'static str>(17)
            });
            assert_eq!(value, Ok(17));
            let failure = observe_completion_stage("fixture", "admission", || {
                calls += 1;
                Err::<(), _>("retained for recovery")
            });
            assert_eq!(failure, Err("retained for recovery"));
            assert_eq!(calls, 2);
        }

        #[test]
        fn coalesced_queue_age_uses_actual_selected_tick_not_inflight_history() {
            let heard_age = Instant::now();
            let fresh_age = heard_age + Duration::from_secs(1);
            let window = PreparationWindow {
                enqueued_at: fresh_age,
                tick: 56,
                urgent: false,
                heard: false,
                retained_heard: Some((55, heard_age)),
            };
            assert_eq!(window.enqueued_at_for(55), heard_age);
            assert_eq!(window.enqueued_at_for(56), fresh_age);
            assert!(window.contains_tick(55));
            assert!(window.contains_tick(56));
            assert!(!window.contains_tick(57));
        }

        #[tokio::test]
        async fn unreserved_retry_coalesces_heard_and_preserves_original_queue_age() {
            let queue = Arc::new(AsyncMutex::new(HashMap::new()));
            let mut heard = recovery_test_perception();
            heard.heard_text = "original heard intent".into();
            let original = Arc::new(PreparationWindow {
                enqueued_at: Instant::now(),
                tick: heard.tick.0,
                urgent: true,
                heard: true,
                retained_heard: None,
            });
            assert!(queue_bounded_retry(&queue, heard.clone(), Some(Arc::clone(&original))).await);
            let mut newer = heard.clone();
            newer.tick = Tick(56);
            newer.heard_text.clear();
            newer.has_operator_impulse = true;
            let newer_window = Arc::new(PreparationWindow {
                enqueued_at: Instant::now(),
                tick: 56,
                urgent: true,
                heard: false,
                retained_heard: None,
            });
            assert!(queue_bounded_retry(&queue, newer, Some(newer_window)).await);
            {
                let pending = queue.lock().await;
                assert_eq!(pending.len(), 1);
                let retained = pending.get(&heard.agent_id).unwrap();
                assert_eq!(retained.perception.tick, heard.tick);
                assert_eq!(retained.perception.heard_text, heard.heard_text);
                assert!(retained.perception.has_operator_impulse);
                assert!(Arc::ptr_eq(retained.window.as_ref().unwrap(), &original));
            }
            let mut distinct = heard.clone();
            distinct.tick = Tick(57);
            distinct.heard_text = "distinct heard intent".into();
            let distinct_window = Arc::new(PreparationWindow {
                enqueued_at: Instant::now(),
                tick: 57,
                urgent: true,
                heard: true,
                retained_heard: None,
            });
            assert!(
                queue_bounded_retry(&queue, distinct, Some(Arc::clone(&distinct_window))).await
            );
            let pending = queue.lock().await;
            let retained = pending.get(&heard.agent_id).unwrap();
            assert_eq!(retained.perception.tick, Tick(57));
            assert!(Arc::ptr_eq(
                retained.window.as_ref().unwrap(),
                &distinct_window
            ));
        }

        #[tokio::test]
        async fn bounded_unreserved_retry_overflow_preserves_pending_intents() {
            let queue = Arc::new(AsyncMutex::new(HashMap::new()));
            for agent in 0..256 {
                let mut perception = recovery_test_perception();
                perception.agent_id = AgentId(agent);
                perception.is_directly_addressed = true;
                assert!(queue_bounded_retry(&queue, perception, None).await);
            }
            let mut overflow = recovery_test_perception();
            overflow.agent_id = AgentId(256);
            overflow.is_directly_addressed = true;
            assert!(!queue_bounded_retry(&queue, overflow, None).await);
            let mut duplicate = recovery_test_perception();
            duplicate.is_directly_addressed = true;
            duplicate.heard_text = "retained exact urgent intent".into();
            assert!(queue_bounded_retry(&queue, duplicate.clone(), None).await);
            let pending = queue.lock().await;
            assert_eq!(pending.len(), 256);
            assert!(!pending.contains_key(&AgentId(256)));
            assert_eq!(
                pending
                    .get(&duplicate.agent_id)
                    .unwrap()
                    .perception
                    .heard_text,
                duplicate.heard_text
            );
        }

        fn recovery_lane_test_dependencies(
            directory: &std::path::Path,
            runtime: &tokio::runtime::Runtime,
            executor: BlockingExecutor,
        ) -> ProviderPreparationDependencies<EventStore> {
            let (action_tx, _) = mpsc::channel();
            ProviderPreparationDependencies {
                config: LlmBridgeConfig::default(),
                state_store: Arc::new(
                    StateStore::open(directory.join("state.redb").to_str().unwrap()).unwrap(),
                ),
                event_store: Arc::new(EventStore::open(":memory:").unwrap()),
                action_tx,
                active_requests: Arc::new(Mutex::new(HashMap::new())),
                preparing_requests: Arc::new(Mutex::new(HashMap::new())),
                completion_max_attempts: 3,
                executor,
                runtime: runtime.handle().clone(),
                last_call_tick: Arc::new(Mutex::new(HashMap::new())),
                activity: Arc::new(Mutex::new(HashMap::new())),
                telemetry: Arc::new(BridgeTelemetry::default()),
            }
        }

        fn assert_owned_transport_cancel_boundary(cancel_before_poll: bool) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let mut lanes = OwnedBlockingLanes::spawn(1, 32).unwrap();
            let store = Arc::new(EventStore::open(":memory:").unwrap());
            let worker_store = Arc::clone(&store);
            let active = Arc::new(Mutex::new(HashMap::new()));
            let worker_active = Arc::clone(&active);
            let polls = Arc::new(AtomicU64::new(0));
            let worker_polls = Arc::clone(&polls);
            let handle = runtime.handle().clone();
            let (reserved_tx, reserved_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let (polled_tx, polled_rx) = mpsc::channel();
            let digest = "a".repeat(64);
            let worker_digest = digest.clone();
            let job = lanes
                .executor()
                .submit(Lane::Provider, None, move |cancellation| {
                    // Guard creation, classification, and Drop all belong to this accepted worker.
                    let mut guard = ProviderOutcomeGuard::new(
                        worker_store,
                        worker_active,
                        "owned-first-poll",
                        &worker_digest,
                    );
                    assert_eq!(
                        reserve_provider_request_until(
                            &RwLock::new(true),
                            guard.store.as_ref(),
                            &guard.request_id,
                            &guard.request_digest,
                            &AgentId(7).to_string(),
                            &|| false
                        )
                        .unwrap(),
                        Some(true)
                    );
                    guard.armed = true;
                    reserved_tx.send(()).unwrap();
                    let _ = release_rx.recv_timeout(Duration::from_secs(8));
                    let first_poll = AtomicBool::new(false);
                    let transport = std::future::poll_fn(|_| {
                        if worker_polls.fetch_add(1, Ordering::SeqCst) == 0 {
                            polled_tx.send(()).unwrap();
                        }
                        std::task::Poll::<()>::Pending
                    });
                    assert!(handle
                        .block_on(await_owned_transport(&cancellation, transport, &first_poll))
                        .is_none());
                    let polled = first_poll.load(Ordering::Acquire);
                    classify_cancelled_transport(&mut guard, polled);
                    drop(guard);
                    polled
                })
                .unwrap();
            reserved_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let cancellation = job.cancellation();
            if cancel_before_poll {
                cancellation.cancel();
            }
            release_tx.send(()).unwrap();
            if !cancel_before_poll {
                polled_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                cancellation.cancel();
            }
            assert_eq!(runtime.block_on(job.wait()).unwrap(), !cancel_before_poll);
            lanes.stop_and_join().unwrap();
            assert!(active.lock().unwrap().is_empty());
            if cancel_before_poll {
                assert_eq!(polls.load(Ordering::SeqCst), 0);
                assert!(store.get_completion("owned-first-poll").unwrap().is_none());
                assert!(store
                    .reserve_request("owned-first-poll", &digest, &AgentId(7).to_string())
                    .unwrap());
            } else {
                assert!(polls.load(Ordering::SeqCst) >= 1);
                let unknown = store.get_completion("owned-first-poll").unwrap().unwrap();
                assert_eq!(unknown.status, "failed");
                assert_eq!(
                    unknown.last_error.as_deref(),
                    Some("UnknownOutcome: bridge_task_ended_without_durable_response")
                );
                assert!(!store
                    .reserve_request("owned-first-poll", &digest, &AgentId(7).to_string())
                    .unwrap());
            }
            assert!(store.get_all_events().unwrap().is_empty());
        }

        #[test]
        fn owned_transport_cancel_before_first_poll_releases_undispatched_reservation() {
            assert_owned_transport_cancel_boundary(true);
        }

        #[test]
        fn owned_transport_cancel_after_first_poll_preserves_unknown_outcome() {
            assert_owned_transport_cancel_boundary(false);
        }

        #[test]
        fn terminal_unavailability_survives_circuit_success_and_reset() {
            let unavailable = Arc::new(AtomicBool::new(false));
            let mut circuit = CircuitBreaker::new(2, Duration::ZERO, Arc::clone(&unavailable));
            circuit.mark_terminal();
            circuit.record_success();
            assert!(unavailable.load(Ordering::Acquire));
            assert!(circuit.is_open());
            circuit.record_failure();
            circuit.record_success();
            assert!(circuit.is_open());
            assert!(unavailable.load(Ordering::Acquire));
        }

        #[test]
        fn required_recovery_lane_full_fails_closed_without_running_callback() {
            let directory = tempfile::tempdir().unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            // This synchronous test owns and joins lanes outside Tokio, as production does.
            let mut lanes = OwnedBlockingLanes::spawn(1, 32).unwrap();
            let executor = lanes.executor();
            let dependencies =
                recovery_lane_test_dependencies(directory.path(), &runtime, executor.clone());
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let running = executor
                .submit_required(Lane::Recovery, move |_| {
                    entered_tx.send(()).unwrap();
                    let _ = release_rx.recv_timeout(Duration::from_secs(8));
                })
                .unwrap();
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let queued: Vec<_> = (0..8)
                .map(|_| executor.submit_required(Lane::Recovery, |_| {}).unwrap())
                .collect();
            let called = Arc::new(AtomicBool::new(false));
            let callback_called = Arc::clone(&called);
            assert_eq!(
                recover_on_owned_lane(&dependencies, move |_| {
                    callback_called.store(true, Ordering::SeqCst);
                }),
                Err("llm_recovery_lane_full")
            );
            assert!(!called.load(Ordering::SeqCst));
            drop(release_tx);
            drop(queued);
            drop(running);
            lanes.stop_and_join().unwrap();
            assert!(dependencies
                .event_store
                .get_all_events()
                .unwrap()
                .is_empty());
        }

        #[test]
        fn required_recovery_lane_closed_fails_closed_without_running_callback() {
            let directory = tempfile::tempdir().unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let mut lanes = OwnedBlockingLanes::spawn(1, 32).unwrap();
            let dependencies =
                recovery_lane_test_dependencies(directory.path(), &runtime, lanes.executor());
            lanes.stop_and_join().unwrap();
            let called = Arc::new(AtomicBool::new(false));
            let callback_called = Arc::clone(&called);
            assert_eq!(
                recover_on_owned_lane(&dependencies, move |_| {
                    callback_called.store(true, Ordering::SeqCst);
                }),
                Err("llm_recovery_lane_closed")
            );
            assert!(!called.load(Ordering::SeqCst));
            assert!(dependencies
                .event_store
                .get_all_events()
                .unwrap()
                .is_empty());
        }

        #[test]
        fn required_recovery_lane_panic_is_propagated() {
            let directory = tempfile::tempdir().unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let mut lanes = OwnedBlockingLanes::spawn(1, 32).unwrap();
            let dependencies =
                recovery_lane_test_dependencies(directory.path(), &runtime, lanes.executor());
            let recovery: Result<(), _> = recover_on_owned_lane(&dependencies, |_| {
                panic!("synthetic recovery callback panic");
            });
            assert_eq!(recovery, Err("llm_recovery_job_panicked"));
            lanes.stop_and_join().unwrap();
            assert!(dependencies
                .event_store
                .get_all_events()
                .unwrap()
                .is_empty());
        }

        #[tokio::test]
        async fn preparation_guard_suppresses_exact_queued_request_and_releases_on_abort() {
            let requests: PreparingProviderRequests = Arc::new(Mutex::new(HashMap::new()));
            let guard =
                ProviderPreparationGuard::try_acquire(&requests, "agent36-effect-one").unwrap();
            let semaphore = Arc::new(Semaphore::new(0));
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                let _guard = guard;
                started_tx.send(()).unwrap();
                let _permit = semaphore.acquire_owned().await.unwrap();
            });
            started_rx.await.unwrap();
            assert!(
                ProviderPreparationGuard::try_acquire(&requests, "agent36-effect-one").is_none()
            );
            let different = ProviderPreparationGuard::try_acquire(&requests, "agent36-effect-two")
                .expect("a different effect of the same employee remains eligible");
            drop(different);
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            assert!(requests.lock().unwrap().is_empty());
            assert!(
                ProviderPreparationGuard::try_acquire(&requests, "agent36-effect-one").is_some()
            );
        }

        #[tokio::test]
        async fn preparation_guard_releases_on_timeout_and_shutdown_before_reservation() {
            let requests: PreparingProviderRequests = Arc::new(Mutex::new(HashMap::new()));
            {
                let _guard = ProviderPreparationGuard::try_acquire(&requests, "timeout").unwrap();
                let semaphore = Arc::new(Semaphore::new(0));
                assert!(
                    tokio::time::timeout(Duration::from_millis(1), semaphore.acquire_owned())
                        .await
                        .is_err()
                );
            }
            assert!(requests.lock().unwrap().is_empty());
            let guard = ProviderPreparationGuard::try_acquire(&requests, "shutdown").unwrap();
            let (tx, mut rx) = watch::channel(false);
            let task = tokio::spawn(async move {
                let _guard = guard;
                rx.changed().await.unwrap();
            });
            tx.send(true).unwrap();
            task.await.unwrap();
            assert!(requests.lock().unwrap().is_empty());
        }

        #[test]
        fn preparation_guard_is_not_orphan_authority_and_releases_after_unwind() {
            let requests: PreparingProviderRequests = Arc::new(Mutex::new(HashMap::new()));
            let active: ActiveProviderRequests = Arc::new(Mutex::new(HashMap::new()));
            let result = std::panic::catch_unwind(|| {
                let _guard = ProviderPreparationGuard::try_acquire(&requests, "panic").unwrap();
                assert!(active.lock().unwrap().is_empty());
                panic!("synthetic preparation failure");
            });
            assert!(result.is_err());
            assert!(requests.lock().unwrap().is_empty());
            assert!(active.lock().unwrap().is_empty());
        }

        #[test]
        fn bound_preparation_timings_are_correlated_at_info_without_unbound_noise() {
            #[derive(Clone)]
            struct Writer(Arc<Mutex<Vec<u8>>>);
            impl std::io::Write for Writer {
                fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                    self.0.lock().unwrap().extend_from_slice(bytes);
                    Ok(bytes.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let output = Arc::new(Mutex::new(Vec::new()));
            let writer = Writer(Arc::clone(&output));
            let subscriber = tracing_subscriber::fmt()
                .json()
                .without_time()
                .with_max_level(tracing::Level::INFO)
                .with_writer(move || writer.clone())
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                for phase in ["selection", "context"] {
                    log_provider_preparation_duration(
                        AgentId(36),
                        Some("company-test-request"),
                        phase,
                        Instant::now(),
                        true,
                        true,
                    );
                }
                log_provider_preparation_duration(
                    AgentId(36),
                    Some("company-test-request"),
                    "context",
                    Instant::now() - Duration::from_secs(2),
                    true,
                    false,
                );
                log_provider_preparation_duration(
                    AgentId(36),
                    None,
                    "context",
                    Instant::now(),
                    false,
                    true,
                );
            });
            let bytes = output.lock().unwrap();
            let rows: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(rows.len(), 3);
            for row in &rows {
                let fields = &row["fields"];
                assert_eq!(fields["request_id"], "company-test-request");
                let elapsed_us = fields["elapsed_us"].as_u64().unwrap();
                assert_eq!(fields["elapsed_ms"].as_u64().unwrap(), elapsed_us / 1_000);
            }
            assert_eq!(rows[0]["level"], "INFO");
            assert_eq!(rows[1]["fields"]["phase"], "context");
            assert_eq!(rows[2]["level"], "WARN");
            assert_eq!(rows[2]["fields"]["accepted"], false);
        }

        #[test]
        fn output_schema_diagnostic_requires_exact_single_post_dispatch_header() {
            use reqwest::header::{HeaderMap, HeaderValue};
            let mut headers = HeaderMap::new();
            let status = reqwest::StatusCode::BAD_GATEWAY;
            assert!(!is_output_schema_rejection(status, &headers));
            headers.insert(
                "x-sentinel-provider-diagnostic",
                HeaderValue::from_static("codex_output_schema_rejected"),
            );
            assert!(is_output_schema_rejection(status, &headers));
            assert!(!is_output_schema_rejection(
                reqwest::StatusCode::SERVICE_UNAVAILABLE,
                &headers
            ));
            headers.insert(
                "x-sentinel-provider-io",
                HeaderValue::from_static("not-started"),
            );
            assert!(!is_output_schema_rejection(status, &headers));
            headers.remove("x-sentinel-provider-io");
            headers.append(
                "x-sentinel-provider-diagnostic",
                HeaderValue::from_static("codex_output_schema_rejected"),
            );
            assert!(!is_output_schema_rejection(status, &headers));
            headers.insert(
                "x-sentinel-provider-diagnostic",
                HeaderValue::from_static("unknown"),
            );
            assert!(!is_output_schema_rejection(status, &headers));
            headers.insert(
                "x-sentinel-provider-diagnostic",
                HeaderValue::from_bytes(&[0xff]).unwrap(),
            );
            assert!(!is_output_schema_rejection(status, &headers));
        }

        #[test]
        fn contradictory_schema_diagnostic_cannot_release_pre_provider_reservation() {
            use reqwest::header::{HeaderMap, HeaderValue};
            let store = EventStore::open(":memory:").unwrap();
            store
                .reserve_llm_request("diagnostic-contradiction", "digest", "AGENT-07")
                .unwrap();
            let mut headers = HeaderMap::new();
            headers.insert(
                "x-sentinel-provider-io",
                HeaderValue::from_static("not-started"),
            );
            headers.insert(
                "x-sentinel-provider-diagnostic",
                HeaderValue::from_static("codex_output_schema_rejected"),
            );
            assert!(!handle_gateway_rejection(
                &store,
                None,
                "diagnostic-contradiction",
                "digest",
                7,
                reqwest::StatusCode::BAD_GATEWAY,
                &headers
            ));
            assert_eq!(
                store
                    .get_llm_completion("diagnostic-contradiction")
                    .unwrap()
                    .unwrap()
                    .status,
                "provider_in_flight"
            );
            headers.remove("x-sentinel-provider-diagnostic");
            headers.append(
                "x-sentinel-provider-io",
                HeaderValue::from_static("not-started"),
            );
            assert!(!handle_gateway_rejection(
                &store,
                None,
                "diagnostic-contradiction",
                "digest",
                7,
                reqwest::StatusCode::BAD_GATEWAY,
                &headers
            ));
            headers.insert(
                "x-sentinel-provider-io",
                HeaderValue::from_static("not-started"),
            );
            assert!(handle_gateway_rejection(
                &store,
                None,
                "diagnostic-contradiction",
                "digest",
                7,
                reqwest::StatusCode::BAD_GATEWAY,
                &headers
            ));
            assert!(store
                .get_llm_completion("diagnostic-contradiction")
                .unwrap()
                .is_none());
        }

        fn reserved_guard(
            store: &Arc<EventStore>,
            active: &ActiveProviderRequests,
            id: &str,
        ) -> ProviderOutcomeGuard<EventStore> {
            let mut guard =
                ProviderOutcomeGuard::new(Arc::clone(store), Arc::clone(active), id, "digest");
            assert!(store
                .reserve_request(id, "digest", &AgentId(6).to_string())
                .unwrap());
            guard.armed = true;
            guard
        }

        #[test]
        fn overdue_orphan_is_unknown_without_retry_or_late_response() {
            let dir = tempfile::tempdir().unwrap();
            let store =
                EventStore::open(dir.path().join("events.sqlite").to_str().unwrap()).unwrap();
            let active = Arc::new(Mutex::new(HashMap::new()));
            assert!(store
                .reserve_request("orphan", "digest", &AgentId(6).to_string())
                .unwrap());
            let created_at = store.get_completion("orphan").unwrap().unwrap().created_at;
            classify_stale_provider_requests(
                &store,
                &active,
                created_at + 44_999,
                Duration::from_secs(35),
            );
            assert_eq!(
                store.get_completion("orphan").unwrap().unwrap().status,
                "provider_in_flight"
            );
            classify_stale_provider_requests(
                &store,
                &active,
                created_at + 45_000,
                Duration::from_secs(35),
            );
            let entry = store.get_completion("orphan").unwrap().unwrap();
            assert_eq!(entry.status, "failed");
            assert_eq!(
                entry.last_error.as_deref(),
                Some("UnknownOutcome: provider_transport_deadline_elapsed")
            );
            assert_eq!(entry.attempt_count, 0);
            classify_stale_provider_requests(
                &store,
                &active,
                created_at + 21 * 60_000,
                Duration::from_secs(35),
            );
            assert!(!store
                .reserve_request("orphan", "digest", &AgentId(6).to_string())
                .unwrap());
            assert!(store
                .enqueue_completion("orphan", "digest", "late response")
                .is_err());
            let (tx, rx) = mpsc::channel();
            recover_completion(&store, entry, &tx, 3, None);
            assert!(rx.try_recv().is_err());
            assert_eq!(
                store
                    .get_completion("orphan")
                    .unwrap()
                    .unwrap()
                    .last_error
                    .as_deref(),
                Some("UnknownOutcome: provider_transport_deadline_elapsed")
            );
        }

        #[test]
        fn healthy_live_task_is_not_classified_by_reservation_age() {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(
                EventStore::open(dir.path().join("events.sqlite").to_str().unwrap()).unwrap(),
            );
            let active = Arc::new(Mutex::new(HashMap::new()));
            let guard = reserved_guard(&store, &active, "live");
            let created_at = store.get_completion("live").unwrap().unwrap().created_at;
            classify_stale_provider_requests(
                store.as_ref(),
                &active,
                created_at + 21 * 60_000,
                Duration::from_secs(35),
            );
            assert_eq!(
                store.get_completion("live").unwrap().unwrap().status,
                "provider_in_flight"
            );
            drop(guard);
            let entry = store.get_completion("live").unwrap().unwrap();
            assert_eq!(entry.status, "failed");
            assert_eq!(
                entry.last_error.as_deref(),
                Some("UnknownOutcome: bridge_task_ended_without_durable_response")
            );
            assert!(active.lock().unwrap().is_empty());
        }

        #[test]
        fn duplicate_guard_does_not_abandon_the_original_task() {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(
                EventStore::open(dir.path().join("events.sqlite").to_str().unwrap()).unwrap(),
            );
            let active = Arc::new(Mutex::new(HashMap::new()));
            let original = reserved_guard(&store, &active, "live");
            let duplicate = ProviderOutcomeGuard::new(
                Arc::clone(&store),
                Arc::clone(&active),
                "live",
                "digest",
            );
            assert!(!store
                .reserve_request("live", "digest", &AgentId(6).to_string())
                .unwrap());
            drop(duplicate);
            assert_eq!(active.lock().unwrap().get("live"), Some(&1));
            assert_eq!(
                store.get_completion("live").unwrap().unwrap().status,
                "provider_in_flight"
            );
            let conflicting = ProviderOutcomeGuard::new(
                Arc::clone(&store),
                Arc::clone(&active),
                "live",
                "different-digest",
            );
            assert_eq!(active.lock().unwrap().get("live"), Some(&2));
            assert!(store
                .reserve_request("live", "different-digest", &AgentId(6).to_string())
                .is_err());
            drop(conflicting);
            assert_eq!(active.lock().unwrap().get("live"), Some(&1));
            let pending = store.get_completion("live").unwrap().unwrap();
            assert_eq!(pending.status, "provider_in_flight");
            assert_eq!(pending.request_digest, "digest");
            drop(original);
        }

        #[test]
        fn guard_preserves_durable_response_even_before_disarm() {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(
                EventStore::open(dir.path().join("events.sqlite").to_str().unwrap()).unwrap(),
            );
            let active = Arc::new(Mutex::new(HashMap::new()));
            let guard = reserved_guard(&store, &active, "durable");
            store
                .enqueue_completion("durable", "digest", "durable response")
                .unwrap();
            drop(guard);
            let entry = store.get_completion("durable").unwrap().unwrap();
            assert_eq!(entry.status, "pending_usage");
            assert_eq!(entry.payload, "durable response");
            assert!(entry.last_error.is_none());
            classify_stale_provider_requests(
                store.as_ref(),
                &active,
                entry.created_at + 21 * 60_000,
                Duration::from_secs(35),
            );
            assert_eq!(
                store.get_completion("durable").unwrap().unwrap().status,
                "pending_usage"
            );
        }

        #[test]
        fn guard_disarms_for_released_and_removed_completion_rows() {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(
                EventStore::open(dir.path().join("events.sqlite").to_str().unwrap()).unwrap(),
            );
            let active = Arc::new(Mutex::new(HashMap::new()));
            let mut guard = reserved_guard(&store, &active, "released");
            assert!(store
                .release_undispatched_request("released", "digest")
                .unwrap());
            guard.disarm_if_resolved();
            assert!(!guard.armed);
            drop(guard);
            assert!(store.get_completion("released").unwrap().is_none());
            let mut removed = reserved_guard(&store, &active, "removed");
            store
                .enqueue_completion("removed", "digest", "durable response")
                .unwrap();
            let response: GatewayResponse = serde_json::from_value(serde_json::json!({
                "request_id": "removed", "tokens_used": 1
            }))
            .unwrap();
            let usage = build_usage_event(AgentId(6), 1, "", None, &response, false).unwrap();
            store.persist_usage("removed", "digest", &usage).unwrap();
            assert!(store.claim_actions("removed", "digest").unwrap());
            assert!(store.complete_actions("removed", "digest").unwrap());
            removed.disarm_if_resolved();
            assert!(!removed.armed);
            drop(removed);
            assert!(active.lock().unwrap().is_empty());
        }

        struct ModelWorkResolver {
            context: ModelWorkContext,
            admissions: Mutex<Vec<String>>,
            fail_next: AtomicBool,
        }

        impl ProviderUsageAuthorityResolver for ModelWorkResolver {
            fn resolve_provider_usage_authority(
                &self,
                _: AgentId,
            ) -> Result<Option<ProviderExecutionAuthority>, &'static str> {
                Ok(Some(self.context.binding()))
            }
            fn model_work_context(
                &self,
                _: &ProviderExecutionAuthority,
            ) -> Result<Option<ModelWorkContext>, &'static str> {
                Ok(Some(self.context.clone()))
            }
            fn admit_model_work(
                &self,
                completion: &ModelWorkCompletion,
                id: &str,
                _: &str,
            ) -> Result<(), &'static str> {
                if self.fail_next.swap(false, Ordering::SeqCst) {
                    return Err("injected admission failure");
                }
                if !completion.admissible || completion.context != self.context {
                    return Err("provider response is not admissible");
                }
                self.admissions.lock().unwrap().push(id.to_owned());
                Ok(())
            }
        }

        #[test]
        fn leadership_review_kind_is_grant_derived_and_legacy_wire_stays_absent() {
            assert_eq!(
                leadership_review_kind(&serde_json::json!({"schema_version": 1})),
                Ok(None)
            );
            assert_eq!(
                leadership_review_kind(&serde_json::json!({"schema_version": 1, "subject": null})),
                Ok(None)
            );
            for kind in ["unknown_model", "blocked_continuation"] {
                assert_eq!(
                    leadership_review_kind(&serde_json::json!({
                        "schema_version": 2, "subject": {"kind": kind}
                    })),
                    Ok(Some(kind))
                );
            }
            assert_eq!(
                leadership_review_kind(&serde_json::json!({"schema_version":3,
                    "subject":{"kind":"budget_window_exhausted"}})),
                Ok(Some("budget_window_exhausted"))
            );
            assert_eq!(
                leadership_review_kind(&serde_json::json!({"schema_version":4,
                    "subject":{"kind":"budget_window_exhausted"},
                    "recovery_epoch":{"schema_version":2}})),
                Ok(Some("admission_repair"))
            );
            for grant in [
                serde_json::json!({"schema_version": 2}),
                serde_json::json!({"schema_version": 2, "subject": null}),
                serde_json::json!({"schema_version": 2, "subject": {"kind": "tool"}}),
                serde_json::json!({"schema_version": 1, "subject": {"kind": "unknown_model"}}),
                serde_json::json!({"schema_version": 3, "subject": {"kind": "unknown_model"}}),
                serde_json::json!({"schema_version": 2, "subject": {"kind": "budget_window_exhausted"}}),
                serde_json::json!({"schema_version": 4, "subject": {"kind": "budget_window_exhausted"}}),
                serde_json::json!({"schema_version": 4, "subject": {"kind": "budget_window_exhausted"}, "recovery_epoch": {"schema_version": 1}}),
                serde_json::json!({"schema_version": 4, "subject": {"kind": "unknown_model"}, "recovery_epoch": {"schema_version": 2}}),
                serde_json::json!({"schema_version": 5, "subject": {"kind": "budget_window_exhausted"}}),
                serde_json::json!({"schema_version": 5, "subject": {"kind": "unknown_model"}, "work_funding": {}}),
                serde_json::json!({"schema_version": 5, "subject": {"kind": "budget_window_exhausted"}, "work_funding": {}, "resume_policy": {}}),
                serde_json::json!({"schema_version": 5, "subject": {"kind": "budget_window_exhausted"}, "work_funding": {}, "recovery_epoch": {}}),
                serde_json::json!({"schema_version": 5, "subject": {"kind": "budget_window_exhausted"}, "work_funding": {}, "funded_review_recovery": {"schema_version": 1}}),
                serde_json::json!({"schema_version": 6, "subject": {"kind": "budget_window_exhausted"}, "work_funding": {}}),
                serde_json::json!({"schema_version": 6, "subject": {"kind": "budget_window_exhausted"}, "work_funding": {}, "funded_review_recovery": {"schema_version": 2}}),
                serde_json::json!({"schema_version": 6, "subject": {"kind": "unknown_model"}, "work_funding": {}, "funded_review_recovery": {"schema_version": 1}}),
                serde_json::json!({"schema_version": 6, "subject": {"kind": "budget_window_exhausted"}, "work_funding": {}, "funded_review_recovery": {"schema_version": 1}, "recovery_epoch": {}}),
            ] {
                assert!(leadership_review_kind(&grant).is_err());
            }
            assert_eq!(
                leadership_review_kind(&serde_json::json!({"schema_version": 5,
                "subject": {"kind": "budget_window_exhausted"}, "work_funding": {}})),
                Ok(Some("work_funding"))
            );
            assert_eq!(
                leadership_review_kind(&serde_json::json!({"schema_version": 6,
                    "subject": {"kind": "budget_window_exhausted"}, "work_funding": {},
                    "funded_review_recovery": {"schema_version": 1}})),
                Ok(Some("funded_undispatched_recovery"))
            );
            let dir = tempfile::tempdir().unwrap();
            let (_, review) = crate::workflow_api::adaptive_leadership_review::tests::fixture(
                &dir.path().join("company.sqlite"),
                &dir.path().join("events.sqlite"),
            );
            let context = ModelWorkContext::AdaptiveLeadershipReview(Box::new(review));
            let binding = context.binding();
            let state = StateStore::open(dir.path().join("state.redb").to_str().unwrap()).unwrap();
            let perception =
                make_perception(binding.agent_id().0, "Review supplied evidence", true);
            let mut request =
                build_gateway_request(&perception, &state, &binding.request_id(), Some(&binding));
            request.metadata.insert(
                "leadership_review_kind".to_owned(),
                "unknown_model".to_owned(),
            );
            request.metadata.insert(
                "leadership_evidence_refs".to_owned(),
                "[\"model-authored-foreign-reference\"]".to_owned(),
            );
            bind_model_work_request(&mut request, &context).unwrap();
            assert!(!request.metadata.contains_key("leadership_review_kind"));
            let ModelWorkContext::AdaptiveLeadershipReview(review) = &context else {
                unreachable!();
            };
            assert_eq!(
                serde_json::from_str::<Vec<String>>(&request.metadata["leadership_evidence_refs"])
                    .unwrap(),
                review.source.evidence_refs
            );
            assert_eq!(request.metadata["company_execution_schema"], "5");
            assert_eq!(
                request.metadata["company_execution_subject"],
                "adaptive_leadership_review"
            );
        }

        #[test]
        fn leadership_dispatch_schema_five_adopts_usage_schema_six_without_legacy_actions() {
            let dir = tempfile::tempdir().unwrap();
            let event_path = dir.path().join("events.sqlite");
            let (api, review) = crate::workflow_api::adaptive_leadership_review::tests::fixture(
                &dir.path().join("company.sqlite"),
                &event_path,
            );
            let context = ModelWorkContext::AdaptiveLeadershipReview(Box::new(review.clone()));
            let binding = context.binding();
            let agent = binding.agent_id();
            let perception = make_perception(agent.0, "Review blocked work", true);
            let id = agent_runtime_request_id(&perception, Some(&binding));
            let state = StateStore::open(dir.path().join("state.redb").to_str().unwrap()).unwrap();
            let mut request = build_gateway_request(&perception, &state, &id, Some(&binding));
            bind_model_work_request(&mut request, &context).unwrap();
            assert_eq!(request.metadata["company_execution_schema"], "5");
            assert_eq!(
                request.metadata["company_execution_subject"],
                "adaptive_leadership_review"
            );
            assert_eq!(
                request.metadata["company_execution_output_kind"],
                "leadership_decision"
            );
            assert_eq!(
                request.metadata["company_execution_context_digest"],
                review.context_digest
            );
            assert_eq!(
                request.metadata["leadership_review_id"],
                review.binding.grant.review_id.to_string()
            );
            assert_eq!(
                request.metadata["subscription_allowance_id"],
                review.binding.allowance_id
            );
            assert_eq!(
                request.metadata["assignment_version"],
                review
                    .binding
                    .grant
                    .assignee_authority
                    .assignment_version
                    .to_string()
            );
            assert_eq!(request.model, review.binding.grant.model);
            assert_eq!(
                serde_json::from_str::<Vec<String>>(&request.metadata["leadership_evidence_refs"])
                    .unwrap(),
                review.source.evidence_refs
            );
            let original_digest = gateway_request_digest(&request).unwrap();
            let mut altered = build_gateway_request(&perception, &state, &id, Some(&binding));
            bind_model_work_request(&mut altered, &context).unwrap();
            altered.metadata.insert(
                "leadership_evidence_refs".to_owned(),
                "[\"truncated-reference\"]".to_owned(),
            );
            assert_ne!(gateway_request_digest(&altered).unwrap(), original_digest);
            for key in [
                "adaptive_session_id",
                "adaptive_effect_id",
                "adaptive_session_version",
                "customer_request_id",
                "customer_request_version",
                "project_version",
            ] {
                let mut mixed = build_gateway_request(&perception, &state, &id, Some(&binding));
                mixed.metadata.insert(key.to_owned(), "foreign".to_owned());
                assert!(
                    bind_model_work_request(&mut mixed, &context).is_err(),
                    "{key}"
                );
            }
            let digest = gateway_request_digest(&request).unwrap();
            let store = Arc::new(EventStore::open(event_path.to_str().unwrap()).unwrap());
            let active = Arc::new(Mutex::new(HashMap::new()));
            let mut guard = ProviderOutcomeGuard::new(Arc::clone(&store), active, &id, &digest);
            assert!(store
                .reserve_request(&id, &digest, &agent.to_string())
                .unwrap());
            guard.armed = true;
            let dispatch = serde_json::json!({
                "schema_version": 5, "allowance_id": review.binding.allowance_id,
                "agent_id": agent.0, "request_id": id, "request_digest": digest,
                "context_digest": review.context_digest, "provider": review.binding.grant.provider,
                "model": request.model, "catalog_digest": review.binding.grant.catalog_digest,
                "subject": {"kind": "adaptive_leadership_review", "review_id": review.binding.grant.review_id}
            });
            assert_eq!(
                api.subscription_dispatch(&serde_json::to_vec(&dispatch).unwrap())
                    .status,
                200
            );
            // Fixture inference, not live provider evidence.
            let content = serde_json::json!({"schema_version": 1, "decision": {
                "kind": "keep_blocked", "rationale": "Supplied evidence still shows a dependency.",
                "evidence_refs": review.source.evidence_refs
            }})
            .to_string();
            let response: GatewayResponse = serde_json::from_value(serde_json::json!({
                "content": content, "decision": "forward", "request_id": id,
                "provider": review.binding.grant.provider, "effective_model": request.model,
                "tokens_used": 15, "input_tokens": 5, "output_tokens": 10, "tier": "mid",
                "hierarchy_tier": 2, "cost_source": "provider_reported", "cost_usd": 0.0,
                "actions": [{"type": "tool_use", "content": "must never execute"}]
            }))
            .unwrap();
            let (tx, rx) = mpsc::channel();
            store_gateway_completion(
                store.as_ref(),
                &tx,
                GatewayCompletionContext {
                    request_id: &id,
                    request_digest: &digest,
                    agent_id: agent,
                    tick: 1,
                    requested_model: &request.model,
                    authority: Some(&binding),
                    authority_resolver: Some(&api),
                    gateway_response: &response,
                    usage_v2_enabled: true,
                    model_work: Some(&context),
                },
                3,
            )
            .unwrap();
            let usage = store
                .event_by_operation_id(&format!("llm_usage_{id}"))
                .unwrap()
                .unwrap();
            assert_eq!(usage.schema_version, 6);
            assert!(store.get_completion(&id).unwrap().is_none());
            guard.disarm_if_resolved();
            assert!(!guard.armed);
            drop(guard);
            assert!(!store
                .reserve_request(&id, &digest, &agent.to_string())
                .unwrap());
            assert!(rx.try_recv().is_err());
        }

        #[test]
        fn sealed_unknown_models_retain_late_usage_without_admission_or_legacy_actions() {
            let dir = tempfile::tempdir().unwrap();
            let (api, adaptive, _) = crate::workflow_api::model_work::configured_adaptive_test_api(
                &dir.path().join("adaptive-company.sqlite"),
                &dir.path().join("adaptive-events.sqlite"),
            );
            let adaptive_authority = ProviderExecutionAuthority::Adaptive(Box::new(adaptive));
            let adaptive_context = api
                .model_work_context(&adaptive_authority)
                .unwrap()
                .unwrap();
            let (_, review) = crate::workflow_api::adaptive_leadership_review::tests::fixture(
                &dir.path().join("leadership-company.sqlite"),
                &dir.path().join("leadership-events.sqlite"),
            );
            for (context, oversized) in [
                (adaptive_context, false),
                (
                    ModelWorkContext::AdaptiveLeadershipReview(Box::new(review.clone())),
                    false,
                ),
                (
                    ModelWorkContext::AdaptiveLeadershipReview(Box::new(review)),
                    true,
                ),
            ] {
                let binding = context.binding();
                let id = binding.request_id();
                let digest = "a".repeat(64);
                let store = EventStore::open(":memory:").unwrap();
                let resolver = ModelWorkResolver {
                    context: context.clone(),
                    admissions: Mutex::new(Vec::new()),
                    fail_next: AtomicBool::new(false),
                };
                assert!(store
                    .reserve_request(&id, &digest, &binding.agent_id().to_string())
                    .unwrap());
                validate_pre_dispatch_provider_authority(
                    &store,
                    Some(&resolver),
                    Some(&binding),
                    binding.agent_id(),
                    &id,
                    &digest,
                    Some(&context),
                )
                .unwrap();
                assert!(
                    sealed_unknown_model_evidence(&store, &binding, &id, &digest)
                        .unwrap()
                        .is_none()
                );
                store
                    .mark_provider_unknown(
                        &id,
                        &digest,
                        "UnknownOutcome: provider_transport_deadline_elapsed",
                    )
                    .unwrap();
                let original = store.get_completion(&id).unwrap().unwrap();
                let evidence = sealed_unknown_model_evidence(&store, &binding, &id, &digest)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    evidence.reservation,
                    model_reservation(&context, &id, &digest).unwrap().unwrap()
                );
                assert_eq!(original.status, "failed");
                assert!(original.payload.is_empty());
                let content = if oversized {
                    "x".repeat(MAX_MODEL_WORK_BYTES + 1)
                } else if let ModelWorkContext::AdaptiveLeadershipReview(review) = &context {
                    // A bounded, otherwise admissible leadership decision must
                    // remain unadopted because the terminal unknown seal won.
                    serde_json::json!({"schema_version": 1, "decision": {
                        "kind": "keep_blocked", "rationale": "Supplied evidence still shows a dependency.",
                        "evidence_refs": review.source.evidence_refs
                    }})
                    .to_string()
                } else {
                    "{}".to_owned()
                };
                let response: GatewayResponse = serde_json::from_value(serde_json::json!({
                    "content": content, "decision": "forward", "request_id": id,
                    "provider": binding.provider(), "effective_model": evidence.reservation.usage_binding.model,
                    "tokens_used": 15, "input_tokens": 5, "output_tokens": 10, "tier": "mid",
                    "hierarchy_tier": 2, "cost_source": "provider_reported", "cost_usd": 0.125,
                    "actions": [{"type": "tool_use", "content": "must never execute"}],
                })).unwrap();
                let (tx, rx) = mpsc::channel();
                store_gateway_completion(
                    &store,
                    &tx,
                    GatewayCompletionContext {
                        request_id: &id,
                        request_digest: &digest,
                        agent_id: binding.agent_id(),
                        tick: 1,
                        requested_model: &evidence.reservation.usage_binding.model,
                        authority: Some(&binding),
                        authority_resolver: Some(&resolver),
                        gateway_response: &response,
                        usage_v2_enabled: true,
                        model_work: Some(&context),
                    },
                    3,
                )
                .unwrap();
                let usage = store
                    .event_by_operation_id(&format!("llm_usage_{id}"))
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    usage.schema_version,
                    match &binding {
                        ProviderExecutionAuthority::Adaptive(_) => 3,
                        _ => 6,
                    }
                );
                let payload: serde_json::Value = serde_json::from_str(&usage.payload).unwrap();
                assert_eq!(payload["cost_usd"], serde_json::json!(0.125));
                assert_eq!(payload["input_tokens"], serde_json::json!(5));
                assert_eq!(payload["output_tokens"], serde_json::json!(10));
                assert!(
                    !retain_sealed_unknown_model_usage(&store, &context, &id, &digest, &usage)
                        .unwrap()
                );
                let mut changed = usage.clone();
                changed.event_id = uuid::Uuid::new_v4().to_string();
                assert!(retain_sealed_unknown_model_usage(
                    &store, &context, &id, &digest, &changed
                )
                .is_err());
                let mut altered_context = context.clone();
                match &mut altered_context {
                    ModelWorkContext::Adaptive(value) => {
                        value.binding.grant.authority.assignment_version += 1
                    }
                    ModelWorkContext::AdaptiveLeadershipReview(value) => {
                        value.binding.grant.assignee_authority.assignment_version += 1
                    }
                    _ => unreachable!(),
                }
                assert!(sealed_unknown_model_evidence(
                    &store,
                    &altered_context.binding(),
                    &id,
                    &digest
                )
                .is_err());
                assert!(retain_sealed_unknown_model_usage(
                    &store,
                    &altered_context,
                    &id,
                    &digest,
                    &usage
                )
                .is_err());
                assert!(
                    sealed_unknown_model_evidence(&store, &binding, &id, &"b".repeat(64)).is_err()
                );
                assert_eq!(store.get_completion(&id).unwrap().unwrap(), original);
                assert_eq!(
                    sealed_unknown_model_evidence(&store, &binding, &id, &digest)
                        .unwrap()
                        .unwrap(),
                    evidence
                );
                assert!(!store
                    .reserve_request(&id, &digest, &binding.agent_id().to_string())
                    .unwrap());
                assert_eq!(store.get_all_events().unwrap().len(), 1);
                assert!(store.poll_completions(10).unwrap().is_empty());
                assert!(resolver.admissions.lock().unwrap().is_empty());
                assert!(rx.try_recv().is_err());
            }
        }

        #[test]
        fn retrospective_model_accounting_requires_original_authority_not_prompt_reconstruction() {
            let dir = tempfile::tempdir().unwrap();
            let (_, adaptive, _) = crate::workflow_api::model_work::configured_adaptive_test_api(
                &dir.path().join("company.sqlite"),
                &dir.path().join("events.sqlite"),
            );
            let authority = ProviderExecutionAuthority::Adaptive(Box::new(adaptive));
            let ProviderExecutionAuthority::Adaptive(value) = &authority else {
                unreachable!()
            };
            let grant = &value.grant;
            let id = authority.request_id();
            let digest = "a".repeat(64);
            let store = EventStore::open(":memory:").unwrap();
            store
                .reserve_llm_request(&id, &digest, &authority.agent_id().to_string())
                .unwrap();
            store
                .mark_llm_provider_outcome_unknown(
                    &id,
                    &digest,
                    "UnknownOutcome: provider_transport_deadline_elapsed",
                )
                .unwrap();
            let original = store.get_llm_completion(&id).unwrap().unwrap();
            assert!(
                retrospective_unknown_model_evidence(&store, &authority, &id, &digest)
                    .unwrap()
                    .is_none()
            );
            // Synthetic reviewed descriptors exercise the API, not deployment proof.
            let receipt = |version, entry_digest: &str, time| {
                serde_json::json!({
                    "session_id": grant.session_id, "effect_id": value.effect_id, "request_digest": digest,
                    "session_version": version, "operation_id": uuid::Uuid::new_v4(),
                    "entry_digest": entry_digest.repeat(64), "timestamp_ms": time,
                })
            };
            let binding: sentinel_limbo::LlmRetrospectiveModelBindingV1 = serde_json::from_value(serde_json::json!({
                "schema_version": 1, "request_id": id, "request_digest": digest,
                "owner_scope": original.owner_scope,
                "subject": {"kind": "adaptive", "session_id": grant.session_id,
                    "effect_id": value.effect_id, "session_version": value.session_version},
                "allowance_id": grant.provider_allowance_id, "historical_context_digest": null,
                "authority_digest": format!("{:x}", Sha256::digest(serde_json::to_vec(&authority).unwrap())),
                "usage_binding": {
                    "agent_id": grant.authority.agent_id, "tenant_id": grant.authority.tenant_id.0,
                    "project_id": grant.authority.project_id.0, "work_item_id": grant.authority.work_item_id.0,
                    "reservation_id": grant.provider_allowance_id, "assignment_id": value.assignment_id,
                    "assignment_version": grant.authority.assignment_version, "provider": grant.provider, "model": grant.model,
                },
                "provenance": {
                    "kind": "journal_and_pinned_inference_boundary",
                    "claim_model": receipt(2, "b", original.created_at),
                    "mark_unknown": receipt(3, "c", original.updated_at),
                    "journal_head_digest": "c".repeat(64), "model_claims": 1, "tool_claims": 0,
                    "collaboration_claims": 0, "original_allowance_digest": grant.provider_authority_digest,
                    "boundary": {"release_git_sha": "a".repeat(40), "release_manifest_sha256": "b".repeat(64),
                        "gateway_binary_sha256": "c".repeat(64), "cli_binary_sha256": "d".repeat(64),
                        "cli_profile_sha256": "e".repeat(64), "boundary_receipt_sha256": "f".repeat(64),
                        "valid_from_ms": original.created_at, "valid_until_ms": original.created_at + 1},
                },
            })).unwrap();
            store
                .import_retrospective_unknown_llm_model_binding(&binding)
                .unwrap();
            let evidence = retrospective_unknown_model_evidence(&store, &authority, &id, &digest)
                .unwrap()
                .unwrap();
            assert!(evidence.binding.historical_context_digest.is_none());
            assert!(
                sealed_unknown_model_evidence(&store, &authority, &id, &digest)
                    .unwrap()
                    .is_none()
            );
            let response: GatewayResponse = serde_json::from_value(serde_json::json!({
                "content": "discarded proposal", "decision": "forward", "request_id": id,
                "provider": grant.provider, "effective_model": grant.model, "tokens_used": 15,
                "input_tokens": 5, "output_tokens": 10, "tier": "mid", "hierarchy_tier": 2,
                "cost_source": "provider_reported", "cost_usd": 0.125,
                "actions": [{"type": "tool_use", "content": "must not execute"}],
            }))
            .unwrap();
            let usage = build_usage_event(
                authority.agent_id(),
                1,
                &grant.model,
                Some(&authority),
                &response,
                true,
            )
            .unwrap();
            assert!(retain_retrospective_unknown_model_usage(
                &store, &authority, &id, &digest, &usage
            )
            .unwrap());
            assert!(!retain_retrospective_unknown_model_usage(
                &store, &authority, &id, &digest, &usage
            )
            .unwrap());
            let mut conflict = usage.clone();
            conflict.event_id = uuid::Uuid::new_v4().to_string();
            assert!(retain_retrospective_unknown_model_usage(
                &store, &authority, &id, &digest, &conflict
            )
            .is_err());
            for field in ["assignment", "allowance", "original_allowance", "effect"] {
                let mut changed = authority.clone();
                let ProviderExecutionAuthority::Adaptive(value) = &mut changed else {
                    unreachable!()
                };
                match field {
                    "assignment" => value.assignment_id = "current-assignment".into(),
                    "allowance" => value.grant.provider_allowance_id = "fresh-allowance".into(),
                    "original_allowance" => value.grant.provider_authority_digest = "f".repeat(64),
                    _ => value.effect_id = uuid::Uuid::new_v4(),
                }
                assert!(
                    retrospective_unknown_model_evidence(&store, &changed, &id, &digest).is_err(),
                    "{field}"
                );
                assert!(retain_retrospective_unknown_model_usage(
                    &store, &changed, &id, &digest, &usage
                )
                .is_err());
            }
            assert_eq!(store.get_llm_completion(&id).unwrap().unwrap(), original);
            assert!(store.poll_llm_completions(10).unwrap().is_empty());
            assert_eq!(store.get_all_events().unwrap().len(), 1);
            let separate = EventStore::open(":memory:").unwrap();
            separate
                .reserve_llm_request(&id, &digest, &authority.agent_id().to_string())
                .unwrap();
            separate
                .mark_llm_provider_outcome_unknown(
                    &id,
                    &digest,
                    "UnknownOutcome: provider_transport_deadline_elapsed",
                )
                .unwrap();
            let entry = separate.get_llm_completion(&id).unwrap().unwrap();
            let mut inconsistent = binding;
            let sentinel_limbo::LlmRetrospectiveModelProvenanceV1::JournalAndPinnedInferenceBoundary {
                claim_model, mark_unknown, original_allowance_digest, boundary, ..
            } = &mut inconsistent.provenance;
            claim_model.timestamp_ms = entry.created_at;
            mark_unknown.timestamp_ms = entry.updated_at;
            boundary.valid_from_ms = entry.created_at;
            boundary.valid_until_ms = entry.created_at + 1;
            *original_allowance_digest = "a".repeat(64);
            assert_ne!(
                original_allowance_digest.as_str(),
                grant.provider_authority_digest.as_str()
            );
            separate
                .import_retrospective_unknown_llm_model_binding(&inconsistent)
                .unwrap();
            assert!(
                retrospective_unknown_model_evidence(&separate, &authority, &id, &digest).is_err()
            );
            assert!(retain_retrospective_unknown_model_usage(
                &separate, &authority, &id, &digest, &usage
            )
            .is_err());
            assert!(separate.get_all_events().unwrap().is_empty());
        }

        #[test]
        fn unregistered_unknown_model_response_cannot_invent_accounting_authority() {
            let dir = tempfile::tempdir().unwrap();
            let (_, review) = crate::workflow_api::adaptive_leadership_review::tests::fixture(
                &dir.path().join("company.sqlite"),
                &dir.path().join("events.sqlite"),
            );
            let context = ModelWorkContext::AdaptiveLeadershipReview(Box::new(review));
            let binding = context.binding();
            let id = binding.request_id();
            let digest = "a".repeat(64);
            let store = EventStore::open(":memory:").unwrap();
            store
                .reserve_request(&id, &digest, &binding.agent_id().to_string())
                .unwrap();
            store
                .mark_provider_unknown(
                    &id,
                    &digest,
                    "UnknownOutcome: provider_transport_deadline_elapsed",
                )
                .unwrap();
            assert!(
                sealed_unknown_model_evidence(&store, &binding, &id, &digest)
                    .unwrap()
                    .is_none()
            );
            let response: GatewayResponse = serde_json::from_value(serde_json::json!({
                "content": "{}", "decision": "forward", "request_id": id,
                "provider": binding.provider(), "effective_model": "gpt-5.4", "tokens_used": 15,
                "input_tokens": 5, "output_tokens": 10, "tier": "mid", "hierarchy_tier": 2,
                "cost_source": "provider_reported", "cost_usd": 0.125,
            }))
            .unwrap();
            let (tx, rx) = mpsc::channel();
            assert!(store_gateway_completion(
                &store,
                &tx,
                GatewayCompletionContext {
                    request_id: &id,
                    request_digest: &digest,
                    agent_id: binding.agent_id(),
                    tick: 1,
                    requested_model: "gpt-5.4",
                    authority: Some(&binding),
                    authority_resolver: None,
                    gateway_response: &response,
                    usage_v2_enabled: true,
                    model_work: Some(&context),
                },
                3
            )
            .is_err());
            assert!(store.get_all_events().unwrap().is_empty());
            assert!(rx.try_recv().is_err());
        }

        #[test]
        fn leadership_raw_response_digest_fences_recovery_and_accounts_oversized_results() {
            let dir = tempfile::tempdir().unwrap();
            let (_api, review) = crate::workflow_api::adaptive_leadership_review::tests::fixture(
                &dir.path().join("company.sqlite"),
                &dir.path().join("events.sqlite"),
            );
            let context = ModelWorkContext::AdaptiveLeadershipReview(Box::new(review));
            let binding = context.binding();
            let id = binding.request_id();
            let empty_digest = format!("{:x}", Sha256::digest(b""));
            let mut oversized_digests = Vec::new();
            for content in [
                "{}".to_owned(),
                "x".repeat(MAX_MODEL_WORK_BYTES + 1),
                "y".repeat(MAX_MODEL_WORK_BYTES + 1),
            ] {
                let oversized = content.len() > MAX_MODEL_WORK_BYTES;
                let raw_digest = format!("{:x}", Sha256::digest(content.as_bytes()));
                let resolver = ModelWorkResolver {
                    context: context.clone(),
                    admissions: Mutex::new(Vec::new()),
                    fail_next: AtomicBool::new(!oversized),
                };
                let store = EventStore::open(":memory:").unwrap();
                assert!(store
                    .reserve_request(&id, "digest", &binding.agent_id().to_string())
                    .unwrap());
                let response: GatewayResponse = serde_json::from_value(serde_json::json!({
                    "content": content, "decision": "forward", "request_id": id,
                    "provider": binding.provider(), "effective_model": "gpt-5.4", "tokens_used": 15,
                    "input_tokens": 5, "output_tokens": 10, "tier": "mid", "hierarchy_tier": 2,
                    "cost_source": "provider_reported", "cost_usd": 0.0
                }))
                .unwrap();
                let (tx, rx) = mpsc::channel();
                store_gateway_completion(
                    &store,
                    &tx,
                    GatewayCompletionContext {
                        request_id: &id,
                        request_digest: "digest",
                        agent_id: binding.agent_id(),
                        tick: 1,
                        requested_model: "gpt-5.4",
                        authority: Some(&binding),
                        authority_resolver: Some(&resolver),
                        gateway_response: &response,
                        usage_v2_enabled: true,
                        model_work: Some(&context),
                    },
                    if oversized { 1 } else { 3 },
                )
                .unwrap();
                let entry = store.get_completion(&id).unwrap().unwrap();
                assert_eq!(
                    entry.status,
                    if oversized {
                        "failed"
                    } else {
                        "ready_for_action"
                    }
                );
                let usage = store
                    .event_by_operation_id(&format!("llm_usage_{id}"))
                    .unwrap()
                    .unwrap();
                let mut completed: CompletedLlmResponse =
                    serde_json::from_str(&entry.payload).unwrap();
                assert_eq!(usage.schema_version, 6);
                assert_eq!(usage.payload, completed.usage_event.payload);
                assert_eq!(usage.operation_id, completed.usage_event.operation_id);
                assert_eq!(
                    completed.model_response_digest.as_deref(),
                    Some(raw_digest.as_str())
                );
                assert_eq!(
                    completed.model_work.as_ref().unwrap().admissible,
                    !oversized
                );
                if oversized {
                    assert!(completed.model_work.as_ref().unwrap().content.is_empty());
                    assert_ne!(raw_digest, empty_digest);
                    let mut falsely_admissible = completed.model_work.as_ref().unwrap().clone();
                    falsely_admissible.admissible = true;
                    assert!(!leadership_response_digest_matches(
                        &falsely_admissible,
                        Some(&raw_digest)
                    ));
                    assert_eq!(
                        entry.last_error.as_deref(),
                        Some("provider response is not admissible")
                    );
                    assert!(!store
                        .reserve_request(&id, "digest", &binding.agent_id().to_string())
                        .unwrap());
                    // Recover the durable payload from pending_usage without any
                    // provider I/O, and retain exact accounting despite rejection.
                    let recovered = EventStore::open(":memory:").unwrap();
                    assert!(recovered
                        .reserve_request(&id, "digest", &binding.agent_id().to_string())
                        .unwrap());
                    recovered
                        .enqueue_completion(&id, "digest", &entry.payload)
                        .unwrap();
                    let pending = recovered.get_completion(&id).unwrap().unwrap();
                    assert_eq!(pending.status, "pending_usage");
                    recover_completion(&recovered, pending, &tx, 1, Some(&resolver));
                    let recovered_usage = recovered
                        .event_by_operation_id(&format!("llm_usage_{id}"))
                        .unwrap()
                        .unwrap();
                    assert_eq!(recovered_usage.payload, completed.usage_event.payload);
                    assert_eq!(
                        recovered_usage.schema_version,
                        completed.usage_event.schema_version
                    );
                    let failed = recovered.get_completion(&id).unwrap().unwrap();
                    assert_eq!(failed.status, "failed");
                    assert_eq!(
                        failed.last_error.as_deref(),
                        Some("provider response is not admissible")
                    );
                    recover_completion(&recovered, failed, &tx, 1, Some(&resolver));
                    assert!(!recovered
                        .reserve_request(&id, "digest", &binding.agent_id().to_string())
                        .unwrap());
                    for invalid_digest in [
                        None,
                        Some(String::new()),
                        Some("a".repeat(63)),
                        Some("g".repeat(64)),
                    ] {
                        let mut invalid: CompletedLlmResponse =
                            serde_json::from_str(&entry.payload).unwrap();
                        invalid.model_response_digest = invalid_digest;
                        let rejected = EventStore::open(":memory:").unwrap();
                        assert!(rejected
                            .reserve_request(&id, "digest", &binding.agent_id().to_string())
                            .unwrap());
                        rejected
                            .enqueue_completion(
                                &id,
                                "digest",
                                &serde_json::to_string(&invalid).unwrap(),
                            )
                            .unwrap();
                        recover_completion(
                            &rejected,
                            rejected.get_completion(&id).unwrap().unwrap(),
                            &tx,
                            1,
                            Some(&resolver),
                        );
                        assert_eq!(
                            rejected
                                .get_completion(&id)
                                .unwrap()
                                .unwrap()
                                .last_error
                                .as_deref(),
                            Some("completion identity mismatch")
                        );
                        assert!(!rejected.has_operation(&format!("llm_usage_{id}")).unwrap());
                    }
                    oversized_digests.push(raw_digest);
                }
                completed
                    .model_work
                    .as_mut()
                    .unwrap()
                    .content
                    .push_str("tampered");
                let corrupted = EventStore::open(":memory:").unwrap();
                assert!(corrupted
                    .reserve_request(&id, "digest", &binding.agent_id().to_string())
                    .unwrap());
                corrupted
                    .enqueue_completion(&id, "digest", &serde_json::to_string(&completed).unwrap())
                    .unwrap();
                recover_completion(
                    &corrupted,
                    corrupted.get_completion(&id).unwrap().unwrap(),
                    &tx,
                    1,
                    Some(&resolver),
                );
                assert_eq!(
                    corrupted.get_completion(&id).unwrap().unwrap().status,
                    "failed"
                );
                assert!(!corrupted.has_operation(&format!("llm_usage_{id}")).unwrap());
                assert!(resolver.admissions.lock().unwrap().is_empty());
                assert!(rx.try_recv().is_err());
            }
            assert_eq!(oversized_digests.len(), 2);
            assert_ne!(oversized_digests[0], oversized_digests[1]);
        }

        #[test]
        fn model_work_request_binds_perception_snapshot_and_authority() {
            let context: ModelWorkContext = crate::workflow_api::model_work::test_context().into();
            let dir = tempfile::tempdir().unwrap();
            let state = StateStore::open(dir.path().join("state.redb").to_str().unwrap()).unwrap();
            let first = make_perception(6, "First conversation", true);
            let id = agent_runtime_request_id(&first, Some(&context.binding()));
            let mut a = build_gateway_request(&first, &state, &id, Some(&context.binding()));
            let mut same = build_gateway_request(&first, &state, &id, Some(&context.binding()));
            a.metadata.insert(
                "company_execution_fresh_observation_required".to_owned(),
                "true".to_owned(),
            );
            a.metadata.insert(
                "leadership_evidence_refs".to_owned(),
                "[\"foreign\"]".to_owned(),
            );
            bind_model_work_request(&mut a, &context).unwrap();
            bind_model_work_request(&mut same, &context).unwrap();
            assert!(!a
                .metadata
                .contains_key("company_execution_fresh_observation_required"));
            assert!(!a.metadata.contains_key("leadership_evidence_refs"));
            assert_eq!(a.metadata["company_execution_output_kind"], "tool_plan");
            let mut qa_context = context.clone();
            let ModelWorkContext::Project(qa_work) = &mut qa_context else {
                panic!("project fixture");
            };
            qa_work.task.required_role = sentinel_workflow::CompanyRoleV1::Qa;
            qa_work.authority.profile_id = "web-review-v1".into();
            qa_work.authority.capabilities =
                std::collections::BTreeSet::from(["file.write".into(), "artifact.commit".into()]);
            qa_work.task.outputs[0].media_type = "application/vnd.sentinel.qa-report+json".into();
            let input = sentinel_workflow::WorkInputContractV1 {
                name: "source".into(),
                producer_work_item_id: sentinel_workflow::WorkItemId::parse("source-work").unwrap(),
                producer_output_name: "site".into(),
                expected_contract_generation: 1,
                expected_contract_digest: "a".repeat(64),
            };
            qa_work
                .task
                .dependency_ids
                .insert(input.producer_work_item_id.clone());
            qa_work.task.inputs.push(input.clone());
            let content = "let timer = null;\n".to_owned();
            qa_work
                .artifact_inputs
                .push(crate::workflow_api::model_work::ModelArtifactInput {
                    contract: input,
                    producer_agent: AgentId(3),
                    manifest_digest: "b".repeat(64),
                    artifact_kind: "source_tree".into(),
                    media_type: "application/vnd.sentinel.source-tree".into(),
                    files: vec![crate::workbench::VerifiedArtifactTextFile {
                        path: "app.js".into(),
                        sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
                        content,
                    }],
                });
            qa_work.schema_retry_feedback =
                Some("Return the complete report as strict JSON.".into());
            let mut qa_request =
                build_gateway_request(&first, &state, &id, Some(&qa_context.binding()));
            bind_model_work_request(&mut qa_request, &qa_context).unwrap();
            assert_eq!(qa_request.metadata["company_execution_schema"], "1");
            assert_eq!(
                qa_request.metadata["company_execution_output_kind"],
                "source_review"
            );
            assert_ne!(
                gateway_request_digest(&qa_request).unwrap(),
                gateway_request_digest(&a).unwrap()
            );
            assert_eq!(
                gateway_request_digest(&a).unwrap(),
                gateway_request_digest(&same).unwrap()
            );
            assert!(a.metadata.contains_key("agent_perception_snapshot"));
            assert!(a.messages[0].content.contains("First conversation"));
            let mut changed_perception = first.clone();
            changed_perception.tick = Tick(999);
            changed_perception.heard_text = "Different conversation".to_owned();
            changed_perception.body_text = "Different body state".to_owned();
            let mut changed =
                build_gateway_request(&changed_perception, &state, &id, Some(&context.binding()));
            bind_model_work_request(&mut changed, &context).unwrap();
            assert_ne!(
                gateway_request_digest(&a).unwrap(),
                gateway_request_digest(&changed).unwrap()
            );
            let mut changed_authority = context.clone();
            let ModelWorkContext::Project(work) = &mut changed_authority else {
                panic!("project fixture");
            };
            work.authority.principal.principal_generation += 1;
            bind_model_work_request(&mut same, &changed_authority).unwrap();
            assert_ne!(
                gateway_request_digest(&a).unwrap(),
                gateway_request_digest(&same).unwrap()
            );
            same.metadata
                .insert("project_id".to_owned(), "foreign".to_owned());
            assert!(bind_model_work_request(&mut same, &context).is_err());
        }

        #[test]
        fn reserved_model_effect_ignores_later_perception_and_survives_reopen() {
            let context: ModelWorkContext = crate::workflow_api::model_work::test_context().into();
            let authority = context.binding();
            let request_id = authority.request_id();
            let dir = tempfile::tempdir().unwrap();
            let state = StateStore::open(dir.path().join("state.redb").to_str().unwrap()).unwrap();
            let path = dir.path().join("events.db");
            let first = make_perception(6, "Original conversation", true);
            let mut request = build_gateway_request(&first, &state, &request_id, Some(&authority));
            bind_model_work_request(&mut request, &context).unwrap();
            let original_digest = gateway_request_digest(&request).unwrap();
            let store = EventStore::open(path.to_str().unwrap()).unwrap();
            store
                .reserve_request(&request_id, &original_digest, "AGENT-06")
                .unwrap();
            let original = store.get_completion(&request_id).unwrap().unwrap();
            drop(store);
            let store = EventStore::open(path.to_str().unwrap()).unwrap();
            let mut later = first;
            later.tick = Tick(999);
            later.heard_text = "Later conversation".into();
            let mut changed = build_gateway_request(&later, &state, &request_id, Some(&authority));
            bind_model_work_request(&mut changed, &context).unwrap();
            assert_ne!(original_digest, gateway_request_digest(&changed).unwrap());
            let (tx, rx) = mpsc::channel();
            assert!(recover_reserved_model_request(
                &store,
                None,
                &authority,
                &tx,
                3,
                u64::MAX,
                Duration::ZERO,
            )
            .unwrap());
            assert_eq!(
                store.get_completion(&request_id).unwrap().unwrap(),
                original
            );
            assert!(rx.try_recv().is_err());
            assert!(!store
                .reserve_request(&request_id, &original_digest, "AGENT-06")
                .unwrap());
        }

        #[test]
        fn reserved_model_recovery_rejects_another_owner_before_any_effect() {
            let context: ModelWorkContext = crate::workflow_api::model_work::test_context().into();
            let authority = context.binding();
            let request_id = authority.request_id();
            let store = EventStore::open(":memory:").unwrap();
            let (tx, rx) = mpsc::channel();
            assert!(!recover_reserved_model_request(
                &store,
                None,
                &authority,
                &tx,
                3,
                u64::MAX,
                Duration::ZERO,
            )
            .unwrap());
            store
                .reserve_request(&request_id, &"a".repeat(64), "AGENT-03")
                .unwrap();
            let original = store.get_completion(&request_id).unwrap().unwrap();
            assert!(recover_reserved_model_request(
                &store,
                None,
                &authority,
                &tx,
                3,
                u64::MAX,
                Duration::ZERO,
            )
            .is_err());
            assert_eq!(
                store.get_completion(&request_id).unwrap().unwrap(),
                original
            );
            assert!(rx.try_recv().is_err());
        }

        #[test]
        fn sales_request_uses_the_normal_gateway_and_durable_question_adoption_path() {
            let dir = tempfile::tempdir().unwrap();
            let (api, sales) = crate::workflow_api::model_execution::tests::fixture(
                &dir.path().join("company.sqlite"),
            );
            let context = ModelWorkContext::RequestSales(Box::new(sales.clone()));
            let binding = context.binding();
            let state = StateStore::open(dir.path().join("state.redb").to_str().unwrap()).unwrap();
            let perception = make_perception(3, "Customer inquiry", true);
            let id = agent_runtime_request_id(&perception, Some(&binding));
            let mut request = build_gateway_request(&perception, &state, &id, Some(&binding));
            bind_model_work_request(&mut request, &context).unwrap();
            assert_eq!(request.metadata["company_execution_schema"], "2");
            assert_eq!(
                request.metadata["customer_request_id"],
                sales.source_request.request_id
            );
            assert!(!request.metadata.contains_key("project_id"));
            assert!(request.messages[0]
                .content
                .contains("Three accessible pages"));
            let mut mixed = build_gateway_request(&perception, &state, &id, Some(&binding));
            mixed
                .metadata
                .insert("project_id".to_owned(), "foreign".to_owned());
            assert!(bind_model_work_request(&mut mixed, &context).is_err());
            let digest = gateway_request_digest(&request).unwrap();
            let store =
                EventStore::open(dir.path().join("company.events.sqlite").to_str().unwrap())
                    .unwrap();
            store
                .reserve_request(&id, &digest, &AgentId(3).to_string())
                .unwrap();
            let dispatch = serde_json::json!({"schema_version":2,"allowance_id":sales.binding.allowance_id,
                "agent_id":3,"request_id":id,"request_digest":digest,
                "context_digest":request.metadata["company_execution_context_digest"],
                "provider":"codex-cli","model":"model-test","catalog_digest":"a".repeat(64),
                "subject":{"kind":"customer_request","request_id":sales.source_request.request_id,"request_version":1}});
            assert_eq!(
                api.subscription_dispatch(&serde_json::to_vec(&dispatch).unwrap())
                    .status,
                200
            );
            // Fixture response, not live provider evidence. The production bridge,
            // EventStore recovery and workflow adapter perform the actual adoption.
            let response: GatewayResponse = serde_json::from_value(serde_json::json!({
                "content":r#"{"schema_version":1,"kind":"ask_question","content":"Which pages do you need?"}"#,
                "decision":"forward","request_id":id,"provider":"codex-cli","tokens_used":15,
                "input_tokens":5,"output_tokens":10,"hierarchy_tier":2,"tier":"mid",
                "cost_source":"provider_reported","effective_model":"model-test"
            })).unwrap();
            let (tx, rx) = mpsc::channel();
            store_gateway_completion(
                &store,
                &tx,
                GatewayCompletionContext {
                    request_id: &id,
                    request_digest: &digest,
                    agent_id: AgentId(3),
                    tick: 1,
                    requested_model: "model-test",
                    authority: Some(&binding),
                    authority_resolver: Some(&api),
                    gateway_response: &response,
                    usage_v2_enabled: true,
                    model_work: Some(&context),
                },
                3,
            )
            .unwrap();
            assert!(store.get_completion(&id).unwrap().is_none());
            assert!(!store
                .reserve_request(&id, &digest, &AgentId(3).to_string())
                .unwrap());
            assert!(rx.try_recv().is_err());
            let customer = api
                .request_sales_call()
                .unwrap()
                .unwrap()
                .question_response
                .unwrap();
            assert_eq!(customer.consultation.len(), 1);
            assert!(api.resolve_provider_usage_authority(AgentId(3)).is_err());
        }

        #[test]
        fn adaptive_gateway_result_is_adopted_once_and_invalid_json_stays_recoverable() {
            fn run_case(
                root: &std::path::Path,
                content: &str,
            ) -> (
                crate::workflow_api::WorkflowApi,
                EventStore,
                ProviderExecutionAuthority,
                String,
                ModelWorkContext,
                String,
            ) {
                let (api, authority, store) =
                    crate::workflow_api::model_work::configured_adaptive_test_api(
                        &root.join("company.sqlite"),
                        &root.join("events.sqlite"),
                    );
                let binding = ProviderExecutionAuthority::Adaptive(Box::new(authority.clone()));
                let context = api.model_work_context(&binding).unwrap().unwrap();
                let state = StateStore::open(root.join("state.redb").to_str().unwrap()).unwrap();
                let perception = make_perception(6, "Continue assigned work", true);
                let id = agent_runtime_request_id(&perception, Some(&binding));
                let mut request = build_gateway_request(&perception, &state, &id, Some(&binding));
                bind_model_work_request(&mut request, &context).unwrap();
                assert_eq!(request.metadata["company_execution_schema"], "3");
                assert_eq!(
                    request.metadata["company_execution_output_kind"],
                    "adaptive_decision"
                );
                assert_eq!(
                    request.metadata["adaptive_session_id"],
                    authority.grant.session_id.to_string()
                );
                assert_eq!(
                    request.metadata["adaptive_effect_id"],
                    authority.effect_id.to_string()
                );
                let digest = gateway_request_digest(&request).unwrap();
                store
                    .reserve_request(&id, &digest, &AgentId(6).to_string())
                    .unwrap();
                let dispatch = serde_json::json!({
                    "schema_version": 3,
                    "allowance_id": authority.grant.provider_allowance_id,
                    "agent_id": 6,
                    "request_id": id,
                    "request_digest": digest,
                    "context_digest": request.metadata["company_execution_context_digest"],
                    "provider": authority.grant.provider,
                    "model": authority.grant.model,
                    "catalog_digest": authority.grant.catalog_digest,
                    "subject": {
                        "kind": "adaptive_session",
                        "session_id": authority.grant.session_id,
                        "effect_id": authority.effect_id,
                        "session_version": authority.session_version,
                    },
                });
                assert_eq!(
                    api.subscription_dispatch(&serde_json::to_vec(&dispatch).unwrap())
                        .status,
                    200
                );
                let response: GatewayResponse = serde_json::from_value(serde_json::json!({
                    "content": content,
                    "decision": "forward",
                    "request_id": id,
                    "provider": "codex-cli",
                    "tokens_used": 15,
                    "input_tokens": 5,
                    "output_tokens": 10,
                    "hierarchy_tier": 2,
                    "tier": "mid",
                    "cost_source": "provider_reported",
                    "cost_usd": 0.0,
                    "effective_model": "gpt-5.4"
                }))
                .unwrap();
                let (tx, rx) = mpsc::channel();
                store_gateway_completion(
                    &store,
                    &tx,
                    GatewayCompletionContext {
                        request_id: &id,
                        request_digest: &digest,
                        agent_id: AgentId(6),
                        tick: 1,
                        requested_model: "gpt-5.4",
                        authority: Some(&binding),
                        authority_resolver: Some(&api),
                        gateway_response: &response,
                        usage_v2_enabled: true,
                        model_work: Some(&context),
                    },
                    3,
                )
                .unwrap();
                assert!(rx.try_recv().is_err());
                (api, store, binding, id, context, digest)
            }

            let accepted = tempfile::tempdir().unwrap();
            let (api, store, _, id, context, digest) = run_case(
                accepted.path(),
                r#"{"schema_version":1,"decision":{"kind":"blocked","reason_code":"dependency_unavailable"}}"#,
            );
            assert!(store.get_completion(&id).unwrap().is_none());
            let usage = store
                .event_by_operation_id(&format!("llm_usage_{id}"))
                .unwrap()
                .unwrap();
            assert_eq!(usage.schema_version, 3);
            assert!(usage.payload.contains("\"project_id\""));
            assert_eq!(
                api.resolve_provider_usage_authority(AgentId(6)).unwrap(),
                None,
                "a terminal adaptive campaign cannot fall back to legacy execution"
            );
            assert!(api.resolve_provider_usage_authority(AgentId(7)).is_err());
            assert!(!api.allows_unbound_provider_usage());
            assert!(!api.is_provider_usage_candidate(AgentId(6)).unwrap());
            assert!(validate_provider_usage_mode(None, true, false).is_err());
            assert!(
                validate_current_provider_usage_authority(Some(&api), None, AgentId(6)).is_err()
            );
            let mut replay = ModelWorkCompletion {
                context,
                content: r#"{"schema_version":1,"decision":{"kind":"blocked","reason_code":"dependency_unavailable"}}"#.to_owned(),
                admissible: true,
            };
            api.admit_model_work(&replay, &id, &digest).unwrap();
            assert!(store.get_completion(&id).unwrap().is_none());
            replay.content = replay
                .content
                .replace("dependency_unavailable", "changed_reason");
            assert!(api.admit_model_work(&replay, &id, &digest).is_err());

            let rejected = tempfile::tempdir().unwrap();
            let (api, store, binding, id, _, _) =
                run_case(rejected.path(), r#"{"not":"a decision"}"#);
            assert!(store.get_completion(&id).unwrap().is_some());
            assert_eq!(
                api.resolve_provider_usage_authority(AgentId(6))
                    .unwrap()
                    .unwrap(),
                binding,
                "invalid provider output must leave the exact effect recoverable"
            );
        }

        #[test]
        fn model_work_outbox_retries_after_restart_without_provider_or_legacy_action() {
            let context: ModelWorkContext = crate::workflow_api::model_work::test_context().into();
            let resolver = ModelWorkResolver {
                context: context.clone(),
                admissions: Mutex::new(Vec::new()),
                fail_next: AtomicBool::new(true),
            };
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("events.db");
            let store = EventStore::open(path.to_str().unwrap()).unwrap();
            let request_id = "company-provider-reservation-m0";
            let digest = "a".repeat(64);
            let response: GatewayResponse = serde_json::from_value(serde_json::json!({
                "content": "{\"schema_version\":1,\"tools\":[]}", "decision": "forward",
                "actions": [{"type":"tool_use","content":"must not execute"}],
                "request_id":request_id, "provider":"codex-cli", "tokens_used":30,
                "input_tokens":10,"output_tokens":20,"hierarchy_tier":2,"tier":"mid",
                "cost_source":"provider_reported","effective_model":"test-model"
            }))
            .unwrap();
            assert!(store
                .reserve_request(
                    request_id,
                    &digest,
                    &context.binding().agent_id().to_string()
                )
                .unwrap());
            let (tx, rx) = mpsc::channel();
            // One completed provider response enters the production outbox. Only
            // local admission is retried after reopening the durable EventStore.
            store_gateway_completion(
                &store,
                &tx,
                GatewayCompletionContext {
                    request_id,
                    request_digest: &digest,
                    agent_id: context.binding().agent_id(),
                    tick: 1,
                    requested_model: "",
                    authority: Some(&context.binding()),
                    authority_resolver: Some(&resolver),
                    gateway_response: &response,
                    usage_v2_enabled: true,
                    model_work: Some(&context),
                },
                3,
            )
            .unwrap();
            let pending = store.get_completion(request_id).unwrap().unwrap();
            assert_eq!(pending.status, "ready_for_action");
            assert_eq!(pending.attempt_count, 1);
            assert!(resolver.admissions.lock().unwrap().is_empty());
            assert!(rx.try_recv().is_err());
            drop(store);
            let restored = EventStore::open(path.to_str().unwrap()).unwrap();
            assert!(recover_reserved_model_request(
                &restored,
                Some(&resolver),
                &context.binding(),
                &tx,
                3,
                u64::MAX,
                Duration::ZERO,
            )
            .unwrap());
            assert_eq!(
                *resolver.admissions.lock().unwrap(),
                vec![request_id.to_owned()]
            );
            assert!(restored.get_completion(request_id).unwrap().is_none());
            assert!(!restored
                .reserve_request(
                    request_id,
                    &digest,
                    &context.binding().agent_id().to_string()
                )
                .unwrap());
            recover_completion_batch(&restored, &tx, 3, Some(&resolver));
            assert_eq!(resolver.admissions.lock().unwrap().len(), 1);
            assert!(rx.try_recv().is_err());
        }

        #[test]
        fn model_work_failed_completion_cannot_reenter_admission() {
            let context: ModelWorkContext = crate::workflow_api::model_work::test_context().into();
            let resolver = ModelWorkResolver {
                context: context.clone(),
                admissions: Mutex::new(Vec::new()),
                fail_next: AtomicBool::new(false),
            };
            let store = EventStore::open(":memory:").unwrap();
            let id = "company-provider-reservation-m0";
            let digest = "a".repeat(64);
            store.reserve_request(id, &digest, "AGENT-06").unwrap();
            store.enqueue_completion(id, &digest, "{}").unwrap();
            store.record_failure(id, &digest, "terminal", 1).unwrap();
            let failed = store.get_completion(id).unwrap().unwrap();
            assert_eq!(failed.status, "failed");
            let (tx, _rx) = mpsc::channel();
            recover_completion(&store, failed, &tx, 3, Some(&resolver));
            assert!(resolver.admissions.lock().unwrap().is_empty());
        }

        #[test]
        fn model_work_rejected_or_revoked_response_retains_usage_without_execution() {
            for revoked in [false, true] {
                let context: ModelWorkContext =
                    crate::workflow_api::model_work::test_context().into();
                let mut current = context.clone();
                if revoked {
                    let ModelWorkContext::Project(work) = &mut current else {
                        panic!("project fixture");
                    };
                    work.authority.principal.principal_generation += 1;
                }
                let resolver = ModelWorkResolver {
                    context: current,
                    admissions: Mutex::new(Vec::new()),
                    fail_next: AtomicBool::new(false),
                };
                let store = EventStore::open(":memory:").unwrap();
                let request_id = "company-provider-reservation-m0";
                let digest = "a".repeat(64);
                let response: GatewayResponse = serde_json::from_value(serde_json::json!({
                    "content":"{}", "decision":if revoked { "forward" } else { "dropped" },
                    "request_id":request_id, "provider":"codex-cli", "tokens_used":30,
                    "input_tokens":10,"output_tokens":20,"hierarchy_tier":2,"tier":"mid",
                    "cost_source":"provider_reported","effective_model":"test-model"
                }))
                .unwrap();
                store
                    .reserve_request(
                        request_id,
                        &digest,
                        &context.binding().agent_id().to_string(),
                    )
                    .unwrap();
                let (tx, rx) = mpsc::channel();
                store_gateway_completion(
                    &store,
                    &tx,
                    GatewayCompletionContext {
                        request_id,
                        request_digest: &digest,
                        agent_id: context.binding().agent_id(),
                        tick: 1,
                        requested_model: "",
                        authority: Some(&context.binding()),
                        authority_resolver: Some(&resolver),
                        gateway_response: &response,
                        usage_v2_enabled: true,
                        model_work: Some(&context),
                    },
                    1,
                )
                .unwrap();
                assert_eq!(
                    store.get_completion(request_id).unwrap().unwrap().status,
                    "failed"
                );
                assert!(store
                    .has_operation(&format!("llm_usage_{request_id}"))
                    .unwrap());
                assert!(resolver.admissions.lock().unwrap().is_empty());
                assert!(rx.try_recv().is_err());
                assert!(!store
                    .reserve_request(
                        request_id,
                        &digest,
                        &context.binding().agent_id().to_string()
                    )
                    .unwrap());
            }
        }

        #[test]
        fn agent_runtime_request_uses_dedicated_path_and_bearer_credential() {
            let request = GatewayRequest {
                messages: vec![],
                temperature: 0.1,
                max_tokens: 10,
                model: String::new(),
                metadata: BTreeMap::new(),
            };
            let built = agent_runtime_request(
                &reqwest::Client::new(),
                "http://127.0.0.1:8080/internal/agent-runtime",
                "agent-runtime-test-credential",
                "agent-runtime-07-55",
                "digest-07-55",
                &request,
            )
            .build()
            .unwrap();
            assert_eq!(built.url().path(), "/internal/agent-runtime");
            assert_eq!(
                built.headers().get(reqwest::header::AUTHORIZATION).unwrap(),
                "Bearer agent-runtime-test-credential"
            );
            assert_eq!(
                built.headers().get("X-Request-ID").unwrap(),
                "agent-runtime-07-55"
            );
            assert_eq!(
                built.headers().get("X-Request-Digest").unwrap(),
                "digest-07-55"
            );
        }

        struct FailFirstCompletionStore {
            inner: EventStore,
            reservation_calls: std::sync::atomic::AtomicUsize,
            fail_next_usage: AtomicBool,
            append_calls: Arc<std::sync::atomic::AtomicUsize>,
            failure_observed: Arc<tokio::sync::Notify>,
        }

        struct StaticProviderUsageAuthority {
            authority: ProviderUsageAuthority,
        }

        struct ExactValidationResolver {
            context: ModelWorkContext,
            scheduled: Option<ProviderExecutionAuthority>,
            result: Result<bool, &'static str>,
            unbound_allowed: bool,
            selection_calls: std::sync::atomic::AtomicUsize,
            validation_calls: std::sync::atomic::AtomicUsize,
            context_calls: std::sync::atomic::AtomicUsize,
            selected: tokio::sync::Notify,
            checked: tokio::sync::Notify,
        }

        impl ExactValidationResolver {
            fn new(context: ModelWorkContext) -> Self {
                Self {
                    scheduled: Some(context.binding()),
                    context,
                    result: Ok(true),
                    unbound_allowed: true,
                    selection_calls: std::sync::atomic::AtomicUsize::new(0),
                    validation_calls: std::sync::atomic::AtomicUsize::new(0),
                    context_calls: std::sync::atomic::AtomicUsize::new(0),
                    selected: tokio::sync::Notify::new(),
                    checked: tokio::sync::Notify::new(),
                }
            }
        }

        impl ProviderUsageAuthorityResolver for ExactValidationResolver {
            fn allows_unbound_provider_usage(&self) -> bool {
                self.unbound_allowed
            }

            fn resolve_provider_usage_authority(
                &self,
                agent_id: AgentId,
            ) -> Result<Option<ProviderExecutionAuthority>, &'static str> {
                self.selection_calls.fetch_add(1, Ordering::SeqCst);
                self.selected.notify_one();
                Ok(if agent_id == self.context.binding().agent_id() {
                    self.scheduled.clone()
                } else {
                    None
                })
            }

            fn validate_provider_usage_authority(
                &self,
                expected: &ProviderExecutionAuthority,
            ) -> Result<bool, &'static str> {
                self.validation_calls.fetch_add(1, Ordering::SeqCst);
                self.checked.notify_one();
                if expected != &self.context.binding() {
                    return Ok(false);
                }
                self.result
            }

            fn model_work_context(
                &self,
                expected: &ProviderExecutionAuthority,
            ) -> Result<Option<ModelWorkContext>, &'static str> {
                self.context_calls.fetch_add(1, Ordering::SeqCst);
                Ok((expected == &self.context.binding()).then(|| self.context.clone()))
            }
        }

        struct PreparedGateResolver {
            legacy: ExactValidationResolver,
            result: Mutex<Result<Option<()>, String>>,
            calls: std::sync::atomic::AtomicUsize,
        }

        impl ProviderUsageAuthorityResolver for PreparedGateResolver {
            fn resolve_provider_usage_authority(
                &self,
                agent: AgentId,
            ) -> Result<Option<ProviderExecutionAuthority>, &'static str> {
                self.legacy.resolve_provider_usage_authority(agent)
            }

            fn validate_provider_usage_authority(
                &self,
                expected: &ProviderExecutionAuthority,
            ) -> Result<bool, &'static str> {
                self.legacy.validate_provider_usage_authority(expected)
            }

            fn model_work_context(
                &self,
                expected: &ProviderExecutionAuthority,
            ) -> Result<Option<ModelWorkContext>, &'static str> {
                self.legacy.model_work_context(expected)
            }

            fn validate_prepared_provider_model_context(
                &self,
                _: &ProviderExecutionAuthority,
                _: &ModelWorkContext,
            ) -> Result<Option<()>, String> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.result.lock().unwrap().clone()
            }
        }

        #[test]
        fn prepared_context_hook_is_fresh_per_gate_and_releases_after_authority_change() {
            let dir = tempfile::tempdir().unwrap();
            let (_, review) = crate::workflow_api::adaptive_leadership_review::tests::fixture(
                &dir.path().join("company.sqlite"),
                &dir.path().join("events.sqlite"),
            );
            let context = ModelWorkContext::AdaptiveLeadershipReview(Box::new(review));
            let expected = context.binding();
            for handled in [Some(()), None] {
                let resolver = PreparedGateResolver {
                    legacy: ExactValidationResolver::new(context.clone()),
                    result: Mutex::new(Ok(handled)),
                    calls: std::sync::atomic::AtomicUsize::new(0),
                };
                validate_unreserved_provider_authority(
                    Some(&resolver),
                    Some(&expected),
                    expected.agent_id(),
                    Some(&context),
                )
                .unwrap();
                assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
                assert_eq!(
                    resolver.legacy.validation_calls.load(Ordering::SeqCst),
                    usize::from(handled.is_none())
                );
                assert_eq!(
                    resolver.legacy.context_calls.load(Ordering::SeqCst),
                    usize::from(handled.is_none())
                );
                let store = EventStore::open(":memory:").unwrap();
                let id = expected.request_id();
                let digest = "a".repeat(64);
                assert!(store
                    .reserve_request(&id, &digest, &expected.agent_id().to_string())
                    .unwrap());
                validate_pre_dispatch_provider_authority(
                    &store,
                    Some(&resolver),
                    Some(&expected),
                    expected.agent_id(),
                    &id,
                    &digest,
                    Some(&context),
                )
                .unwrap();
                assert_eq!(resolver.calls.load(Ordering::SeqCst), 2);
                assert!(store.get_completion(&id).unwrap().is_some());
                *resolver.result.lock().unwrap() = Err("fresh authority changed".into());
                assert_eq!(
                    validate_pre_dispatch_provider_authority(
                        &store,
                        Some(&resolver),
                        Some(&expected),
                        expected.agent_id(),
                        &id,
                        &digest,
                        Some(&context),
                    ),
                    Err("fresh authority changed".into())
                );
                assert_eq!(resolver.calls.load(Ordering::SeqCst), 3);
                assert!(store.get_completion(&id).unwrap().is_none());
                assert_eq!(
                    validate_unreserved_provider_authority(
                        Some(&resolver),
                        Some(&expected),
                        expected.agent_id(),
                        Some(&context),
                    ),
                    Err("fresh authority changed".into())
                );
                assert!(store.get_completion(&id).unwrap().is_none());
                assert_eq!(resolver.legacy.selection_calls.load(Ordering::SeqCst), 0);
            }
        }

        struct CandidateProbeResolver {
            candidate: Result<bool, &'static str>,
            authority: Result<Option<ProviderExecutionAuthority>, &'static str>,
            resolution_calls: std::sync::atomic::AtomicUsize,
        }

        impl ProviderUsageAuthorityResolver for CandidateProbeResolver {
            fn is_provider_usage_candidate(&self, _: AgentId) -> Result<bool, &'static str> {
                self.candidate
            }

            fn resolve_provider_usage_authority(
                &self,
                _: AgentId,
            ) -> Result<Option<ProviderExecutionAuthority>, &'static str> {
                self.resolution_calls.fetch_add(1, Ordering::SeqCst);
                self.authority.clone()
            }

            fn model_work_context(
                &self,
                _: &ProviderExecutionAuthority,
            ) -> Result<Option<ModelWorkContext>, &'static str> {
                panic!("legacy candidate preparation must defer context until recovery");
            }
        }

        #[test]
        fn candidate_preparation_default_preserves_skip_errors_and_unbound_authority() {
            let mut resolver = CandidateProbeResolver {
                candidate: Ok(false),
                authority: Err("resolution unavailable"),
                resolution_calls: std::sync::atomic::AtomicUsize::new(0),
            };
            assert!(resolver
                .prepare_provider_usage_candidate(AgentId(7))
                .unwrap()
                .is_none());
            assert_eq!(resolver.resolution_calls.load(Ordering::SeqCst), 0);
            resolver.candidate = Err("candidate unavailable");
            assert_eq!(
                resolver
                    .prepare_provider_usage_candidate(AgentId(7))
                    .unwrap_err(),
                "candidate unavailable"
            );
            assert_eq!(resolver.resolution_calls.load(Ordering::SeqCst), 0);
            resolver.candidate = Ok(true);
            assert_eq!(
                resolver
                    .prepare_provider_usage_candidate(AgentId(7))
                    .unwrap_err(),
                "resolution unavailable"
            );
            assert_eq!(resolver.resolution_calls.load(Ordering::SeqCst), 1);
            resolver.authority = Ok(None);
            let candidate = resolver
                .prepare_provider_usage_candidate(AgentId(7))
                .unwrap()
                .unwrap();
            assert!(candidate.authority.is_none());
            assert!(candidate.prepared_context.is_none());
            assert_eq!(resolver.resolution_calls.load(Ordering::SeqCst), 2);
        }

        #[test]
        fn prepared_context_is_reused_initially_but_rechecked_before_dispatch() {
            let context: ModelWorkContext = crate::workflow_api::model_work::test_context().into();
            let expected = context.binding();
            let mut resolver = ExactValidationResolver::new(context.clone());
            assert_eq!(
                initial_model_work_context(Some(&resolver), Some(&expected), Some(context.clone()))
                    .unwrap(),
                Some(context.clone())
            );
            assert_eq!(resolver.context_calls.load(Ordering::SeqCst), 0);
            validate_unreserved_provider_authority(
                Some(&resolver),
                Some(&expected),
                expected.agent_id(),
                Some(&context),
            )
            .unwrap();
            assert_eq!(resolver.context_calls.load(Ordering::SeqCst), 1);

            let ModelWorkContext::Project(current) = &mut resolver.context else {
                panic!("project fixture");
            };
            current.task.objective.push_str(" changed after queuing");
            assert!(validate_unreserved_provider_authority(
                Some(&resolver),
                Some(&expected),
                expected.agent_id(),
                Some(&context),
            )
            .is_err());
            let store = EventStore::open(":memory:").unwrap();
            let id = expected.request_id();
            let digest = "a".repeat(64);
            assert!(store
                .reserve_request(&id, &digest, &expected.agent_id().to_string())
                .unwrap());
            assert!(validate_pre_dispatch_provider_authority(
                &store,
                Some(&resolver),
                Some(&expected),
                expected.agent_id(),
                &id,
                &digest,
                Some(&context),
            )
            .is_err());
            assert!(store.get_completion(&id).unwrap().is_none());
            assert_eq!(resolver.context_calls.load(Ordering::SeqCst), 3);
            assert_eq!(resolver.validation_calls.load(Ordering::SeqCst), 3);
            assert_eq!(resolver.selection_calls.load(Ordering::SeqCst), 0);
            assert!(initial_model_work_context(None, None, Some(context)).is_err());
        }

        struct SelectionErrorResolver;

        impl ProviderUsageAuthorityResolver for SelectionErrorResolver {
            fn resolve_provider_usage_authority(
                &self,
                _: AgentId,
            ) -> Result<Option<ProviderExecutionAuthority>, &'static str> {
                Err("selection unavailable")
            }
        }

        #[test]
        fn exact_validation_does_not_reselect_another_leadership_review() {
            let dir = tempfile::tempdir().unwrap();
            let (_, review) = crate::workflow_api::adaptive_leadership_review::tests::fixture(
                &dir.path().join("company.sqlite"),
                &dir.path().join("events.sqlite"),
            );
            let context = ModelWorkContext::AdaptiveLeadershipReview(Box::new(review));
            let expected = context.binding();
            let mut resolver = ExactValidationResolver::new(context.clone());
            let mut another = expected.clone();
            let ProviderExecutionAuthority::AdaptiveLeadershipReview(binding) = &mut another else {
                panic!("leadership fixture");
            };
            binding.grant.review_id = uuid::Uuid::new_v4();
            binding.reservation_id = binding.grant.review_id.to_string();
            binding.grant.project_id =
                sentinel_workflow::ProjectId::parse("another-project").unwrap();
            resolver.scheduled = Some(another);
            assert_ne!(resolver.scheduled.as_ref(), Some(&expected));
            assert!(validate_unreserved_provider_authority(
                Some(&resolver),
                Some(&expected),
                expected.agent_id(),
                Some(&context),
            )
            .is_ok());
            let store = EventStore::open(":memory:").unwrap();
            let id = expected.request_id();
            let digest = "a".repeat(64);
            assert!(store
                .reserve_request(&id, &digest, &expected.agent_id().to_string())
                .unwrap());
            assert!(validate_pre_dispatch_provider_authority(
                &store,
                Some(&resolver),
                Some(&expected),
                expected.agent_id(),
                &id,
                &digest,
                Some(&context),
            )
            .is_ok());
            assert_eq!(
                store.get_completion(&id).unwrap().unwrap().request_digest,
                digest
            );
            let response: GatewayResponse = serde_json::from_value(serde_json::json!({
                "content": "review", "actions": [], "tokens_used": 7,
                "request_id": expected.request_id(), "provider": expected.provider(),
                "input_tokens": 5, "output_tokens": 2, "cache_read": 0, "cache_creation": 0,
                "tier": "mid", "hierarchy_tier": 2, "cost_usd": 0.0,
                "cost_source": "provider_reported", "effective_model": "mock/model"
            }))
            .unwrap();
            assert!(validate_gateway_completion_authority(
                Some(&resolver),
                Some(&expected),
                &response,
                expected.agent_id(),
            )
            .is_ok());
            assert_eq!(resolver.selection_calls.load(Ordering::SeqCst), 0);
            assert_eq!(resolver.validation_calls.load(Ordering::SeqCst), 3);
            for result in [Ok(false), Err("exact authority unavailable")] {
                resolver.result = result;
                assert!(validate_gateway_completion_authority(
                    Some(&resolver),
                    Some(&expected),
                    &response,
                    expected.agent_id(),
                )
                .is_err());
            }
            assert_eq!(resolver.selection_calls.load(Ordering::SeqCst), 0);
            assert_eq!(resolver.validation_calls.load(Ordering::SeqCst), 5);
        }

        #[test]
        fn unreserved_validation_rejects_false_error_and_wrong_agent_without_row_changes() {
            let context: ModelWorkContext = crate::workflow_api::model_work::test_context().into();
            let expected = context.binding();
            let store = Arc::new(EventStore::open(":memory:").unwrap());
            let active = Arc::new(Mutex::new(HashMap::new()));
            let original = reserved_guard(&store, &active, "existing");
            let pending = store.get_completion("existing").unwrap().unwrap();
            let foreign_id = expected.request_id();
            assert!(store
                .reserve_request(&foreign_id, "foreign-digest", "AGENT-42")
                .unwrap());
            let foreign = store.get_completion(&foreign_id).unwrap().unwrap();
            for result in [Ok(false), Err("exact authority unavailable")] {
                let mut resolver = ExactValidationResolver::new(context.clone());
                resolver.result = result;
                let error = validate_unreserved_provider_authority(
                    Some(&resolver),
                    Some(&expected),
                    expected.agent_id(),
                    Some(&context),
                )
                .unwrap_err();
                assert!(
                    error.contains(if result.is_err() {
                        "provider usage reauthorization failed: exact authority unavailable"
                    } else {
                        "provider usage authority changed"
                    }),
                    "{error}"
                );
                assert_eq!(store.get_completion("existing").unwrap().unwrap(), pending);
                assert_eq!(store.get_completion(&foreign_id).unwrap().unwrap(), foreign);
                assert_eq!(active.lock().unwrap().get("existing"), Some(&1));
            }
            let resolver = ExactValidationResolver::new(context);
            assert!(validate_current_provider_usage_authority(
                Some(&resolver),
                Some(&expected),
                AgentId(expected.agent_id().0 + 1),
            )
            .unwrap_err()
            .contains("another agent"));
            assert_eq!(resolver.validation_calls.load(Ordering::SeqCst), 0);
            assert_eq!(store.get_completion("existing").unwrap().unwrap(), pending);
            assert_eq!(store.get_completion(&foreign_id).unwrap().unwrap(), foreign);
            drop(original);
        }

        #[test]
        fn authority_validation_default_and_unbound_paths_remain_fail_closed() {
            let resolver = StaticProviderUsageAuthority {
                authority: ProviderUsageAuthority {
                    tenant_id: "tenant-m0".into(),
                    project_id: "project-m0".into(),
                    work_item_id: "work-m0".into(),
                    reservation_id: "reservation-m0".into(),
                    assignment_id: "assignment-m0".into(),
                    assignment_version: 1,
                    agent_id: AgentId(7),
                    provider: "local-loop".into(),
                    subscription_grant: None,
                },
            };
            let expected: ProviderExecutionAuthority = resolver.authority.clone().into();
            assert_eq!(
                resolver.validate_provider_usage_authority(&expected),
                Ok(true)
            );
            let changed: ProviderExecutionAuthority = ProviderUsageAuthority {
                assignment_version: 2,
                ..resolver.authority.clone()
            }
            .into();
            assert_eq!(
                resolver.validate_provider_usage_authority(&changed),
                Ok(false)
            );
            assert_eq!(
                SelectionErrorResolver.validate_provider_usage_authority(&expected),
                Err("selection unavailable")
            );
            assert!(validate_unreserved_provider_authority(
                Some(&resolver),
                None,
                AgentId(7),
                None,
            )
            .is_err());
            assert!(validate_unreserved_provider_authority(
                Some(&resolver),
                None,
                AgentId(8),
                None,
            )
            .is_ok());
            assert!(validate_unreserved_provider_authority(None, None, AgentId(7), None).is_ok());
            assert!(validate_unreserved_provider_authority(
                None,
                Some(&expected),
                AgentId(7),
                None,
            )
            .is_err());
            for authority in [None, Some(&expected)] {
                assert!(validate_unreserved_provider_authority(
                    Some(&SelectionErrorResolver),
                    authority,
                    AgentId(7),
                    None,
                )
                .unwrap_err()
                .contains("provider usage reauthorization failed: selection unavailable"));
            }
            let mut bound = ExactValidationResolver::new(
                crate::workflow_api::model_work::test_context().into(),
            );
            bound.unbound_allowed = false;
            assert_eq!(
                validate_unreserved_provider_authority(Some(&bound), None, AgentId(7), None),
                Err("provider work requires explicit authority".into())
            );
            assert_eq!(bound.selection_calls.load(Ordering::SeqCst), 0);
        }

        struct DispatchProofResolver {
            authority: ProviderUsageAuthority,
            absent: bool,
        }

        impl ProviderUsageAuthorityResolver for StaticProviderUsageAuthority {
            fn resolve_provider_usage_authority(
                &self,
                agent_id: AgentId,
            ) -> Result<Option<ProviderExecutionAuthority>, &'static str> {
                Ok((self.authority.agent_id == agent_id).then(|| self.authority.clone().into()))
            }
        }

        impl ProviderUsageAuthorityResolver for DispatchProofResolver {
            fn resolve_provider_usage_authority(
                &self,
                agent_id: AgentId,
            ) -> Result<Option<ProviderExecutionAuthority>, &'static str> {
                Ok((self.authority.agent_id == agent_id).then(|| self.authority.clone().into()))
            }

            fn provider_dispatch_is_definitively_absent(
                &self,
                authority: &ProviderExecutionAuthority,
                request_id: &str,
                request_digest: &str,
            ) -> Result<bool, &'static str> {
                if authority != &self.authority.clone().into()
                    || request_id != format!("company-provider-{}", self.authority.reservation_id)
                    || request_digest.len() != 64
                {
                    return Err("provider dispatch proof identity changed");
                }
                Ok(self.absent)
            }
        }

        #[test]
        fn provider_admission_recovery_releases_only_an_explicit_not_started_reservation() {
            let store = EventStore::open(":memory:").unwrap();
            let request_id = "company-provider-reservation-release";
            let request_digest = "a".repeat(64);
            assert!(store
                .reserve_request(request_id, &request_digest, "AGENT-03")
                .unwrap());

            assert!(!release_pre_provider_rejection(
                &store,
                request_id,
                &request_digest,
                None,
            ));
            assert!(!release_pre_provider_rejection(
                &store,
                request_id,
                &request_digest,
                Some("ambiguous"),
            ));
            assert!(store.get_completion(request_id).unwrap().is_some());
            assert!(release_pre_provider_rejection(
                &store,
                request_id,
                &request_digest,
                Some("not-started"),
            ));
            assert!(store.get_completion(request_id).unwrap().is_none());
        }

        #[test]
        fn provider_admission_recovery_requires_grace_and_durable_absence_proof() {
            let store = EventStore::open(":memory:").unwrap();
            let request_id = "company-provider-reservation-recovery";
            let request_digest = "b".repeat(64);
            let authority = ProviderUsageAuthority {
                tenant_id: "tenant-m0".to_owned(),
                project_id: "project-m0".to_owned(),
                work_item_id: "design-site".to_owned(),
                reservation_id: "reservation-recovery".to_owned(),
                assignment_id: "assignment-m0".to_owned(),
                assignment_version: 1,
                agent_id: AgentId(3),
                provider: "codex-cli".to_owned(),
                subscription_grant: None,
            };
            let execution_authority: ProviderExecutionAuthority = authority.clone().into();
            assert!(store
                .reserve_request(request_id, &request_digest, "AGENT-03")
                .unwrap());
            let entry = store.get_completion(request_id).unwrap().unwrap();
            let unavailable = DispatchProofResolver {
                authority: authority.clone(),
                absent: false,
            };
            let absent = DispatchProofResolver {
                authority,
                absent: true,
            };

            assert!(!release_stale_undispatched_subscription(
                &store,
                Some(&absent),
                Some(&execution_authority),
                &entry,
                entry.created_at + 9_999,
                Duration::ZERO,
            ));
            assert!(!release_stale_undispatched_subscription(
                &store,
                Some(&unavailable),
                Some(&execution_authority),
                &entry,
                entry.created_at + 10_000,
                Duration::ZERO,
            ));
            assert!(store.get_completion(request_id).unwrap().is_some());
            let (tx, _rx) = mpsc::channel();
            assert!(recover_reserved_model_request(
                &store,
                Some(&absent),
                &execution_authority,
                &tx,
                3,
                entry.created_at + 10_000,
                Duration::ZERO,
            )
            .unwrap());
            assert!(store.get_completion(request_id).unwrap().is_none());
        }

        #[test]
        fn provider_admission_recovery_waits_for_request_timeout_plus_grace() {
            let store = EventStore::open(":memory:").unwrap();
            let request_id = "company-provider-reservation-timeout";
            let request_digest = "c".repeat(64);
            let authority = ProviderUsageAuthority {
                tenant_id: "tenant-m0".to_owned(),
                project_id: "project-m0".to_owned(),
                work_item_id: "design-site".to_owned(),
                reservation_id: "reservation-timeout".to_owned(),
                assignment_id: "assignment-m0".to_owned(),
                assignment_version: 1,
                agent_id: AgentId(3),
                provider: "codex-cli".to_owned(),
                subscription_grant: None,
            };
            let execution_authority: ProviderExecutionAuthority = authority.clone().into();
            let resolver = DispatchProofResolver {
                authority,
                absent: true,
            };
            assert!(store
                .reserve_request(request_id, &request_digest, "AGENT-03")
                .unwrap());
            let entry = store.get_completion(request_id).unwrap().unwrap();

            assert!(!release_stale_undispatched_subscription(
                &store,
                Some(&resolver),
                Some(&execution_authority),
                &entry,
                entry.created_at + 44_999,
                Duration::from_secs(35),
            ));
            assert!(release_stale_undispatched_subscription(
                &store,
                Some(&resolver),
                Some(&execution_authority),
                &entry,
                entry.created_at + 45_000,
                Duration::from_secs(35),
            ));
            assert!(store.get_completion(request_id).unwrap().is_none());
        }

        #[test]
        fn pre_dispatch_authority_change_releases_the_undispatched_reservation() {
            let dir = tempfile::tempdir().unwrap();
            let store = EventStore::open(dir.path().join("events.db").to_str().unwrap()).unwrap();
            let expected = ProviderUsageAuthority {
                tenant_id: "tenant-m0".to_owned(),
                project_id: "project-m0".to_owned(),
                work_item_id: "build-site".to_owned(),
                reservation_id: "reservation-m0".to_owned(),
                assignment_id: "assignment-m0".to_owned(),
                assignment_version: 1,
                agent_id: AgentId(7),
                provider: "local-loop".to_owned(),
                subscription_grant: None,
            };
            let stale_resolver = StaticProviderUsageAuthority {
                authority: ProviderUsageAuthority {
                    assignment_version: 2,
                    ..expected.clone()
                },
            };
            assert!(store
                .reserve_request("company-provider-reservation-m0", "digest-m0", "AGENT-07")
                .unwrap());

            let error = validate_pre_dispatch_provider_authority(
                &store,
                Some(&stale_resolver),
                Some(&expected.clone().into()),
                AgentId(7),
                "company-provider-reservation-m0",
                "digest-m0",
                None,
            )
            .unwrap_err();

            assert!(error.contains("authority changed"), "{error}");
            assert!(store
                .get_completion("company-provider-reservation-m0")
                .unwrap()
                .is_none());
        }

        impl CompletionStore for FailFirstCompletionStore {
            fn bind_model_reservation(
                &self,
                reservation: &LlmModelReservationV1,
            ) -> anyhow::Result<()> {
                self.inner.bind_llm_model_reservation(reservation)
            }
            fn sealed_model_evidence(
                &self,
                request_id: &str,
                request_digest: &str,
                owner: &sentinel_common::StateTransferScope,
            ) -> anyhow::Result<Option<LlmSealedUnknownModelEvidenceV1>> {
                self.inner
                    .sealed_unknown_llm_model_evidence(request_id, request_digest, owner)
            }
            fn persist_sealed_model_usage(
                &self,
                evidence: &LlmSealedUnknownModelEvidenceV1,
                event: &DomainEvent,
            ) -> anyhow::Result<bool> {
                self.inner
                    .persist_sealed_unknown_llm_model_usage(evidence, event)
            }
            fn poll_provider_in_flight(
                &self,
                limit: usize,
            ) -> anyhow::Result<Vec<LlmCompletionEntry>> {
                self.inner.poll_llm_provider_in_flight(limit)
            }

            fn mark_provider_unknown(
                &self,
                request_id: &str,
                request_digest: &str,
                reason: &str,
            ) -> anyhow::Result<bool> {
                self.inner
                    .mark_llm_provider_outcome_unknown(request_id, request_digest, reason)
            }
            fn reserve_request(
                &self,
                request_id: &str,
                request_digest: &str,
                agent_id: &str,
            ) -> anyhow::Result<bool> {
                self.reservation_calls.fetch_add(1, Ordering::SeqCst);
                self.inner
                    .reserve_llm_request(request_id, request_digest, agent_id)
            }

            fn release_undispatched_request(
                &self,
                request_id: &str,
                request_digest: &str,
            ) -> anyhow::Result<bool> {
                self.inner
                    .release_undispatched_llm_request(request_id, request_digest)
            }

            fn enqueue_completion(
                &self,
                request_id: &str,
                request_digest: &str,
                payload: &str,
            ) -> anyhow::Result<()> {
                self.inner
                    .enqueue_llm_completion(request_id, request_digest, payload)
            }

            fn get_completion(
                &self,
                request_id: &str,
            ) -> anyhow::Result<Option<LlmCompletionEntry>> {
                self.inner.get_llm_completion(request_id)
            }

            fn poll_completions(&self, limit: usize) -> anyhow::Result<Vec<LlmCompletionEntry>> {
                self.inner.poll_llm_completions(limit)
            }

            fn persist_usage(
                &self,
                request_id: &str,
                request_digest: &str,
                event: &DomainEvent,
            ) -> anyhow::Result<()> {
                self.append_calls.fetch_add(1, Ordering::SeqCst);
                if self.fail_next_usage.swap(false, Ordering::SeqCst) {
                    self.failure_observed.notify_one();
                    anyhow::bail!("injected local usage append failure");
                }
                self.inner
                    .persist_llm_completion_usage(request_id, request_digest, event)
            }

            fn record_failure(
                &self,
                request_id: &str,
                request_digest: &str,
                error: &str,
                max_attempts: u32,
            ) -> anyhow::Result<(u32, bool)> {
                self.inner.record_llm_completion_failure(
                    request_id,
                    request_digest,
                    error,
                    max_attempts,
                )
            }

            fn claim_actions(
                &self,
                request_id: &str,
                request_digest: &str,
            ) -> anyhow::Result<bool> {
                self.inner
                    .claim_llm_completion_actions(request_id, request_digest)
            }

            fn complete_actions(
                &self,
                request_id: &str,
                request_digest: &str,
            ) -> anyhow::Result<bool> {
                self.inner
                    .complete_llm_completion_actions(request_id, request_digest)
            }

            fn has_operation(&self, operation_id: &str) -> anyhow::Result<bool> {
                self.inner.has_event_operation_id(operation_id)
            }
        }

        async fn start_mock_agent_provider(
            provider_calls: Arc<std::sync::atomic::AtomicUsize>,
            request_observed: Option<Arc<tokio::sync::Notify>>,
            release_response: Option<Arc<tokio::sync::Notify>>,
        ) -> (String, tokio::task::JoinHandle<()>) {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                loop {
                    let (mut stream, _) = match listener.accept().await {
                        Ok(connection) => connection,
                        Err(_) => return,
                    };
                    let provider_calls = Arc::clone(&provider_calls);
                    let request_observed = request_observed.clone();
                    let release_response = release_response.clone();
                    tokio::spawn(async move {
                        let mut request = Vec::new();
                        let mut chunk = [0u8; 4096];
                        loop {
                            let read = stream.read(&mut chunk).await.unwrap();
                            if read == 0 {
                                return;
                            }
                            request.extend_from_slice(&chunk[..read]);
                            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                break;
                            }
                        }
                        let request_text = String::from_utf8_lossy(&request);
                        assert!(request_text.starts_with("POST /internal/agent-runtime "));
                        assert!(request_text.lines().any(|line| line
                            .eq_ignore_ascii_case("authorization: Bearer test-credential")));
                        let request_id = request_text
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("X-Request-ID: ")
                                    .or_else(|| line.strip_prefix("x-request-id: "))
                            })
                            .unwrap()
                            .trim();
                        assert!(request_text.lines().any(|line| {
                            line.to_ascii_lowercase().starts_with("x-request-digest: ")
                        }));
                        provider_calls.fetch_add(1, Ordering::SeqCst);
                        if let Some(observed) = request_observed {
                            observed.notify_one();
                        }
                        if let Some(release) = release_response {
                            release.notified().await;
                        }
                        let body = serde_json::json!({
                            "content": "move",
                            "actions": [{"type": "move", "target": "meeting-room"}],
                            "tokens_used": 7,
                            "request_id": request_id,
                            "provider": "local-loop",
                            "input_tokens": 5,
                            "output_tokens": 2,
                            "cache_read": 0,
                            "cache_creation": 0,
                            "tier": "mid",
                            "cost_usd": 0.0,
                            "hierarchy_tier": 2,
                            "cost_source": "non_provider_zero",
                            "effective_model": "mock/model"
                        })
                        .to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        stream.write_all(response.as_bytes()).await.unwrap();
                    });
                }
            });
            (format!("http://{address}"), task)
        }

        fn recovery_test_perception() -> Perception {
            Perception {
                agent_id: AgentId(7),
                circadian_text: String::new(),
                body_text: "ready".to_string(),
                environment_text: String::new(),
                acoustic_text: String::new(),
                heard_text: String::new(),
                presence_text: String::new(),
                impulse_text: "move".to_string(),
                is_directly_addressed: false,
                timestamp: Timestamp(1234),
                tick: Tick(55),
                room_id: "office".to_string(),
                max_priority: "P2".to_string(),
                synth_fingerprint: "test".to_string(),
                personality_type: "E".to_string(),
                has_operator_impulse: false,
            }
        }

        struct CooldownTestResolver {
            legacy: ExactValidationResolver,
            selections: Mutex<HashMap<AgentId, usize>>,
            fail_context: bool,
            selection_barrier: Option<Arc<OwnedPipelineBarrier>>,
        }

        impl ProviderUsageAuthorityResolver for CooldownTestResolver {
            fn allows_unbound_provider_usage(&self) -> bool {
                true
            }

            fn resolve_provider_usage_authority(
                &self,
                agent_id: AgentId,
            ) -> Result<Option<ProviderExecutionAuthority>, &'static str> {
                self.legacy.resolve_provider_usage_authority(agent_id)
            }

            fn prepare_provider_usage_candidate(
                &self,
                agent_id: AgentId,
            ) -> Result<Option<PreparedProviderCandidate>, &'static str> {
                let first = {
                    let mut selections = self.selections.lock().unwrap();
                    let count = selections.entry(agent_id).or_default();
                    *count += 1;
                    *count == 1
                };
                if first && agent_id == self.legacy.context.binding().agent_id() {
                    if let Some(barrier) = &self.selection_barrier {
                        barrier.wait();
                    }
                }
                Ok(Some(PreparedProviderCandidate {
                    authority: self.legacy.resolve_provider_usage_authority(agent_id)?,
                    prepared_context: None,
                }))
            }

            fn validate_provider_usage_authority(
                &self,
                expected: &ProviderExecutionAuthority,
            ) -> Result<bool, &'static str> {
                self.legacy.validate_provider_usage_authority(expected)
            }

            fn model_work_context(
                &self,
                expected: &ProviderExecutionAuthority,
            ) -> Result<Option<ModelWorkContext>, &'static str> {
                if self.fail_context {
                    self.legacy.context_calls.fetch_add(1, Ordering::SeqCst);
                    return Err("synthetic owned preparation failure");
                }
                self.legacy.model_work_context(expected)
            }
        }

        struct CooldownBridgeFixture {
            _release_on_drop: ReleaseOnDrop,
            _directory: tempfile::TempDir,
            resolver: Arc<CooldownTestResolver>,
            store: Arc<EventStore>,
            telemetry: Arc<BridgeTelemetry>,
            activity: SharedLlmActivityTicks,
            llm_unavailable: Arc<AtomicBool>,
            provider_calls: Arc<std::sync::atomic::AtomicUsize>,
            active_requests: ActiveProviderRequests,
            release_response: Arc<tokio::sync::Notify>,
            perception_tx: mpsc::Sender<Perception>,
            shutdown_tx: watch::Sender<bool>,
            bridge: tokio::task::JoinHandle<Result<(), &'static str>>,
            provider_task: tokio::task::JoinHandle<()>,
        }

        impl CooldownBridgeFixture {
            async fn start(fail_context: bool, reserved: bool) -> Self {
                Self::start_with_selection_barrier(fail_context, reserved, None).await
            }

            async fn start_with_selection_barrier(
                fail_context: bool,
                reserved: bool,
                selection_barrier: Option<Arc<OwnedPipelineBarrier>>,
            ) -> Self {
                let directory = tempfile::tempdir().unwrap();
                let context: ModelWorkContext =
                    crate::workflow_api::model_work::test_context().into();
                let resolver = Arc::new(CooldownTestResolver {
                    legacy: ExactValidationResolver::new(context),
                    selections: Mutex::new(HashMap::new()),
                    fail_context,
                    selection_barrier,
                });
                let store = Arc::new(EventStore::open(":memory:").unwrap());
                if reserved {
                    let authority = resolver.legacy.context.binding();
                    assert!(store
                        .reserve_request(
                            &authority.request_id(),
                            &"a".repeat(64),
                            &authority.agent_id().to_string(),
                        )
                        .unwrap());
                }
                let state = Arc::new(
                    StateStore::open(directory.path().join("state.redb").to_str().unwrap())
                        .unwrap(),
                );
                let provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let active_requests = Arc::new(Mutex::new(HashMap::new()));
                let release_response = Arc::new(tokio::sync::Notify::new());
                let (gateway_url, provider_task) = start_mock_agent_provider(
                    Arc::clone(&provider_calls),
                    None,
                    Some(Arc::clone(&release_response)),
                )
                .await;
                let config = LlmBridgeConfig {
                    gateway_url,
                    credential: "test-credential".into(),
                    max_concurrent: 1,
                    min_ticks_between_calls: 5,
                    request_timeout: Duration::from_secs(5),
                    shutdown_drain_timeout: Duration::from_secs(2),
                    usage_v2_enabled: true,
                    completion_retry_interval: Duration::from_secs(3600),
                    provider_usage_authority: Some(resolver.clone()),
                    active_provider_requests: Some(Arc::clone(&active_requests)),
                    ..Default::default()
                };
                let (perception_tx, perception_rx) = mpsc::channel();
                let (action_tx, _action_rx) = mpsc::channel();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let telemetry = Arc::new(BridgeTelemetry::default());
                let activity = Arc::new(Mutex::new(HashMap::new()));
                let llm_unavailable = Arc::new(AtomicBool::new(false));
                let bridge = tokio::spawn(run_llm_bridge_with_store(
                    config,
                    perception_rx,
                    action_tx,
                    Arc::clone(&telemetry),
                    state,
                    Arc::clone(&store),
                    Arc::clone(&llm_unavailable),
                    Arc::clone(&activity),
                    shutdown_rx,
                    Arc::new(RwLock::new(true)),
                ));
                Self {
                    _release_on_drop: ReleaseOnDrop(Arc::clone(&release_response)),
                    _directory: directory,
                    resolver,
                    store,
                    telemetry,
                    activity,
                    llm_unavailable,
                    provider_calls,
                    active_requests,
                    release_response,
                    perception_tx,
                    shutdown_tx,
                    bridge,
                    provider_task,
                }
            }

            fn bound_agent(&self) -> AgentId {
                self.resolver.legacy.context.binding().agent_id()
            }

            fn perception(&self, tick: u64) -> Perception {
                let mut perception = recovery_test_perception();
                perception.agent_id = self.bound_agent();
                perception.tick = Tick(tick);
                perception
            }

            fn selection_count(&self, agent_id: AgentId) -> usize {
                self.resolver
                    .selections
                    .lock()
                    .unwrap()
                    .get(&agent_id)
                    .copied()
                    .unwrap_or(0)
            }

            async fn send_selected(&self, perception: Perception) {
                let agent_id = perception.agent_id;
                let before = self.selection_count(agent_id);
                let processed = self.telemetry.preparations_processed.load(Ordering::SeqCst);
                self.perception_tx.send(perception).unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while self.selection_count(agent_id) == before
                        || self.telemetry.preparations_processed.load(Ordering::SeqCst) == processed
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("eligible perception did not finish owned preparation");
            }

            async fn send_rate_limited(&self, perception: Perception) {
                let agent_id = perception.agent_id;
                let selections = self.selection_count(agent_id);
                let before = self
                    .telemetry
                    .calls_skipped_rate_limit
                    .load(Ordering::SeqCst);
                self.perception_tx.send(perception).unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while self
                        .telemetry
                        .calls_skipped_rate_limit
                        .load(Ordering::SeqCst)
                        == before
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("owned attempt did not enforce cooldown");
                assert_eq!(self.selection_count(agent_id), selections);
            }

            async fn wait_provider_calls(&self, count: usize) {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while self.provider_calls.load(Ordering::SeqCst) < count {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("provider request was not observed");
                assert_eq!(self.provider_calls.load(Ordering::SeqCst), count);
            }

            async fn stop(self) {
                self.shutdown_tx.send(true).unwrap();
                self.release_response.notify_waiters();
                tokio::time::timeout(Duration::from_secs(3), self.bridge)
                    .await
                    .expect("cooldown bridge did not drain")
                    .unwrap()
                    .unwrap();
                self.provider_task.abort();
            }
        }

        struct ReleaseOnDrop(Arc<tokio::sync::Notify>);

        impl Drop for ReleaseOnDrop {
            fn drop(&mut self) {
                self.0.notify_waiters();
                self.0.notify_one();
            }
        }

        async fn assert_queued_preparation_cooldown(urgent: bool) {
            let (release_tx, release_rx) = mpsc::channel();
            let barrier = Arc::new(OwnedPipelineBarrier {
                entered: tokio::sync::Notify::new(),
                release: Mutex::new(release_rx),
                exited: AtomicBool::new(false),
            });
            let f = CooldownBridgeFixture::start_with_selection_barrier(
                true,
                false,
                Some(Arc::clone(&barrier)),
            )
            .await;
            f.perception_tx.send(f.perception(55)).unwrap();
            tokio::time::timeout(Duration::from_secs(2), barrier.entered.notified())
                .await
                .unwrap();
            let mut queued = f.perception(56);
            queued.is_directly_addressed = urgent;
            f.perception_tx.send(queued).unwrap();
            owned_pipeline_wait(|| f.telemetry.perceptions_offered.load(Ordering::SeqCst) == 2)
                .await;
            assert!(!f.activity.lock().unwrap().contains_key(&f.bound_agent()));
            release_tx.send(()).unwrap();
            owned_pipeline_wait(|| f.telemetry.preparations_processed.load(Ordering::SeqCst) == 2)
                .await;
            assert_eq!(
                f.selection_count(f.bound_agent()),
                if urgent { 2 } else { 1 }
            );
            assert_eq!(
                f.resolver.legacy.context_calls.load(Ordering::SeqCst),
                if urgent { 2 } else { 1 }
            );
            let expected_tick = if urgent { 56 } else { 55 };
            assert_eq!(
                f.activity.lock().unwrap().get(&f.bound_agent()),
                Some(&expected_tick)
            );
            assert_eq!(
                f.telemetry.calls_skipped_rate_limit.load(Ordering::SeqCst),
                if urgent { 0 } else { 1 }
            );
            assert_eq!(f.provider_calls.load(Ordering::SeqCst), 0);
            assert!(f.store.get_all_events().unwrap().is_empty());
            assert!(f
                .store
                .get_completion(&f.resolver.legacy.context.binding().request_id())
                .unwrap()
                .is_none());
            f.stop().await;
        }

        #[tokio::test]
        async fn queued_normal_rechecks_cooldown_after_slow_selection_context_failure() {
            assert_queued_preparation_cooldown(false).await;
        }

        #[tokio::test]
        async fn queued_urgent_preserves_bypass_after_slow_selection_context_failure() {
            assert_queued_preparation_cooldown(true).await;
        }

        #[tokio::test]
        async fn productive_bridge_reports_live_ownership_to_shared_recovery_registry() {
            let f = CooldownBridgeFixture::start(false, false).await;
            let request_id = f.resolver.legacy.context.binding().request_id();
            let active = Arc::clone(&f.active_requests);
            f.send_selected(f.perception(55)).await;
            f.wait_provider_calls(1).await;
            assert_eq!(active.lock().unwrap().get(&request_id), Some(&1));
            let reserved = f.store.get_llm_completion(&request_id).unwrap().unwrap();
            assert_eq!(reserved.status, "provider_in_flight");
            assert!(reserved.payload.is_empty());
            f.stop().await;
            assert!(active.lock().unwrap().is_empty());
        }

        #[tokio::test]
        async fn preparation_panic_publishes_terminal_unavailable_before_inflight_success() {
            let (release_tx, release_rx) = mpsc::channel();
            let barrier = Arc::new(OwnedPipelineBarrier {
                entered: tokio::sync::Notify::new(),
                release: Mutex::new(release_rx),
                exited: AtomicBool::new(false),
            });
            let f = CooldownBridgeFixture::start_with_selection_barrier(
                false,
                false,
                Some(Arc::clone(&barrier)),
            )
            .await;
            let other = recovery_test_perception();
            assert_ne!(other.agent_id, f.bound_agent());
            f.send_selected(other).await;
            f.wait_provider_calls(1).await;
            f.perception_tx.send(f.perception(55)).unwrap();
            tokio::time::timeout(Duration::from_secs(2), barrier.entered.notified())
                .await
                .unwrap();
            // Closing the fixture callback channel injects a native preparation panic.
            drop(release_tx);
            owned_pipeline_wait(|| f.llm_unavailable.load(Ordering::Acquire)).await;
            assert!(!*f.shutdown_tx.borrow(), "no daemon shutdown requested");
            assert!(
                !f.bridge.is_finished(),
                "in-flight response still belongs to the bridge"
            );
            f.release_response.notify_one();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(3), f.bridge)
                    .await
                    .unwrap()
                    .unwrap(),
                Err("llm_preparation_job_failed")
            );
            assert_eq!(f.telemetry.calls_success.load(Ordering::SeqCst), 1);
            assert_eq!(f.provider_calls.load(Ordering::SeqCst), 1);
            assert!(
                f.llm_unavailable.load(Ordering::Acquire),
                "in-flight circuit success must not revive a stopped bridge"
            );
            assert!(!*f.shutdown_tx.borrow());
            f.provider_task.abort();
        }

        #[test]
        fn rejected_operator_impulse_retry_preserves_admitted_urgency_not_new_frames() {
            let directory = tempfile::tempdir().unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let mut lanes = OwnedBlockingLanes::spawn(1, 32).unwrap();
            let mut dependencies =
                recovery_lane_test_dependencies(directory.path(), &runtime, lanes.executor());
            let resolver = Arc::new(CooldownTestResolver {
                legacy: ExactValidationResolver::new(
                    crate::workflow_api::model_work::test_context().into(),
                ),
                selections: Mutex::new(HashMap::new()),
                fail_context: true,
                selection_barrier: None,
            });
            dependencies.config.min_ticks_between_calls = 5;
            dependencies.config.usage_v2_enabled = true;
            dependencies.config.provider_usage_authority = Some(resolver.clone());
            let dependencies = Arc::new(dependencies);
            let mut perception = recovery_test_perception();
            perception.agent_id = resolver.legacy.context.binding().agent_id();
            let mut acked = HashMap::new();
            let window = intake_preparation_window(&perception, None, &mut acked, Instant::now());
            let first_dependencies = Arc::clone(&dependencies);
            let first_perception = perception.clone();
            let first_window = Arc::clone(&window);
            let first = lanes
                .executor()
                .submit(Lane::Preparation, None, move |cancellation| {
                    prepare_provider_call(
                        &first_dependencies,
                        first_perception,
                        &cancellation,
                        first_window,
                    )
                })
                .unwrap();
            let outcome = runtime.block_on(first.wait()).unwrap();
            assert!(outcome.attempt_started);
            assert!(outcome.call.is_none());
            assert_eq!(
                dependencies
                    .activity
                    .lock()
                    .unwrap()
                    .get(&perception.agent_id),
                Some(&55)
            );
            perception.has_operator_impulse = true;
            let admitted =
                intake_preparation_window(&perception, Some(&window), &mut acked, Instant::now());
            assert!(admitted.urgent_for(55));
            let mut rejected = ProviderPreparationController::<ProviderPreparationOutcome>::new(
                0,
                1,
                insert_prefer_heard,
            );
            let perception = rejected.offer(perception).unwrap_err();
            let retries = Arc::new(AsyncMutex::new(HashMap::new()));
            assert!(runtime.block_on(queue_bounded_retry(
                &retries,
                *perception,
                Some(Arc::clone(&admitted))
            )));
            let retry = runtime.block_on(async {
                retries
                    .lock()
                    .await
                    .remove(&resolver.legacy.context.binding().agent_id())
                    .unwrap()
            });
            let retry_window = intake_preparation_window(
                &retry.perception,
                retry.window.as_ref(),
                &mut acked,
                Instant::now(),
            );
            assert!(retry_window.urgent_for(55));
            assert_eq!(
                retry_window.enqueued_at_for(55),
                admitted.enqueued_at_for(55)
            );
            assert_eq!(acked.get(&retry.perception.agent_id), Some(&55));
            let mut new_frame = retry.perception.clone();
            new_frame.tick = Tick(56);
            assert!(!intake_preparation_window(
                &new_frame,
                Some(&retry_window),
                &mut acked,
                Instant::now()
            )
            .urgent_for(56));
            let mut accepted = ProviderPreparationController::new(1, 1, insert_prefer_heard);
            accepted.offer(retry.perception).unwrap();
            let worker_dependencies = Arc::clone(&dependencies);
            let (_, result) = runtime.block_on(async {
                assert_eq!(
                    accepted.pump(&lanes.executor(), None, move |perception, cancellation| {
                        prepare_provider_call(
                            &worker_dependencies,
                            perception,
                            &cancellation,
                            Arc::clone(&retry_window),
                        )
                    }),
                    1
                );
                accepted.next_result().await.unwrap()
            });
            let outcome = result.unwrap();
            assert!(outcome.attempt_started);
            assert!(outcome.call.is_none());
            assert_eq!(
                resolver.selections.lock().unwrap().get(&new_frame.agent_id),
                Some(&2)
            );
            assert_eq!(resolver.legacy.context_calls.load(Ordering::SeqCst), 2);
            assert_eq!(
                dependencies
                    .activity
                    .lock()
                    .unwrap()
                    .get(&new_frame.agent_id),
                Some(&55)
            );
            assert!(dependencies
                .event_store
                .get_all_events()
                .unwrap()
                .is_empty());
            lanes.stop_and_join().unwrap();
        }

        fn assert_backpressure_preserves_first_intake_age(circuit_open: bool) {
            let directory = tempfile::tempdir().unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let mut lanes = OwnedBlockingLanes::spawn(1, 32).unwrap();
            let dependencies = Arc::new(recovery_lane_test_dependencies(
                directory.path(),
                &runtime,
                lanes.executor(),
            ));
            let mut perception = recovery_test_perception();
            perception.is_directly_addressed = true;
            let mut acked = HashMap::new();
            // Controlled scheduler age, not a fabricated model/grant deadline or speed claim.
            let original_age =
                Instant::now() - (UNBOUND_ADMISSION_MAX_AGE + Duration::from_secs(1));
            let original = intake_preparation_window(&perception, None, &mut acked, original_age);
            let retries = Arc::new(AsyncMutex::new(HashMap::new()));
            if circuit_open {
                let mut circuit = CircuitBreaker::new(
                    1,
                    Duration::from_secs(60),
                    Arc::new(AtomicBool::new(false)),
                );
                circuit.record_failure();
                assert!(circuit.is_open());
            } else {
                let mut controller =
                    ProviderPreparationController::<ProviderPreparationOutcome>::new(
                        0,
                        1,
                        insert_prefer_heard,
                    );
                assert!(controller.offer(perception.clone()).is_err());
            }
            assert!(runtime.block_on(queue_bounded_retry(
                &retries,
                perception.clone(),
                Some(original)
            )));
            let retry = runtime
                .block_on(async { retries.lock().await.remove(&perception.agent_id).unwrap() });
            let window = intake_preparation_window(
                &retry.perception,
                retry.window.as_ref(),
                &mut acked,
                Instant::now(),
            );
            assert_eq!(window.enqueued_at_for(perception.tick.0), original_age);
            assert!(
                window.enqueued_at_for(perception.tick.0).elapsed() > UNBOUND_ADMISSION_MAX_AGE
            );
            let worker_dependencies = Arc::clone(&dependencies);
            let job = lanes
                .executor()
                .submit(Lane::Preparation, None, move |cancellation| {
                    prepare_provider_call(
                        &worker_dependencies,
                        retry.perception,
                        &cancellation,
                        window,
                    )
                })
                .unwrap();
            let outcome = runtime.block_on(job.wait()).unwrap();
            assert!(
                outcome.call.is_none(),
                "acceptance must not create a fresh ambient admission age"
            );
            assert!(dependencies
                .event_store
                .get_completion(&agent_runtime_request_id(&perception, None))
                .unwrap()
                .is_none());
            assert!(dependencies
                .event_store
                .get_all_events()
                .unwrap()
                .is_empty());
            lanes.stop_and_join().unwrap();
        }

        #[test]
        fn circuit_retry_preserves_first_intake_age_before_acceptance() {
            assert_backpressure_preserves_first_intake_age(true);
        }

        #[test]
        fn preparation_overflow_retry_preserves_first_intake_age_before_acceptance() {
            assert_backpressure_preserves_first_intake_age(false);
        }

        #[tokio::test]
        async fn active_exact_duplicates_do_not_extend_nonzero_agent_cooldown() {
            let f = CooldownBridgeFixture::start(false, false).await;
            f.send_selected(f.perception(55)).await;
            f.wait_provider_calls(1).await;
            let contexts = f.resolver.legacy.context_calls.load(Ordering::SeqCst);
            for tick in [60, 61] {
                let mut duplicate = f.perception(tick);
                duplicate.is_directly_addressed = true;
                f.send_selected(duplicate).await;
                assert_eq!(f.activity.lock().unwrap().get(&f.bound_agent()), Some(&55));
            }
            f.send_selected(f.perception(62)).await;
            assert_eq!(f.activity.lock().unwrap().get(&f.bound_agent()), Some(&55));
            assert_eq!(f.telemetry.calls_total.load(Ordering::SeqCst), 1);
            assert_eq!(
                f.resolver.legacy.context_calls.load(Ordering::SeqCst),
                contexts
            );
            assert_eq!(
                f.telemetry.calls_skipped_rate_limit.load(Ordering::SeqCst),
                0
            );
            assert_eq!(f.provider_calls.load(Ordering::SeqCst), 1);
            f.stop().await;
        }

        #[tokio::test]
        async fn recovery_only_exact_requests_do_not_start_nonzero_agent_cooldown() {
            let f = CooldownBridgeFixture::start(false, true).await;
            let request_id = f.resolver.legacy.context.binding().request_id();
            let reserved = f.store.get_completion(&request_id).unwrap().unwrap();
            for tick in [55, 56] {
                f.send_selected(f.perception(tick)).await;
                assert!(!f.activity.lock().unwrap().contains_key(&f.bound_agent()));
            }
            assert_eq!(f.resolver.legacy.context_calls.load(Ordering::SeqCst), 0);
            assert_eq!(f.resolver.legacy.validation_calls.load(Ordering::SeqCst), 0);
            assert_eq!(f.telemetry.calls_total.load(Ordering::SeqCst), 0);
            assert_eq!(
                f.telemetry.calls_skipped_rate_limit.load(Ordering::SeqCst),
                0
            );
            assert_eq!(f.provider_calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                f.store.get_completion(&request_id).unwrap().unwrap(),
                reserved
            );
            f.stop().await;
        }

        #[tokio::test]
        async fn owned_preparation_failure_preserves_cooldown_other_agent_and_urgency() {
            let f = CooldownBridgeFixture::start(true, false).await;
            f.send_selected(f.perception(55)).await;
            assert_eq!(f.activity.lock().unwrap().get(&f.bound_agent()), Some(&55));
            f.send_rate_limited(f.perception(56)).await;
            let other = recovery_test_perception();
            assert_ne!(other.agent_id, f.bound_agent());
            f.send_selected(other.clone()).await;
            f.wait_provider_calls(1).await;
            let mut other_limited = other.clone();
            other_limited.tick = Tick(56);
            f.send_rate_limited(other_limited).await;
            for tick in [56, 57] {
                let mut urgent = f.perception(tick);
                if tick == 56 {
                    urgent.is_directly_addressed = true;
                } else {
                    urgent.heard_text = "continue".into();
                }
                f.send_selected(urgent).await;
                assert_eq!(
                    f.activity.lock().unwrap().get(&f.bound_agent()),
                    Some(&tick)
                );
            }
            let mut impulse = f.perception(60);
            impulse.has_operator_impulse = true;
            f.send_selected(impulse.clone()).await;
            impulse.tick = Tick(61);
            f.send_rate_limited(impulse).await;
            f.send_selected(f.perception(65)).await;
            assert_eq!(f.activity.lock().unwrap().get(&f.bound_agent()), Some(&65));
            assert_eq!(f.activity.lock().unwrap().get(&other.agent_id), Some(&55));
            assert_eq!(f.resolver.legacy.context_calls.load(Ordering::SeqCst), 5);
            assert!(f
                .store
                .get_completion(&f.resolver.legacy.context.binding().request_id())
                .unwrap()
                .is_none());
            assert_eq!(f.provider_calls.load(Ordering::SeqCst), 1);
            assert_eq!(f.telemetry.calls_total.load(Ordering::SeqCst), 1);
            f.stop().await;
        }

        #[tokio::test]
        async fn owned_capacity_miss_preserves_nonzero_cooldown_and_unbound_backoff() {
            let f = CooldownBridgeFixture::start(false, false).await;
            let blocker = recovery_test_perception();
            assert_ne!(blocker.agent_id, f.bound_agent());
            f.send_selected(blocker.clone()).await;
            f.wait_provider_calls(1).await;
            f.send_selected(f.perception(55)).await;
            assert_eq!(f.activity.lock().unwrap().get(&f.bound_agent()), Some(&55));
            f.send_rate_limited(f.perception(56)).await;
            let mut blocker_limited = blocker;
            blocker_limited.tick = Tick(56);
            f.send_rate_limited(blocker_limited).await;
            f.send_selected(f.perception(60)).await;
            assert_eq!(f.activity.lock().unwrap().get(&f.bound_agent()), Some(&60));
            assert_eq!(f.resolver.legacy.context_calls.load(Ordering::SeqCst), 2);
            assert!(f
                .store
                .get_completion(&f.resolver.legacy.context.binding().request_id())
                .unwrap()
                .is_none());
            assert_eq!(f.provider_calls.load(Ordering::SeqCst), 1);
            assert_eq!(f.telemetry.calls_total.load(Ordering::SeqCst), 3);
            assert_eq!(
                f.telemetry.calls_skipped_rate_limit.load(Ordering::SeqCst),
                2
            );
            f.stop().await;
        }

        #[tokio::test]
        async fn reserved_model_recovery_precedes_deferred_context_preparation() {
            let dir = tempfile::tempdir().unwrap();
            let context: ModelWorkContext = crate::workflow_api::model_work::test_context().into();
            let expected = context.binding();
            let resolver = Arc::new(ExactValidationResolver::new(context));
            let store = Arc::new(EventStore::open(":memory:").unwrap());
            let id = expected.request_id();
            let digest = "a".repeat(64);
            assert!(store
                .reserve_request(&id, &digest, &expected.agent_id().to_string())
                .unwrap());
            let reserved = store.get_completion(&id).unwrap().unwrap();
            let state = Arc::new(
                StateStore::open(dir.path().join("state.redb").to_str().unwrap()).unwrap(),
            );
            let provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (gateway_url, provider_task) =
                start_mock_agent_provider(Arc::clone(&provider_calls), None, None).await;
            let config = LlmBridgeConfig {
                gateway_url,
                credential: "test-credential".into(),
                min_ticks_between_calls: 0,
                usage_v2_enabled: true,
                completion_retry_interval: Duration::from_secs(3600),
                provider_usage_authority: Some(resolver.clone()),
                ..Default::default()
            };
            let (perception_tx, perception_rx) = mpsc::channel();
            let (action_tx, _action_rx) = mpsc::channel();
            let (shutdown_tx, shutdown_rx) = watch::channel(false);
            let bridge = tokio::spawn(run_llm_bridge_with_store(
                config,
                perception_rx,
                action_tx,
                Arc::new(BridgeTelemetry::default()),
                state,
                Arc::clone(&store),
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(HashMap::new())),
                shutdown_rx,
                Arc::new(RwLock::new(true)),
            ));
            let mut perception = recovery_test_perception();
            perception.agent_id = expected.agent_id();
            perception_tx.send(perception).unwrap();
            tokio::time::timeout(Duration::from_secs(2), resolver.selected.notified())
                .await
                .expect("candidate was not selected");
            shutdown_tx.send(true).unwrap();
            drop(perception_tx);
            tokio::time::timeout(Duration::from_secs(2), bridge)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(resolver.selection_calls.load(Ordering::SeqCst), 1);
            assert_eq!(resolver.context_calls.load(Ordering::SeqCst), 0);
            assert_eq!(resolver.validation_calls.load(Ordering::SeqCst), 0);
            assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
            assert_eq!(store.get_completion(&id).unwrap().unwrap(), reserved);
            provider_task.abort();
        }

        #[tokio::test]
        async fn queued_exact_duplicates_preserve_nonzero_cooldown_and_fresh_expiry() {
            let dir = tempfile::tempdir().unwrap();
            let event_path = dir.path().join("events.sqlite");
            let (_, mut review) = crate::workflow_api::adaptive_leadership_review::tests::fixture(
                &dir.path().join("company.sqlite"),
                &event_path,
            );
            review.binding.grant.max_duration_ms = 2_000;
            review.binding.grant.expires_at_unix_ms = review.binding.issued_at_ms + 2_000;
            let expires_at = review.binding.grant.expires_at_unix_ms;
            let context = ModelWorkContext::AdaptiveLeadershipReview(Box::new(review));
            context.validate_dispatch(unix_now_ms()).unwrap();
            let expected = context.binding();
            let resolver = Arc::new(ExactValidationResolver::new(context));
            let store = Arc::new(FailFirstCompletionStore {
                inner: EventStore::open(event_path.to_str().unwrap()).unwrap(),
                reservation_calls: std::sync::atomic::AtomicUsize::new(0),
                fail_next_usage: AtomicBool::new(false),
                append_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                failure_observed: Arc::new(tokio::sync::Notify::new()),
            });
            let state = Arc::new(
                StateStore::open(dir.path().join("state.redb").to_str().unwrap()).unwrap(),
            );
            let provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let _release_on_drop = ReleaseOnDrop(Arc::clone(&release));
            let (gateway_url, provider_task) = start_mock_agent_provider(
                Arc::clone(&provider_calls),
                Some(Arc::clone(&observed)),
                Some(Arc::clone(&release)),
            )
            .await;
            let config = LlmBridgeConfig {
                gateway_url,
                credential: "test-credential".into(),
                max_concurrent: 1,
                request_timeout: Duration::from_secs(5),
                shutdown_drain_timeout: Duration::from_secs(2),
                min_ticks_between_calls: 5,
                usage_v2_enabled: true,
                completion_retry_interval: Duration::from_secs(3600),
                provider_usage_authority: Some(resolver.clone()),
                ..Default::default()
            };
            let (perception_tx, perception_rx) = mpsc::channel();
            let (action_tx, _action_rx) = mpsc::channel();
            let (shutdown_tx, shutdown_rx) = watch::channel(false);
            let telemetry = Arc::new(BridgeTelemetry::default());
            let activity = Arc::new(Mutex::new(HashMap::new()));
            let bridge = tokio::spawn(run_llm_bridge_with_store(
                config,
                perception_rx,
                action_tx,
                Arc::clone(&telemetry),
                state,
                Arc::clone(&store),
                Arc::new(AtomicBool::new(false)),
                Arc::clone(&activity),
                shutdown_rx,
                Arc::new(RwLock::new(true)),
            ));
            let mut blocker = recovery_test_perception();
            blocker.agent_id = if expected.agent_id() == AgentId(7) {
                AgentId(8)
            } else {
                AgentId(7)
            };
            perception_tx.send(blocker).unwrap();
            tokio::time::timeout(Duration::from_secs(2), observed.notified())
                .await
                .expect("blocking provider request was not observed");
            let mut queued = recovery_test_perception();
            queued.agent_id = expected.agent_id();
            queued.is_directly_addressed = true;
            perception_tx.send(queued).unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while telemetry.calls_total.load(Ordering::SeqCst) != 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("review did not reach semaphore admission");
            assert!(
                unix_now_ms() < expires_at,
                "review expired before it was queued"
            );
            assert_eq!(resolver.validation_calls.load(Ordering::SeqCst), 0);
            assert_eq!(store.reservation_calls.load(Ordering::SeqCst), 1);
            assert_eq!(resolver.context_calls.load(Ordering::SeqCst), 1);
            for tick in 56..=58 {
                let selections = resolver.selection_calls.load(Ordering::SeqCst);
                let processed = telemetry.preparations_processed.load(Ordering::SeqCst);
                let mut duplicate = recovery_test_perception();
                duplicate.agent_id = expected.agent_id();
                duplicate.is_directly_addressed = true;
                duplicate.tick = Tick(tick);
                perception_tx.send(duplicate).unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while resolver.selection_calls.load(Ordering::SeqCst) <= selections
                        || telemetry.preparations_processed.load(Ordering::SeqCst) == processed
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("duplicate queued perception was not evaluated");
                assert_eq!(
                    activity.lock().unwrap().get(&expected.agent_id()),
                    Some(&55)
                );
            }
            assert_eq!(resolver.context_calls.load(Ordering::SeqCst), 1);
            assert_eq!(telemetry.calls_total.load(Ordering::SeqCst), 2);
            tokio::time::timeout(Duration::from_secs(3), async {
                while unix_now_ms() < expires_at {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("review did not expire within its bound");
            release.notify_one();
            tokio::time::timeout(Duration::from_secs(2), async {
                while telemetry.providers_retired.load(Ordering::SeqCst) < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("expired queued provider job did not actually retire");
            assert_eq!(resolver.validation_calls.load(Ordering::SeqCst), 0);
            let processed = telemetry.preparations_processed.load(Ordering::SeqCst);
            let mut retry = recovery_test_perception();
            retry.agent_id = expected.agent_id();
            retry.tick = Tick(60);
            perception_tx.send(retry).unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while telemetry.preparations_processed.load(Ordering::SeqCst) == processed {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("failed admission retained exact-request preparation ownership");
            shutdown_tx.send(true).unwrap();
            tokio::time::timeout(Duration::from_secs(2), bridge)
                .await
                .expect("bridge did not drain")
                .unwrap()
                .unwrap();
            assert_eq!(resolver.validation_calls.load(Ordering::SeqCst), 0);
            assert_eq!(resolver.context_calls.load(Ordering::SeqCst), 2);
            assert_eq!(
                activity.lock().unwrap().get(&expected.agent_id()),
                Some(&60)
            );
            assert_eq!(store.reservation_calls.load(Ordering::SeqCst), 1);
            assert!(store
                .get_completion(&expected.request_id())
                .unwrap()
                .is_none());
            assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
            provider_task.abort();
            drop(perception_tx);
        }

        #[tokio::test]
        async fn completed_response_survives_restart_without_provider_or_action_replay() {
            let dir = tempfile::tempdir().unwrap();
            let event_path = dir.path().join("events.db");
            let state_path = dir.path().join("state.redb");
            let state_store = Arc::new(StateStore::open(state_path.to_str().unwrap()).unwrap());
            let provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let append_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let failure_observed = Arc::new(tokio::sync::Notify::new());
            let (gateway_url, provider_task) =
                start_mock_agent_provider(Arc::clone(&provider_calls), None, None).await;
            let config = LlmBridgeConfig {
                gateway_url,
                credential: "test-credential".to_string(),
                usage_v2_enabled: true,
                completion_retry_interval: Duration::from_secs(3600),
                completion_max_attempts: 3,
                ..Default::default()
            };
            let first_store = Arc::new(FailFirstCompletionStore {
                inner: EventStore::open(event_path.to_str().unwrap()).unwrap(),
                reservation_calls: std::sync::atomic::AtomicUsize::new(0),
                fail_next_usage: AtomicBool::new(true),
                append_calls: Arc::clone(&append_calls),
                failure_observed: Arc::clone(&failure_observed),
            });
            let (first_perception_tx, first_perception_rx) = mpsc::channel();
            let (action_tx, action_rx) = mpsc::channel();
            let (first_shutdown_tx, first_shutdown_rx) = watch::channel(false);
            let first_provider_admission = Arc::new(RwLock::new(true));
            let first_failure = failure_observed.notified();
            let first_bridge = tokio::spawn(run_llm_bridge_with_store(
                config.clone(),
                first_perception_rx,
                action_tx.clone(),
                Arc::new(BridgeTelemetry::default()),
                Arc::clone(&state_store),
                Arc::clone(&first_store),
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(HashMap::new())),
                first_shutdown_rx,
                first_provider_admission,
            ));
            first_perception_tx
                .send(recovery_test_perception())
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), first_failure)
                .await
                .expect("first usage append was not attempted");
            assert!(action_rx.try_recv().is_err());
            // Notification occurs inside append; the owned worker still has to
            // durably record that failure before this checkpoint is observable.
            let pending = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let pending = first_store
                        .get_completion("agent-runtime-07-55")
                        .unwrap()
                        .unwrap();
                    if pending.attempt_count >= 1 {
                        break pending;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("first usage append failure was not durably recorded");
            assert_eq!(pending.status, "pending_usage");
            assert_eq!(pending.attempt_count, 1);

            // End the first bridge after the completed response is durable but before
            // the usage append succeeds, then construct a fresh store from the same DB.
            drop(first_perception_tx);
            drop(first_shutdown_tx);
            tokio::time::timeout(Duration::from_secs(5), first_bridge)
                .await
                .expect("first bridge did not shut down")
                .unwrap()
                .unwrap();
            drop(first_store);

            let second_store = Arc::new(FailFirstCompletionStore {
                inner: EventStore::open(event_path.to_str().unwrap()).unwrap(),
                reservation_calls: std::sync::atomic::AtomicUsize::new(0),
                fail_next_usage: AtomicBool::new(false),
                append_calls: Arc::clone(&append_calls),
                failure_observed,
            });
            let (second_perception_tx, second_perception_rx) = mpsc::channel();
            drop(second_perception_tx);
            let (second_shutdown_tx, second_shutdown_rx) = watch::channel(false);
            let second_provider_admission = Arc::new(RwLock::new(true));
            run_llm_bridge_with_store(
                config,
                second_perception_rx,
                action_tx,
                Arc::new(BridgeTelemetry::default()),
                state_store,
                Arc::clone(&second_store),
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(HashMap::new())),
                second_shutdown_rx,
                second_provider_admission,
            )
            .await
            .unwrap();
            drop(second_shutdown_tx);

            let action = action_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(action.agent_id, AgentId(7));
            assert_eq!(action.action_type, ActionType::Move);
            assert_eq!(action.target_room.as_deref(), Some("meeting-room"));
            assert!(action_rx.try_recv().is_err());
            assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
            assert_eq!(append_calls.load(Ordering::SeqCst), 2);
            let usage_events = second_store
                .inner
                .get_events_since(0, 10)
                .unwrap()
                .into_iter()
                .filter(|event| event.event_type == "agent_llm_usage")
                .collect::<Vec<_>>();
            assert_eq!(usage_events.len(), 1);
            assert!(second_store
                .get_completion("agent-runtime-07-55")
                .unwrap()
                .is_none());
            provider_task.abort();
        }

        #[tokio::test]
        async fn project_reservation_allows_one_provider_effect_across_new_perceptions() {
            let dir = tempfile::tempdir().unwrap();
            let event_path = dir.path().join("events.db");
            let state_path = dir.path().join("state.redb");
            let state_store = Arc::new(StateStore::open(state_path.to_str().unwrap()).unwrap());
            let event_store = Arc::new(EventStore::open(event_path.to_str().unwrap()).unwrap());
            let provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (gateway_url, provider_task) =
                start_mock_agent_provider(Arc::clone(&provider_calls), None, None).await;
            let config = LlmBridgeConfig {
                gateway_url,
                credential: "test-credential".to_owned(),
                min_ticks_between_calls: 0,
                usage_v2_enabled: true,
                provider_usage_authority: Some(Arc::new(StaticProviderUsageAuthority {
                    authority: ProviderUsageAuthority {
                        tenant_id: "tenant-m0".to_owned(),
                        project_id: "project-m0".to_owned(),
                        work_item_id: "build-site".to_owned(),
                        reservation_id: "reservation-m0".to_owned(),
                        assignment_id: "assignment-m0".to_owned(),
                        assignment_version: 1,
                        agent_id: AgentId(7),
                        provider: "local-loop".to_owned(),
                        subscription_grant: None,
                    },
                })),
                ..Default::default()
            };
            let (perception_tx, perception_rx) = mpsc::channel();
            let (action_tx, _action_rx) = mpsc::channel();
            let (shutdown_tx, shutdown_rx) = watch::channel(false);
            let activity_ticks = Arc::new(Mutex::new(HashMap::new()));
            let telemetry = Arc::new(BridgeTelemetry::default());
            let bridge = tokio::spawn(run_llm_bridge_with_store(
                config,
                perception_rx,
                action_tx,
                Arc::clone(&telemetry),
                state_store,
                Arc::clone(&event_store),
                Arc::new(AtomicBool::new(false)),
                Arc::clone(&activity_ticks),
                shutdown_rx,
                Arc::new(RwLock::new(true)),
            ));

            perception_tx.send(recovery_test_perception()).unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !event_store
                    .has_event_operation_id("llm_usage_company-provider-reservation-m0")
                    .unwrap()
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("project usage event was not persisted");

            let mut second = recovery_test_perception();
            second.tick = Tick(56);
            second.impulse_text = "continue".to_owned();
            perception_tx.send(second).unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while telemetry.preparations_processed.load(Ordering::SeqCst) < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("second perception was not evaluated");

            shutdown_tx.send(true).unwrap();
            tokio::time::timeout(Duration::from_secs(2), bridge)
                .await
                .expect("bridge did not shut down")
                .unwrap()
                .unwrap();
            assert_eq!(telemetry.preparations_processed.load(Ordering::SeqCst), 2);
            assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
            let usage_count = event_store
                .get_all_events()
                .unwrap()
                .into_iter()
                .filter(|event| event.event_type == "agent_llm_usage")
                .count();
            assert_eq!(usage_count, 1);
            let usage = event_store
                .event_by_operation_id("llm_usage_company-provider-reservation-m0")
                .unwrap()
                .unwrap();
            assert_eq!(usage.schema_version, 3);
            assert!(usage.payload.contains("\"tenant_id\":\"tenant-m0\""));
            assert!(usage
                .payload
                .contains("\"reservation_id\":\"reservation-m0\""));
            provider_task.abort();
        }

        #[tokio::test]
        async fn connect_failure_releases_only_undispatched_reservation() {
            let dir = tempfile::tempdir().unwrap();
            let event_path = dir.path().join("events.db");
            let state_path = dir.path().join("state.redb");
            let state_store = Arc::new(StateStore::open(state_path.to_str().unwrap()).unwrap());
            let event_store = Arc::new(EventStore::open(event_path.to_str().unwrap()).unwrap());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let unavailable_address = listener.local_addr().unwrap();
            drop(listener);
            let config = LlmBridgeConfig {
                gateway_url: format!("http://{unavailable_address}"),
                credential: "test-credential".to_string(),
                request_timeout: Duration::from_secs(1),
                ..Default::default()
            };
            let (perception_tx, perception_rx) = mpsc::channel();
            let (action_tx, _action_rx) = mpsc::channel();
            let (shutdown_tx, shutdown_rx) = watch::channel(false);
            let telemetry = Arc::new(BridgeTelemetry::default());
            let bridge = tokio::spawn(run_llm_bridge_with_store(
                config,
                perception_rx,
                action_tx,
                Arc::clone(&telemetry),
                state_store,
                Arc::clone(&event_store),
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(HashMap::new())),
                shutdown_rx,
                Arc::new(RwLock::new(true)),
            ));

            perception_tx.send(recovery_test_perception()).unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while telemetry.calls_failed.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("connect failure was not observed");
            assert!(event_store
                .get_llm_completion("agent-runtime-07-55")
                .unwrap()
                .is_none());

            shutdown_tx.send(true).unwrap();
            tokio::time::timeout(Duration::from_secs(2), bridge)
                .await
                .expect("bridge did not shut down")
                .unwrap()
                .unwrap();
        }

        #[tokio::test]
        async fn shutdown_drains_reserved_provider_call_before_returning() {
            let dir = tempfile::tempdir().unwrap();
            let event_path = dir.path().join("events.db");
            let state_path = dir.path().join("state.redb");
            let state_store = Arc::new(StateStore::open(state_path.to_str().unwrap()).unwrap());
            let event_store = Arc::new(EventStore::open(event_path.to_str().unwrap()).unwrap());
            let provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let request_observed = Arc::new(tokio::sync::Notify::new());
            let release_response = Arc::new(tokio::sync::Notify::new());
            let _release_on_drop = ReleaseOnDrop(Arc::clone(&release_response));
            let (gateway_url, provider_task) = start_mock_agent_provider(
                Arc::clone(&provider_calls),
                Some(Arc::clone(&request_observed)),
                Some(Arc::clone(&release_response)),
            )
            .await;
            let config = LlmBridgeConfig {
                gateway_url,
                credential: "test-credential".to_string(),
                max_concurrent: 1,
                usage_v2_enabled: true,
                completion_retry_interval: Duration::from_secs(3600),
                shutdown_drain_timeout: Duration::from_secs(2),
                ..Default::default()
            };
            let (perception_tx, perception_rx) = mpsc::channel();
            let (action_tx, action_rx) = mpsc::channel();
            let (shutdown_tx, shutdown_rx) = watch::channel(false);
            let provider_admission = Arc::new(RwLock::new(true));
            let telemetry = Arc::new(BridgeTelemetry::default());
            let bridge = tokio::spawn(run_llm_bridge_with_store(
                config,
                perception_rx,
                action_tx,
                Arc::clone(&telemetry),
                state_store,
                Arc::clone(&event_store),
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(HashMap::new())),
                shutdown_rx,
                Arc::clone(&provider_admission),
            ));

            perception_tx.send(recovery_test_perception()).unwrap();
            tokio::time::timeout(Duration::from_secs(2), request_observed.notified())
                .await
                .expect("provider request was not observed");

            let mut queued_perception = recovery_test_perception();
            queued_perception.agent_id = AgentId(8);
            queued_perception.tick = Tick(56);
            queued_perception.is_directly_addressed = true;
            perception_tx.send(queued_perception).unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while telemetry.calls_total.load(Ordering::SeqCst) != 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("urgent provider task did not reach semaphore admission");

            stop_provider_admission(provider_admission.as_ref());
            shutdown_tx.send(true).unwrap();
            tokio::time::sleep(Duration::from_millis(25)).await;
            assert!(!bridge.is_finished());
            release_response.notify_one();

            tokio::time::timeout(Duration::from_secs(2), bridge)
                .await
                .expect("bridge did not drain")
                .unwrap()
                .unwrap();
            let action = action_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(action.agent_id, AgentId(7));
            assert!(event_store
                .get_llm_completion("agent-runtime-07-55")
                .unwrap()
                .is_none());
            assert!(event_store
                .get_llm_completion("agent-runtime-08-56")
                .unwrap()
                .is_none());
            assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
            provider_task.abort();
        }

        #[tokio::test]
        async fn shutdown_timeout_fails_closed_with_reserved_request() {
            let dir = tempfile::tempdir().unwrap();
            let event_path = dir.path().join("events.db");
            let state_path = dir.path().join("state.redb");
            let state_store = Arc::new(StateStore::open(state_path.to_str().unwrap()).unwrap());
            let event_store = Arc::new(EventStore::open(event_path.to_str().unwrap()).unwrap());
            let provider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let request_observed = Arc::new(tokio::sync::Notify::new());
            let release_response = Arc::new(tokio::sync::Notify::new());
            let _release_on_drop = ReleaseOnDrop(Arc::clone(&release_response));
            let (gateway_url, provider_task) = start_mock_agent_provider(
                Arc::clone(&provider_calls),
                Some(Arc::clone(&request_observed)),
                Some(release_response),
            )
            .await;
            let config = LlmBridgeConfig {
                gateway_url,
                credential: "test-credential".to_string(),
                shutdown_drain_timeout: Duration::from_millis(25),
                ..Default::default()
            };
            let (perception_tx, perception_rx) = mpsc::channel();
            let (action_tx, _action_rx) = mpsc::channel();
            let (shutdown_tx, shutdown_rx) = watch::channel(false);
            let provider_admission = Arc::new(RwLock::new(true));
            let bridge = tokio::spawn(run_llm_bridge_with_store(
                config,
                perception_rx,
                action_tx,
                Arc::new(BridgeTelemetry::default()),
                state_store,
                Arc::clone(&event_store),
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(HashMap::new())),
                shutdown_rx,
                Arc::clone(&provider_admission),
            ));

            perception_tx.send(recovery_test_perception()).unwrap();
            tokio::time::timeout(Duration::from_secs(2), request_observed.notified())
                .await
                .expect("provider request was not observed");
            stop_provider_admission(provider_admission.as_ref());
            shutdown_tx.send(true).unwrap();
            let result = tokio::time::timeout(Duration::from_secs(2), bridge)
                .await
                .expect("bridge did not fail closed")
                .unwrap();
            assert_eq!(result, Err("llm_provider_drain_timeout"));
            assert_eq!(
                event_store
                    .get_llm_completion("agent-runtime-07-55")
                    .unwrap()
                    .unwrap()
                    .status,
                "failed"
            );
            assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
            provider_task.abort();
        }

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

            let request = build_gateway_request(&perception, &store, "agent-runtime-07-55", None);

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
                CircuitBreaker::new(2, Duration::from_secs(1), Arc::clone(&open_signal));

            assert!(!breaker.is_open());
            assert!(!open_signal.load(Ordering::Relaxed));

            breaker.record_failure();
            assert!(!breaker.is_open());
            assert!(!open_signal.load(Ordering::Relaxed));

            breaker.record_failure();
            assert!(breaker.is_open());
            assert!(open_signal.load(Ordering::Relaxed));

            // Expire the stored deadline directly so instrumentation cannot race the clock.
            breaker.last_failure = Some(Instant::now() - breaker.reset_duration);
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
        fn insert_prefer_heard_preserves_incoming_im_when_retaining_older_heard_frame() {
            let mut batch = HashMap::new();
            let chat = make_perception(16, "Thomas sagte: Hallo", false);
            let original_tick = chat.tick;
            let original_body = chat.body_text.clone();
            insert_prefer_heard(&mut batch, chat);

            let mut gaia = make_perception(16, "", true);
            gaia.tick = Tick(original_tick.0 + 1);
            insert_prefer_heard(&mut batch, gaia);

            let merged = batch.get(&AgentId(16)).unwrap();
            assert_eq!(merged.heard_text, "Thomas sagte: Hallo");
            assert_eq!(merged.tick, original_tick);
            assert_eq!(merged.body_text, original_body);
            assert!(merged.has_operator_impulse);
            assert!(merged.synth_fingerprint.contains("|IM:1"));
            assert!(merged.synth_fingerprint.contains("|HR:1"));
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

        #[test]
        fn build_usage_event_reconstructs_fresh_input_and_keys() {
            // #427: the daemon recovers fresh input from the folded input and keys
            // the event by AGENT-NN + request_id (deterministic operation_id).
            let resp = GatewayResponse {
                content: String::new(),
                decision: "forward".to_owned(),
                actions: vec![],
                tokens_used: 1800,
                request_id: "req-abc".to_string(),
                provider: "anthropic-direct".to_string(),
                input_tokens: 1300, // folded (fresh 1000 + cache 200 + 100)
                output_tokens: 500,
                cache_read: 200,
                cache_creation: 100,
                tier: "high".to_string(),
                cost_usd: 0.0195,
                hierarchy_tier: Some(HierarchyTier::TIER_2),
                cost_source: Some(CostSource::ProviderReported),
                effective_model: "claude-sonnet-5".to_string(),
            };
            let ev =
                build_usage_event(AgentId(8), 42, "claude-sonnet-5", None, &resp, true).unwrap();
            assert_eq!(ev.event_type, "agent_llm_usage");
            assert_eq!(ev.aggregate_id, "AGENT-08");
            assert_eq!(ev.correlation_id, "req-abc");
            assert_eq!(ev.operation_id, "llm_usage_req-abc");
            assert_eq!(ev.tick, 42);
            assert!(ev.payload.contains("\"input_tokens\":1000"));
            assert!(ev.payload.contains("\"cache_read\":200"));
            assert!(ev.payload.contains("\"cache_creation\":100"));
            assert!(ev.payload.contains("\"tier\":\"high\""));
            assert!(ev.payload.contains("\"hierarchy_tier\":2"));
            assert!(ev.payload.contains("\"cost_source\":\"provider_reported\""));
            assert!(ev
                .payload
                .contains("\"effective_model\":\"claude-sonnet-5\""));
            assert!(ev.payload.contains("\"provider\":\"anthropic-direct\""));
            assert!(ev.payload.contains("\"caller_role\":\"agent_runtime\""));
            assert_eq!(ev.schema_version, 2);
        }

        #[test]
        fn project_provider_usage_is_reservation_keyed_and_authority_bound() {
            let perception = make_perception(8, "Bitte bearbeite das Projekt.", true);
            let authority = ProviderUsageAuthority {
                tenant_id: "tenant-m0".to_owned(),
                project_id: "project-m0".to_owned(),
                work_item_id: "build-site".to_owned(),
                reservation_id: "reservation-m0".to_owned(),
                assignment_id: "assignment-m0".to_owned(),
                assignment_version: 2,
                agent_id: AgentId(8),
                provider: "local-loop".to_owned(),
                subscription_grant: None,
            };
            assert_eq!(
                agent_runtime_request_id(&perception, Some(&authority.clone().into())),
                "company-provider-reservation-m0"
            );
            assert_eq!(
                validate_provider_usage_mode(Some(&authority.clone().into()), true, false),
                Ok(())
            );
            assert!(
                validate_provider_usage_mode(Some(&authority.clone().into()), false, false)
                    .is_err()
            );
            assert_eq!(validate_provider_usage_mode(None, false, true), Ok(()));
            let mut response = GatewayResponse {
                content: "done".to_owned(),
                decision: "forward".to_owned(),
                actions: Vec::new(),
                tokens_used: 0,
                request_id: "company-provider-reservation-m0".to_owned(),
                provider: "local-loop".to_owned(),
                input_tokens: 10,
                output_tokens: 2,
                cache_read: 0,
                cache_creation: 0,
                tier: "mid".to_owned(),
                cost_usd: 0.0,
                hierarchy_tier: Some(HierarchyTier::TIER_2),
                cost_source: Some(CostSource::ProviderReported),
                effective_model: "local-loop-tier2".to_owned(),
            };
            let event = build_usage_event(
                AgentId(8),
                42,
                "",
                Some(&authority.clone().into()),
                &response,
                true,
            )
            .unwrap();
            assert_eq!(event.schema_version, 3);
            assert!(event.payload.contains("\"tenant_id\":\"tenant-m0\""));
            assert!(event.payload.contains("\"project_id\":\"project-m0\""));
            assert!(event.payload.contains("\"work_item_id\":\"build-site\""));
            assert!(event
                .payload
                .contains("\"reservation_id\":\"reservation-m0\""));
            assert!(event
                .payload
                .contains("\"requested_model\":\"gateway-policy-default\""));

            let resolver = StaticProviderUsageAuthority {
                authority: authority.clone(),
            };
            assert_eq!(
                validate_gateway_completion_authority(
                    Some(&resolver),
                    Some(&authority.clone().into()),
                    &response,
                    AgentId(8),
                ),
                Ok(())
            );
            response.provider = "anthropic-direct".to_owned();
            assert!(validate_gateway_completion_authority(
                Some(&resolver),
                Some(&authority.clone().into()),
                &response,
                AgentId(8),
            )
            .is_err());
            response.provider = "local-loop".to_owned();
            let stale_resolver = StaticProviderUsageAuthority {
                authority: ProviderUsageAuthority {
                    assignment_version: authority.assignment_version + 1,
                    ..authority.clone()
                },
            };
            assert!(validate_gateway_completion_authority(
                Some(&stale_resolver),
                Some(&authority.clone().into()),
                &response,
                AgentId(8),
            )
            .is_err());
        }

        #[test]
        fn usage_v2_rejects_missing_gateway_resolution() {
            let resp: GatewayResponse =
                serde_json::from_str(r#"{"tier":"mid","cost_usd":0,"request_id":"missing-v2"}"#)
                    .unwrap();
            assert!(build_usage_event(AgentId(1), 1, "", None, &resp, true).is_err());
            assert!(build_usage_event(AgentId(1), 1, "", None, &resp, false).is_ok());
        }

        #[test]
        fn issue_395_gateway_response_golden_decodes_in_rust() {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/contracts/issue-395-agent-runtime-response-v2.json");
            let golden = std::fs::read_to_string(path).expect("read shared Go/Rust fixture");
            let response: GatewayResponse =
                serde_json::from_str(&golden).expect("decode shared gateway response");

            assert_eq!(response.hierarchy_tier, Some(HierarchyTier::TIER_2));
            assert_eq!(response.cost_source, Some(CostSource::ProviderReported));
            assert_eq!(response.effective_model, "claude-sonnet-5");
            assert!(
                build_usage_event(AgentId(8), 42, "claude-sonnet-5", None, &response, true).is_ok()
            );
        }
    }
}
