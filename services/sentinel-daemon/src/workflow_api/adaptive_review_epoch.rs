//! Operator intervention authorizes one model review, never an employee decision.
#[cfg(test)]
#[path = "tests/admission_repair.rs"]
pub(crate) mod admission_repair_tests;

use super::*;
use sentinel_workflow::{
    adaptive_leadership_admission_repair_history_digest, adaptive_leadership_admission_repair_key,
    adaptive_leadership_evidence_fingerprint, adaptive_leadership_recovery_history_digest,
    adaptive_leadership_recovery_project_digest, adaptive_leadership_recovery_session_digest,
    adaptive_leadership_review_id, AdaptiveLeadershipAdmissionRepairEpochV1,
    AdaptiveLeadershipAdmissionRepairV1, AdaptiveLeadershipRecoveryEpochV1,
    AdaptiveLeadershipRecoveryRequestV1, AdaptiveLeadershipReviewContextV1,
    AdaptiveLeadershipReviewGrantV1, AdaptiveLeadershipReviewSubjectV2,
};

fn receipt(epoch: &AdaptiveLeadershipRecoveryEpochV1, replayed: bool) -> WorkflowHttpResponse {
    json(
        200,
        &serde_json::json!({
            "schema_version":1, "epoch_key":epoch.epoch_key,
            "operation_id":epoch.request.operation_id, "review_id":epoch.review_id,
            "project_id":epoch.request.project_id, "work_item_id":epoch.request.work_item_id,
            "session_id":epoch.request.session_id, "expires_at_unix_ms":epoch.expires_at_unix_ms,
            "replayed":replayed, "receipt_kind":"immutable_issuance",
            "decision_state":"not_asserted_by_issuance_receipt",
            "authority":"one_leadership_review"
        }),
    )
}

fn admission_repair_receipt(
    repair: &AdaptiveLeadershipAdmissionRepairEpochV1,
    replayed: bool,
) -> WorkflowHttpResponse {
    let epoch = &repair.epoch;
    json(
        200,
        &serde_json::json!({
            "schema_version":2, "epoch_key":epoch.epoch_key,
            "operation_id":epoch.request.operation_id, "review_id":epoch.review_id,
            "project_id":epoch.request.project_id, "work_item_id":epoch.request.work_item_id,
            "session_id":epoch.request.session_id, "expires_at_unix_ms":epoch.expires_at_unix_ms,
            "replayed":replayed, "receipt_kind":"immutable_issuance",
            "decision_state":"not_asserted_by_issuance_receipt",
            "authority":"one_exceptional_admission_repair_review", "review_cap_exception":true,
            "issuance_snapshot":admission_repair_accounting(&repair.source),
            "exceptional_review_count":1, "counters_and_windows_unchanged_at_issuance":true,
        }),
    )
}

fn admission_repair_accounting(
    source: &sentinel_workflow::AdaptiveLeadershipAdmissionRepairSourceV1,
) -> serde_json::Value {
    let session = &source.session;
    serde_json::json!({
        "session_version":session.version, "root_max_model_calls":session.grant.max_model_calls,
        "root_max_tool_calls":session.grant.max_tool_calls,
        "spent_model_calls":session.model_calls, "spent_tool_calls":session.tool_calls,
        "active_model_ceiling":session.active_model_ceiling(),
        "root_deadline_ms":session.grant.deadline_ms, "active_deadline_ms":session.active_deadline_ms(),
        "continuation_window_count":session.continuation.as_ref().map_or(0, |state| state.authorizations.len()),
        "ordinary_review_count":source.calls.iter().filter(|call| call.grant.schema_version == 3
            && call.grant.recovery_epoch.is_none()).count(),
    })
}

