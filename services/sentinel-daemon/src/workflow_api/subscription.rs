//! Final dispatch consumes workflow authority, never an in-memory call counter.
use super::*;
use tracing::info;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchRequest {
    schema_version: u16,
    allowance_id: String,
    agent_id: u16,
    request_id: String,
    request_digest: String,
    context_digest: String,
    provider: String,
    model: String,
    catalog_digest: String,
    #[serde(default)]
    subject: Option<RequestSubject>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RequestSubject {
    AdaptiveLeadershipReview {
        review_id: Uuid,
        #[serde(default)]
        review_kind: Option<String>,
    },
    CustomerRequest {
        request_id: String,
        request_version: u64,
    },
    AdaptiveSession {
        session_id: Uuid,
        effect_id: Uuid,
        session_version: u64,
    },
    ProjectPlanning {
        project_id: ProjectId,
        project_version: u64,
    },
}

impl WorkflowApi {
    // Only the operator-secret-authenticated route calls this method. Ordinary
    // company APIs cannot submit ClaimSubscriptionCall or choose its principal.
    pub(crate) fn subscription_dispatch(&self, body: &[u8]) -> WorkflowHttpResponse {
        let request: DispatchRequest = match decode_body(body) {
            Ok(request) => request,
            Err(response) => return response,
        };
        let started = std::time::Instant::now();
        match self.claim_subscription_dispatch(&request) {
            Ok(deadline) => {
                info!(
                    request_id = %request.request_id,
                    agent_id = %request.agent_id,
                    schema_version = request.schema_version,
                    elapsed_ms = started.elapsed().as_millis(),
                    "Subscription dispatch accepted before provider I/O"
                );
                json(
                    200,
                    &serde_json::json!({
                        "schema_version": request.schema_version,
                        "allowance_id": request.allowance_id,
                        "request_id": request.request_id,
                        "request_digest": request.request_digest,
                        "deadline_unix_ms": deadline,
                    }),
                )
            }
            Err(reason) => {
                warn!(
                    allowance_id = %request.allowance_id,
                    request_id = %request.request_id,
                    agent_id = %request.agent_id,
                    schema_version = request.schema_version,
                    elapsed_ms = started.elapsed().as_millis(),
                    reason,
                    "Subscription dispatch denied before provider I/O"
                );
                json_error(
                    403,
                    "subscription_dispatch_denied",
                    "subscription dispatch authority unavailable or consumed",
                    false,
                )
            }
        }
    }

    fn claim_subscription_dispatch(&self, request: &DispatchRequest) -> Result<u64, &'static str> {
        // A delayed claim could consume authority after the Gateway times out.
        let _fence = self
            .mutation_fence
            .try_read()
            .map_err(|_| "workflow recovery active")?;
        let now_ms = now_unix_ms();
        if !self.enabled
            || !self.model_work_enabled
            || !matches!(request.schema_version, 1..=5)
            || ((request.schema_version == 4
                || (request.schema_version == 2 && !self.request_sales_autonomous_enabled))
                && self.subscription_allowance_id.as_deref() != Some(request.allowance_id.as_str()))
        {
            return Err("subscription mode unavailable");
        }
        if request.schema_version == 2 {
            return self.claim_sales_dispatch(request, now_ms);
        }
        if request.schema_version == 3 {
            return self.claim_adaptive_dispatch(request, now_ms);
        }
        if request.schema_version == 4 {
            return self.claim_project_planning_dispatch(request, now_ms);
        }
        if request.schema_version == 5 {
            return self.claim_leadership_dispatch(request, now_ms);
        }
        if request.subject.is_some() {
            return Err("subscription subject mismatch");
        }
        let binding = self
            .provider_usage_binding_for_agent(AgentId(request.agent_id))?
            .ok_or("project binding unavailable")?;
        let binding = crate::llm_bridge::bridge::ProviderUsageAuthority {
            tenant_id: binding.tenant_id,
            project_id: binding.project_id,
            work_item_id: binding.work_item_id,
            reservation_id: binding.reservation_id,
            assignment_id: binding.assignment_id,
            assignment_version: binding.assignment_version,
            agent_id: binding.agent_id,
            provider: binding.provider,
            subscription_grant: binding.subscription_grant,
        };
        let grant = binding
            .subscription_grant
            .as_ref()
            .ok_or("subscription grant unavailable")?;
        if binding.reservation_id != request.allowance_id
            || grant.provider != request.provider
            || grant.model != request.model
            || grant.catalog_digest != request.catalog_digest
            || request.request_id != format!("company-provider-{}", request.allowance_id)
        {
            return Err("subscription binding changed");
        }
        let context = self
            .prepare_model_work(&binding)?
            .ok_or("model context unavailable")?;
        context.validate_dispatch(now_ms)?;
        let context_bytes = serde_json::to_vec(&context).map_err(|_| "model context invalid")?;
        if format!("{:x}", Sha256::digest(context_bytes)) != request.context_digest {
            return Err("model context changed");
        }
        self.validate_company_employee(
            &self
                .principals
                .principal(&context.authority.principal.principal_id)
                .ok_or("subscription principal unavailable")?
                .principal,
        )?;
        let event_store = self
            .event_store
            .as_ref()
            .ok_or("request store unavailable")?;
        let pending = event_store
            .get_llm_completion(&request.request_id)
            .map_err(|_| "request reservation unavailable")?
            .ok_or("request not reserved")?;
        if pending.request_digest != request.request_digest
            || pending.status != "provider_in_flight"
            || !pending.payload.is_empty()
            || pending.owner_scope
                != sentinel_common::StateTransferScope::for_aggregate(&binding.agent_id.to_string())
        {
            return Err("request reservation changed");
        }
        let principal = self
            .principals
            .principal(&context.authority.principal.principal_id)
            .ok_or("subscription principal unavailable")?;
        let tenant = TenantId::parse(&binding.tenant_id).map_err(|_| "invalid tenant")?;
        let project_id = ProjectId::parse(&binding.project_id).map_err(|_| "invalid project")?;
        let project = self
            .store
            .company_project(&tenant, &project_id)
            .map_err(|_| "project unavailable")?
            .ok_or("project missing")?;
        // A fresh operation is deliberate: replaying an HTTP response must not
        // mint another permission after the permanent dispatch tombstone exists.
        let now_ms = now_unix_ms();
        context.validate_dispatch(now_ms)?;
        self.core
            .apply_company_command(
                &principal.principal,
                Uuid::new_v4(),
                &CompanyWorkflowCommandV1::ClaimSubscriptionCall {
                    project_id,
                    expected_version: project.version,
                    allowance_id: request.allowance_id.clone(),
                    request_id: request.request_id.clone(),
                    request_digest: request.request_digest.clone(),
                },
                now_ms,
            )
            .map_err(|_| "subscription claim denied")?;
        Ok(grant
            .expires_at_unix_ms
            .min(now_ms.saturating_add(grant.max_duration_ms)))
    }

    fn claim_leadership_dispatch(
        &self,
        request: &DispatchRequest,
        now: u64,
    ) -> Result<u64, &'static str> {
        let Some(RequestSubject::AdaptiveLeadershipReview {
            review_id,
            review_kind,
        }) = request.subject.as_ref()
        else {
            return Err("leadership subject missing");
        };
        let (call, prepared) = self
            .leadership_review_for_dispatch_with_context(AgentId(request.agent_id), *review_id)?;
        let grant = &call.grant;
        let expected_kind = match &grant.subject {
            None => None,
            Some(sentinel_workflow::AdaptiveLeadershipReviewSubjectV2::UnknownModel { .. }) => {
                Some("unknown_model")
            }
            Some(sentinel_workflow::AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                ..
            }) => Some("blocked_continuation"),
            Some(sentinel_workflow::AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted {
                ..
            }) => Some("budget_window_exhausted"),
        };
        if grant.review_id != *review_id
            || review_kind.as_deref() != expected_kind
            || request.allowance_id != call.allowance_id
            || request.request_id != call.request_id()
            || request.provider != grant.provider
            || request.model != grant.model
            || request.catalog_digest != grant.catalog_digest
        {
            return Err("leadership dispatch binding mismatch");
        }
        let context = super::model_execution::ModelExecutionContext::AdaptiveLeadershipReview(
            Box::new(prepared),
        );
        context.validate_dispatch(now)?;
        if call
            .context_digest()
            .map_err(|_| "leadership context invalid")?
            != request.context_digest
        {
            return Err("leadership dispatch context mismatch");
        }
        let pending = self
            .event_store
            .as_ref()
            .ok_or("leadership EventStore missing")?
            .get_llm_completion(&request.request_id)
            .map_err(|_| "leadership reservation unavailable")?
            .ok_or("leadership reservation missing")?;
        if pending.request_digest != request.request_digest
            || pending.status != "provider_in_flight"
            || !pending.payload.is_empty()
            || pending.owner_scope
                != sentinel_common::StateTransferScope::for_agent(
                    AgentId(request.agent_id).to_string(),
                )
        {
            return Err("leadership reservation mismatch");
        }
        let now = now_unix_ms();
        context.validate_dispatch(now)?;
        self.store
            .claim_adaptive_leadership_review_call(
                &grant.leadership_principal,
                &sentinel_workflow::ClaimAdaptiveLeadershipReviewCallV1 {
                    review_id: *review_id,
                    allowance_id: call.allowance_id.clone(),
                    request_id: request.request_id.clone(),
                    request_digest: request.request_digest.clone(),
                    context_digest: request.context_digest.clone(),
                },
                now,
            )
            .map_err(|_| "leadership dispatch consumed or denied")?;
        Ok(grant
            .expires_at_unix_ms
            .min(now.saturating_add(grant.max_duration_ms)))
    }

    fn claim_project_planning_dispatch(
        &self,
        request: &DispatchRequest,
        now_ms: u64,
    ) -> Result<u64, &'static str> {
        use super::model_execution::{
            ModelExecutionContext, ProjectPlanningAuthority, ProviderExecutionAuthority,
        };

        let Some(RequestSubject::ProjectPlanning {
            project_id,
            project_version,
        }) = &request.subject
        else {
            return Err("project planning subject missing");
        };
        let call = self
            .store
            .project_planning_call(
                &self
                    .request_sales_tenant
                    .clone()
                    .ok_or("project planning tenant unavailable")?,
                project_id,
            )
            .map_err(|_| "project planning store unavailable")?
            .ok_or("project planning allowance unavailable")?;
        let grant = &call.grant;
        let dispatch_deadline = grant
            .expires_at_unix_ms
            .min(now_ms.saturating_add(grant.max_duration_ms));
        let binding = ProjectPlanningAuthority {
            schema_version: 4,
            allowance_id: call.allowance_id.clone(),
            grant: grant.clone(),
        };
        if project_id != &grant.project_id
            || *project_version != grant.expected_version
            || grant.planner_principal.agent_id != Some(AgentId(request.agent_id))
            || request.allowance_id != call.allowance_id
            || request.provider != grant.provider
            || request.model != grant.model
            || request.catalog_digest != grant.catalog_digest
            || request.request_id
                != ProviderExecutionAuthority::ProjectPlanning(Box::new(binding.clone()))
                    .request_id()
        {
            return Err("project planning dispatch binding mismatch");
        }
        let context = ModelExecutionContext::ProjectPlanning(Box::new(
            self.prepare_project_planning(&binding)?,
        ));
        context.validate_dispatch(now_ms)?;
        if format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&context).map_err(|_| "project planning context invalid")?
            )
        ) != request.context_digest
        {
            return Err("project planning dispatch context mismatch");
        }
        let pending = self
            .event_store
            .as_ref()
            .ok_or("project planning EventStore unavailable")?
            .get_llm_completion(&request.request_id)
            .map_err(|_| "project planning reservation unavailable")?
            .ok_or("project planning request not reserved")?;
        if pending.request_digest != request.request_digest
            || pending.status != "provider_in_flight"
            || !pending.payload.is_empty()
            || pending.owner_scope
                != sentinel_common::StateTransferScope::for_agent(
                    AgentId(request.agent_id).to_string(),
                )
        {
            return Err("project planning reservation mismatch");
        }
        context.validate_dispatch(now_unix_ms())?;
        self.store
            .claim_project_planning_call(
                &grant.planner_principal,
                &sentinel_workflow::ClaimProjectPlanningCallV1 {
                    allowance_id: call.allowance_id.clone(),
                    project_id: project_id.clone(),
                    request_id: request.request_id.clone(),
                    request_digest: request.request_digest.clone(),
                    context_digest: request.context_digest.clone(),
                },
                now_unix_ms(),
            )
            .map_err(|_| "project planning dispatch already consumed or denied")?;
        Ok(dispatch_deadline)
    }

    fn claim_adaptive_dispatch(
        &self,
        request: &DispatchRequest,
        now_ms: u64,
    ) -> Result<u64, &'static str> {
        let Some(RequestSubject::AdaptiveSession {
            session_id,
            effect_id,
            session_version,
        }) = &request.subject
        else {
            return Err("adaptive dispatch subject missing");
        };
        let binding = self
            .adaptive_provider_authority_for_claim(AgentId(request.agent_id))?
            .ok_or("adaptive provider authority unavailable")?;
        if binding.grant.session_id != *session_id
            || binding.effect_id != *effect_id
            || binding.session_version != *session_version
            || binding.grant.provider_allowance_id != request.allowance_id
            || binding.grant.provider != request.provider
            || binding.grant.model != request.model
            || binding.grant.catalog_digest != request.catalog_digest
            || binding.request_id() != request.request_id
        {
            return Err("adaptive dispatch binding changed");
        }
        let context = super::model_execution::ModelExecutionContext::Adaptive(Box::new(
            self.prepare_adaptive_model(&binding)?,
        ));
        context.validate_dispatch(now_ms)?;
        if format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&context).map_err(|_| "adaptive context invalid")?)
        ) != request.context_digest
        {
            return Err("adaptive dispatch context changed");
        }
        let event_store = self
            .event_store
            .as_ref()
            .ok_or("request store unavailable")?;
        let pending = event_store
            .get_llm_completion(&request.request_id)
            .map_err(|_| "request reservation unavailable")?
            .ok_or("request not reserved")?;
        if pending.request_digest != request.request_digest
            || pending.status != "provider_in_flight"
            || !pending.payload.is_empty()
            || pending.owner_scope
                != sentinel_common::StateTransferScope::for_aggregate(
                    &binding.grant.authority.agent_id.to_string(),
                )
        {
            return Err("adaptive request reservation changed");
        }
        let session = self
            .core
            .adaptive_session(*session_id, &binding.grant.authority)
            .map_err(|_| "adaptive session unavailable")?
            .ok_or("adaptive session missing")?;
        if !matches!(session.cursor, AdaptiveCursorV1::ReadyForModel)
            || session.version != *session_version
        {
            return Err("adaptive model claim changed or was consumed");
        }
        self.claim_fresh_adaptive_model(&binding, request, now_ms)
    }

    fn claim_fresh_adaptive_model(
        &self,
        binding: &super::model_execution::AdaptiveProviderAuthority,
        request: &DispatchRequest,
        now_ms: u64,
    ) -> Result<u64, &'static str> {
        let Some(RequestSubject::AdaptiveSession {
            session_id,
            effect_id,
            session_version,
        }) = &request.subject
        else {
            return Err("adaptive dispatch subject missing");
        };
        let operation_id = stable_operation_id(
            "sentinel.workflow.adaptive-claim-model.v1",
            &request.request_id,
            *session_version,
        );
        let (replayed, _) = self
            .core
            .advance_adaptive_session(
                *session_id,
                *session_version,
                operation_id,
                &AdaptiveTransitionV1::ClaimModel {
                    effect: AdaptiveEffectV1 {
                        id: *effect_id,
                        request_digest: request.request_digest.clone(),
                    },
                    previous_observation_digest: binding
                        .previous_observation
                        .as_ref()
                        .map(|value| value.observation_digest.clone()),
                },
                &binding.grant.authority,
                now_ms,
            )
            .map_err(|_| "adaptive model claim denied")?;
        // A durable replay proves the earlier claim, not permission for new provider I/O.
        if replayed {
            return Err("adaptive model claim already consumed");
        }
        Ok(binding
            .grant
            .deadline_ms
            .min(now_ms.saturating_add(binding.grant.max_call_duration_ms)))
    }

    fn claim_sales_dispatch(
        &self,
        request: &DispatchRequest,
        now_ms: u64,
    ) -> Result<u64, &'static str> {
        use super::model_execution::{ModelExecutionContext, RequestSalesAuthority};
        let Some(RequestSubject::CustomerRequest {
            request_id,
            request_version,
        }) = &request.subject
        else {
            return Err("Sales request subject missing");
        };
        let tenant = self
            .request_sales_tenant
            .as_ref()
            .ok_or("Sales tenant unavailable")?;
        let call = self.request_sales_call_for(tenant, &request.allowance_id)?;
        let grant = &call.grant;
        if request_id != &grant.request_id
            || *request_version != grant.expected_version
            || grant.sales_principal.agent_id != Some(AgentId(request.agent_id))
            || request.allowance_id != call.allowance_id
            || request.provider != grant.provider
            || request.model != grant.model
            || request.catalog_digest != grant.catalog_digest
            || request.request_id != format!("company-provider-{}", call.allowance_id)
        {
            return Err("Sales dispatch binding mismatch");
        }
        let context = ModelExecutionContext::RequestSales(Box::new(self.prepare_request_sales(
            &RequestSalesAuthority {
                schema_version: 2,
                allowance_id: call.allowance_id.clone(),
                grant: grant.clone(),
            },
        )?));
        context.validate_dispatch(now_ms)?;
        if format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&context).map_err(|_| "Sales context invalid")?)
        ) != request.context_digest
        {
            return Err("Sales dispatch context mismatch");
        }
        let pending = self
            .event_store
            .as_ref()
            .ok_or("Sales EventStore unavailable")?
            .get_llm_completion(&request.request_id)
            .map_err(|_| "Sales reservation unavailable")?
            .ok_or("Sales request not reserved")?;
        if pending.request_digest != request.request_digest
            || pending.status != "provider_in_flight"
            || !pending.payload.is_empty()
            || pending.owner_scope
                != sentinel_common::StateTransferScope::for_agent(
                    AgentId(request.agent_id).to_string(),
                )
        {
            return Err("Sales request reservation mismatch");
        }
        let now_ms = now_unix_ms();
        context.validate_dispatch(now_ms)?;
        self.store
            .claim_request_provider_call(
                &grant.sales_principal,
                &sentinel_workflow::ClaimRequestProviderCallV1 {
                    allowance_id: call.allowance_id,
                    request_id: request.request_id.clone(),
                    request_digest: request.request_digest.clone(),
                    context_digest: request.context_digest.clone(),
                },
                now_ms,
            )
            .map_err(|_| "Sales dispatch already consumed or denied")?;
        Ok(grant
            .expires_at_unix_ms
            .min(now_ms.saturating_add(grant.max_duration_ms)))
    }
}

