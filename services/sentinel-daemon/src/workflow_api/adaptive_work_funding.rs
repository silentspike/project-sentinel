//! Explicit operator issuance describes funding; only reviewed adoption permits work.
use super::*;
use sentinel_workflow::{
    AdaptiveWorkFundingLimitsV1, AdaptiveWorkFundingReceiptV1, AdaptiveWorkFundingRequestV1,
};

#[cfg(test)]
mod tests;

// Public drafts carry a digest instead of private source/authority. A typed
// request is also accepted, but its caller-supplied source is never trusted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FundingSubmission {
    schema_version: u16,
    operation_id: Uuid,
    project_id: ProjectId,
    session_id: Uuid,
    reason_ref: String,
    limits: AdaptiveWorkFundingLimitsV1,
    source_digest: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FundingInput {
    Request(Box<AdaptiveWorkFundingRequestV1>),
    Draft(FundingSubmission),
}

fn source_conflict() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::AuthorityConflict,
        false,
        "work funding source evidence unavailable or changed",
    )
}

fn submission(request: &AdaptiveWorkFundingRequestV1) -> Result<FundingSubmission, WorkflowError> {
    let source = &request.source.resume_source;
    Ok(FundingSubmission {
        schema_version: request.schema_version,
        operation_id: request.operation_id,
        project_id: source.project_id.clone(),
        session_id: source.session_id,
        reason_ref: request.reason_ref.clone(),
        limits: request.limits.clone(),
        source_digest: request.canonical_digest()?,
    })
}

fn receipt_http(receipt: &AdaptiveWorkFundingReceiptV1, replayed: bool) -> WorkflowHttpResponse {
    let digest = match receipt.receipt_digest() {
        Ok(value) => value,
        Err(error) => return workflow_error(error),
    };
    let source = &receipt.request.source.resume_source;
    json(
        200,
        &serde_json::json!({
            "schema_version": 1, "replayed": replayed, "funding_id": receipt.funding_id,
            "receipt_digest": digest, "operation_id": receipt.request.operation_id,
            "project_id": source.project_id, "session_id": source.session_id,
            "issued_at_unix_ms": receipt.issued_at_unix_ms, "limits": receipt.request.limits,
            "model_decision_recorded": false, "developer_window_created": false,
        }),
    )
}

