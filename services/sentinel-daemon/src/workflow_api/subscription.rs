//! Final dispatch consumes workflow authority, never an in-memory call counter.
use super::*;

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
        match self.claim_subscription_dispatch(&request) {
            Ok(deadline) => json(
                200,
                &serde_json::json!({
                    "schema_version": request.schema_version,
                    "allowance_id": request.allowance_id,
                    "request_id": request.request_id,
                    "request_digest": request.request_digest,
                    "deadline_unix_ms": deadline,
                }),
            ),
            Err(reason) => {
                warn!(
                    allowance_id = %request.allowance_id,
                    request_id = %request.request_id,
                    agent_id = %request.agent_id,
                    schema_version = request.schema_version,
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
            .try_write()
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
        let call = self.leadership_review_for_dispatch(AgentId(request.agent_id), *review_id)?;
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
        let binding = super::adaptive_leadership_review::LeadershipAuthority::from_call(&call);
        let context = super::model_execution::ModelExecutionContext::AdaptiveLeadershipReview(
            Box::new(self.prepare_leadership_review(&binding)?),
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
        let operation_id = stable_operation_id(
            "sentinel.workflow.adaptive-claim-model.v1",
            &request.request_id,
            *session_version,
        );
        self.core
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
    use super::*;
    use std::sync::mpsc;
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
