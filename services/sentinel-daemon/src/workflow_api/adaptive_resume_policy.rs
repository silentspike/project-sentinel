//! Immutable operator policy issuance does not record a model decision.
use super::model_execution::{AdaptiveProviderAuthority, ProviderExecutionAuthority};
use super::*;
use sentinel_workflow::{
    AdaptiveResumePolicyReceiptV1, AdaptiveResumePolicyRequestV1, AdaptiveResumeSubjectV1,
};

#[cfg(test)]
pub(crate) mod tests;

fn source_conflict() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::AuthorityConflict,
        false,
        "resume policy source evidence unavailable or changed",
    )
}

impl WorkflowApi {
    pub(super) fn adaptive_resume_policy_http(
        &self,
        principal: &BoundPrincipal,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> WorkflowHttpResponse {
        if principal.principal.kind != CompanyPrincipalKindV1::Operator
            || !matches!(principal.principal.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead)
            || !self.principals.principal(&principal.principal.principal_id)
                .is_some_and(|registered| registered.principal == principal.principal
                    && registered.execution_authority == principal.execution_authority)
        {
            return json_error(403, "authority_conflict", "operator resume policy authority required", false);
        }
        if !self.enabled || !self.model_work_enabled {
            return workflow_error(workflow_unavailable());
        }
        let Ok(_fence) = self.mutation_fence.write() else {
            return workflow_error(workflow_unavailable());
        };
        if method == "GET" {
            return self.adaptive_resume_policy_draft_http(principal, path);
        }
        if method != "POST" {
            return json_error(405, "invalid_input", "resume policy method unsupported", false);
        }
        let request: AdaptiveResumePolicyRequestV1 = match decode_body(body) {
            Ok(value) => value,
            Err(response) => return response,
        };
        if request.source.tenant_id != principal.principal.tenant_id {
            return workflow_error(source_conflict());
        }
        if request.validate_shape().is_err() {
            return json_error(400, "invalid_input", "invalid resume policy request", false);
        }
        // Exact sealed replay is independent of subsequent source/proof expiry.
        match self.store.adaptive_resume_policy(&principal.principal.tenant_id, request.source.session_id) {
            Ok(Some(_)) => return match self.store.authorize_adaptive_resume_policy(
                &principal.principal, &request, now_unix_ms(),
            ) {
                Ok((replayed, receipt)) => policy_receipt(&receipt, replayed),
                Err(error) => workflow_error(error),
            },
            Ok(None) => {},
            Err(error) => return workflow_error(error),
        }
        if request.validate_at(&principal.principal, now_unix_ms()).is_err() {
            return json_error(400, "invalid_input", "invalid resume policy request or expiry", false);
        }
        let (project, session) = match self.adaptive_resume_policy_current_source(
            principal, &request.source.project_id, request.source.session_id,
        ) {
            Ok(value) => value,
            Err(error) => return workflow_error(error),
        };
        if project.version != request.source.expected_project_version
            || session.version != request.source.expected_session_version
            || session.grant.authority != request.source.assignee_authority
            || session.model_calls != request.source.base_model_calls
            || session.tool_calls != request.source.base_tool_calls
            || request.limits.max_call_duration_ms != session.grant.max_call_duration_ms
        {
            return workflow_error(source_conflict());
        }
        if !matches!(request.source.subject, AdaptiveResumeSubjectV1::ModelUnknown { .. }) {
            return match self.store.authorize_adaptive_resume_policy(
                &principal.principal, &request, now_unix_ms(),
            ) {
                Ok((replayed, receipt)) => policy_receipt(&receipt, replayed),
                Err(error) => workflow_error(error),
            };
        }
        let AdaptiveCursorV1::ModelUnknown { effect } = &session.cursor else {
            return workflow_error(source_conflict());
        };
        let verified = match self.read_only_unknown_model_proof_digest(&project, &session, effect) {
            Ok(Some(proof)) if session.active_deadline_ms() <= now_unix_ms() => proof,
            _ => return workflow_error(source_conflict()),
        };
        let captured = match self.store.adaptive_resume_policy_draft_with_unknown_proof(
            &principal.principal, &request.source.project_id, request.source.session_id,
            request.operation_id, &request.reason_ref, request.limits.expires_at_unix_ms,
            now_unix_ms(), &verified,
        ) {
            Ok(fresh) if fresh.source == request.source => fresh.source,
            Ok(_) => return workflow_error(source_conflict()),
            Err(error) => return workflow_error(error),
        };
        match self.store.authorize_adaptive_resume_policy_with_unknown_proof(
            &principal.principal, &request, now_unix_ms(), |_, source| {
                let (AdaptiveResumeSubjectV1::ModelUnknown { effect, .. },
                    AdaptiveCursorV1::ModelUnknown { effect: current }) = (&source.subject, &session.cursor)
                else { return Err(source_conflict()); };
                if source != &captured || effect != current {
                    return Err(source_conflict());
                }
                let current_proof = self.adaptive_resume_policy_source_proof(&project, &session)?
                    .ok_or_else(source_conflict)?;
                if current_proof != verified {
                    return Err(source_conflict());
                }
                if now_unix_ms() >= request.limits.expires_at_unix_ms {
                    return Err(source_conflict());
                }
                Ok(current_proof)
            },
        ) {
            Ok((replayed, receipt)) => policy_receipt(&receipt, replayed),
            Err(error) => workflow_error(error),
        }
    }

    fn adaptive_resume_policy_draft_http(
        &self,
        principal: &BoundPrincipal,
        path: &str,
    ) -> WorkflowHttpResponse {
        let Some(project_id) = query_parameter(path, "project_id").and_then(|value| ProjectId::parse(value).ok()) else {
            return json_error(400, "invalid_input", "project_id required", false);
        };
        let Some(session_id) = query_parameter(path, "session_id").and_then(|value| Uuid::parse_str(value).ok()).filter(|value| !value.is_nil()) else {
            return json_error(400, "invalid_input", "session_id required", false);
        };
        match self.store.adaptive_resume_policy(&principal.principal.tenant_id, session_id) {
            Ok(Some(receipt)) if receipt.request.source.project_id == project_id => {
                return policy_receipt(&receipt, true);
            }
            Ok(Some(_)) => return workflow_error(source_conflict()),
            Ok(None) => {},
            Err(error) => return workflow_error(error),
        }
        let (project, session) = match self.adaptive_resume_policy_current_source(principal, &project_id, session_id) {
            Ok(value) => value,
            Err(error) => return workflow_error(error),
        };
        let proof = if let AdaptiveCursorV1::ModelUnknown { effect } = &session.cursor {
            match self.read_only_unknown_model_proof_digest(&project, &session, effect) {
                Ok(Some(proof)) if session.active_deadline_ms() <= now_unix_ms() => Some(proof),
                _ => return workflow_error(source_conflict()),
            }
        } else { None };
        let now = now_unix_ms();
        let expires = match query_parameter(path, "expires_at_unix_ms") {
            Some(value) => match value.parse::<u64>() {
                Ok(value) => value,
                Err(_) => return json_error(400, "invalid_input", "invalid policy expiry", false),
            },
            None => match now.checked_add(sentinel_workflow::ADAPTIVE_RESUME_MAX_POLICY_MS) {
                Some(value) => value,
                None => return workflow_error(workflow_unavailable()),
            },
        };
        let operation = Uuid::new_v4();
        let reason = query_parameter(path, "reason_ref").unwrap_or("finite-session-resume-policy");
        let request = match proof {
            Some(proof) => self.store.adaptive_resume_policy_draft_with_unknown_proof(
                &principal.principal, &project_id, session_id, operation, reason, expires, now, &proof,
            ),
            None => self.store.adaptive_resume_policy_draft(
                &principal.principal, &project_id, session_id, operation, reason, expires, now,
            ),
        };
        match request {
            Ok(request) => json(200, &serde_json::json!({
                "schema_version": 1, "requires_explicit_submission": true, "request": request,
                "model_decision_recorded": false, "developer_window_created": false,
            })),
            Err(error) => workflow_error(error),
        }
    }

    fn adaptive_resume_policy_current_source(
        &self,
        principal: &BoundPrincipal,
        project_id: &ProjectId,
        session_id: Uuid,
    ) -> Result<(sentinel_workflow::ProjectV1, AdaptiveSessionV1), WorkflowError> {
        let project = self.store.company_project(&principal.principal.tenant_id, project_id)?
            .ok_or_else(source_conflict)?;
        let session = self.review_sessions(&project).map_err(|_| workflow_unavailable())?
            .into_iter().find(|session| session.grant.session_id == session_id)
            .ok_or_else(source_conflict)?;
        Ok((project, session))
    }

    pub(super) fn validate_resume_policy_review(
        &self,
        call: &sentinel_workflow::AdaptiveLeadershipReviewCallV1,
    ) -> Result<(), &'static str> {
        let policy = self.store.adaptive_resume_policy(
            &call.grant.leadership_principal.tenant_id, call.grant.session_id,
        ).map_err(|_| "resume policy unavailable")?;
        match (&policy, &call.grant.resume_policy) {
            (None, None) => Ok(()),
            (Some(_), None) if call.decision.is_some() || call.retired_at_unix_ms.is_some() => Ok(()),
            (Some(policy), Some(binding)) => {
                policy.validate_binding(binding).map_err(|_| "resume policy binding changed")?;
                if policy.request.source.assignee_authority != call.grant.assignee_authority
                    || policy.request.source.project_id != call.grant.project_id
                    || policy.request.source.work_item_id != call.grant.work_item_id
                    || call.grant_issued_at_unix_ms < policy.issued_at_unix_ms
                {
                    return Err("resume policy source changed");
                }
                Ok(())
            }
            _ => Err("leadership review superseded by resume policy"),
        }
    }