impl WorkflowApi {
    pub(super) fn review_recovery_epoch(
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
                "operator recovery authority required",
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
            match query_parameter(path, "mode") {
                Some("admission_repair_source") => {
                    return self.admission_repair_source_http(principal, path);
                }
                Some("admission_repair" | "admission-repair") => {
                    return self.review_admission_repair_epoch(principal, method, path, body);
                }
                Some(_) => return json_error(400, "invalid_input", "unknown recovery mode", false),
                None => {}
            }
        } else if serde_json::from_slice::<AdaptiveLeadershipRecoveryRequestV1>(body)
            .is_ok_and(|request| request.admission_repair.is_some())
        {
            return self.review_admission_repair_epoch(principal, method, path, body);
        }
        if method == "GET" {
            let project = match query_parameter(path, "project_id")
                .and_then(|id| ProjectId::parse(id).ok())
            {
                Some(id) => id,
                None => return json_error(400, "invalid_input", "project_id required", false),
            };
            let session =
                match query_parameter(path, "session_id").and_then(|id| Uuid::parse_str(id).ok()) {
                    Some(id) if !id.is_nil() => id,
                    _ => return json_error(400, "invalid_input", "session_id required", false),
                };
            match self
                .store
                .adaptive_leadership_recovery_epoch(&principal.principal.tenant_id, session)
            {
                Ok(Some(epoch)) if epoch.request.project_id == project => {
                    return receipt(&epoch, true)
                }
                Ok(Some(_)) => {
                    return json_error(
                        403,
                        "authority_conflict",
                        "recovery project mismatch",
                        false,
                    )
                }
                Ok(None) => {}
                Err(error) => return workflow_error(error),
            }
            let now = now_unix_ms();
            let expires = match now.checked_add(300_000) {
                Some(value) => value,
                None => return workflow_error(workflow_unavailable()),
            };
            match self.recovery_review_candidate(
                principal,
                &project,
                session,
                Uuid::new_v4(),
                "gateway-schema-repair".to_owned(),
                expires,
                None,
            ) {
                Ok((request, _, _)) => json(
                    200,
                    &serde_json::json!({
                        "schema_version":1, "requires_explicit_submission":true, "request":request,
                        "model_decision_recorded":false
                    }),
                ),
                Err(reason) => json_error(409, "adaptive_recovery_conflict", reason, false),
            }
        } else {
            let request: AdaptiveLeadershipRecoveryRequestV1 = match decode_body(body) {
                Ok(value) => value,
                Err(response) => return response,
            };
            if request.tenant_id != principal.principal.tenant_id {
                return json_error(403, "authority_conflict", "recovery tenant mismatch", false);
            }
            // Historical replay does not require a new clock, release or serving head.
            match self
                .store
                .adaptive_leadership_recovery_epoch(&request.tenant_id, request.session_id)
            {
                Ok(Some(epoch)) => {
                    return match self.store.authorize_adaptive_leadership_recovery_epoch(
                        &principal.principal,
                        &request,
                        &epoch.review_grant,
                        &epoch.source_context,
                        now_unix_ms(),
                    ) {
                        Ok((replayed, value)) => receipt(&value, replayed),
                        Err(error) => workflow_error(error),
                    }
                }
                Ok(None) => {}
                Err(error) => return workflow_error(error),
            }
            if let Err(error) = request.validate(&principal.principal, now_unix_ms()) {
                return workflow_error(error);
            }
            let (expected, grant, context) = match self.recovery_review_candidate(
                principal,
                &request.project_id,
                request.session_id,
                request.operation_id,
                request.reason_ref.clone(),
                request.expires_at_unix_ms,
                Some(request.max_window_ms),
            ) {
                Ok(value) => value,
                Err(_) => {
                    return json_error(
                        409,
                        "adaptive_recovery_conflict",
                        "recovery binding changed",
                        false,
                    )
                }
            };
            if expected != request {
                return json_error(
                    409,
                    "adaptive_recovery_conflict",
                    "recovery binding changed",
                    false,
                );
            }
            if adaptive_recovery_release::verified_current_repair().ok()
                != Some((request.release.clone(), request.repair_digest.clone()))
            {
                return json_error(
                    409,
                    "adaptive_recovery_conflict",
                    "installed repair changed",
                    false,
                );
            }
            match self.store.authorize_adaptive_leadership_recovery_epoch(
                &principal.principal,
                &request,
                &grant,
                &context,
                now_unix_ms(),
            ) {
                Ok((replayed, epoch)) => receipt(&epoch, replayed),
                Err(error) => workflow_error(error),
            }
        }
    }

    // Bootstrap facts are store-validated and deliberately contain no private payload.
    // No installed repair proof is needed and no authority is issued by this read.
    fn admission_repair_source_http(
        &self,
        operator: &BoundPrincipal,
        path: &str,
    ) -> WorkflowHttpResponse {
        let project =
            match query_parameter(path, "project_id").and_then(|id| ProjectId::parse(id).ok()) {
                Some(value) => value,
                None => return json_error(400, "invalid_input", "project_id required", false),
            };
        let session =
            match query_parameter(path, "session_id").and_then(|id| Uuid::parse_str(id).ok()) {
                Some(value) if !value.is_nil() => value,
                _ => return json_error(400, "invalid_input", "session_id required", false),
            };
        let source = match self
            .store
            .adaptive_leadership_admission_repair_source(&operator.principal.tenant_id, session)
        {
            Ok(value) => value,
            Err(error) => return workflow_error(error),
        };
        if source.project.project_id != project {
            return json_error(
                403,
                "authority_conflict",
                "recovery project mismatch",
                false,
            );
        }
        let result = (|| -> Result<serde_json::Value, &'static str> {
            let candidates = source
                .qualifying_review_ids
                .iter()
                .map(|id| {
                    let call = source
                        .calls
                        .iter()
                        .find(|call| call.grant.review_id == *id)
                        .ok_or("admission candidate missing")?;
                    adaptive_recovery_release::retired_admission_evidence(call)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let history = source.calls.iter().map(|call| serde_json::json!({
                "review_id":call.grant.review_id, "grant_schema_version":call.grant.schema_version,
                "session_version":call.grant.expected_session_version,
                "classification":if source.qualifying_review_ids.contains(&call.grant.review_id) {
                    "current_head_undispatched_retired_candidate"
                } else if call.continuation.is_some() { "verified_continuation" }
                else if call.decision.is_some() { "retained_decision" }
                else if call.dispatch.is_some() { "retired_dispatched" }
                else { "historical_retired" },
                "no_io":"not_asserted", "local_adoption":call.continuation.as_ref()
                    .is_some_and(|authorization| authorization.local_adoption.is_some()),
            })).collect::<Vec<_>>();
            Ok(serde_json::json!({
                "schema_version":1, "purpose":"admission-repair-source",
                "tenant_id":source.project.tenant_id, "project_id":project,
                "work_item_id":source.session.grant.authority.work_item_id, "session_id":session,
                "project_version":source.project.version, "session_version":source.session.version,
                "session_head_digest":source.session_head_digest,
                "source_digest":source.canonical_digest().map_err(|_| "admission source digest invalid")?,
                "inventory_digest":adaptive_leadership_admission_repair_history_digest(&source.calls)
                    .map_err(|_| "admission inventory digest invalid")?,
                "qualifying_review_ids":source.qualifying_review_ids, "candidates":candidates,
                "history":history, "accounting":admission_repair_accounting(&source),
                "authority_issued":false, "model_decision_recorded":false,
            }))
        })();
        match result {
            Ok(value) => json(200, &value),
            Err(reason) => json_error(409, "adaptive_recovery_conflict", reason, false),
        }
    }

    // The outer route owns mutation_fence. The explicit typed request selects
    // a separate permanent slot; an occupied legacy slot is never overwritten.
    fn review_admission_repair_epoch(
        &self,
        operator: &BoundPrincipal,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> WorkflowHttpResponse {
        let request = if method == "GET" {
            None
        } else {
            let request: AdaptiveLeadershipRecoveryRequestV1 = match decode_body(body) {
                Ok(value) => value,
                Err(response) => return response,
            };
            if request.tenant_id != operator.principal.tenant_id {
                return json_error(403, "authority_conflict", "recovery tenant mismatch", false);
            }
            if request.schema_version != 3 || request.admission_repair.is_none() {
                return json_error(
                    400,
                    "invalid_input",
                    "typed admission repair required",
                    false,
                );
            }
            Some(request)
        };
        let (project, session) = if let Some(request) = &request {
            (request.project_id.clone(), request.session_id)
        } else {
            let project = match query_parameter(path, "project_id")
                .and_then(|id| ProjectId::parse(id).ok())
            {
                Some(value) => value,
                None => return json_error(400, "invalid_input", "project_id required", false),
            };
            let session =
                match query_parameter(path, "session_id").and_then(|id| Uuid::parse_str(id).ok()) {
                    Some(value) if !value.is_nil() => value,
                    _ => return json_error(400, "invalid_input", "session_id required", false),
                };
            (project, session)
        };
        let prior = match self
            .store
            .adaptive_leadership_admission_repair_epoch(&operator.principal.tenant_id, session)
        {
            Ok(value) => value,
            Err(error) => return workflow_error(error),
        };
        if let Some(prior) = prior {
            if prior.epoch.request.project_id != project {
                return json_error(
                    403,
                    "authority_conflict",
                    "recovery project mismatch",
                    false,
                );
            }
            let Some(request) = request else {
                return admission_repair_receipt(&prior, true);
            };
            return match self.store.authorize_adaptive_leadership_admission_repair(
                &operator.principal,
                &request,
                &prior.epoch.review_grant,
                &prior.epoch.source_context,
                now_unix_ms(),
                |_, _| Err(workflow_unavailable()),
            ) {
                Ok((replayed, value)) => admission_repair_receipt(&value, replayed),
                Err(error) => workflow_error(error),
            };
        }
        let now = now_unix_ms();
        let (operation, reason, expires, window) = if let Some(request) = &request {
            if let Err(error) = request.validate(&operator.principal, now) {
                return workflow_error(error);
            }
            (
                request.operation_id,
                request.reason_ref.clone(),
                request.expires_at_unix_ms,
                Some(request.max_window_ms),
            )
        } else {
            let Some(expires) = now.checked_add(300_000) else {
                return workflow_error(workflow_unavailable());
            };
            (
                Uuid::new_v4(),
                "gateway-admission-repair".into(),
                expires,
                None,
            )
        };
        let (expected, grant, context) = match self.admission_repair_review_candidate(
            operator, &project, session, operation, reason, expires, window,
        ) {
            Ok(value) => value,
            Err(reason) => return json_error(409, "adaptive_recovery_conflict", reason, false),
        };
        let Some(request) = request else {
            return json(
                200,
                &serde_json::json!({
                    "schema_version":1, "requires_explicit_submission":true, "request":expected,
                    "model_decision_recorded":false,
                }),
            );
        };
        if expected != request {
            return json_error(
                409,
                "adaptive_recovery_conflict",
                "admission repair binding changed",
                false,
            );
        }
        match self.store.authorize_adaptive_leadership_admission_repair(
            &operator.principal,
            &request,
            &grant,
            &context,
            now_unix_ms(),
            |source, request| {
                let evidence = adaptive_recovery_release::verified_current_admission_repair(source)
                    .map_err(|_| workflow_unavailable())?;
                let repair = request
                    .admission_repair
                    .as_ref()
                    .ok_or_else(workflow_unavailable)?;
                if evidence.source_digest != repair.source_digest
                    || evidence.disposition_digest != repair.disposition_digest
                    || evidence.failed_release != repair.failed_release
                    || evidence.release != request.release
                    || evidence.repair_digest != request.repair_digest
                {
                    return Err(workflow_unavailable());
                }
                Ok(evidence)
            },
        ) {
            Ok((replayed, value)) => admission_repair_receipt(&value, replayed),
            Err(error) => workflow_error(error),
        }
    }

    fn admission_repair_review_candidate(
        &self,
        operator: &BoundPrincipal,
        project_id: &ProjectId,
        session_id: Uuid,
        operation_id: Uuid,
        reason_ref: String,
        expires_at_unix_ms: u64,
        requested_window_ms: Option<u64>,
    ) -> Result<
        (
            AdaptiveLeadershipRecoveryRequestV1,
            AdaptiveLeadershipReviewGrantV1,
            AdaptiveLeadershipReviewContextV1,
        ),
        &'static str,
    > {
        let source = self
            .store
            .adaptive_leadership_admission_repair_source(&operator.principal.tenant_id, session_id)
            .map_err(|_| "admission repair complete source unavailable")?;
        let session = &source.session;
        let project = &source.project;
        let now = now_unix_ms();
        if project.project_id != *project_id || !session.model_window_exhausted_at(now) {
            return Err("admission repair current exhausted head unavailable");
        }
        let (evidence, proven_reviews) =
            adaptive_recovery_release::verified_current_admission_repair_with_reviews(&source)?;
        let prior = source
            .calls
            .iter()
            .filter(|call| proven_reviews.contains(&call.grant.review_id))
            .max_by_key(|call| (call.grant_issued_at_unix_ms, call.grant.review_id))
            .ok_or("admission repair retired admission missing")?;
        let leader = self
            .principals
            .principal(&prior.grant.leadership_principal.principal_id)
            .ok_or("admission repair leader missing")?;
        if leader.principal != prior.grant.leadership_principal
            || leader.execution_authority != prior.grant.leadership_authority
        {
            return Err("admission repair leader changed");
        }
        self.validate_company_employee(&leader.principal)?;
        let authority = self
            .authority
            .as_ref()
            .ok_or("admission repair runtime missing")?;
        let current = authority
            .snapshot_for_admission(
                &project.tenant_id,
                project_id,
                &session.grant.authority.work_item_id,
                session.grant.authority.agent_id,
                false,
            )
            .map_err(|_| "admission repair assignee unavailable")?;
        if current != session.grant.authority {
            return Err("admission repair assignee changed");
        }
        let work = project
            .work_items
            .get(&session.grant.authority.work_item_id)
            .ok_or("admission repair work missing")?;
        let (profile, digest) = authority
            .profile_for_binding(&session.grant.authority.profile_id)
            .map_err(|_| "admission repair profile missing")?;
        if digest != session.grant.authority.profile_digest
            || super::model_execution::tool_catalog::adaptive_tool_catalog(
                profile, &current, &work.spec,
            )? != prior.context.tool_catalog
        {
            return Err("admission repair catalogue changed");
        }
        let root_window = session
            .grant
            .deadline_ms
            .checked_sub(session.grant.created_at_ms)
            .ok_or("admission repair root clock invalid")?;
        let max_window_ms = requested_window_ms.unwrap_or(root_window.min(120_000));
        if max_window_ms > root_window {
            return Err("admission repair window exceeds root");
        }
        let remaining = session
            .grant
            .max_model_calls
            .checked_sub(session.model_calls)
            .filter(|calls| *calls > 0)
            .ok_or("admission repair root model calls exhausted")?;
        let request = AdaptiveLeadershipRecoveryRequestV1 {
            schema_version: 3,
            operation_id,
            tenant_id: project.tenant_id.clone(),
            project_id: project_id.clone(),
            work_item_id: session.grant.authority.work_item_id.clone(),
            session_id,
            expected_project_version: project.version,
            expected_session_version: session.version,
            session_head_digest: source.session_head_digest.clone(),
            session_digest: adaptive_leadership_recovery_session_digest(session)
                .map_err(|_| "admission repair session digest invalid")?,
            project_digest: adaptive_leadership_recovery_project_digest(project)
                .map_err(|_| "admission repair project digest invalid")?,
            unknown_effect: None,
            sealed_unknown_proof_digest: None,
            blocked_subject: None,
            admission_repair: Some(AdaptiveLeadershipAdmissionRepairV1 {
                schema_version: 1,
                source_digest: evidence.source_digest.clone(),
                disposition_digest: evidence.disposition_digest.clone(),
                failed_release: evidence.failed_release.clone(),
            }),
            prior_review_history_digest: adaptive_leadership_admission_repair_history_digest(
                &source.calls,
            )
            .map_err(|_| "admission repair inventory digest invalid")?,
            repair_digest: evidence.repair_digest.clone(),
            release: evidence.release.clone(),
            reason_ref,
            expires_at_unix_ms,
            max_additional_model_calls: remaining,
            max_window_ms,
        };
        request
            .validate(&operator.principal, now)
            .map_err(|_| "admission repair request invalid")?;
        let mut context = prior.context.clone();
        context.evidence_refs.push(format!(
            "recovery-request:{}",
            request
                .canonical_digest()
                .map_err(|_| "admission repair request digest invalid")?
        ));
        context.evidence_refs.sort();
        let fingerprint =
            adaptive_leadership_evidence_fingerprint(&context.tool_catalog, &context.evidence_refs)
                .map_err(|_| "admission repair fingerprint invalid")?;
        let mut grant = prior.grant.clone();
        grant.schema_version = 4;
        grant.recovery_epoch = None;
        grant.review_id = adaptive_leadership_review_id(session_id, session.version, &fingerprint)
            .map_err(|_| "admission repair review identity invalid")?;
        grant.evidence_fingerprint = fingerprint;
        grant.expires_at_unix_ms = expires_at_unix_ms;
        let proposed = AdaptiveLeadershipAdmissionRepairEpochV1 {
            epoch: Box::new(AdaptiveLeadershipRecoveryEpochV1 {
                schema_version: 2,
                epoch_key: adaptive_leadership_admission_repair_key(&request.tenant_id, session_id)
                    .map_err(|_| "admission repair epoch identity invalid")?,
                request: request.clone(),
                issuer_principal: operator.principal.clone(),
                issuer_authority: operator.execution_authority.clone(),
                review_grant: grant.clone(),
                source_context: context.clone(),
                review_id: grant.review_id,
                issued_at_unix_ms: now,
                expires_at_unix_ms,
            }),
            source: Box::new(source),
            evidence,
        };
        proposed
            .validate()
            .map_err(|_| "admission repair source limits invalid")?;
        Ok((request, grant, context))
    }

    fn recovery_review_candidate(
        &self,
        operator: &BoundPrincipal,
        project_id: &ProjectId,
        session_id: Uuid,
        operation_id: Uuid,
        reason_ref: String,
        expires_at_unix_ms: u64,
        requested_window_ms: Option<u64>,
    ) -> Result<
        (
            AdaptiveLeadershipRecoveryRequestV1,
            AdaptiveLeadershipReviewGrantV1,
            AdaptiveLeadershipReviewContextV1,
        ),
        &'static str,
    > {
        let project = self
            .store
            .company_project(&operator.principal.tenant_id, project_id)
            .map_err(|_| "recovery project unavailable")?
            .ok_or("recovery project missing")?;
        let session = self
            .review_sessions(&project)?
            .into_iter()
            .find(|value| value.grant.session_id == session_id)
            .ok_or("recovery session missing")?;
        let (unknown_effect, proof, blocked_subject, subject) = match &session.cursor {
            AdaptiveCursorV1::ModelUnknown { effect } => {
                let proof = self
                    .read_only_unknown_model_proof_digest(&project, &session, effect)?
                    .ok_or("recovery unknown proof missing")?;
                (
                    Some(effect.clone()),
                    Some(proof.clone()),
                    None,
                    AdaptiveLeadershipReviewSubjectV2::UnknownModel {
                        effect: effect.clone(),
                        sealed_unknown_proof_digest: proof,
                    },
                )
            }
            AdaptiveCursorV1::Blocked { reason_code } => {
                let model_response_digest = session
                    .last_model_result_digest
                    .clone()
                    .ok_or("recovery blocked model receipt missing")?;
                (
                    None,
                    None,
                    Some(
                        sentinel_workflow::AdaptiveLeadershipRecoveryBlockedSubjectV1 {
                            reason_code: reason_code.clone(),
                            model_response_digest,
                        },
                    ),
                    AdaptiveLeadershipReviewSubjectV2::BlockedContinuation {
                        reason_code: reason_code.clone(),
                        resolution_event_id: None,
                    },
                )
            }
            _ => return Err("recovery source is not eligible model or blocked state"),
        };
        let calls = self
            .store
            .adaptive_leadership_review_calls(&project.tenant_id, session_id)
            .map_err(|_| "recovery reviews unavailable")?;
        let history = adaptive_leadership_recovery_history_digest(&calls)
            .map_err(|_| "recovery review history inadmissible")?;
        let prior = calls
            .iter()
            .max_by_key(|call| call.updated_at_unix_ms)
            .ok_or("recovery prior review missing")?;
        if prior.context.source_project != project || prior.context.source_session != session {
            return Err("recovery prior source changed");
        }
        prior
            .validate_continuation_source()
            .map_err(|_| "recovery continuation allowance source invalid")?;
        let leader = self
            .principals
            .principal(&prior.grant.leadership_principal.principal_id)
            .ok_or("recovery leader missing")?;
        if leader.principal != prior.grant.leadership_principal
            || leader.execution_authority != prior.grant.leadership_authority
        {
            return Err("recovery leader changed");
        }
        self.validate_company_employee(&leader.principal)?;
        let current = self
            .authority
            .as_ref()
            .ok_or("recovery runtime missing")?
            .snapshot_for_admission(
                &project.tenant_id,
                project_id,
                &session.grant.authority.work_item_id,
                session.grant.authority.agent_id,
                false,
            )
            .map_err(|_| "recovery assignee unavailable")?;
        if current != session.grant.authority {
            return Err("recovery assignee changed");
        }
        let work = project
            .work_items
            .get(&session.grant.authority.work_item_id)
            .ok_or("recovery work missing")?;
        let (profile, digest) = self
            .authority
            .as_ref()
            .ok_or("recovery runtime missing")?
            .profile_for_binding(&session.grant.authority.profile_id)
            .map_err(|_| "recovery profile missing")?;
        if digest != session.grant.authority.profile_digest {
            return Err("recovery profile changed");
        }
        let catalog = super::model_execution::tool_catalog::adaptive_tool_catalog(
            profile,
            &session.grant.authority,
            &work.spec,
        )?;
        if catalog != prior.context.tool_catalog {
            return Err("recovery catalog changed");
        }
        let root_window = session
            .grant
            .deadline_ms
            .checked_sub(session.grant.created_at_ms)
            .ok_or("recovery root clock invalid")?;
        let max_window_ms = requested_window_ms.unwrap_or(root_window.min(120_000));
        if max_window_ms > root_window {
            return Err("recovery window exceeds root authority");
        }
        let remaining_model_calls = session
            .grant
            .max_model_calls
            .checked_sub(session.model_calls)
            .filter(|calls| *calls > 0)
            .ok_or("recovery root model budget exhausted")?;
        let head_digest = self
            .store
            .adaptive_session_head_digest(session_id, &current)
            .map_err(|_| "recovery head unavailable")?
            .ok_or("recovery head missing")?;
        let (release, repair_digest) = adaptive_recovery_release::verified_current_repair()?;
        let request = AdaptiveLeadershipRecoveryRequestV1 {
            schema_version: if blocked_subject.is_some() { 2 } else { 1 },
            operation_id,
            tenant_id: project.tenant_id.clone(),
            project_id: project_id.clone(),
            work_item_id: session.grant.authority.work_item_id.clone(),
            session_id,
            expected_project_version: project.version,
            expected_session_version: session.version,
            session_head_digest: head_digest,
            session_digest: adaptive_leadership_recovery_session_digest(&session)
                .map_err(|_| "recovery session digest invalid")?,
            project_digest: adaptive_leadership_recovery_project_digest(&project)
                .map_err(|_| "recovery project digest invalid")?,
            unknown_effect,
            sealed_unknown_proof_digest: proof,
            blocked_subject,
            admission_repair: None,
            prior_review_history_digest: history,
            repair_digest,
            release,
            reason_ref,
            expires_at_unix_ms,
            max_additional_model_calls: remaining_model_calls,
            max_window_ms,
        };
        request
            .validate(&operator.principal, now_unix_ms())
            .map_err(|_| "recovery request invalid")?;
        let mut context = prior.context.clone();
        context.evidence_refs.push(format!(
            "recovery-request:{}",
            request
                .canonical_digest()
                .map_err(|_| "recovery request digest invalid")?
        ));
        context.evidence_refs.sort();
        let fingerprint =
            adaptive_leadership_evidence_fingerprint(&context.tool_catalog, &context.evidence_refs)
                .map_err(|_| "recovery fingerprint invalid")?;
        let mut grant = prior.grant.clone();
        grant.recovery_epoch = None;
        grant.review_id = adaptive_leadership_review_id(session_id, session.version, &fingerprint)
            .map_err(|_| "recovery review identity invalid")?;
        grant.evidence_fingerprint = fingerprint;
        grant.subject = Some(subject);
        grant.expires_at_unix_ms = expires_at_unix_ms;
        let proposed = AdaptiveLeadershipRecoveryEpochV1 {
            schema_version: 1,
            epoch_key: sentinel_workflow::adaptive_leadership_recovery_epoch_key(
                &request.tenant_id,
                request.session_id,
            )
            .map_err(|_| "recovery epoch identity invalid")?,
            request: request.clone(),
            issuer_principal: operator.principal.clone(),
            issuer_authority: operator.execution_authority.clone(),
            review_grant: grant.clone(),
            source_context: context.clone(),
            review_id: grant.review_id,
            issued_at_unix_ms: now_unix_ms(),
            expires_at_unix_ms,
        };
        proposed
            .validate()
            .map_err(|_| "recovery source limits inadmissible")?;
        proposed
            .validate_history(&calls)
            .map_err(|_| "recovery retired history inadmissible")?;
        Ok((request, grant, context))
    }

    pub(super) fn verify_recovery_review_release(
        &self,
        call: &sentinel_workflow::AdaptiveLeadershipReviewCallV1,
    ) -> Result<(), &'static str> {
        let Some(binding) = &call.grant.recovery_epoch else {
            return Ok(());
        };
        if binding.schema_version == 2 {
            let repair = self
                .store
                .adaptive_leadership_admission_repair_epoch(
                    &call.grant.leadership_principal.tenant_id,
                    call.grant.session_id,
                )
                .map_err(|_| "admission repair epoch unavailable")?
                .ok_or("admission repair epoch missing")?;
            repair
                .epoch
                .validate_review(&call.grant, &call.context, call.grant_issued_at_unix_ms)
                .map_err(|_| "admission repair review membership invalid")?;
            let issuer = self
                .principals
                .principal(&repair.epoch.issuer_principal.principal_id)
                .ok_or("admission repair issuer missing")?;
            if issuer.principal != repair.epoch.issuer_principal
                || issuer.execution_authority != repair.epoch.issuer_authority
            {
                return Err("admission repair issuer authority changed");
            }
            if adaptive_recovery_release::verified_current_admission_repair(&repair.source)?
                != repair.evidence
            {
                return Err("admission repair installed evidence changed");
            }
            return Ok(());
        }
        let epoch = self
            .store
            .adaptive_leadership_recovery_epoch(
                &call.grant.leadership_principal.tenant_id,
                call.grant.session_id,
            )
            .map_err(|_| "recovery epoch unavailable")?
            .ok_or("recovery epoch missing")?;
        if epoch.binding().map_err(|_| "recovery epoch invalid")? != *binding {
            return Err("recovery epoch binding changed");
        }
        epoch
            .validate_review(&call.grant, &call.context, call.grant_issued_at_unix_ms)
            .map_err(|_| "recovery review membership invalid")?;
        let issuer = self
            .principals
            .principal(&epoch.issuer_principal.principal_id)
            .ok_or("recovery issuer missing")?;
        if issuer.principal != epoch.issuer_principal
            || issuer.execution_authority != epoch.issuer_authority
        {
            return Err("recovery issuer authority changed");
        }
        if let Some(adoption) = self
            .store
            .adaptive_leadership_local_adoption(
                &call.grant.leadership_principal.tenant_id,
                call.grant.review_id,
            )
            .map_err(|_| "local adoption authority unavailable")?
        {
            adoption
                .validate_against(&epoch, call)
                .map_err(|_| "local adoption epoch authority invalid")?;
            let issuer = self
                .principals
                .principal(&adoption.issuer_principal.principal_id)
                .ok_or("local adoption issuer missing")?;
            if issuer.principal != adoption.issuer_principal
                || issuer.execution_authority != adoption.issuer_authority
            {
                return Err("local adoption issuer authority changed");
            }
            if adaptive_recovery_release::verified_current_repair()?
                != (adoption.request.release, adoption.request.repair_digest)
            {
                return Err("local adoption installed repair changed");
            }
            return Ok(());
        }
        if adaptive_recovery_release::verified_current_repair()?
            != (epoch.request.release, epoch.request.repair_digest)
        {
            return Err("recovery installed repair changed");
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::adaptive_leadership_review::tests::{
        change_review_assignee, discovery_state, exhaust_review_history, reconcile_review_at,
        stop_review_agent,
    };
    use super::*;

    pub(crate) fn exhausted_epoch_fixture(
        unknown: bool,
    ) -> (
        tempfile::TempDir,
        adaptive_recovery_release::TestRepairGuard,
        WorkflowApi,
        super::super::adaptive_leadership_review::LeadershipContext,
    ) {
        let repair = adaptive_recovery_release::TestRepairGuard::fixture().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let (api, context) =
            super::super::adaptive_continuation_tests::fixture_schema2_recovery_source(
                &temp.path().join("company.sqlite"),
                &temp.path().join("events.sqlite"),
                unknown,
            );
        exhaust_review_history(&api, &context);
        (temp, repair, api, context)
    }

    /// Mixed historical continuation plus nine ordinary retired admissions.
    /// Synthetic model content and file evidence are fixture data only.
    pub(crate) fn admission_repair_fixture() -> (
        tempfile::TempDir,
        WorkflowApi,
        sentinel_workflow::AdaptiveLeadershipAdmissionRepairSourceV1,
        adaptive_recovery_release::TestAdmissionRepairGuard,
    ) {
        use super::super::adaptive_continuation_tests::reserve_and_claim_schema2;
        use super::super::adaptive_leadership_review::LeadershipAuthority;
        use super::super::model_execution::{ModelExecutionCompletion, ModelExecutionContext};
        let (temp, legacy_proof, api, initial) = exhausted_epoch_fixture(false);
        let operator = api.principals.principal("operator").unwrap();
        let sid = initial.binding.grant.session_id;
        let project_id = initial.binding.grant.project_id.clone();
        let path = format!("{ADAPTIVE_REVIEW_EPOCH_PATH}?project_id={project_id}&session_id={sid}");
        let draft = api.review_recovery_epoch(&operator, "GET", &path, &[]);
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
            .adaptive_leadership_recovery_epoch(&operator.principal.tenant_id, sid)
            .unwrap()
            .unwrap();
        let call = api
            .store
            .adaptive_leadership_review_call(&operator.principal.tenant_id, epoch.review_id)
            .unwrap()
            .unwrap();
        let context = api
            .prepare_leadership_review(&LeadershipAuthority::from_call(&call))
            .unwrap();
        let (id, digest) = reserve_and_claim_schema2(&api, &context);
        let completion = ModelExecutionCompletion {
            context: ModelExecutionContext::AdaptiveLeadershipReview(Box::new(context.clone())),
            content: serde_json::json!({"schema_version":2,"decision":{
                "kind":"continue", "additional_model_calls":1, "window_ms":1_000,
                "rationale":"Fixture-only retained continuation.",
                "evidence_refs":[context.source.evidence_refs[0]],
            }})
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
        api.accept_leadership_review(&completion, &context, &id, &digest)
            .unwrap();
        drop(legacy_proof);
        let session = api
            .store
            .adaptive_session_for_authority(&context.binding.grant.assignee_authority)
            .unwrap()
            .unwrap();
        let project = api
            .store
            .company_project(&operator.principal.tenant_id, &project_id)
            .unwrap()
            .unwrap();
        let wait = session.active_deadline_ms().saturating_sub(now_unix_ms());
        assert!(wait <= 1_000);
        if wait > 0 {
            std::thread::sleep(std::time::Duration::from_millis(wait));
        }
        let root = api
            .store
            .historical_adaptive_provider_project(&session.grant, session.grant.created_at_ms)
            .unwrap()
            .unwrap()
            .subscription_call
            .unwrap();
        let active_digest = sentinel_workflow::adaptive_budget_allowance_digest(
            project.subscription_call.as_ref().unwrap(),
        )
        .unwrap();
        let history_digest =
            sentinel_workflow::adaptive_budget_history_digest(&session.continuation).unwrap();
        for group in 0..3 {
            for index in 0..3 {
                let now = now_unix_ms();
                let mut source = context.source.clone();
                source.source_project = project.clone();
                source.source_session = session.clone();
                source.evidence_refs = vec![
                    format!(
                        "adaptive-budget-root:{}:{}",
                        root.allowance_id, session.grant.provider_authority_digest
                    ),
                    format!("adaptive-budget-current:{active_digest}"),
                    format!("adaptive-budget-history:{history_digest}"),
                    format!("admission-repair-fixture:{group}:{index}"),
                ];
                source.evidence_refs.sort();
                let mut grant = context.binding.grant.clone();
                grant.schema_version = 3;
                grant.recovery_epoch = None;
                grant.expected_project_version = project.version;
                grant.expected_session_version = session.version;
                grant.expected_reason_code.clear();
                grant.subject = Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted {
                    budget: Box::new(sentinel_workflow::AdaptiveBudgetWindowAuthorityV1 {
                        schema_version: 1,
                        root_allowance: root.clone(),
                        active_allowance_digest: active_digest.clone(),
                        continuation_history_digest: history_digest.clone(),
                        observed_at_ms: now,
                        deadline_expired: true,
                        model_calls_exhausted: session.model_calls
                            >= session.active_model_ceiling(),
                    }),
                });
                grant.evidence_fingerprint = adaptive_leadership_evidence_fingerprint(
                    &source.tool_catalog,
                    &source.evidence_refs,
                )
                .unwrap();
                grant.review_id = adaptive_leadership_review_id(
                    sid,
                    session.version,
                    &grant.evidence_fingerprint,
                )
                .unwrap();
                grant.expires_at_unix_ms = now + 1;
                let call = api
                    .store
                    .authorize_adaptive_leadership_review_call(
                        &grant.leadership_principal,
                        Uuid::new_v4(),
                        &format!("repair-fixture-{group}-{index}"),
                        &grant,
                        &source,
                        now,
                    )
                    .unwrap();
                api.store
                    .expire_adaptive_leadership_review_call(
                        &grant.leadership_principal,
                        grant.review_id,
                        call.version,
                        grant.expires_at_unix_ms,
                    )
                    .unwrap();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            reconcile_review_at(&api, &project, now_unix_ms());
            if group < 2 {
                let now = now_unix_ms();
                let extension = if group == 0 {
                    api.store.budget_review_extension_draft(
                        &operator.principal,
                        &project_id,
                        sid,
                        Uuid::new_v4(),
                        3,
                        "fixture-only-extension",
                        now + 300_000,
                        now,
                    )
                } else {
                    api.store.budget_review_extension_successor_draft(
                        &operator.principal,
                        &project_id,
                        sid,
                        Uuid::new_v4(),
                        3,
                        "fixture-only-successor",
                        now + 300_000,
                        now,
                    )
                }
                .unwrap();
                api.store
                    .authorize_budget_review_extension(&operator.principal, &extension, now)
                    .unwrap();
            }
        }
        let source = api
            .store
            .adaptive_leadership_admission_repair_source(&operator.principal.tenant_id, sid)
            .unwrap();
        let guard = adaptive_recovery_release::TestAdmissionRepairGuard::fixture(&source);
        (temp, api, source, guard)
    }

    #[test]
    fn stopped_assignee_epoch_draft_and_issue_preserve_root_accounting() {
        for unknown in [false, true] {
            let (temp, _repair, api, context) = exhausted_epoch_fixture(unknown);
            let authority = &context.binding.grant.assignee_authority;
            stop_review_agent(&api, authority.agent_id, false);
            let operator = api.principals.principal("operator").unwrap();
            let path = format!(
                "{ADAPTIVE_REVIEW_EPOCH_PATH}?project_id={}&session_id={}",
                authority.project_id, context.binding.grant.session_id
            );
            let before = discovery_state(
                &temp.path().join("company.sqlite"),
                &temp.path().join("events.sqlite"),
            );
            let draft = api.review_recovery_epoch(&operator, "GET", &path, &[]);
            assert_eq!(
                draft.status,
                200,
                "{}",
                String::from_utf8_lossy(&draft.body)
            );
            assert_eq!(
                discovery_state(
                    &temp.path().join("company.sqlite"),
                    &temp.path().join("events.sqlite"),
                ),
                before
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
            assert_eq!(
                api.store.adaptive_session_for_authority(authority).unwrap(),
                Some(context.source.source_session.clone())
            );
            assert_eq!(
                api.store
                    .company_project(&authority.tenant_id, &authority.project_id)
                    .unwrap(),
                Some(context.source.source_project.clone())
            );
            assert!(api
                .authority
                .as_ref()
                .unwrap()
                .snapshot(
                    &authority.tenant_id,
                    &authority.project_id,
                    &authority.work_item_id,
                    authority.agent_id,
                )
                .is_err());
        }
    }

    #[test]
    fn epoch_assignee_revocation_before_draft_or_issue_fails_without_writes() {
        for unknown in [false, true] {
            for after_draft in [false, true] {
                for change in ["assignment", "principal", "leader"] {
                    let (temp, _repair, mut api, context) = exhausted_epoch_fixture(unknown);
                    stop_review_agent(
                        &api,
                        context.binding.grant.assignee_authority.agent_id,
                        false,
                    );
                    let operator = api.principals.principal("operator").unwrap();
                    let path = format!(
                        "{ADAPTIVE_REVIEW_EPOCH_PATH}?project_id={}&session_id={}",
                        context.binding.grant.project_id, context.binding.grant.session_id
                    );
                    let request = if after_draft {
                        let draft = api.review_recovery_epoch(&operator, "GET", &path, &[]);
                        assert_eq!(draft.status, 200);
                        let draft: serde_json::Value = serde_json::from_slice(&draft.body).unwrap();
                        Some(serde_json::to_vec(&draft["request"]).unwrap())
                    } else {
                        None
                    };
                    if change == "leader" {
                        stop_review_agent(
                            &api,
                            context.binding.grant.leadership_principal.agent_id.unwrap(),
                            false,
                        );
                    } else {
                        change_review_assignee(&mut api, &context, change);
                    }
                    let before = discovery_state(
                        &temp.path().join("company.sqlite"),
                        &temp.path().join("events.sqlite"),
                    );
                    assert_eq!(
                        api.review_recovery_epoch(&operator, "GET", &path, &[])
                            .status,
                        409
                    );
                    if let Some(request) = request {
                        assert_eq!(
                            api.review_recovery_epoch(
                                &operator,
                                "POST",
                                ADAPTIVE_REVIEW_EPOCH_PATH,
                                &request,
                            )
                            .status,
                            409
                        );
                    }
                    assert_eq!(
                        discovery_state(
                            &temp.path().join("company.sqlite"),
                            &temp.path().join("events.sqlite"),
                        ),
                        before,
                        "{change}"
                    );
                }
            }
        }
    }

    #[test]
    fn recovery_epoch_route_rejects_employee_and_customer_authority_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let (api, session) = super::super::adaptive_recovery::tests::fixture(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
            true,
        );
        let path = format!(
            "{ADAPTIVE_REVIEW_EPOCH_PATH}?project_id={}&session_id={}",
            session.grant.authority.project_id, session.grant.session_id
        );
        let before = api
            .event_store
            .as_ref()
            .unwrap()
            .get_all_events()
            .unwrap()
            .len();
        for actor in ["customer", "sales", "developer-6", "pm", "technical-lead"] {
            let principal = api.principals.principal(actor).unwrap();
            assert_eq!(
                api.review_recovery_epoch(&principal, "GET", &path, &[])
                    .status,
                403,
                "{actor}"
            );
            assert_eq!(
                api.review_recovery_epoch(&principal, "POST", &path, b"{}")
                    .status,
                403,
                "{actor}"
            );
        }
        assert_eq!(
            api.event_store
                .as_ref()
                .unwrap()
                .get_all_events()
                .unwrap()
                .len(),
            before
        );
        assert!(api
            .store
            .adaptive_leadership_recovery_epoch(
                &session.grant.authority.tenant_id,
                session.grant.session_id,
            )
            .unwrap()
            .is_none());
    }

    #[test]
    fn recovery_epoch_rejected_draft_does_not_import_evidence_or_issue_authority() {
        let temp = tempfile::tempdir().unwrap();
        let (api, session) = super::super::adaptive_recovery::tests::fixture(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
            true,
        );
        let mut operator = api.principals.principal("operator").unwrap();
        operator.principal.role = CompanyRoleV1::ProjectManager;
        let path = format!(
            "{ADAPTIVE_REVIEW_EPOCH_PATH}?project_id={}&session_id={}",
            session.grant.authority.project_id, session.grant.session_id
        );
        let before = api
            .event_store
            .as_ref()
            .unwrap()
            .get_all_events()
            .unwrap()
            .len();
        assert_eq!(
            api.review_recovery_epoch(&operator, "GET", &path, &[])
                .status,
            409
        );
        assert_eq!(
            api.event_store
                .as_ref()
                .unwrap()
                .get_all_events()
                .unwrap()
                .len(),
            before
        );
        assert_eq!(
            api.store
                .adaptive_session(session.grant.session_id, &session.grant.authority)
                .unwrap(),
            Some(session)
        );
    }
}
