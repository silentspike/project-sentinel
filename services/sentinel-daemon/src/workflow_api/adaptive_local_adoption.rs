//! Explicit local processing of a retained decision, never a second provider call.
use super::adaptive_leadership_review::{LeadershipAuthority, LeadershipContext};
use super::model_execution::{ModelExecutionCompletion, ModelExecutionContext};
use super::*;
use sentinel_workflow::{
    adaptive_leadership_local_adoption_source_call_digest,
    adaptive_leadership_recovery_project_digest, adaptive_leadership_recovery_session_digest,
    AdaptiveLeadershipLocalAdoptionRequestV1, AdaptiveLeadershipReviewDecisionKindV1,
};

impl WorkflowApi {
    pub(super) fn reconcile_retired_leadership_adoption(
        &self,
        call: &sentinel_workflow::AdaptiveLeadershipReviewCallV1,
    ) -> Result<(), &'static str> {
        if call.retired_at_unix_ms.is_none()
            || call.decision.is_some()
            || call.continuation.is_some()
        {
            return Err("local adoption retirement not terminal");
        }
        let record = self
            .store
            .adaptive_leadership_local_adoption(
                &call.grant.leadership_principal.tenant_id,
                call.grant.review_id,
            )
            .map_err(|_| "retired local adoption authority unavailable")?
            .ok_or("retired local adoption authority missing")?;
        if self
            .store
            .adaptive_leadership_review_call(
                &call.grant.leadership_principal.tenant_id,
                call.grant.review_id,
            )
            .map_err(|_| "retired local adoption call unavailable")?
            .as_ref()
            != Some(call)
            || record.request.original_call_digest
                != adaptive_leadership_local_adoption_source_call_digest(call)
                    .map_err(|_| "retired local adoption source invalid")?
        {
            return Err("retired local adoption source changed");
        }
        let events = self
            .event_store
            .as_ref()
            .ok_or("local adoption EventStore missing")?;
        if let Some(row) = events
            .get_llm_completion(&record.request.request_id)
            .map_err(|_| "retired local adoption disposition unavailable")?
        {
            if row.status != "failed"
                || row.request_digest != record.request.request_digest
                || sentinel_common::sha256_hex(row.payload.as_bytes())
                    != record.request.payload_digest
                || row.owner_scope
                    != sentinel_common::StateTransferScope::for_agent(
                        call.grant
                            .leadership_principal
                            .agent_id
                            .ok_or("retired local adoption leader missing")?
                            .to_string(),
                    )
                || !matches!(
                    row.last_error.as_deref(),
                    Some("continuation audit invalid" | "leadership_review_stale")
                )
            {
                return Err("retired local adoption disposition changed");
            }
            let digest = sentinel_common::sha256_hex(
                &sentinel_common::canonical_json(call)
                    .map_err(|_| "retired local adoption receipt encoding invalid")?,
            );
            events
                .retire_known_leadership_local_adoption(
                    &record.request.request_id,
                    &record.request.request_digest,
                    &record.request.payload_digest,
                    &record.adoption_key,
                    &digest,
                )
                .map_err(|_| "retired local adoption disposition failed")?;
        }
        Ok(())
    }

    pub(super) fn reconcile_local_leadership_adoption(
        &self,
        call: &sentinel_workflow::AdaptiveLeadershipReviewCallV1,
        clock: &impl Fn() -> u64,
    ) -> Result<bool, &'static str> {
        let Some(record) = self
            .store
            .adaptive_leadership_local_adoption(
                &call.grant.leadership_principal.tenant_id,
                call.grant.review_id,
            )
            .map_err(|_| "local adoption authority unavailable")?
        else {
            return Ok(false);
        };
        if call.retired_at_unix_ms.is_some() {
            self.reconcile_retired_leadership_adoption(call)?;
            return Ok(false);
        }
        let events = self
            .event_store
            .as_ref()
            .ok_or("local adoption EventStore missing")?;
        let row = events
            .get_llm_completion(&record.request.request_id)
            .map_err(|_| "local adoption response unavailable")?;
        let Some(row) = row else {
            // Ordinary outbox pruning is safe only after both durable receipts agree.
            record
                .validate_call(call)
                .map_err(|_| "local adoption historical call invalid")?;
            if call.decision.as_ref() != Some(&record.request.decision)
                || call
                    .continuation
                    .as_ref()
                    .and_then(|authorization| authorization.local_adoption.as_deref())
                    != Some(&record)
            {
                return Err("local adoption response missing before domain completion");
            }
            let digest = sentinel_common::sha256_hex(
                &sentinel_common::canonical_json(call)
                    .map_err(|_| "local adoption receipt encoding invalid")?,
            );
            let receipt = events
                .event_by_operation_id(&format!("llm_local_adoption_{}", record.request.request_id))
                .map_err(|_| "local adoption cleanup receipt unavailable")?
                .ok_or("local adoption cleanup receipt missing")?;
            let expected = serde_json::json!({"request_id":record.request.request_id,
                "request_digest":record.request.request_digest,"payload_digest":record.request.payload_digest,
                "adoption_key":record.adoption_key,"domain_receipt_digest":digest});
            if receipt.event_type != "llm_completion_locally_adopted"
                || serde_json::from_str::<serde_json::Value>(&receipt.payload)
                    .map_err(|_| "local adoption cleanup receipt invalid")?
                    != expected
            {
                return Err("local adoption cleanup receipt changed");
            }
            return Ok(false);
        };
        if row.payload.len() > 2 * 1024 * 1024
            || record.request.payload_digest != sentinel_common::sha256_hex(row.payload.as_bytes())
        {
            return Err("local adoption response changed during recovery");
        }
        let payload: serde_json::Value =
            serde_json::from_str(&row.payload).map_err(|_| "local adoption payload invalid")?;
        let completion: ModelExecutionCompletion = serde_json::from_value(
            payload
                .get("model_work")
                .cloned()
                .ok_or("local adoption model response missing")?,
        )
        .map_err(|_| "local adoption model response invalid")?;
        let ModelExecutionContext::AdaptiveLeadershipReview(context) = &completion.context else {
            return Err("local adoption is not a leadership response");
        };
        self.accept_leadership_review_fenced(
            &completion,
            context,
            &record.request.request_id,
            &record.request.request_digest,
            clock,
        )?;
        let completed = self
            .store
            .adaptive_leadership_review_call(&record.request.tenant_id, record.request.review_id)
            .map_err(|_| "local adoption completed receipt unavailable")?
            .ok_or("local adoption completed receipt missing")?;
        if completed.retired_at_unix_ms.is_some() {
            return Ok(true);
        }
        if completed.decision.as_ref() != Some(&record.request.decision)
            || completed
                .continuation
                .as_ref()
                .and_then(|authorization| authorization.local_adoption.as_deref())
                != Some(&record)
        {
            return Err("local adoption domain receipt not committed");
        }
        let digest = sentinel_common::sha256_hex(
            &sentinel_common::canonical_json(&completed)
                .map_err(|_| "local adoption receipt encoding invalid")?,
        );
        events
            .finish_known_leadership_local_adoption(
                &record.request.request_id,
                &record.request.request_digest,
                &record.request.payload_digest,
                &record.adoption_key,
                &digest,
            )
            .map_err(|_| "local adoption completion cleanup failed")?;
        Ok(call.decision.is_none())
    }

    pub(super) fn local_adoption_candidate(
        &self,
        operator: &BoundPrincipal,
        review_id: Uuid,
        operation_id: Uuid,
        expires_at_unix_ms: Option<u64>,
    ) -> Result<AdaptiveLeadershipLocalAdoptionRequestV1, &'static str> {
        let call = self
            .store
            .adaptive_leadership_review_call(&operator.principal.tenant_id, review_id)
            .map_err(|_| "local adoption call unavailable")?
            .ok_or("local adoption call missing")?;
        if call.decision.is_some() || call.retired_at_unix_ms.is_some() || call.version != 2 {
            return Err("local adoption call is not pending dispatched review");
        }
        let dispatch = call
            .dispatch
            .as_ref()
            .ok_or("local adoption dispatch missing")?;
        let epoch = self
            .store
            .adaptive_leadership_recovery_epoch(
                &operator.principal.tenant_id,
                call.grant.session_id,
            )
            .map_err(|_| "local adoption epoch unavailable")?
            .ok_or("local adoption epoch missing")?;
        epoch
            .validate_review(&call.grant, &call.context, call.grant_issued_at_unix_ms)
            .map_err(|_| "local adoption epoch membership invalid")?;
        let events = self
            .event_store
            .as_ref()
            .ok_or("local adoption EventStore missing")?;
        let row = events
            .get_llm_completion(&dispatch.request_id)
            .map_err(|_| "local adoption response unavailable")?
            .ok_or("local adoption response missing")?;
        if row.status != "failed"
            || row.last_error.as_deref() != Some("continuation audit invalid")
            || row.request_digest != dispatch.request_digest
            || row.payload.len() > 2 * 1024 * 1024
            || row.owner_scope
                != sentinel_common::StateTransferScope::for_agent(
                    call.grant
                        .leadership_principal
                        .agent_id
                        .ok_or("local adoption leader missing")?
                        .to_string(),
                )
        {
            return Err("local adoption response is not an exact known local failure");
        }
        let payload: serde_json::Value =
            serde_json::from_str(&row.payload).map_err(|_| "local adoption payload invalid")?;
        let completion: ModelExecutionCompletion = serde_json::from_value(
            payload
                .get("model_work")
                .cloned()
                .ok_or("local adoption model response missing")?,
        )
        .map_err(|_| "local adoption model response invalid")?;
        let expected = LeadershipContext {
            private_observation: None,
            binding: LeadershipAuthority::from_call(&call),
            source: call.context.clone(),
            context_digest: call
                .context_digest()
                .map_err(|_| "local adoption context invalid")?,
        };
        if !completion.admissible
            || completion.context
                != ModelExecutionContext::AdaptiveLeadershipReview(Box::new(expected))
            || payload.get("version").and_then(|v| v.as_u64()) != Some(2)
            || !payload
                .get("actions")
                .and_then(|v| v.as_array())
                .is_some_and(|v| v.is_empty())
            || payload.get("request_id").and_then(|v| v.as_str())
                != Some(dispatch.request_id.as_str())
            || payload.get("request_digest").and_then(|v| v.as_str())
                != Some(dispatch.request_digest.as_str())
        {
            return Err("local adoption response binding changed");
        }
        let usage: DomainEvent = serde_json::from_value(
            payload
                .get("usage_event")
                .cloned()
                .ok_or("local adoption usage missing")?,
        )
        .map_err(|_| "local adoption usage invalid")?;
        completion.validate_usage(&usage)?;
        let persisted = events
            .event_by_operation_id(&format!("llm_usage_{}", dispatch.request_id))
            .map_err(|_| "local adoption usage unavailable")?
            .ok_or("local adoption usage not durable")?;
        if serde_json::to_value(&persisted).map_err(|_| "local adoption persisted usage invalid")?
            != serde_json::to_value(&usage).map_err(|_| "local adoption usage invalid")?
            || usage.timestamp_ms > call.grant.expires_at_unix_ms
        {
            return Err("local adoption usage or original response time changed");
        }
        let digest = sentinel_common::sha256_hex(completion.content.as_bytes());
        if payload
            .get("model_response_digest")
            .and_then(|v| v.as_str())
            != Some(digest.as_str())
        {
            return Err("local adoption raw response digest changed");
        }
        let decision = super::adaptive_leadership_review::parse_decision(
            &completion.content,
            &call.context.evidence_refs,
        )?;
        decision
            .validate_subject(&call.grant)
            .map_err(|_| "local adoption decision subject invalid")?;
        let AdaptiveLeadershipReviewDecisionKindV1::Continue { window_ms, .. } = &decision.decision
        else {
            return Err("local adoption has no continuation decision");
        };
        let project = self
            .store
            .company_project(&operator.principal.tenant_id, &call.grant.project_id)
            .map_err(|_| "local adoption project unavailable")?
            .ok_or("local adoption project missing")?;
        let current = self
            .authority
            .as_ref()
            .ok_or("local adoption runtime missing")?
            .snapshot(
                &project.tenant_id,
                &project.project_id,
                &call.grant.work_item_id,
                call.grant.assignee_authority.agent_id,
            )
            .map_err(|_| "local adoption assignee unavailable")?;
        let session = self
            .store
            .adaptive_session(call.grant.session_id, &current)
            .map_err(|_| "local adoption session unavailable")?
            .ok_or("local adoption session missing")?;
        if project != call.context.source_project
            || session != call.context.source_session
            || current != call.grant.assignee_authority
        {
            return Err("local adoption source changed");
        }
        call.validate_continuation_source()
            .map_err(|_| "local adoption allowance source invalid")?;
        let leader = self
            .principals
            .principal(&call.grant.leadership_principal.principal_id)
            .ok_or("local adoption leader unavailable")?;
        if leader.principal != call.grant.leadership_principal
            || leader.execution_authority != call.grant.leadership_authority
        {
            return Err("local adoption leader authority changed");
        }
        self.validate_company_employee(&leader.principal)?;
        let (release, repair_digest) = adaptive_recovery_release::verified_current_repair()?;
        let now = now_unix_ms();
        let request = AdaptiveLeadershipLocalAdoptionRequestV1 {
            schema_version: 1,
            operation_id,
            tenant_id: project.tenant_id.clone(),
            project_id: project.project_id.clone(),
            work_item_id: call.grant.work_item_id.clone(),
            session_id: call.grant.session_id,
            review_id,
            epoch_digest: epoch
                .canonical_digest()
                .map_err(|_| "local adoption epoch digest invalid")?,
            original_call_digest: adaptive_leadership_local_adoption_source_call_digest(&call)
                .map_err(|_| "local adoption call digest invalid")?,
            project_digest: adaptive_leadership_recovery_project_digest(&project)
                .map_err(|_| "local adoption project digest invalid")?,
            session_digest: adaptive_leadership_recovery_session_digest(&session)
                .map_err(|_| "local adoption session digest invalid")?,
            session_head_digest: self
                .store
                .adaptive_session_head_digest(call.grant.session_id, &current)
                .map_err(|_| "local adoption head unavailable")?
                .ok_or("local adoption head missing")?,
            request_id: dispatch.request_id.clone(),
            request_digest: dispatch.request_digest.clone(),
            context_digest: dispatch.context_digest.clone(),
            payload_digest: sentinel_common::sha256_hex(row.payload.as_bytes()),
            model_response_digest: digest,
            usage_event_digest: sentinel_common::sha256_hex(
                &sentinel_common::canonical_json(&usage)
                    .map_err(|_| "local adoption usage digest invalid")?,
            ),
            original_completion_error: "continuation audit invalid".to_owned(),
            completion_attempts: row.attempt_count,
            release,
            repair_digest,
            expires_at_unix_ms: expires_at_unix_ms.unwrap_or(
                now.checked_add(*window_ms)
                    .ok_or("local adoption clock overflow")?,
            ),
            decision,
        };
        request
            .validate(&operator.principal, now)
            .map_err(|_| "local adoption request invalid")?;
        Ok(request)
    }

    pub(super) fn local_adoption_http(
        &self,
        principal: &BoundPrincipal,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> WorkflowHttpResponse {
        if principal.principal.kind != CompanyPrincipalKindV1::Operator
            || !matches!(
                principal.principal.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
        {
            return json_error(
                403,
                "authority_conflict",
                "operator local adoption authority required",
                false,
            );
        }
        let Ok(_fence) = self.mutation_fence.write() else {
            return workflow_error(workflow_unavailable());
        };
        if !self.model_work_enabled {
            return workflow_error(workflow_unavailable());
        }
        if method == "GET" {
            let Some(review) = query_parameter(path, "review_id")
                .and_then(|v| Uuid::parse_str(v).ok())
                .filter(|id| !id.is_nil())
            else {
                return json_error(400, "invalid_input", "review_id required", false);
            };
            match self
                .store
                .adaptive_leadership_local_adoption(&principal.principal.tenant_id, review)
            {
                Ok(Some(record)) => {
                    return json(
                        200,
                        &serde_json::json!({"schema_version":1,
                    "adoption_key":record.adoption_key,"review_id":record.request.review_id,
                    "replayed":true,"receipt_kind":"local_adoption_authorized_not_completed"}),
                    )
                }
                Ok(None) => {}
                Err(error) => return workflow_error(error),
            }
            match self.local_adoption_candidate(principal, review, Uuid::new_v4(), None) {
                Ok(request) => json(
                    200,
                    &serde_json::json!({"schema_version":1,"requires_explicit_submission":true,"request":request}),
                ),
                Err(reason) => json_error(409, "adaptive_recovery_conflict", reason, false),
            }
        } else {
            let request: AdaptiveLeadershipLocalAdoptionRequestV1 = match decode_body(body) {
                Ok(value) => value,
                Err(response) => return response,
            };
            if request.tenant_id != principal.principal.tenant_id {
                return json_error(
                    403,
                    "authority_conflict",
                    "local adoption tenant mismatch",
                    false,
                );
            }
            match self
                .store
                .adaptive_leadership_local_adoption(&request.tenant_id, request.review_id)
            {
                Ok(Some(_)) => {
                    return match self.store.authorize_adaptive_leadership_local_adoption(
                        &principal.principal,
                        &request,
                        now_unix_ms(),
                    ) {
                        Ok((replayed, record)) => json(
                            200,
                            &serde_json::json!({"schema_version":1,
                        "adoption_key":record.adoption_key,"review_id":record.request.review_id,
                        "replayed":replayed,"receipt_kind":"local_adoption_authorized_not_completed"}),
                        ),
                        Err(error) => workflow_error(error),
                    }
                }
                Ok(None) => {}
                Err(error) => return workflow_error(error),
            }
            match self.local_adoption_candidate(
                principal,
                request.review_id,
                request.operation_id,
                Some(request.expires_at_unix_ms),
            ) {
                Ok(expected) if expected == request => {}
                Ok(_) => {
                    return json_error(
                        409,
                        "adaptive_recovery_conflict",
                        "local adoption binding changed",
                        false,
                    )
                }
                Err(reason) => return json_error(409, "adaptive_recovery_conflict", reason, false),
            }
            match self.store.authorize_adaptive_leadership_local_adoption(
                &principal.principal,
                &request,
                now_unix_ms(),
            ) {
                Ok((replayed, record)) => json(
                    200,
                    &serde_json::json!({"schema_version":1,
                    "adoption_key":record.adoption_key,"review_id":record.request.review_id,
                    "replayed":replayed,"receipt_kind":"local_adoption_authorized_not_completed"}),
                ),
                Err(error) => workflow_error(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::adaptive_continuation_tests::{
        fixture_schema2_recovery_source, fixture_schema2_recovery_source_with_current_allowance,
        reserve_and_claim_schema2,
    };
    use sentinel_workflow::{
        AdaptiveContinuationAuthorizationV1, AdaptiveContinuationSourceV1,
        AdaptiveLeadershipLocalAdoptionV1, AdaptiveLeadershipReviewCallV1,
        AdaptiveLeadershipReviewDecisionV1, AdaptiveLeadershipReviewSubjectV2,
        ClaimAdaptiveLeadershipReviewCallV1, CompleteAdaptiveLeadershipReviewCallV1, WorkflowStore,
        ADAPTIVE_LEADERSHIP_MAX_REVIEWS,
    };

    struct LocalFixture {
        temp: tempfile::TempDir,
        api: WorkflowApi,
        pending: AdaptiveLeadershipReviewCallV1,
        result: CompleteAdaptiveLeadershipReviewCallV1,
        adoption: AdaptiveLeadershipLocalAdoptionV1,
        payload: String,
        usage: DomainEvent,
        unknown_row: Option<sentinel_limbo::LlmCompletionEntry>,
        _repair: adaptive_recovery_release::TestRepairGuard,
    }

    impl LocalFixture {
        fn new(unknown: bool) -> Self {
            Self::with_current_allowance(unknown, false)
        }

        fn with_current_allowance(unknown: bool, replace_current_allowance: bool) -> Self {
            // Substitutes installed evidence only. All durable receipts come from real stores.
            let guard = adaptive_recovery_release::TestRepairGuard::fixture().unwrap();
            let temp = tempfile::tempdir().unwrap();
            let source_fixture = if replace_current_allowance {
                fixture_schema2_recovery_source_with_current_allowance
            } else {
                fixture_schema2_recovery_source
            };
            let (api, initial) = source_fixture(
                &temp.path().join("company.sqlite"),
                &temp.path().join("events.sqlite"),
                unknown,
            );
            let leader = api.principals.principal("pm").unwrap();
            let mut call = api
                .store
                .adaptive_leadership_review_call(
                    &leader.principal.tenant_id,
                    initial.binding.grant.review_id,
                )
                .unwrap()
                .unwrap();
            for index in 0..ADAPTIVE_LEADERSHIP_MAX_REVIEWS {
                // Synthetic dispatch admission only; no provider request or response is implied.
                let claimed = api
                    .store
                    .claim_adaptive_leadership_review_call(
                        &leader.principal,
                        &ClaimAdaptiveLeadershipReviewCallV1 {
                            review_id: call.grant.review_id,
                            allowance_id: call.allowance_id.clone(),
                            request_id: call.request_id(),
                            request_digest: "b".repeat(64),
                            context_digest: call.context_digest().unwrap(),
                        },
                        call.grant_issued_at_unix_ms + 1,
                    )
                    .unwrap();
                let retired = api
                    .store
                    .expire_adaptive_leadership_review_call(
                        &leader.principal,
                        call.grant.review_id,
                        claimed.version,
                        call.grant.expires_at_unix_ms,
                    )
                    .unwrap();
                if index + 1 < ADAPTIVE_LEADERSHIP_MAX_REVIEWS {
                    let issued = retired.updated_at_unix_ms + 1;
                    let mut context = retired.context.clone();
                    context.evidence_refs.push(format!(
                        "leadership-review-retired:{}:{}",
                        retired.grant.review_id,
                        retired.retired_at_unix_ms.unwrap()
                    ));
                    context.evidence_refs.sort();
                    let mut grant = retired.grant.clone();
                    grant.evidence_fingerprint =
                        sentinel_workflow::adaptive_leadership_evidence_fingerprint(
                            &context.tool_catalog,
                            &context.evidence_refs,
                        )
                        .unwrap();
                    grant.review_id = sentinel_workflow::adaptive_leadership_review_id(
                        grant.session_id,
                        grant.expected_session_version,
                        &grant.evidence_fingerprint,
                    )
                    .unwrap();
                    grant.expires_at_unix_ms = issued + 1_000;
                    call = api
                        .store
                        .authorize_adaptive_leadership_review_call(
                            &leader.principal,
                            Uuid::new_v4(),
                            &format!("local-fixture-history-{index}"),
                            &grant,
                            &context,
                            issued,
                        )
                        .unwrap();
                }
            }
            let operator = api.principals.principal("operator").unwrap();
            let epoch_path = format!(
                "{ADAPTIVE_REVIEW_EPOCH_PATH}?project_id={}&session_id={}",
                call.grant.project_id, call.grant.session_id
            );
            let draft = api.review_recovery_epoch(&operator, "GET", &epoch_path, &[]);
            assert_eq!(
                draft.status,
                200,
                "{}",
                String::from_utf8_lossy(&draft.body)
            );
            let draft: serde_json::Value = serde_json::from_slice(&draft.body).unwrap();
            let issued = api.review_recovery_epoch(
                &operator,
                "POST",
                ADAPTIVE_REVIEW_EPOCH_PATH,
                &serde_json::to_vec(&draft["request"]).unwrap(),
            );
            assert_eq!(
                issued.status,
                200,
                "{}",
                String::from_utf8_lossy(&issued.body)
            );
            let epoch = api
                .store
                .adaptive_leadership_recovery_epoch(
                    &leader.principal.tenant_id,
                    call.grant.session_id,
                )
                .unwrap()
                .unwrap();
            let epoch_call = api
                .store
                .adaptive_leadership_review_call(&leader.principal.tenant_id, epoch.review_id)
                .unwrap()
                .unwrap();
            let context = LeadershipContext {
                private_observation: None,
                binding: LeadershipAuthority::from_call(&epoch_call),
                context_digest: epoch_call.context_digest().unwrap(),
                source: epoch_call.context.clone(),
            };
            let (id, digest) = reserve_and_claim_schema2(&api, &context);
            let completion = ModelExecutionCompletion {
                context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
                content: serde_json::json!({"schema_version":2,"decision":{
                    "kind":"continue","additional_model_calls":1,"window_ms":120_000,
                    "rationale":"Inspect the unchanged assignment before continuing.",
                    "evidence_refs":[context.source.evidence_refs[0]]}})
                .to_string(),
                admissible: true,
            };
            super::super::adaptive_leadership_review::tests::persist(
                &api,
                &completion,
                &context,
                &id,
                &digest,
                true,
            );
            let events = api.event_store.as_ref().unwrap();
            events
                .record_llm_completion_failure(&id, &digest, "continuation audit invalid", 1)
                .unwrap();
            let row = events.get_llm_completion(&id).unwrap().unwrap();
            let usage = events
                .event_by_operation_id(&format!("llm_usage_{id}"))
                .unwrap()
                .unwrap();
            let request = api
                .local_adoption_candidate(
                    &operator,
                    epoch.review_id,
                    Uuid::new_v4(),
                    Some(now_unix_ms() + 60_000),
                )
                .unwrap();
            let (_, adoption) = api
                .store
                .authorize_adaptive_leadership_local_adoption(
                    &operator.principal,
                    &request,
                    now_unix_ms(),
                )
                .unwrap();
            let pending = api
                .store
                .adaptive_leadership_review_call(&leader.principal.tenant_id, epoch.review_id)
                .unwrap()
                .unwrap();
            assert!(adoption.request.expires_at_unix_ms < pending.grant.expires_at_unix_ms);
            let decision: AdaptiveLeadershipReviewDecisionV1 =
                serde_json::from_str(&completion.content).unwrap();
            let response = sentinel_common::sha256_hex(completion.content.as_bytes());
            let audit_id = sentinel_workflow::adaptive_leadership_continuation_audit_id(
                pending.grant.review_id,
                &digest,
                &response,
                &decision,
            )
            .unwrap();
            let allowance = pending
                .continuation_allowance(
                    adoption.issued_at_unix_ms,
                    adoption.continuation_deadline_ms,
                    1,
                )
                .unwrap();
            let (source, abandoned_model_effect) = match pending.grant.subject.as_ref().unwrap() {
                AdaptiveLeadershipReviewSubjectV2::UnknownModel { effect, .. } => (
                    AdaptiveContinuationSourceV1::ModelUnknown,
                    Some(effect.clone()),
                ),
                AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                    reason_code,
                    resolution_event_id: None,
                } => (
                    AdaptiveContinuationSourceV1::Blocked {
                        reason_code: reason_code.clone(),
                    },
                    None,
                ),
                _ => panic!("unexpected local fixture subject"),
            };
            let result = CompleteAdaptiveLeadershipReviewCallV1 {
                review_id: pending.grant.review_id, allowance_id: pending.allowance_id.clone(),
                request_digest: digest, model_response_digest: response, decision,
                resolution_event_id: Some(audit_id),
                continuation: Some(AdaptiveContinuationAuthorizationV1 {
                    schema_version: 1, operation_id: pending.operation_id, review_id: pending.grant.review_id,
                    resolution_event_id: audit_id, session_id: pending.grant.session_id,
                    source_session_version: pending.grant.expected_session_version,
                    source, abandoned_model_effect, provider_allowance_id: allowance.allowance_id.clone(),
                    provider_authority_digest: sentinel_workflow::adaptive_leadership_continuation_provider_authority_digest(
                        &allowance, &pending.grant.assignee_authority,
                    ).unwrap(),
                    issued_at_ms: adoption.issued_at_unix_ms, deadline_ms: adoption.continuation_deadline_ms,
                    additional_model_calls: 1, local_adoption: Some(Box::new(adoption.clone())),
                }),
            };
            pending.validate_completion_proposal(&result).unwrap();
            let unknown_row = match &result.continuation.as_ref().unwrap().abandoned_model_effect {
                Some(effect) => Some(
                    events
                        .get_llm_completion(&format!(
                            "company-adaptive-{}-{}",
                            pending.grant.session_id, effect.id
                        ))
                        .unwrap()
                        .unwrap(),
                ),
                None => None,
            };
            Self {
                temp,
                api,
                pending,
                result,
                adoption,
                payload: row.payload,
                usage,
                unknown_row,
                _repair: guard,
            }
        }

        fn reopen(&mut self) {
            self.api.store =
                Arc::new(WorkflowStore::open(self.temp.path().join("company.sqlite")).unwrap());
            self.api.event_store = Some(
                sentinel_limbo::EventStore::open(
                    self.temp.path().join("events.sqlite").to_str().unwrap(),
                )
                .unwrap(),
            );
        }

        fn call(&self) -> AdaptiveLeadershipReviewCallV1 {
            self.api
                .store
                .adaptive_leadership_review_call(
                    &self.pending.grant.leadership_principal.tenant_id,
                    self.pending.grant.review_id,
                )
                .unwrap()
                .unwrap()
        }

        fn session(&self) -> AdaptiveSessionV1 {
            self.api
                .store
                .adaptive_session(
                    self.pending.grant.session_id,
                    &self.pending.grant.assignee_authority,
                )
                .unwrap()
                .unwrap()
        }

        fn project(&self) -> sentinel_workflow::ProjectV1 {
            self.api
                .store
                .company_project(
                    &self.adoption.request.tenant_id,
                    &self.pending.grant.project_id,
                )
                .unwrap()
                .unwrap()
        }

        fn reconcile(&self, now: u64) -> bool {
            let call = self.call();
            let _fence = self.api.mutation_fence.write().unwrap();
            self.api
                .reconcile_local_leadership_adoption(&call, &|| now)
                .unwrap()
        }

        fn reconcile_project(&self) {
            let project = self.project();
            let calls = self
                .api
                .store
                .adaptive_leadership_review_calls(
                    &self.adoption.request.tenant_id,
                    self.pending.grant.session_id,
                )
                .unwrap();
            let _fence = self.api.mutation_fence.write().unwrap();
            self.api
                .reconcile_adaptive_leadership_reviews(&project)
                .unwrap();
            assert_eq!(
                self.api
                    .store
                    .adaptive_leadership_review_calls(
                        &self.adoption.request.tenant_id,
                        self.pending.grant.session_id,
                    )
                    .unwrap(),
                calls
            );
        }

        fn append_audit(&self) -> sentinel_common::EventEnvelopeV2 {
            let _fence = self.api.mutation_fence.write().unwrap();
            assert_eq!(
                self.api
                    .append_continuation_audit(&self.pending, &self.result)
                    .unwrap(),
                self.result
            );
            let event = self
                .api
                .event_store
                .as_ref()
                .unwrap()
                .event_v2_by_id(&self.result.resolution_event_id.unwrap().to_string())
                .unwrap()
                .unwrap();
            event.validate_seals().unwrap();
            let appended = u64::try_from(event.appended_at_ms).unwrap();
            assert!(appended >= self.adoption.issued_at_unix_ms);
            assert!(appended < self.adoption.request.expires_at_unix_ms);
            event
        }

        fn outbox(&self) -> sentinel_limbo::LlmCompletionEntry {
            self.api
                .event_store
                .as_ref()
                .unwrap()
                .get_llm_completion(&self.adoption.request.request_id)
                .unwrap()
                .unwrap()
        }

        fn cleanup_receipt(&self) -> Option<DomainEvent> {
            self.api
                .event_store
                .as_ref()
                .unwrap()
                .event_by_operation_id(&format!(
                    "llm_local_adoption_{}",
                    self.adoption.request.request_id
                ))
                .unwrap()
        }

        fn retirement_receipt(&self) -> Option<DomainEvent> {
            self.api
                .event_store
                .as_ref()
                .unwrap()
                .event_by_operation_id(&format!(
                    "llm_local_adoption_retired_{}",
                    self.adoption.request.request_id
                ))
                .unwrap()
        }

        fn assert_retirement_receipt(
            &self,
            retired: &AdaptiveLeadershipReviewCallV1,
        ) -> DomainEvent {
            let receipt = self.retirement_receipt().unwrap();
            assert_eq!(receipt.event_type, "llm_completion_local_adoption_retired");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&receipt.payload).unwrap(),
                serde_json::json!({
                    "request_id":self.adoption.request.request_id,"request_digest":self.adoption.request.request_digest,
                    "payload_digest":self.adoption.request.payload_digest,"adoption_key":self.adoption.adoption_key,
                    "domain_receipt_digest":sentinel_common::sha256_hex(&sentinel_common::canonical_json(retired).unwrap()),
                    "original_completion_error":"continuation audit invalid"
                })
            );
            receipt
        }

        fn domain_event_count(&self) -> i64 {
            let connection = sentinel_limbo::rusqlite::Connection::open_with_flags(
                self.temp.path().join("company.sqlite"),
                sentinel_limbo::rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            connection
                .query_row("SELECT COUNT(*) FROM company_events", [], |row| row.get(0))
                .unwrap()
        }

        fn original_provider_project(&self) -> sentinel_workflow::ProjectV1 {
            let root = &self.pending.context.source_session.grant;
            self.api
                .store
                .historical_adaptive_provider_project(root, root.created_at_ms + 1)
                .unwrap()
                .unwrap()
        }

        fn assert_only_current_allowance_replaced(
            &self,
            completed: &AdaptiveLeadershipReviewCallV1,
        ) {
            let source = &self.pending.context.source_session;
            let current = self
                .pending
                .context
                .source_project
                .subscription_call
                .as_ref()
                .unwrap();
            let root = &source.grant;
            assert_ne!(root.provider_allowance_id, current.allowance_id);
            assert!(current.dispatch.is_none());
            assert_eq!(
                self.original_provider_project()
                    .subscription_call
                    .as_ref()
                    .unwrap()
                    .allowance_id,
                root.provider_allowance_id
            );
            let continued = self.session();
            assert_eq!(continued.grant, *root);
            assert_eq!(continued.model_calls, source.model_calls);
            assert_eq!(continued.tool_calls, source.tool_calls);
            assert_eq!(continued.version, source.version + 1);
            let project = self.project();
            let fresh = project.subscription_call.as_ref().unwrap();
            assert_eq!(
                fresh.allowance_id,
                self.result
                    .continuation
                    .as_ref()
                    .unwrap()
                    .provider_allowance_id
            );
            assert_ne!(fresh.allowance_id, root.provider_allowance_id);
            assert_ne!(fresh.allowance_id, current.allowance_id);
            assert_eq!(fresh.grant.max_calls, 1);
            assert!(fresh.grant.max_calls <= root.max_model_calls - source.model_calls);
            assert_eq!(
                fresh.grant.expires_at_unix_ms,
                self.adoption.continuation_deadline_ms
            );
            assert_eq!(
                project.work_items,
                self.pending.context.source_project.work_items
            );
            assert_eq!(
                project.governance,
                self.pending.context.source_project.governance
            );
            let abandoned = self
                .api
                .store
                .adaptive_leadership_abandoned_allowance(
                    &self.adoption.request.tenant_id,
                    &current.allowance_id,
                )
                .unwrap()
                .unwrap();
            assert_eq!(abandoned.review, *completed);
            assert!(self
                .api
                .store
                .adaptive_leadership_abandoned_allowance(
                    &self.adoption.request.tenant_id,
                    &root.provider_allowance_id,
                )
                .unwrap()
                .is_none());
        }

        fn assert_source_unchanged(&self) {
            assert_eq!(self.session(), self.pending.context.source_session);
            assert_eq!(self.project(), self.pending.context.source_project);
        }

        fn assert_retained_payload(&self, status: &str, error: Option<&str>) {
            let events = self.api.event_store.as_ref().unwrap();
            let row = events
                .get_llm_completion(&self.adoption.request.request_id)
                .unwrap()
                .unwrap();
            assert_eq!(row.payload, self.payload);
            assert_eq!(row.request_digest, self.adoption.request.request_digest);
            assert_eq!(row.status, status);
            assert_eq!(row.last_error.as_deref(), error);
            assert_eq!(
                serde_json::to_value(
                    events
                        .event_by_operation_id(&format!(
                            "llm_usage_{}",
                            self.adoption.request.request_id
                        ),)
                        .unwrap()
                        .unwrap()
                )
                .unwrap(),
                serde_json::to_value(&self.usage).unwrap()
            );
            assert_eq!(
                self.api
                    .store
                    .adaptive_leadership_local_adoption(
                        &self.adoption.request.tenant_id,
                        self.adoption.request.review_id,
                    )
                    .unwrap(),
                Some(self.adoption.clone())
            );
            assert_eq!(
                self.api
                    .store
                    .adaptive_leadership_review_calls(
                        &self.adoption.request.tenant_id,
                        self.pending.grant.session_id,
                    )
                    .unwrap()
                    .len(),
                ADAPTIVE_LEADERSHIP_MAX_REVIEWS + 1
            );
            if let Some(row) = &self.unknown_row {
                assert_eq!(
                    events.get_llm_completion(&row.request_id).unwrap(),
                    Some(row.clone())
                );
            }
        }

        fn assert_no_adoption_effects(&self) {
            let events = self.api.event_store.as_ref().unwrap();
            assert!(events
                .event_v2_by_id(&self.result.resolution_event_id.unwrap().to_string())
                .unwrap()
                .is_none());
            assert!(events
                .event_by_operation_id(&format!(
                    "llm_local_adoption_{}",
                    self.adoption.request.request_id
                ),)
                .unwrap()
                .is_none());
        }
    }

    #[test]
    fn local_adoption_audited_fixed_deadline_retirement_replays_and_repairs_interrupted_disposition(
    ) {
        for unknown in [false, true] {
            for interrupted in [false, true] {
                let mut f = LocalFixture::with_current_allowance(unknown, true);
                let audit = f.append_audit();
                f.assert_source_unchanged();
                let deadline = f.adoption.continuation_deadline_ms;
                let original_row = f.outbox();
                let before_disposition = f
                    .api
                    .event_store
                    .as_ref()
                    .unwrap()
                    .get_all_events()
                    .unwrap()
                    .len();
                if interrupted {
                    let _fence = f.api.mutation_fence.write().unwrap();
                    f.api
                        .store
                        .retire_expired_adaptive_continuation_call(
                            &f.pending.grant.leadership_principal,
                            &f.result,
                            deadline,
                        )
                        .unwrap();
                    f.assert_retained_payload("failed", Some("continuation audit invalid"));
                    assert_eq!(f.outbox(), original_row);
                    assert!(f.retirement_receipt().is_none());
                } else {
                    assert!(f.reconcile(deadline));
                    f.assert_retained_payload("failed", Some("leadership_review_stale"));
                }
                let retired = f.call();
                assert_eq!(retired.version, 4);
                assert_eq!(retired.retired_at_unix_ms, Some(deadline));
                assert!(retired.decision.is_none());
                assert!(retired.continuation.is_none());
                assert!(f.cleanup_receipt().is_none());
                let domain_events = f.domain_event_count();
                f.reopen();
                assert!(!f.reconcile(deadline + 1));
                f.assert_retained_payload("failed", Some("leadership_review_stale"));
                let disposed = f.outbox();
                let mut expected_disposition = original_row;
                expected_disposition.last_error = Some("leadership_review_stale".into());
                assert_eq!(disposed, expected_disposition);
                let receipt = f.assert_retirement_receipt(&retired);
                let event_count = f
                    .api
                    .event_store
                    .as_ref()
                    .unwrap()
                    .get_all_events()
                    .unwrap()
                    .len();
                assert_eq!(event_count, before_disposition + 1);
                for replay_at in [deadline + 2, deadline + 1_000] {
                    assert!(!f.reconcile(replay_at));
                    f.reconcile_project();
                    assert_eq!(
                        f.api
                            .store
                            .retire_expired_adaptive_continuation_call(
                                &f.pending.grant.leadership_principal,
                                &f.result,
                                replay_at,
                            )
                            .unwrap(),
                        retired
                    );
                    assert_eq!(f.call(), retired);
                    f.assert_source_unchanged();
                    f.assert_retained_payload("failed", Some("leadership_review_stale"));
                    assert_eq!(f.outbox(), disposed);
                    assert_eq!(f.domain_event_count(), domain_events);
                    assert_eq!(
                        f.api
                            .event_store
                            .as_ref()
                            .unwrap()
                            .get_all_events()
                            .unwrap()
                            .len(),
                        event_count
                    );
                    assert_eq!(
                        f.api
                            .event_store
                            .as_ref()
                            .unwrap()
                            .event_v2_by_id(&audit.event_id)
                            .unwrap(),
                        Some(audit.clone())
                    );
                    assert_eq!(
                        serde_json::to_value(f.assert_retirement_receipt(&retired)).unwrap(),
                        serde_json::to_value(&receipt).unwrap()
                    );
                    assert!(f.cleanup_receipt().is_none());
                }
            }
        }
    }

    #[test]
    fn local_adoption_domain_commit_crash_cleans_once_and_replaces_only_current_allowance() {
        for unknown in [false, true] {
            let mut f = LocalFixture::with_current_allowance(unknown, true);
            let original_project = f.original_provider_project();
            let root = &f.pending.context.source_session.grant;
            let current = f
                .pending
                .context
                .source_project
                .subscription_call
                .as_ref()
                .unwrap();
            assert_ne!(root.provider_allowance_id, current.allowance_id);
            assert!(current.dispatch.is_none());
            let audit = f.append_audit();
            let now = f.adoption.request.expires_at_unix_ms + 1;
            assert!(now < f.adoption.continuation_deadline_ms);
            let completed = {
                let _fence = f.api.mutation_fence.write().unwrap();
                f.api
                    .store
                    .complete_adaptive_leadership_review_call_with_local_adoption_audit(
                        &f.pending.grant.leadership_principal,
                        &f.result,
                        &audit,
                        now,
                    )
                    .unwrap()
            };
            assert_eq!(completed.version, 3);
            assert_eq!(completed.continuation, f.result.continuation);
            f.assert_only_current_allowance_replaced(&completed);
            f.assert_retained_payload("failed", Some("continuation audit invalid"));
            assert!(f.cleanup_receipt().is_none());
            let continued = f.session();
            let project = f.project();
            let domain_events = f.domain_event_count();
            let event_count = f
                .api
                .event_store
                .as_ref()
                .unwrap()
                .get_all_events()
                .unwrap()
                .len();
            f.reopen();
            // The domain receipt already exists: reconciliation must only finish cleanup.
            assert!(!f.reconcile(now + 1));
            assert_eq!(f.call(), completed);
            assert_eq!(f.session(), continued);
            assert_eq!(f.project(), project);
            assert_eq!(f.domain_event_count(), domain_events);
            assert_eq!(
                f.api
                    .event_store
                    .as_ref()
                    .unwrap()
                    .get_all_events()
                    .unwrap()
                    .len(),
                event_count + 1
            );
            f.assert_retained_payload("action_claimed", Some("continuation audit invalid"));
            let receipt = f.cleanup_receipt().unwrap();
            assert_eq!(receipt.event_type, "llm_completion_locally_adopted");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&receipt.payload).unwrap(),
                serde_json::json!({
                    "request_id":f.adoption.request.request_id,"request_digest":f.adoption.request.request_digest,
                    "payload_digest":f.adoption.request.payload_digest,"adoption_key":f.adoption.adoption_key,
                    "domain_receipt_digest":sentinel_common::sha256_hex(&sentinel_common::canonical_json(&completed).unwrap())
                })
            );
            let disposed = f.outbox();
            for replay_at in [now + 2, f.adoption.continuation_deadline_ms + 1] {
                assert!(!f.reconcile(replay_at));
                f.reconcile_project();
                assert_eq!(f.call(), completed);
                assert_eq!(f.session(), continued);
                assert_eq!(f.project(), project);
                assert_eq!(f.original_provider_project(), original_project);
                f.assert_only_current_allowance_replaced(&completed);
                f.assert_retained_payload("action_claimed", Some("continuation audit invalid"));
                assert_eq!(f.outbox(), disposed);
                assert_eq!(f.domain_event_count(), domain_events);
                assert_eq!(
                    f.api
                        .event_store
                        .as_ref()
                        .unwrap()
                        .get_all_events()
                        .unwrap()
                        .len(),
                    event_count + 1
                );
                assert_eq!(
                    f.api
                        .event_store
                        .as_ref()
                        .unwrap()
                        .event_v2_by_id(&audit.event_id)
                        .unwrap(),
                    Some(audit.clone())
                );
                assert_eq!(
                    serde_json::to_value(f.cleanup_receipt().unwrap()).unwrap(),
                    serde_json::to_value(&receipt).unwrap()
                );
            }
        }
    }

    #[test]
    fn local_adoption_source_drift_retires_and_disposes_in_same_reconciliation() {
        for unknown in [false, true] {
            let f = LocalFixture::with_current_allowance(unknown, true);
            let source = f.project();
            let now = now_unix_ms().max(f.adoption.issued_at_unix_ms + 1);
            assert!(now < f.adoption.request.expires_at_unix_ms);
            let changed = {
                let _fence = f.api.mutation_fence.write().unwrap();
                let response =
                    f.api
                        .store
                        .apply_company_command(
                            &f.pending.grant.leadership_principal,
                            Uuid::new_v4(),
                            &CompanyWorkflowCommandV1::RecordDecision {
                                project_id: source.project_id.clone(),
                                expected_version: source.version,
                                work_item_id: None,
                                choice_ref: "Independent fixture decision".into(),
                                rationale_ref:
                                    "Retain assignments and governance while changing the source"
                                        .into(),
                            },
                            now,
                        )
                        .unwrap()
                        .response;
                let sentinel_workflow::CompanyWorkflowResponseV1::Project(project) = response
                else {
                    panic!("source drift project");
                };
                *project
            };
            assert_ne!(changed, source);
            assert_eq!(changed.work_items, source.work_items);
            assert_eq!(changed.governance, source.governance);
            let original_row = f.outbox();
            let domain_events = f.domain_event_count();
            let event_count = f
                .api
                .event_store
                .as_ref()
                .unwrap()
                .get_all_events()
                .unwrap()
                .len();
            assert!(f.reconcile(now));
            let retired = f.call();
            assert_eq!(retired.version, 4);
            assert_eq!(retired.retired_at_unix_ms, Some(now));
            assert!(retired.decision.is_none());
            assert!(retired.continuation.is_none());
            let receipt = f.assert_retirement_receipt(&retired);
            let mut disposed = original_row;
            disposed.last_error = Some("leadership_review_stale".into());
            assert_eq!(f.outbox(), disposed);
            f.assert_retained_payload("failed", Some("leadership_review_stale"));
            assert_eq!(f.domain_event_count(), domain_events + 1);
            assert_eq!(
                f.api
                    .event_store
                    .as_ref()
                    .unwrap()
                    .get_all_events()
                    .unwrap()
                    .len(),
                event_count + 1
            );
            for replay_at in [now + 1, f.adoption.continuation_deadline_ms + 1] {
                assert!(!f.reconcile(replay_at));
                f.reconcile_project();
                assert_eq!(f.call(), retired);
                assert_eq!(f.project(), changed);
                assert_eq!(f.session(), f.pending.context.source_session);
                f.assert_no_adoption_effects();
                f.assert_retained_payload("failed", Some("leadership_review_stale"));
                assert_eq!(f.outbox(), disposed);
                assert_eq!(f.domain_event_count(), domain_events + 1);
                assert_eq!(
                    f.api
                        .event_store
                        .as_ref()
                        .unwrap()
                        .get_all_events()
                        .unwrap()
                        .len(),
                    event_count + 1
                );
                assert_eq!(
                    serde_json::to_value(f.assert_retirement_receipt(&retired)).unwrap(),
                    serde_json::to_value(&receipt).unwrap()
                );
            }
        }
    }

    #[test]
    fn local_adoption_real_audit_late_completion_and_cleanup_replay() {
        for unknown in [false, true] {
            let mut f = LocalFixture::new(unknown);
            let audited = {
                let _fence = f.api.mutation_fence.write().unwrap();
                f.api
                    .append_continuation_audit(&f.pending, &f.result)
                    .unwrap()
            };
            assert_eq!(audited, f.result);
            let event = f
                .api
                .event_store
                .as_ref()
                .unwrap()
                .event_v2_by_id(&f.result.resolution_event_id.unwrap().to_string())
                .unwrap()
                .unwrap();
            event.validate_seals().unwrap();
            assert!(u64::try_from(event.appended_at_ms).unwrap() >= f.adoption.issued_at_unix_ms);
            assert!(
                u64::try_from(event.appended_at_ms).unwrap()
                    < f.adoption.request.expires_at_unix_ms
            );
            f.assert_source_unchanged();
            f.reopen();
            let now = f.adoption.request.expires_at_unix_ms + 1;
            assert!(now < f.adoption.continuation_deadline_ms);
            assert!(f
                .api
                .store
                .complete_adaptive_leadership_review_call(
                    &f.pending.grant.leadership_principal,
                    &f.result,
                    now,
                )
                .is_err());
            assert!(f.reconcile(now));
            let completed = f.call();
            assert_eq!(completed.version, 3);
            assert_eq!(completed.continuation, f.result.continuation);
            let continued = f.session();
            let source = &f.pending.context.source_session;
            assert_eq!(continued.grant, source.grant);
            assert_eq!(continued.model_calls, source.model_calls);
            assert_eq!(continued.tool_calls, source.tool_calls);
            assert_eq!(continued.version, source.version + 1);
            assert_eq!(
                continued
                    .continuation
                    .as_ref()
                    .unwrap()
                    .authorizations
                    .last(),
                f.result.continuation.as_ref()
            );
            let project = f.project();
            assert_eq!(
                project.work_items,
                f.pending.context.source_project.work_items
            );
            assert_eq!(
                project.governance,
                f.pending.context.source_project.governance
            );
            if let Some(effect) = &f
                .result
                .continuation
                .as_ref()
                .unwrap()
                .abandoned_model_effect
            {
                assert!(continued.is_abandoned_model_effect(effect));
            }
            f.assert_retained_payload("action_claimed", Some("continuation audit invalid"));
            let events = f.api.event_store.as_ref().unwrap();
            let receipt = events
                .event_by_operation_id(&format!(
                    "llm_local_adoption_{}",
                    f.adoption.request.request_id
                ))
                .unwrap()
                .unwrap();
            assert_eq!(receipt.event_type, "llm_completion_locally_adopted");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&receipt.payload).unwrap(),
                serde_json::json!({
                    "request_id":f.adoption.request.request_id,"request_digest":f.adoption.request.request_digest,
                    "payload_digest":f.adoption.request.payload_digest,"adoption_key":f.adoption.adoption_key,
                    "domain_receipt_digest":sentinel_common::sha256_hex(&sentinel_common::canonical_json(&completed).unwrap())
                })
            );
            let before = events.get_all_events().unwrap().len();
            for replay_at in [now + 1, f.adoption.continuation_deadline_ms + 1] {
                assert!(!f.reconcile(replay_at));
                assert_eq!(f.call(), completed);
                assert_eq!(f.session(), continued);
                assert_eq!(f.project(), project);
                assert_eq!(
                    events.event_v2_by_id(&event.event_id).unwrap(),
                    Some(event.clone())
                );
                assert_eq!(events.get_all_events().unwrap().len(), before);
                assert_eq!(
                    serde_json::to_value(
                        events
                            .event_by_operation_id(&receipt.operation_id)
                            .unwrap()
                            .unwrap()
                    )
                    .unwrap(),
                    serde_json::to_value(&receipt).unwrap()
                );
            }
        }
    }

    #[test]
    fn local_adoption_pre_audit_expiry_retires_then_reconciles_terminal_unchanged() {
        for unknown in [false, true] {
            let mut f = LocalFixture::new(unknown);
            let now = f.adoption.request.expires_at_unix_ms;
            assert!(now < f.pending.grant.expires_at_unix_ms);
            assert!(f.reconcile(now));
            let retired = f.call();
            assert_eq!(retired.version, 4);
            assert_eq!(retired.retired_at_unix_ms, Some(now));
            assert!(retired.decision.is_none());
            assert!(retired.continuation.is_none());
            f.assert_source_unchanged();
            f.assert_no_adoption_effects();
            f.assert_retained_payload("failed", Some("leadership_review_stale"));
            let row = f
                .api
                .event_store
                .as_ref()
                .unwrap()
                .get_llm_completion(&f.adoption.request.request_id)
                .unwrap()
                .unwrap();
            f.reopen();
            let before = f
                .api
                .event_store
                .as_ref()
                .unwrap()
                .get_all_events()
                .unwrap()
                .len();
            for replay_at in [now + 1, f.adoption.continuation_deadline_ms + 1] {
                assert!(!f.reconcile(replay_at));
                assert_eq!(f.call(), retired);
                f.assert_source_unchanged();
                f.assert_no_adoption_effects();
                f.assert_retained_payload("failed", Some("leadership_review_stale"));
                let after = f
                    .api
                    .event_store
                    .as_ref()
                    .unwrap()
                    .get_llm_completion(&f.adoption.request.request_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(after, row);
                assert_eq!(
                    f.api
                        .event_store
                        .as_ref()
                        .unwrap()
                        .get_all_events()
                        .unwrap()
                        .len(),
                    before
                );
            }
            f.reconcile_project();
            assert_eq!(f.call(), retired);
            f.assert_source_unchanged();
            f.assert_retained_payload("failed", Some("leadership_review_stale"));
        }
    }

    #[test]
    fn local_adoption_retirement_crash_repairs_only_exact_outbox_disposition() {
        for unknown in [false, true] {
            let mut f = LocalFixture::new(unknown);
            let now = f.adoption.request.expires_at_unix_ms;
            let retired = f
                .api
                .store
                .retire_expired_adaptive_local_adoption(
                    &f.pending.grant.leadership_principal,
                    f.pending.grant.review_id,
                    f.pending.version,
                    now,
                )
                .unwrap();
            assert_eq!(retired.version, 4);
            f.assert_retained_payload("failed", Some("continuation audit invalid"));
            f.reopen();
            assert!(!f.reconcile(now + 1));
            assert_eq!(f.call(), retired);
            f.assert_retained_payload("failed", Some("leadership_review_stale"));
            f.assert_source_unchanged();
            f.assert_no_adoption_effects();
            let row = f
                .api
                .event_store
                .as_ref()
                .unwrap()
                .get_llm_completion(&f.adoption.request.request_id)
                .unwrap()
                .unwrap();
            for replay_at in [now + 2, f.adoption.continuation_deadline_ms + 1] {
                assert!(!f.reconcile(replay_at));
                assert_eq!(f.call(), retired);
                f.assert_source_unchanged();
                f.assert_retained_payload("failed", Some("leadership_review_stale"));
                let after = f
                    .api
                    .event_store
                    .as_ref()
                    .unwrap()
                    .get_llm_completion(&f.adoption.request.request_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(after, row);
            }
            f.reconcile_project();
            assert_eq!(f.call(), retired);
            f.assert_source_unchanged();
            f.assert_retained_payload("failed", Some("leadership_review_stale"));
        }
    }

    #[test]
    fn local_adoption_http_requires_authenticated_operator_and_exact_review_id() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let api = super::super::model_work::configured_test_api(&path);
        let mut headers = HashMap::new();
        assert_eq!(
            api.handle("GET", ADAPTIVE_LOCAL_ADOPTION_PATH, &headers, &[])
                .unwrap()
                .status,
            401
        );
        for index in [0, 2, 5, 6] {
            headers.insert(
                "authorization".to_owned(),
                format!("Bearer test-credential-{index}-{}", "x".repeat(32)),
            );
            assert_eq!(
                api.handle("GET", ADAPTIVE_LOCAL_ADOPTION_PATH, &headers, &[])
                    .unwrap()
                    .status,
                403
            );
            assert_eq!(
                api.handle("POST", ADAPTIVE_LOCAL_ADOPTION_PATH, &headers, b"{}")
                    .unwrap()
                    .status,
                403
            );
        }
        headers.insert(
            "authorization".to_owned(),
            format!("Bearer test-credential-8-{}", "x".repeat(32)),
        );
        for query in [
            "",
            "?review_id=invalid",
            "?review_id=00000000-0000-0000-0000-000000000000",
        ] {
            assert_eq!(
                api.handle(
                    "GET",
                    &format!("{ADAPTIVE_LOCAL_ADOPTION_PATH}{query}"),
                    &headers,
                    &[]
                )
                .unwrap()
                .status,
                400
            );
        }
        let review = Uuid::new_v4();
        assert_eq!(
            api.handle(
                "GET",
                &format!("{ADAPTIVE_LOCAL_ADOPTION_PATH}?review_id={review}"),
                &headers,
                &[]
            )
            .unwrap()
            .status,
            409
        );
        assert!(api
            .store
            .adaptive_leadership_local_adoption(&TenantId::parse("tenant-m0").unwrap(), review)
            .unwrap()
            .is_none());
    }

    #[test]
    fn local_adoption_candidate_without_permanent_epoch_preserves_dispatched_response() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events_path = temp.path().join("events.sqlite");
        let (api, context) =
            super::super::adaptive_leadership_review::tests::fixture(&path, &events_path);
        let (id, digest) =
            super::super::adaptive_leadership_review::tests::reserve_and_claim(&api, &context);
        let completion =
            super::super::adaptive_leadership_review::tests::make_completion(&context, "wait");
        super::super::adaptive_leadership_review::tests::persist(
            &api,
            &completion,
            &context,
            &id,
            &digest,
            true,
        );
        let events = api.event_store.as_ref().unwrap();
        events
            .record_llm_completion_failure(&id, &digest, "continuation audit invalid", 1)
            .unwrap();
        let before = events.get_llm_completion(&id).unwrap().unwrap();
        let call_before = api
            .store
            .adaptive_leadership_review_call(
                &context.binding.grant.leadership_principal.tenant_id,
                context.binding.grant.review_id,
            )
            .unwrap()
            .unwrap();
        let operator = api.principals.principal("operator").unwrap();
        assert!(matches!(
            api.local_adoption_candidate(
                &operator,
                context.binding.grant.review_id,
                Uuid::new_v4(),
                None
            ),
            Err("local adoption epoch missing")
        ));
        let after = events.get_llm_completion(&id).unwrap().unwrap();
        assert_eq!(before.payload, after.payload);
        assert_eq!(before.status, after.status);
        assert_eq!(before.attempt_count, after.attempt_count);
        assert_eq!(before.last_error, after.last_error);
        assert_eq!(
            api.store
                .adaptive_leadership_review_call(
                    &context.binding.grant.leadership_principal.tenant_id,
                    context.binding.grant.review_id
                )
                .unwrap()
                .unwrap(),
            call_before
        );
        assert!(api
            .store
            .adaptive_leadership_local_adoption(
                &operator.principal.tenant_id,
                context.binding.grant.review_id
            )
            .unwrap()
            .is_none());
    }
}