    // This verifier may run under the store's issuance transaction. It must
    // read only EventStore evidence, never recursively acquire WorkflowStore.
    pub(super) fn adaptive_resume_policy_source_proof(
        &self,
        project: &sentinel_workflow::ProjectV1,
        session: &AdaptiveSessionV1,
    ) -> Result<Option<String>, WorkflowError> {
        if project.tenant_id != session.grant.authority.tenant_id
            || project.project_id != session.grant.authority.project_id
        {
            return Err(source_conflict());
        }
        let AdaptiveCursorV1::ModelUnknown { effect } = &session.cursor else {
            return if session.model_window_exhausted_at(now_unix_ms()) {
                Ok(None)
            } else {
                Err(source_conflict())
            };
        };
        if now_unix_ms() < session.active_deadline_ms() {
            return Err(source_conflict());
        }
        let assignments: Vec<_> = project
            .work_items
            .get(&session.grant.authority.work_item_id)
            .ok_or_else(source_conflict)?
            .assignments
            .iter()
            .filter(|assignment| assignment.active)
            .collect();
        let assignment = assignments.first().ok_or_else(source_conflict)?;
        if assignments.len() != 1 || assignment.agent_id != session.grant.authority.agent_id {
            return Err(source_conflict());
        }
        let authority = ProviderExecutionAuthority::Adaptive(Box::new(AdaptiveProviderAuthority {
            schema_version: 3,
            grant: session.effective_grant(),
            session_version: session.version.checked_sub(2).ok_or_else(source_conflict)?,
            effect_id: effect.id,
            assignment_id: assignment.assignment_id.clone(),
            previous_observation: session.last_observation.clone(),
        }));
        let events = self.event_store.as_ref().ok_or_else(source_conflict)?;
        let request_id = authority.request_id();
        let bytes = if let Some(evidence) = crate::llm_bridge::bridge::sealed_unknown_model_evidence(
            events,
            &authority,
            &request_id,
            &effect.request_digest,
        )
        .map_err(|_| source_conflict())?
        {
            sentinel_common::canonical_json(&evidence).map_err(|_| source_conflict())?
        } else {
            let evidence = crate::llm_bridge::bridge::retrospective_unknown_model_evidence(
                events,
                &authority,
                &request_id,
                &effect.request_digest,
            )
            .map_err(|_| source_conflict())?
            .ok_or_else(source_conflict)?;
            sentinel_common::canonical_json(&evidence).map_err(|_| source_conflict())?
        };
        Ok(Some(sentinel_common::sha256_hex(&bytes)))
    }
}

fn policy_receipt(receipt: &AdaptiveResumePolicyReceiptV1, replayed: bool) -> WorkflowHttpResponse {
    let digest = match receipt.receipt_digest() {
        Ok(value) => value,
        Err(error) => return workflow_error(error),
    };
    json(200, &serde_json::json!({
        "schema_version": 1, "replayed": replayed, "policy_id": receipt.policy_id,
        "receipt_digest": digest, "issued_at_unix_ms": receipt.issued_at_unix_ms,
        "request": receipt.request, "model_decision_recorded": false,
        "developer_window_created": false,
    }))
}
