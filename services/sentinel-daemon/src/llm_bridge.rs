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

        fn model_work_context(
            &self,
            _authority: &ProviderExecutionAuthority,
        ) -> Result<Option<ModelWorkContext>, &'static str> {
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

    fn reserve_provider_request<S: CompletionStore>(
        admission: &RwLock<bool>,
        store: &S,
        request_id: &str,
        request_digest: &str,
        agent_id: &str,
    ) -> anyhow::Result<Option<bool>> {
        let open = admission
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*open {
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

    fn model_reservation(
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
            if let Err(error) = store.persist_usage(
                &entry.request_id,
                &entry.request_digest,
                &completed.usage_event,
            ) {
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
            let result = resolver
                .ok_or("model work resolver unavailable")
                .and_then(|resolver| {
                    resolver.admit_model_work(model_work, &entry.request_id, &entry.request_digest)
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

        let claimed = match store.claim_actions(&entry.request_id, &entry.request_digest) {
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
        if let Err(error) = store.complete_actions(&entry.request_id, &entry.request_digest) {
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
                let current = resolver
                    .resolve_provider_usage_authority(agent_id)
                    .map_err(|reason| format!("provider usage reauthorization failed: {reason}"))?;
                if current.as_ref() != expected {
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

    fn validate_pre_dispatch_provider_authority<S: CompletionStore>(
        store: &S,
        resolver: Option<&dyn ProviderUsageAuthorityResolver>,
        expected: Option<&ProviderExecutionAuthority>,
        agent_id: AgentId,
        request_id: &str,
        request_digest: &str,
        model_work: Option<&ModelWorkContext>,
    ) -> Result<(), String> {
        let validation = validate_current_provider_usage_authority(resolver, expected, agent_id)
            .and_then(|()| validate_model_work_context(resolver, model_work))
            .and_then(|()| {
                if let Some(context) = model_work {
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
        }
        Ok(())
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
        let payload = serde_json::to_string(&completed).map_err(|error| error.to_string())?;
        if let Err(error) = store.enqueue_completion(request_id, request_digest, &payload) {
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
        let entry = store
            .get_completion(request_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("durable LLM completion {request_id} disappeared"))?;
        recover_completion(store, entry, action_tx, max_attempts, authority_resolver);
        Ok(())
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

    async fn run_llm_bridge_with_store<S: CompletionStore + 'static>(
        config: LlmBridgeConfig,
        perception_rx: mpsc::Receiver<Perception>,
        action_tx: mpsc::Sender<AgentAction>,
        telemetry: Arc<BridgeTelemetry>,
        state_store: Arc<StateStore>,
        event_store: Arc<S>,
        llm_unavailable: Arc<AtomicBool>,
        llm_activity_ticks: SharedLlmActivityTicks,
        mut shutdown_rx: watch::Receiver<bool>,
        provider_admission: Arc<RwLock<bool>>,
    ) -> std::result::Result<(), &'static str> {
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
                return Ok(());
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
        let mut provider_tasks = JoinSet::new();
        let active_provider_requests: ActiveProviderRequests = Arc::new(Mutex::new(HashMap::new()));
        let mut provider_task_failed = false;
        // Debounce: Operator-Impulse (Gaia/Broadcast) nur beim ERSTEN Tick urgent,
        // danach 60 Ticks Cooldown. Verhindert Semaphore-Starvation bei 300-Tick TTL.
        let mut impulse_acked: HashMap<AgentId, u64> = HashMap::new();

        // Recover provider results committed by an earlier process before accepting
        // new perceptions. The periodic worker has a bounded per-record attempt budget
        // and is explicitly cancelled when the bridge receiver closes.
        let completion_max_attempts = config.completion_max_attempts.max(1);
        let completion_retry_interval = if config.completion_retry_interval.is_zero() {
            Duration::from_secs(1)
        } else {
            config.completion_retry_interval
        };
        classify_stale_provider_requests(
            event_store.as_ref(),
            &active_provider_requests,
            unix_now_ms(),
            config.request_timeout,
        );
        recover_completion_batch(
            event_store.as_ref(),
            &action_tx,
            completion_max_attempts,
            config.provider_usage_authority.as_deref(),
        );
        let (recovery_shutdown_tx, mut recovery_shutdown_rx) = tokio::sync::watch::channel(false);
        let recovery_store = Arc::clone(&event_store);
        let recovery_action_tx = action_tx.clone();
        let recovery_interval = completion_retry_interval;
        let recovery_max_attempts = completion_max_attempts;
        let recovery_resolver = config.provider_usage_authority.clone();
        let recovery_request_timeout = config.request_timeout;
        let recovery_active = Arc::clone(&active_provider_requests);
        let recovery_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(recovery_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Startup recovery was performed synchronously above.
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        classify_stale_provider_requests(
                            recovery_store.as_ref(), &recovery_active,
                            unix_now_ms(), recovery_request_timeout,
                        );
                        recover_completion_batch(
                            recovery_store.as_ref(), &recovery_action_tx,
                            recovery_max_attempts, recovery_resolver.as_deref(),
                        );
                    },
                    changed = recovery_shutdown_rx.changed() => {
                        if changed.is_err() || *recovery_shutdown_rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });

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

        loop {
            let first = tokio::select! {
                biased;
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        stop_provider_admission(provider_admission.as_ref());
                        break;
                    }
                    continue;
                }
                perception = async_rx.recv() => match perception {
                    Some(perception) => perception,
                    None => {
                        stop_provider_admission(provider_admission.as_ref());
                        break;
                    }
                },
            };
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

                if let Some(resolver) = config.provider_usage_authority.as_ref() {
                    match resolver.is_provider_usage_candidate(agent_id) {
                        Ok(true) => {}
                        Ok(false) => continue,
                        Err(reason) => {
                            error!(agent = %agent_id, reason, "Provider usage candidate selection failed closed");
                            continue;
                        }
                    }
                }

                let usage_authority = match config.provider_usage_authority.as_ref() {
                    Some(resolver) => match resolver.resolve_provider_usage_authority(agent_id) {
                        Ok(value) => value,
                        Err(reason) => {
                            error!(agent = %agent_id, reason, "Provider usage authority resolution failed closed");
                            continue;
                        }
                    },
                    None => None,
                };
                if usage_authority
                    .as_ref()
                    .is_some_and(|authority| authority.agent_id() != agent_id)
                {
                    error!(agent = %agent_id, "Provider usage authority returned another agent");
                    continue;
                }
                let unbound_allowed = config
                    .provider_usage_authority
                    .as_ref()
                    .is_none_or(|resolver| resolver.allows_unbound_provider_usage());
                if usage_authority.is_none() && !unbound_allowed {
                    continue;
                }
                if let Err(reason) = validate_provider_usage_mode(
                    usage_authority.as_ref(),
                    config.usage_v2_enabled,
                    unbound_allowed,
                ) {
                    error!(agent = %agent_id, reason, "Provider usage accounting is unavailable");
                    continue;
                }

                last_call_tick.insert(agent_id, current_tick);
                {
                    let mut ticks = llm_activity_ticks.lock().unwrap();
                    ticks.insert(agent_id, current_tick);
                    let retention_ticks =
                        config.min_ticks_between_calls.saturating_mul(24).max(120);
                    ticks.retain(|_, last_tick| {
                        current_tick.saturating_sub(*last_tick) <= retention_ticks
                    });
                }

                let request_id = agent_runtime_request_id(&perception, usage_authority.as_ref());
                if active_provider_requests
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .contains_key(&request_id)
                {
                    continue;
                }
                if let Some(authority) = usage_authority.as_ref() {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis()
                        .try_into()
                        .unwrap_or(u64::MAX);
                    match recover_reserved_model_request(
                        event_store.as_ref(),
                        config.provider_usage_authority.as_deref(),
                        authority,
                        &action_tx,
                        completion_max_attempts,
                        now_ms,
                        config.request_timeout,
                    ) {
                        Ok(true) => continue,
                        Ok(false) => {}
                        Err(error) => {
                            error!(request_id, error = %error, "Reserved model recovery failed closed");
                            continue;
                        }
                    }
                }
                let mut request = build_gateway_request(
                    &perception,
                    &state_store,
                    &request_id,
                    usage_authority.as_ref(),
                );
                let model_work = match (
                    config.provider_usage_authority.as_deref(),
                    usage_authority.as_ref(),
                ) {
                    (Some(resolver), Some(authority)) => {
                        match resolver.model_work_context(authority) {
                            Ok(context) => context,
                            Err(reason) => {
                                warn!(agent = %agent_id, reason, "Model work context unavailable");
                                continue;
                            }
                        }
                    }
                    _ => None,
                };
                if let Some(context) = &model_work {
                    if let Err(reason) = bind_model_work_request(&mut request, context) {
                        warn!(agent = %agent_id, reason, "Model work request rejected");
                        continue;
                    }
                }
                let request_digest = match gateway_request_digest(&request) {
                    Ok(digest) => digest,
                    Err(error) => {
                        error!(agent = %agent_id, error = %error, "Gateway request digest failed");
                        continue;
                    }
                };
                match event_store.get_completion(&request_id) {
                    Ok(Some(entry))
                        if entry.request_digest == request_digest
                            && entry.status == "provider_in_flight" =>
                    {
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis()
                            .try_into()
                            .unwrap_or(u64::MAX);
                        if release_stale_undispatched_subscription(
                            event_store.as_ref(),
                            config.provider_usage_authority.as_deref(),
                            usage_authority.as_ref(),
                            &entry,
                            now_ms,
                            config.request_timeout,
                        ) {
                            info!(request_id = %request_id, "Definitively undispatched provider reservation released for retry");
                            continue;
                        }
                        warn!(request_id = %request_id, "Ambiguous prior provider execution remains fail-closed");
                        continue;
                    }
                    Ok(Some(entry)) if entry.request_digest == request_digest => {
                        recover_completion(
                            event_store.as_ref(),
                            entry,
                            &action_tx,
                            completion_max_attempts,
                            config.provider_usage_authority.as_deref(),
                        );
                        continue;
                    }
                    Ok(Some(_)) => {
                        error!(request_id = %request_id, "Stable LLM request ID reused with different request digest");
                        continue;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        error!(request_id = %request_id, error = %error, "LLM completion lookup failed closed");
                        continue;
                    }
                }
                let operation_id = format!("llm_usage_{request_id}");
                match event_store.has_operation(&operation_id) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(error) => {
                        error!(request_id = %request_id, error = %error, "LLM usage lookup failed closed");
                        continue;
                    }
                }

                info!(agent = %agent_id,
                    priority = if perception.is_directly_addressed { "P1" } else { "normal" },
                    has_heard = !perception.heard_text.is_empty(),
                    "LLM call triggered");

                let client = client.clone();
                let url = format!("{}/internal/agent-runtime", config.gateway_url);
                let action_tx = action_tx.clone();
                let telemetry = Arc::clone(&telemetry);
                let cb = Arc::clone(&circuit_breaker);
                let retry_queue = Arc::clone(&pending_retries);
                let retry_perception = perception.clone();
                let bridge_event_store = Arc::clone(&event_store);
                let credential = config.credential.clone();
                let usage_v2_enabled = config.usage_v2_enabled;
                let request_completion_max_attempts = completion_max_attempts;
                let task_provider_admission = Arc::clone(&provider_admission);
                let task_active = Arc::clone(&active_provider_requests);
                let provider_usage_authority = usage_authority.clone();
                let provider_usage_authority_resolver =
                    config.provider_usage_authority.as_ref().map(Arc::clone);

                telemetry.calls_total.fetch_add(1, Ordering::Relaxed);

                if is_urgent {
                    // Urgent (heard_text/P1): acquire_owned().await INNERHALB tokio::spawn.
                    // Wartet auf Permit im eigenen Task — Drain-Loop blockiert NICHT,
                    // urgent Calls werden NIEMALS gedroppt.
                    let sem = llm_semaphore.clone();
                    let mut task_shutdown_rx = shutdown_rx.clone();
                    provider_tasks.spawn(async move {
                        // Urgent Calls duerfen auf Semaphore und Gateway warten, aber nicht ewig.
                        let acquire_timeout = config.request_timeout;
                        let permit = tokio::select! {
                            biased;
                            _ = task_shutdown_rx.changed() => {
                                debug!(agent = %agent_id, "URGENT call cancelled before reservation during shutdown");
                                return;
                            }
                            result = tokio::time::timeout(acquire_timeout, sem.acquire_owned()) => {
                                match result {
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
                                }
                            }
                        };
                        let call_start = Instant::now();
                        let mut outcome_guard = ProviderOutcomeGuard::new(
                            Arc::clone(&bridge_event_store), task_active, &request_id, &request_digest,
                        );
                        match reserve_provider_request(
                            task_provider_admission.as_ref(),
                            bridge_event_store.as_ref(),
                            &request_id,
                            &request_digest,
                            &agent_id.to_string(),
                        ) {
                            Ok(Some(true)) => outcome_guard.armed = true,
                            Ok(Some(false)) => {
                                warn!(request_id = %request_id, "LLM provider request already reserved");
                                return;
                            }
                            Ok(None) => {
                                debug!(request_id = %request_id, "LLM call cancelled before reservation during shutdown");
                                return;
                            }
                            Err(error) => {
                                error!(request_id = %request_id, error = %error, "LLM provider reservation failed closed");
                                return;
                            }
                        }
                        if let Err(error) = validate_pre_dispatch_provider_authority(
                            bridge_event_store.as_ref(),
                            provider_usage_authority_resolver.as_deref(),
                            provider_usage_authority.as_ref(),
                            agent_id,
                            &request_id,
                            &request_digest,
                            model_work.as_ref(),
                        ) {
                            warn!(request_id = %request_id, error, "Provider request reauthorization failed before dispatch");
                            outcome_guard.disarm_if_resolved();
                            return;
                        }
                        match agent_runtime_request(
                            &client,
                            &url,
                            &credential,
                            &request_id,
                            &request_digest,
                            &request,
                        )
                        .send()
                        .await
                        {
                            Ok(response) => {
                                let status = response.status();
                                if status.is_success() {
                                    match response.json::<GatewayResponse>().await {
                                        Ok(gateway_resp) => {
                                            // Provider execution is complete. Release scarce
                                            // capacity before local durable recovery.
                                            drop(permit);
                                            if let Err(e) = store_gateway_completion(
                                                bridge_event_store.as_ref(),
                                                &action_tx,
                                                GatewayCompletionContext {
                                                    request_id: &request_id,
                                                    request_digest: &request_digest,
                                                    agent_id,
                                                    tick: current_tick,
                                                    requested_model: &request.model,
                                                    authority: provider_usage_authority.as_ref(),
                                                    authority_resolver: provider_usage_authority_resolver
                                                        .as_deref(),
                                                    gateway_response: &gateway_resp,
                                                    usage_v2_enabled,
                                                    model_work: model_work.as_ref(),
                                                },
                                                request_completion_max_attempts,
                                            ) {
                                                warn!(agent = %agent_id, error = %e, "Completed gateway response rejected fail-closed");
                                                outcome_guard.disarm_if_resolved();
                                                telemetry
                                                    .calls_failed
                                                    .fetch_add(1, Ordering::Relaxed);
                                                cb.lock().unwrap().record_failure();
                                                return;
                                            }
                                            let latency_ms = call_start.elapsed().as_millis();
                                            outcome_guard.armed = false;
                                            telemetry.calls_success.fetch_add(1, Ordering::Relaxed);
                                            telemetry.tokens_total.fetch_add(
                                                gateway_resp.tokens_used.max(0) as u64,
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
                                        }
                                        Err(e) => {
                                            warn!(agent = %agent_id, error = %e, "Gateway Response Parse-Fehler");
                                            telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                                            cb.lock().unwrap().record_failure();
                                        }
                                    }
                                } else {
                                    let provider_io = response
                                        .headers()
                                        .get("x-sentinel-provider-io")
                                        .and_then(|value| value.to_str().ok());
                                    let reservation_released = release_pre_provider_rejection(
                                        bridge_event_store.as_ref(),
                                        &request_id,
                                        &request_digest,
                                        provider_io,
                                    );
                                    warn!(agent = %agent_id, status = status.as_u16(), reservation_released, "Gateway HTTP Fehler");
                                    if reservation_released {
                                        outcome_guard.armed = false;
                                    }
                                    telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                                    cb.lock().unwrap().record_failure();
                                }
                            }
                            Err(e) => {
                                let is_timeout = e.is_timeout();
                                let reservation_released = release_connect_failed_request(
                                    bridge_event_store.as_ref(),
                                    &request_id,
                                    &request_digest,
                                    &e,
                                );
                                warn!(agent = %agent_id, error = %e, is_timeout = is_timeout, reservation_released, "Gateway Request fehlgeschlagen");
                                if reservation_released {
                                    outcome_guard.armed = false;
                                }
                                telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                                cb.lock().unwrap().record_failure();
                            }
                        }
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
                    provider_tasks.spawn(async move {
                        let call_start = Instant::now();
                        let mut outcome_guard = ProviderOutcomeGuard::new(
                            Arc::clone(&bridge_event_store), task_active, &request_id, &request_digest,
                        );
                        match reserve_provider_request(
                            task_provider_admission.as_ref(),
                            bridge_event_store.as_ref(),
                            &request_id,
                            &request_digest,
                            &agent_id.to_string(),
                        ) {
                            Ok(Some(true)) => outcome_guard.armed = true,
                            Ok(Some(false)) => {
                                warn!(request_id = %request_id, "LLM provider request already reserved");
                                return;
                            }
                            Ok(None) => {
                                debug!(request_id = %request_id, "LLM call cancelled before reservation during shutdown");
                                return;
                            }
                            Err(error) => {
                                error!(request_id = %request_id, error = %error, "LLM provider reservation failed closed");
                                return;
                            }
                        }
                        if let Err(error) = validate_pre_dispatch_provider_authority(
                            bridge_event_store.as_ref(),
                            provider_usage_authority_resolver.as_deref(),
                            provider_usage_authority.as_ref(),
                            agent_id,
                            &request_id,
                            &request_digest,
                            model_work.as_ref(),
                        ) {
                            warn!(request_id = %request_id, error, "Provider request reauthorization failed before dispatch");
                            outcome_guard.disarm_if_resolved();
                            return;
                        }
                        match agent_runtime_request(
                            &client,
                            &url,
                            &credential,
                            &request_id,
                            &request_digest,
                            &request,
                        )
                        .send()
                        .await
                        {
                            Ok(response) => {
                                let status = response.status();
                                if status.is_success() {
                                    match response.json::<GatewayResponse>().await {
                                        Ok(gateway_resp) => {
                                            drop(permit);
                                            if let Err(e) = store_gateway_completion(
                                                bridge_event_store.as_ref(),
                                                &action_tx,
                                                GatewayCompletionContext {
                                                    request_id: &request_id,
                                                    request_digest: &request_digest,
                                                    agent_id,
                                                    tick: current_tick,
                                                    requested_model: &request.model,
                                                    authority: provider_usage_authority.as_ref(),
                                                    authority_resolver: provider_usage_authority_resolver
                                                        .as_deref(),
                                                    gateway_response: &gateway_resp,
                                                    usage_v2_enabled,
                                                    model_work: model_work.as_ref(),
                                                },
                                                request_completion_max_attempts,
                                            ) {
                                                warn!(agent = %agent_id, error = %e, "Completed gateway response rejected fail-closed");
                                                outcome_guard.disarm_if_resolved();
                                                telemetry
                                                    .calls_failed
                                                    .fetch_add(1, Ordering::Relaxed);
                                                cb.lock().unwrap().record_failure();
                                                return;
                                            }
                                            let latency_ms = call_start.elapsed().as_millis();
                                            outcome_guard.armed = false;
                                            telemetry.calls_success.fetch_add(1, Ordering::Relaxed);
                                            telemetry.tokens_total.fetch_add(
                                                gateway_resp.tokens_used.max(0) as u64,
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
                                        }
                                        Err(e) => {
                                            warn!(agent = %agent_id, error = %e, "Gateway Response Parse-Fehler");
                                            telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                                            cb.lock().unwrap().record_failure();
                                        }
                                    }
                                } else {
                                    let provider_io = response
                                        .headers()
                                        .get("x-sentinel-provider-io")
                                        .and_then(|value| value.to_str().ok());
                                    let reservation_released = release_pre_provider_rejection(
                                        bridge_event_store.as_ref(),
                                        &request_id,
                                        &request_digest,
                                        provider_io,
                                    );
                                    warn!(agent = %agent_id, status = status.as_u16(), reservation_released, "Gateway HTTP Fehler");
                                    if reservation_released {
                                        outcome_guard.armed = false;
                                    }
                                    telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                                    cb.lock().unwrap().record_failure();
                                }
                            }
                            Err(e) => {
                                let is_timeout = e.is_timeout();
                                let reservation_released = release_connect_failed_request(
                                    bridge_event_store.as_ref(),
                                    &request_id,
                                    &request_digest,
                                    &e,
                                );
                                warn!(agent = %agent_id, error = %e, is_timeout = is_timeout, reservation_released, "Gateway Request fehlgeschlagen");
                                if reservation_released {
                                    outcome_guard.armed = false;
                                }
                                telemetry.calls_failed.fetch_add(1, Ordering::Relaxed);
                                cb.lock().unwrap().record_failure();
                            }
                        }
                    });
                }
            }
            while let Some(result) = provider_tasks.try_join_next() {
                if let Err(join_error) = result {
                    provider_task_failed = true;
                    error!(error = %join_error, "LLM provider task failed");
                }
            }
        }

        let drain_result = tokio::time::timeout(config.shutdown_drain_timeout, async {
            while let Some(result) = provider_tasks.join_next().await {
                if let Err(join_error) = result {
                    provider_task_failed = true;
                    error!(error = %join_error, "LLM provider task failed during shutdown");
                }
            }
        })
        .await;
        if drain_result.is_err() {
            provider_tasks.abort_all();
            while provider_tasks.join_next().await.is_some() {}
            let _ = recovery_shutdown_tx.send(true);
            let _ = recovery_task.await;
            error!(
                timeout_ms = config.shutdown_drain_timeout.as_millis(),
                "LLM provider drain timed out"
            );
            return Err("llm_provider_drain_timeout");
        }
        let _ = recovery_shutdown_tx.send(true);
        if let Err(error) = recovery_task.await {
            error!(error = %error, "LLM completion recovery task failed");
            return Err("llm_recovery_task_failed");
        }
        if provider_task_failed {
            return Err("llm_provider_task_failed");
        }
        info!("LLM Bridge beendet");
        Ok(())
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
        } else if matches!(context, ModelWorkContext::AdaptiveLeadershipReview(_)) {
            request.metadata.insert(
                "company_execution_output_kind".to_owned(),
                "leadership_decision".to_owned(),
            );
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
            for grant in [
                serde_json::json!({"schema_version": 2}),
                serde_json::json!({"schema_version": 2, "subject": null}),
                serde_json::json!({"schema_version": 2, "subject": {"kind": "tool"}}),
                serde_json::json!({"schema_version": 1, "subject": {"kind": "unknown_model"}}),
                serde_json::json!({"schema_version": 3, "subject": {"kind": "unknown_model"}}),
            ] {
                assert!(leadership_review_kind(&grant).is_err());
            }
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
            bind_model_work_request(&mut request, &context).unwrap();
            assert!(!request.metadata.contains_key("leadership_review_kind"));
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
            bind_model_work_request(&mut a, &context).unwrap();
            bind_model_work_request(&mut same, &context).unwrap();
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
            fail_next_usage: AtomicBool,
            append_calls: Arc<std::sync::atomic::AtomicUsize>,
            failure_observed: Arc<tokio::sync::Notify>,
        }

        struct StaticProviderUsageAuthority {
            authority: ProviderUsageAuthority,
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
            let pending = first_store
                .get_completion("agent-runtime-07-55")
                .unwrap()
                .unwrap();
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
            let bridge = tokio::spawn(run_llm_bridge_with_store(
                config,
                perception_rx,
                action_tx,
                Arc::new(BridgeTelemetry::default()),
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
                while activity_ticks.lock().unwrap().get(&AgentId(7)) != Some(&56) {
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
            assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
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