impl WorkflowApi {
    pub(super) fn validate_work_funding_review(
        &self,
        call: &sentinel_workflow::AdaptiveLeadershipReviewCallV1,
    ) -> Result<(), &'static str> {
        let epoch = call
            .grant
            .work_funding
            .as_deref()
            .ok_or("work funding epoch missing")?;
        epoch.validate().map_err(|_| "work funding epoch invalid")?;
        // This typed read validates the store's funding-review membership leaf
        // and issuance event, not just the descriptive receipt's shape.
        let stored = self
            .store
            .adaptive_leadership_review_call(
                &call.grant.leadership_principal.tenant_id,
                call.grant.review_id,
            )
            .map_err(|_| "work funding review membership unavailable")?
            .ok_or("work funding review membership missing")?;
        if stored.grant != call.grant
            || stored.context != call.context
            || stored.operation_id != call.operation_id
            || stored.allowance_id != call.allowance_id
            || stored.grant_issued_at_unix_ms != call.grant_issued_at_unix_ms
        {
            return Err("work funding sealed review membership changed");
        }
        let receipt = self
            .store
            .adaptive_work_funding(
                &call.grant.leadership_principal.tenant_id,
                call.grant.session_id,
                epoch.receipt.request.operation_id,
            )
            .map_err(|_| "work funding receipt unavailable")?
            .ok_or("work funding receipt missing")?;
        let source = &receipt.request.source.resume_source;
        if receipt != epoch.receipt
            || call.grant.schema_version != 5
            || call.grant.resume_policy.is_some()
            || call.grant.recovery_epoch.is_some()
            || source.project_id != call.grant.project_id
            || source.work_item_id != call.grant.work_item_id
            || source.assignee_authority != call.grant.assignee_authority
            || call.grant_issued_at_unix_ms < receipt.issued_at_unix_ms
        {
            return Err("work funding review membership changed");
        }
        if call.decision.is_none() && call.retired_at_unix_ms.is_none() {
            let calls = self
                .store
                .adaptive_leadership_review_calls(
                    &call.grant.leadership_principal.tenant_id,
                    call.grant.session_id,
                )
                .map_err(|_| "work funding current reviews unavailable")?;
            let current: Vec<_> = calls
                .iter()
                .filter(|candidate| {
                    candidate.decision.is_none()
                        && candidate.retired_at_unix_ms.is_none()
                        && candidate.grant.expected_session_version
                            == call.grant.expected_session_version
                })
                .collect();
            if current.len() != 1 || current[0].grant.review_id != call.grant.review_id {
                return Err("work funding current review is ambiguous");
            }
        }
        call.context
            .validate(&call.grant)
            .map_err(|_| "work funding review source changed")
    }

    pub(super) fn validate_funded_continuation_before_audit(
        &self,
        call: &sentinel_workflow::AdaptiveLeadershipReviewCallV1,
        proposed: &sentinel_workflow::CompleteAdaptiveLeadershipReviewCallV1,
    ) -> Result<(), &'static str> {
        if call.grant.work_funding.is_none() {
            return Ok(());
        }
        self.validate_work_funding_review(call)?;
        proposed
            .decision
            .validate(&call.context.evidence_refs)
            .and_then(|_| proposed.decision.validate_subject(&call.grant))
            .map_err(|_| "work funding continuation decision changed")?;
        let authorization = proposed
            .continuation
            .as_ref()
            .ok_or("work funding continuation missing")?;
        authorization
            .validate()
            .map_err(|_| "work funding continuation invalid")?;
        let sentinel_workflow::AdaptiveLeadershipReviewDecisionKindV1::Continue {
            additional_model_calls,
            window_ms,
            ..
        } = &proposed.decision.decision
        else {
            return Err("work funding pre-audit decision is not Continue");
        };
        if *additional_model_calls != authorization.additional_model_calls
            || authorization
                .deadline_ms
                .checked_sub(authorization.issued_at_ms)
                != Some(*window_ms)
        {
            return Err("work funding pre-audit decision limits changed");
        }
        let allowance = call
            .continuation_allowance(
                authorization.issued_at_ms,
                authorization.deadline_ms,
                authorization.additional_model_calls,
            )
            .map_err(|_| "work funding continuation allowance invalid")?;
        if proposed.review_id != call.grant.review_id
            || proposed.allowance_id != call.allowance_id
            || call
                .dispatch
                .as_ref()
                .is_none_or(|dispatch| dispatch.request_digest != proposed.request_digest)
            || authorization.work_funding != call.grant.work_funding
            || authorization.resume_policy.is_some()
            || authorization.local_adoption.is_some()
            || authorization.session_id != call.grant.session_id
            || authorization.review_id != call.grant.review_id
            || authorization.operation_id != call.operation_id
            || authorization.source_session_version != call.grant.expected_session_version
            || proposed.resolution_event_id != Some(authorization.resolution_event_id)
            || authorization.resolution_event_id
                != sentinel_workflow::adaptive_leadership_continuation_audit_id(
                    call.grant.review_id,
                    &proposed.request_digest,
                    &proposed.model_response_digest,
                    &proposed.decision,
                )
                .map_err(|_| "work funding pre-audit identity invalid")?
            || authorization.provider_allowance_id != allowance.allowance_id
            || authorization.provider_authority_digest
                != sentinel_workflow::adaptive_leadership_continuation_provider_authority_digest(
                    &allowance,
                    &call.grant.assignee_authority,
                )
                .map_err(|_| "work funding continuation provider binding invalid")?
        {
            return Err("work funding pre-audit authorization changed");
        }
        Ok(())
    }

    fn validate_work_funding_operator(
        &self,
        operator: &BoundPrincipal,
    ) -> Result<(), WorkflowError> {
        if operator.principal.kind != CompanyPrincipalKindV1::Operator
            || !matches!(
                operator.principal.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
            || !self
                .principals
                .principal(&operator.principal.principal_id)
                .is_some_and(|bound| {
                    bound.principal == operator.principal
                        && bound.execution_authority == operator.execution_authority
                })
        {
            return Err(source_conflict());
        }
        Ok(())
    }

    fn work_funding_current_source(
        &self,
        operator: &BoundPrincipal,
        project_id: &ProjectId,
        session_id: Uuid,
        now: u64,
    ) -> Result<(), WorkflowError> {
        let (project, session) =
            self.adaptive_resume_policy_current_source(operator, project_id, session_id)?;
        if !matches!(session.cursor, AdaptiveCursorV1::ReadyForModel)
            || !session.model_window_exhausted_at(now)
        {
            return Err(source_conflict());
        }
        let runtime = self.authority.as_ref().ok_or_else(source_conflict)?;
        runtime
            .project_profiles
            .family(&project.governance.project_profile)
            .map_err(|_| source_conflict())?;
        let work = project
            .work_items
            .get(&session.grant.authority.work_item_id)
            .ok_or_else(source_conflict)?;
        let employee = self
            .principals
            .by_principal_id
            .values()
            .find(|bound| {
                bound.principal.tenant_id == project.tenant_id
                    && bound.principal.agent_id == Some(session.grant.authority.agent_id)
                    && bound.principal.kind == CompanyPrincipalKindV1::Agent
                    && bound.principal.role == work.spec.required_role
                    && bound.execution_authority == session.grant.authority.principal
            })
            .ok_or_else(source_conflict)?;
        self.validate_company_employee(&employee.principal)
            .map_err(|_| source_conflict())?;
        let leader_ready = project.governance.participants.iter().any(|participant| {
            self.principals
                .principal(&participant.principal_id)
                .is_some_and(|bound| {
                    bound.principal.tenant_id == project.tenant_id
                        && bound.principal.agent_id == Some(participant.agent_id)
                        && bound.principal.role == participant.role
                        && bound.principal.kind == CompanyPrincipalKindV1::Agent
                        && matches!(
                            participant.role,
                            CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
                        )
                        && self.validate_company_employee(&bound.principal).is_ok()
                })
        });
        if !leader_ready {
            return Err(source_conflict());
        }
        let (profile, digest) = runtime
            .profile_for_binding(&session.grant.authority.profile_id)
            .map_err(|_| source_conflict())?;
        if digest != session.grant.authority.profile_digest {
            return Err(source_conflict());
        }
        super::model_execution::tool_catalog::adaptive_tool_catalog(
            profile,
            &session.grant.authority,
            &work.spec,
        )
        .map_err(|_| source_conflict())?;
        if let Some(reference) = &session.last_observation {
            let observation = self
                .workbench
                .as_ref()
                .ok_or_else(source_conflict)?
                .private_observation(reference.effect.id)
                .map_err(|_| source_conflict())?;
            observation
                .validate(
                    &reference.effect.id.to_string(),
                    &reference.effect.request_digest,
                )
                .map_err(|_| source_conflict())?;
            if observation.digest() != reference.observation_digest {
                return Err(source_conflict());
            }
        }
        Ok(())
    }

    pub(super) fn adaptive_work_funding_http(
        &self,
        operator: &BoundPrincipal,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> WorkflowHttpResponse {
        if self.validate_work_funding_operator(operator).is_err() {
            return json_error(
                403,
                "authority_conflict",
                "registered operator funding authority required",
                false,
            );
        }
        if !self.enabled || !self.model_work_enabled {
            return workflow_error(workflow_unavailable());
        }
        let Ok(_fence) = self.mutation_fence.write() else {
            return workflow_error(workflow_unavailable());
        };
        let now = now_unix_ms();
        if method == "GET" {
            return self.work_funding_draft_http(operator, path, now);
        }
        if method != "POST" {
            return json_error(
                405,
                "invalid_input",
                "work funding method unsupported",
                false,
            );
        }
        let input: FundingInput = match decode_body(body) {
            Ok(value) => value,
            Err(response) => return response,
        };
        let (mut request, submitted) = match input {
            FundingInput::Request(request) => {
                if request.validate_shape().is_err()
                    || request.source.resume_source.tenant_id != operator.principal.tenant_id
                {
                    return workflow_error(source_conflict());
                }
                (*request, None)
            }
            FundingInput::Draft(value) => {
                if value.schema_version != 1 {
                    return workflow_error(source_conflict());
                }
                // An exact historical leaf can be replayed without a fresh head.
                let stored = match self.store.adaptive_work_funding(
                    &operator.principal.tenant_id,
                    value.session_id,
                    value.operation_id,
                ) {
                    Ok(value) => value,
                    Err(error) => return workflow_error(error),
                };
                let request = if let Some(receipt) = stored {
                    receipt.request
                } else {
                    if let Err(error) = self.work_funding_current_source(
                        operator,
                        &value.project_id,
                        value.session_id,
                        now,
                    ) {
                        return workflow_error(error);
                    }
                    match self.store.adaptive_work_funding_draft(
                        &operator.principal,
                        &value.project_id,
                        value.session_id,
                        value.operation_id,
                        &value.reason_ref,
                        value.limits.clone(),
                        now,
                    ) {
                        Ok(request) => request,
                        Err(error) => return workflow_error(error),
                    }
                };
                (request, Some(value))
            }
        };
        if let Some(value) = submitted {
            if submission(&request).ok().is_none_or(|fresh| {
                fresh.schema_version != value.schema_version
                    || fresh.operation_id != value.operation_id
                    || fresh.project_id != value.project_id
                    || fresh.session_id != value.session_id
                    || fresh.reason_ref != value.reason_ref
                    || fresh.limits != value.limits
                    || fresh.source_digest != value.source_digest
            }) {
                return workflow_error(source_conflict());
            }
        }
        let source = &request.source.resume_source;
        match self.store.adaptive_work_funding(
            &operator.principal.tenant_id,
            source.session_id,
            request.operation_id,
        ) {
            Ok(Some(_)) => {}
            Err(error) => return workflow_error(error),
            Ok(None) => {
                if let Err(error) = self.work_funding_current_source(
                    operator,
                    &source.project_id,
                    source.session_id,
                    now,
                ) {
                    return workflow_error(error);
                }
                // Exact comparison of every source field precedes store issuance;
                // the store repeats this check in its own transaction.
                let fresh = match self.store.adaptive_work_funding_draft(
                    &operator.principal,
                    &source.project_id,
                    source.session_id,
                    request.operation_id,
                    &request.reason_ref,
                    request.limits.clone(),
                    now,
                ) {
                    Ok(value) => value,
                    Err(error) => return workflow_error(error),
                };
                if fresh != request {
                    return workflow_error(source_conflict());
                }
                request = fresh;
            }
        }
        match self.store.authorize_adaptive_work_funding(
            &operator.principal,
            &request,
            now_unix_ms(),
        ) {
            Ok((replayed, receipt)) => receipt_http(&receipt, replayed),
            Err(error) => workflow_error(error),
        }
    }

    fn work_funding_draft_http(
        &self,
        operator: &BoundPrincipal,
        path: &str,
        now: u64,
    ) -> WorkflowHttpResponse {
        let parsed = (|| {
            let project = ProjectId::parse(query_parameter(path, "project_id")?).ok()?;
            let session = Uuid::parse_str(query_parameter(path, "session_id")?).ok()?;
            let operation = Uuid::parse_str(query_parameter(path, "operation_id")?).ok()?;
            if session.is_nil() || operation.is_nil() {
                return None;
            }
            Some((project, session, operation))
        })();
        let Some((project, session, operation)) = parsed else {
            return json_error(
                400,
                "invalid_input",
                "project_id, session_id and operation_id required",
                false,
            );
        };
        match self
            .store
            .adaptive_work_funding(&operator.principal.tenant_id, session, operation)
        {
            Ok(Some(receipt)) if receipt.request.source.resume_source.project_id == project => {
                return receipt_http(&receipt, true);
            }
            Ok(Some(_)) => return workflow_error(source_conflict()),
            Ok(None) => {}
            Err(error) => return workflow_error(error),
        }
        let limits = (|| {
            Some(AdaptiveWorkFundingLimitsV1 {
                additional_model_calls: query_parameter(path, "additional_model_calls")?
                    .parse()
                    .ok()?,
                additional_tool_calls: query_parameter(path, "additional_tool_calls")?
                    .parse()
                    .ok()?,
                additional_reviews: query_parameter(path, "additional_reviews")?.parse().ok()?,
                additional_windows: query_parameter(path, "additional_windows")?.parse().ok()?,
                max_window_ms: query_parameter(path, "max_window_ms")?.parse().ok()?,
                max_call_duration_ms: query_parameter(path, "max_call_duration_ms")?
                    .parse()
                    .ok()?,
                dispatch_margin_ms: query_parameter(path, "dispatch_margin_ms")?.parse().ok()?,
                expires_at_unix_ms: query_parameter(path, "expires_at_unix_ms")?.parse().ok()?,
            })
        })();
        let Some(limits) = limits else {
            return json_error(
                400,
                "invalid_input",
                "explicit funding limits required",
                false,
            );
        };
        let Some(reason) = query_parameter(path, "reason_ref") else {
            return json_error(400, "invalid_input", "reason_ref required", false);
        };
        if let Err(error) = self.work_funding_current_source(operator, &project, session, now) {
            return workflow_error(error);
        }
        match self
            .store
            .adaptive_work_funding_draft(
                &operator.principal,
                &project,
                session,
                operation,
                reason,
                limits,
                now,
            )
            .and_then(|request| submission(&request))
        {
            Ok(request) => json(
                200,
                &serde_json::json!({
                    "schema_version": 1, "requires_explicit_submission": true, "request": request,
                    "model_decision_recorded": false, "developer_window_created": false,
                }),
            ),
            Err(error) => workflow_error(error),
        }
    }
}