#[cfg(test)]
mod tests {
    use super::super::adaptive_leadership_review::{
        tests::{discovery_state, fixture},
        LeadershipContext,
    };
    use super::super::model_execution::{AdaptiveProviderAuthority, ModelExecutionContext};
    use super::*;
    use std::sync::{mpsc, Barrier};
    use std::time::Duration;

    fn reserved_leadership_request(
        api: &WorkflowApi,
        context: &LeadershipContext,
    ) -> serde_json::Value {
        let grant = &context.binding.grant;
        let agent = grant.leadership_principal.agent_id.unwrap();
        let request_id = format!("company-leadership-{}", grant.review_id);
        let request_digest = "c".repeat(64);
        api.event_store
            .as_ref()
            .unwrap()
            .reserve_llm_request(&request_id, &request_digest, &agent.to_string())
            .unwrap();
        serde_json::json!({
            "schema_version": 5,
            "allowance_id": context.binding.allowance_id,
            "agent_id": agent.0,
            "request_id": request_id,
            "request_digest": request_digest,
            "context_digest": context.context_digest,
            "provider": grant.provider,
            "model": grant.model,
            "catalog_digest": grant.catalog_digest,
            "subject": {"kind": "adaptive_leadership_review", "review_id": grant.review_id},
        })
    }

    fn reserved_adaptive_request(
        api: &WorkflowApi,
        binding: &AdaptiveProviderAuthority,
    ) -> Vec<u8> {
        let context =
            ModelExecutionContext::Adaptive(Box::new(api.prepare_adaptive_model(binding).unwrap()));
        context.validate_dispatch(now_unix_ms()).unwrap();
        let context_digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&context).unwrap()),
        );
        let request_id = binding.request_id();
        let request_digest = "d".repeat(64);
        api.event_store
            .as_ref()
            .unwrap()
            .reserve_llm_request(
                &request_id,
                &request_digest,
                &binding.grant.authority.agent_id.to_string(),
            )
            .unwrap();
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 3,
            "allowance_id": binding.grant.provider_allowance_id,
            "agent_id": binding.grant.authority.agent_id.0,
            "request_id": request_id,
            "request_digest": request_digest,
            "context_digest": context_digest,
            "provider": binding.grant.provider,
            "model": binding.grant.model,
            "catalog_digest": binding.grant.catalog_digest,
            "subject": {
                "kind": "adaptive_session",
                "session_id": binding.grant.session_id,
                "effect_id": binding.effect_id,
                "session_version": binding.session_version,
            },
        }))
        .unwrap()
    }

    #[test]
    fn normal_reconciliation_pause_allows_schema5_callback_and_skips_second_batch() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (api, context) = fixture(&path, &events);
        let body = serde_json::to_vec(&reserved_leadership_request(&api, &context)).unwrap();
        let api = Arc::new(api);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first_api = Arc::clone(&api);
        let first = std::thread::spawn(move || {
            first_api.reconcile_pending_until(|| {
                // The real entry point acquired both guards before this first hook.
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                true
            });
        });
        let entered = entered_rx.recv_timeout(Duration::from_secs(5));
        let shared_guard_available = api.mutation_fence.try_read().is_ok();
        let recovery_excluded = api.mutation_fence.try_write().is_err();
        let batch_owned = api.reconciliation_fence.try_lock().is_err();

        let second_hook_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let second_hook = Arc::clone(&second_hook_called);
        let second_api = Arc::clone(&api);
        let (second_tx, second_rx) = mpsc::channel();
        let second = std::thread::spawn(move || {
            second_api.reconcile_pending_until(|| {
                second_hook.store(true, Ordering::Release);
                true
            });
            second_tx.send(()).unwrap();
        });
        let dispatch_api = Arc::clone(&api);
        let (callback_tx, callback_rx) = mpsc::channel();
        let callback = std::thread::spawn(move || {
            callback_tx
                .send(dispatch_api.subscription_dispatch(&body))
                .unwrap();
        });
        let response_before_release = callback_rx.recv_timeout(Duration::from_secs(5));
        let second_before_release = second_rx.recv_timeout(Duration::from_secs(5));
        let still_paused = api.reconciliation_fence.try_lock().is_err();
        // Unblock and join all threads before assertions, including lock-regression failures.
        let _ = release_tx.send(());
        let joined = [first.join(), second.join(), callback.join()];
        for result in joined {
            result.unwrap();
        }
        entered.expect("the first real reconciliation must reach its guarded stop hook");
        assert!(shared_guard_available);
        assert!(recovery_excluded);
        assert!(batch_owned);
        assert!(still_paused);
        second_before_release.expect("the second reconciliation must skip the occupied batch");
        assert!(!second_hook_called.load(Ordering::Acquire));
        assert_eq!(
            response_before_release
                .expect("callback must finish while the first reconciliation remains paused")
                .status,
            200,
        );
        let call = api
            .store
            .adaptive_leadership_review_call(
                &context.binding.grant.leadership_principal.tenant_id,
                context.binding.grant.review_id,
            )
            .unwrap()
            .unwrap();
        assert!(call.dispatch.is_some());
        assert!(call.decision.is_none());
        assert_eq!(call.context, context.source);
    }

    #[test]
    fn concurrent_schema5_callbacks_issue_one_permission_and_one_dispatch_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (api, context) = fixture(&path, &events);
        let request = reserved_leadership_request(&api, &context);
        let body = serde_json::to_vec(&request).unwrap();
        let original = api
            .store
            .adaptive_leadership_review_call(
                &context.binding.grant.leadership_principal.tenant_id,
                context.binding.grant.review_id,
            )
            .unwrap()
            .unwrap();
        assert!(original.dispatch.is_none());
        let api = Arc::new(api);
        let ready = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let api = Arc::clone(&api);
            let ready = Arc::clone(&ready);
            let body = body.clone();
            workers.push(std::thread::spawn(move || {
                ready.wait();
                api.subscription_dispatch(&body)
            }));
        }
        ready.wait();
        let joined = workers
            .into_iter()
            .map(|worker| worker.join())
            .collect::<Vec<_>>();
        let mut statuses = joined
            .into_iter()
            .map(|result| result.unwrap().status)
            .collect::<Vec<_>>();
        statuses.sort_unstable();
        assert_eq!(statuses, vec![200, 403]);
        let claimed = api
            .store
            .adaptive_leadership_review_call(
                &original.grant.leadership_principal.tenant_id,
                original.grant.review_id,
            )
            .unwrap()
            .unwrap();
        assert_eq!(claimed.version, original.version + 1);
        assert_eq!(claimed.grant, original.grant);
        assert_eq!(claimed.context, original.context);
        let dispatch = claimed.dispatch.unwrap();
        assert_eq!(dispatch.request_id, request["request_id"].as_str().unwrap());
        assert_eq!(
            dispatch.request_digest,
            request["request_digest"].as_str().unwrap(),
        );
        assert!(claimed.decision.is_none());
        let receipts: i64 = sentinel_limbo::rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM company_events
                 WHERE tenant_id=?1 AND operation_id=?2
                   AND event_type='adaptive_leadership_review_dispatched'",
                sentinel_limbo::rusqlite::params![
                    original.grant.leadership_principal.tenant_id.0,
                    original.operation_id.to_string(),
                ],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(receipts, 1);
        let after = discovery_state(&path, &events);
        assert_eq!(api.subscription_dispatch(&body).status, 403);
        assert_eq!(discovery_state(&path, &events), after);
    }

    #[test]
    fn prevalidated_schema3_same_head_race_consumes_one_permission_and_model_call() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (api, binding, _) =
            super::super::model_work::configured_adaptive_test_api(&path, &events);
        let before = api
            .core
            .adaptive_session(binding.grant.session_id, &binding.grant.authority)
            .unwrap()
            .unwrap();
        assert_eq!(before.cursor, AdaptiveCursorV1::ReadyForModel);
        assert_eq!(before.version, binding.session_version);
        assert!(!before.model_window_exhausted_at(now_unix_ms()));
        let body = reserved_adaptive_request(&api, &binding);
        let now = now_unix_ms();
        let api = Arc::new(api);
        let (prepared_tx, prepared_rx) = mpsc::channel();
        let mut releases = Vec::new();
        let mut workers = Vec::new();
        for _ in 0..2 {
            let api = Arc::clone(&api);
            let binding: AdaptiveProviderAuthority = binding.clone();
            let body = body.clone();
            let prepared_tx = prepared_tx.clone();
            let (release_tx, release_rx) = mpsc::channel();
            releases.push(release_tx);
            workers.push(std::thread::spawn(move || {
                let _fence = api.mutation_fence.try_read().unwrap();
                let request: DispatchRequest = serde_json::from_slice(&body).unwrap();
                assert_eq!(
                    api.adaptive_provider_authority_for_claim(binding.grant.authority.agent_id)
                        .unwrap(),
                    Some(binding.clone()),
                );
                let context = ModelExecutionContext::Adaptive(Box::new(
                    api.prepare_adaptive_model(&binding).unwrap(),
                ));
                context.validate_dispatch(now).unwrap();
                assert_eq!(
                    format!(
                        "{:x}",
                        Sha256::digest(serde_json::to_vec(&context).unwrap())
                    ),
                    request.context_digest,
                );
                let reserved = api
                    .event_store
                    .as_ref()
                    .unwrap()
                    .get_llm_completion(&request.request_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(reserved.request_digest, request.request_digest);
                assert_eq!(reserved.status, "provider_in_flight");
                assert!(reserved.payload.is_empty());
                assert_eq!(
                    reserved.owner_scope,
                    sentinel_common::StateTransferScope::for_agent(
                        binding.grant.authority.agent_id.to_string(),
                    ),
                );
                let current = api
                    .core
                    .adaptive_session(binding.grant.session_id, &binding.grant.authority)
                    .unwrap()
                    .unwrap();
                assert_eq!(current.cursor, AdaptiveCursorV1::ReadyForModel);
                assert_eq!(current.version, binding.session_version);
                prepared_tx.send(current).unwrap();
                release_rx.recv().unwrap();
                // Both callers passed prevalidation for the same head before either claim.
                api.claim_fresh_adaptive_model(&binding, &request, now)
            }));
        }
        drop(prepared_tx);
        let prepared = (0..2)
            .map(|_| prepared_rx.recv_timeout(Duration::from_secs(5)))
            .collect::<Vec<_>>();
        // Release all waiters even if a prevalidation assertion failed in one thread.
        for release in releases {
            let _ = release.send(());
        }
        let joined = workers
            .into_iter()
            .map(|worker| worker.join())
            .collect::<Vec<_>>();
        let outcomes = joined
            .into_iter()
            .map(|result| result.unwrap())
            .collect::<Vec<_>>();
        for prepared in prepared {
            assert_eq!(
                prepared.expect("both callers must prevalidate before either claim"),
                before,
            );
        }
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter_map(|outcome| outcome.as_ref().err().copied())
                .collect::<Vec<_>>(),
            vec!["adaptive model claim already consumed"],
        );
        let after = api
            .core
            .adaptive_session(binding.grant.session_id, &binding.grant.authority)
            .unwrap()
            .unwrap();
        assert_eq!(after.version, before.version + 1);
        assert_eq!(after.model_calls, before.model_calls + 1);
        assert_eq!(after.tool_calls, before.tool_calls);
        assert_eq!(after.grant, before.grant);
        assert_eq!(after.continuation, before.continuation);
        assert_eq!(after.last_observation, before.last_observation);
        assert_eq!(
            after.cursor,
            AdaptiveCursorV1::ModelPending {
                effect: AdaptiveEffectV1 {
                    id: binding.effect_id,
                    request_digest: "d".repeat(64),
                },
            },
        );
        let claimed_state = discovery_state(&path, &events);
        let request: DispatchRequest = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            api.claim_fresh_adaptive_model(&binding, &request, now),
            Err("adaptive model claim already consumed"),
        );
        assert_eq!(discovery_state(&path, &events), claimed_state);
    }

    #[test]
    fn recovery_fence_denies_all_dispatch_schemas_immediately_without_writes_or_late_claims() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (api, context) = fixture(&path, &events);
        let request = reserved_leadership_request(&api, &context);
        let api = Arc::new(api);
        let before = discovery_state(&path, &events);
        let recovery_api = Arc::clone(&api);
        let fence = recovery_api.mutation_fence.write().unwrap();
        let dispatch_api = Arc::clone(&api);
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut results = Vec::new();
            for schema in 1..=5 {
                let mut candidate = request.clone();
                candidate["schema_version"] = serde_json::json!(schema);
                candidate["subject"] = match schema {
                    1 => serde_json::Value::Null,
                    2 => serde_json::json!({
                        "kind": "customer_request",
                        "request_id": "request",
                        "request_version": 1,
                    }),
                    3 => serde_json::json!({
                        "kind": "adaptive_session",
                        "session_id": context.binding.grant.session_id,
                        "effect_id": Uuid::new_v4(),
                        "session_version": 1,
                    }),
                    4 => serde_json::json!({
                        "kind": "project_planning",
                        "project_id": context.binding.grant.project_id,
                        "project_version": 1,
                    }),
                    _ => request["subject"].clone(),
                };
                let body = serde_json::to_vec(&candidate).unwrap();
                let decoded: DispatchRequest = serde_json::from_slice(&body).unwrap();
                results.push((
                    schema,
                    dispatch_api.claim_subscription_dispatch(&decoded),
                    dispatch_api.subscription_dispatch(&body),
                ));
            }
            sender.send(results).unwrap();
        });
        let results = receiver.recv_timeout(Duration::from_secs(1));
        let recovery_still_owns_fence = api.mutation_fence.try_read().is_err();
        let while_fenced = discovery_state(&path, &events);
        // Release and join before asserting so a blocking-lock regression cannot hang the suite.
        drop(fence);
        worker.join().unwrap();
        let after_release = discovery_state(&path, &events);
        let results = results.expect("dispatch must deny while recovery still owns the fence");
        assert!(recovery_still_owns_fence);
        for (schema, claim, response) in results {
            assert_eq!(claim, Err("workflow recovery active"), "schema {schema}");
            assert_eq!(response.status, 403, "schema {schema}");
            let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
            assert_eq!(body["code"], "subscription_dispatch_denied");
        }
        assert_eq!(while_fenced, before);
        assert_eq!(after_release, before);
    }

    #[test]
    fn leadership_callback_rejects_changed_bindings_without_writes_and_claims_once() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (api, context) = fixture(&path, &events);
        let request = reserved_leadership_request(&api, &context);
        let before = discovery_state(&path, &events);
        for field in [
            "allowance_id",
            "request_id",
            "request_digest",
            "context_digest",
            "provider",
            "model",
            "catalog_digest",
            "/subject/review_id",
            "/subject/review_kind",
            "agent_id",
        ] {
            let mut changed = request.clone();
            match field {
                "/subject/review_id" => {
                    changed["subject"]["review_id"] = serde_json::json!(Uuid::new_v4())
                }
                "/subject/review_kind" => {
                    changed["subject"]["review_kind"] = serde_json::json!("unknown_model")
                }
                "agent_id" => changed["agent_id"] = serde_json::json!(u16::MAX),
                _ => changed[field] = serde_json::json!("changed"),
            }
            assert_eq!(
                api.subscription_dispatch(&serde_json::to_vec(&changed).unwrap())
                    .status,
                403,
                "{field}"
            );
            assert_eq!(discovery_state(&path, &events), before, "{field}");
        }
        let body = serde_json::to_vec(&request).unwrap();
        assert_eq!(api.subscription_dispatch(&body).status, 200);
        let claimed = api
            .store
            .adaptive_leadership_review_call(
                &context.binding.grant.leadership_principal.tenant_id,
                context.binding.grant.review_id,
            )
            .unwrap()
            .unwrap();
        assert!(claimed.dispatch.is_some());
        let after_claim = discovery_state(&path, &events);
        assert_eq!(api.subscription_dispatch(&body).status, 403);
        assert_eq!(discovery_state(&path, &events), after_claim);
    }
}
