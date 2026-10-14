//! Explicit replacement of an expired unadopted proposal; issuance is not adoption.
use super::*;
use sentinel_workflow::{
    AdaptiveLeadershipReviewCallV1, AdaptiveSessionV1,
    AdaptiveWorkFundingExpiredReplacementDraftV1, AdaptiveWorkFundingExpiredReplacementEvidenceV1,
    AdaptiveWorkFundingExpiredReplacementReceiptV1, AdaptiveWorkFundingExpiredReplacementRequestV1,
    AdaptiveWorkFundingExpiredReplacementVerificationSourceV1, AdaptiveWorkFundingLimitsV1,
};

struct ReplacementObservationAuthority<'a> {
    runtime: &'a CompanyAuthority,
    project: &'a sentinel_workflow::ProjectV1,
    session: &'a AdaptiveSessionV1,
}

impl crate::workbench::WorkbenchAuthoritySource for ReplacementObservationAuthority<'_> {
    fn current_for_request(
        &self,
        _request: &sentinel_common::WorkbenchRequest,
    ) -> anyhow::Result<crate::workbench::WorkbenchAuthoritySnapshot> {
        anyhow::bail!("replacement observation adapter cannot dispatch tools")
    }

    fn current_for_record(
        &self,
        record: &crate::workbench::WorkbenchInvocationRecord,
    ) -> anyhow::Result<crate::workbench::WorkbenchAuthoritySnapshot> {
        let expected = &self.session.grant.authority;
        anyhow::ensure!(
            record.project_id == self.project.project_id.0
                && record.work_item_id == expected.work_item_id.0
                && record.agent_id == expected.agent_id
                && record.caller_id == expected.principal.principal_id,
            "replacement observation scope changed"
        );
        let current = self
            .runtime
            .snapshot_from_validated_project(
                self.project,
                &self.project.tenant_id,
                &self.project.project_id,
                &expected.work_item_id,
                expected.agent_id,
                false,
            )
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        anyhow::ensure!(
            current == *expected,
            "replacement observation authority changed"
        );
        let principal = self
            .runtime
            .principals
            .principal(&current.principal.principal_id)
            .filter(|bound| {
                bound.execution_authority == current.principal
                    && bound.principal.tenant_id == self.project.tenant_id
                    && bound.principal.agent_id == Some(current.agent_id)
            })
            .ok_or_else(|| anyhow::anyhow!("replacement observation principal changed"))?;
        let (profile, _) = self
            .runtime
            .profile_for_binding(&current.profile_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(crate::workbench::WorkbenchAuthoritySnapshot {
            agent_id: current.agent_id,
            caller_id: current.principal.principal_id,
            caller_role: collaboration_policy_role_name(principal.principal.role).into(),
            project_id: self.project.project_id.0.clone(),
            work_item_id: current.work_item_id.0,
            assignment_version: current.assignment_version,
            credential_generation: current.principal.principal_generation,
            policy_digest: current.policy_digest,
            tool_profile: current.profile_id,
            tool_profile_digest: current.profile_digest,
            runtime_key: current.runtime_key,
            assignment_active: true,
            agent_capabilities: current.capabilities.clone(),
            role_capabilities: role_capabilities(
                principal.principal.role,
                &profile.capabilities,
                &profile.capabilities,
            ),
            assignment_capabilities: current.capabilities.clone(),
            project_capabilities: current.capabilities,
            profile_capabilities: profile.capabilities.clone(),
        })
    }
}

pub(super) const EXPIRED_FUNDING_REPLACEMENT_MODE: &str = "expired_unadopted_replacement";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplacementSubmission {
    schema_version: u16,
    operation_id: Uuid,
    project_id: ProjectId,
    session_id: Uuid,
    old_funding_receipt_digest: String,
    source_digest: String,
    reason_ref: String,
    limits: AdaptiveWorkFundingLimitsV1,
    release: sentinel_workflow::AdaptiveRecoveryReleaseV1,
    proof_digest: String,
}

fn submission(request: &AdaptiveWorkFundingExpiredReplacementRequestV1) -> ReplacementSubmission {
    ReplacementSubmission {
        schema_version: request.schema_version,
        operation_id: request.funding_request.operation_id,
        project_id: request.source.project_id.clone(),
        session_id: request.source.session_id,
        old_funding_receipt_digest: request.source.old_funding_receipt_digest.clone(),
        source_digest: request.source_digest.clone(),
        reason_ref: request.funding_request.reason_ref.clone(),
        limits: request.funding_request.limits.clone(),
        release: request.release.clone(),
        proof_digest: request.proof_digest.clone(),
    }
}

fn conflict() -> WorkflowHttpResponse {
    json_error(
        409,
        "expired_funding_replacement_conflict",
        "expired funding replacement rejected",
        false,
    )
}

fn receipt(
    value: &AdaptiveWorkFundingExpiredReplacementReceiptV1,
    replayed: bool,
    historical: bool,
) -> WorkflowHttpResponse {
    let mut public = serde_json::json!({
        "schema_version": 1, "receipt_kind": "immutable_funding_replacement", "replayed": replayed,
        "replacement_key": value.replacement_key,
        "operation_id": value.request.funding_request.operation_id,
        "project_id": value.request.source.project_id, "session_id": value.request.source.session_id,
        "old_funding_receipt_digest": value.request.source.old_funding_receipt_digest,
        "new_funding_id": value.new_funding_id, "new_funding_receipt_digest": value.new_funding_receipt_digest,
        "issued_at_unix_ms": value.issued_at_unix_ms,
        "expires_at_unix_ms": value.request.funding_request.limits.expires_at_unix_ms,
        "limits": value.request.funding_request.limits,
        "authority": "new_finite_unadopted_funding_proposal",
        "original_submission": submission(&value.request),
        "issuer": value.issuer_principal,
        "model_decision_recorded": false, "developer_window_created": false
    });
    if historical {
        public["historical_lookup"] = serde_json::json!(true);
    }
    json(200, &public)
}

fn scope(path: &str) -> Option<(ProjectId, Uuid, &str)> {
    let project = ProjectId::parse(query_parameter(path, "project_id")?).ok()?;
    let session = Uuid::parse_str(query_parameter(path, "session_id")?).ok()?;
    let digest = query_parameter(path, "old_funding_receipt_digest")?;
    if session.is_nil() || digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((project, session, digest))
}

fn draft_limits(path: &str) -> Option<AdaptiveWorkFundingLimitsV1> {
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
}

fn metadata_modes(path: &str, method: &str) -> Result<(bool, bool), ()> {
    let mut seen = std::collections::HashSet::new();
    let mut source_only = false;
    let mut receipt_only = false;
    let Some((_, query)) = path.split_once('?') else {
        return Ok((false, false));
    };
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if !matches!(
            key,
            "mode"
                | "project_id"
                | "session_id"
                | "old_funding_receipt_digest"
                | "operation_id"
                | "reason_ref"
                | "source_only"
                | "receipt_only"
                | "replacement_key"
                | "additional_model_calls"
                | "additional_tool_calls"
                | "additional_reviews"
                | "additional_windows"
                | "max_window_ms"
                | "max_call_duration_ms"
                | "dispatch_margin_ms"
                | "expires_at_unix_ms"
        ) || value.is_empty()
            || !seen.insert(key)
        {
            return Err(());
        }
        if method == "POST" && key != "mode" {
            return Err(());
        }
        if matches!(key, "source_only" | "receipt_only") {
            if method != "GET" || value != "true" {
                return Err(());
            }
            if key == "source_only" {
                source_only = true;
            } else {
                receipt_only = true;
            }
        }
    }
    if source_only && receipt_only {
        return Err(());
    }
    if seen.contains("replacement_key") && !receipt_only {
        return Err(());
    }
    if source_only
        && seen.iter().any(|key| {
            !matches!(
                *key,
                "mode" | "project_id" | "session_id" | "old_funding_receipt_digest" | "source_only"
            )
        })
    {
        return Err(());
    }
    if receipt_only
        && seen.iter().any(|key| {
            !matches!(
                *key,
                "mode"
                    | "project_id"
                    | "session_id"
                    | "old_funding_receipt_digest"
                    | "receipt_only"
                    | "replacement_key"
                    | "operation_id"
            )
        })
    {
        return Err(());
    }
    Ok((source_only, receipt_only))
}

impl WorkflowApi {
    pub(super) fn expired_funding_replacement_http(
        &self,
        operator: &BoundPrincipal,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> WorkflowHttpResponse {
        self.expired_funding_replacement_http_with_checks(
            operator,
            method,
            path,
            body,
            now_unix_ms,
            adaptive_recovery_release::verified_current_expired_funding_replacement,
        )
    }

    fn expired_funding_replacement_http_with_checks(
        &self,
        operator: &BoundPrincipal,
        method: &str,
        path: &str,
        body: &[u8],
        clock: impl Fn() -> u64,
        verify: impl Fn(
            &AdaptiveWorkFundingExpiredReplacementVerificationSourceV1,
        ) -> Result<
            (
                AdaptiveWorkFundingExpiredReplacementEvidenceV1,
                sentinel_workflow::AdaptiveRecoveryReleaseV1,
            ),
            &'static str,
        >,
    ) -> WorkflowHttpResponse {
        if self.validate_work_funding_operator(operator).is_err() {
            return json_error(
                403,
                "authority_conflict",
                "registered operator replacement authority required",
                false,
            );
        }
        let (source_only, receipt_only) = match metadata_modes(path, method) {
            Ok(modes) => modes,
            Err(()) => {
                return json_error(
                    400,
                    "invalid_input",
                    "invalid or ambiguous replacement query",
                    false,
                )
            }
        };
        if !matches!(method, "GET" | "POST") {
            return json_error(405, "method_not_allowed", "GET or POST required", false);
        }
        if receipt_only {
            let (Some((project, session, old_digest)), Some(key), Some(operation)) = (
                scope(path),
                query_parameter(path, "replacement_key"),
                query_parameter(path, "operation_id")
                    .and_then(|id| Uuid::parse_str(id).ok())
                    .filter(|id| !id.is_nil()),
            ) else {
                return json_error(
                    400,
                    "invalid_input",
                    "exact historical replacement identity required",
                    false,
                );
            };
            let expected_key =
                match sentinel_workflow::adaptive_work_funding_expired_replacement_key(
                    &operator.principal.tenant_id,
                    session,
                    old_digest,
                ) {
                    Ok(key) => key,
                    Err(_) => return conflict(),
                };
            if expected_key != key {
                return conflict();
            }
            return match self
                .store
                .adaptive_work_funding_expired_replacement(&operator.principal.tenant_id, key)
            {
                Ok(Some(prior))
                    if prior.issuer_principal == operator.principal
                        && prior.request.source.project_id == project
                        && prior.request.source.session_id == session
                        && prior.request.source.old_funding_receipt_digest == old_digest
                        && prior.request.funding_request.operation_id == operation =>
                {
                    receipt(&prior, true, true)
                }
                Ok(_) => conflict(),
                Err(error) => workflow_error(error),
            };
        }
        let supplied = if method == "POST" {
            match decode_body::<ReplacementSubmission>(body) {
                Ok(value)
                    if value.schema_version == 1
                        && !value.operation_id.is_nil()
                        && !value.session_id.is_nil() =>
                {
                    Some(value)
                }
                _ => return conflict(),
            }
        } else {
            None
        };
        if let Some(value) = &supplied {
            let key = match sentinel_workflow::adaptive_work_funding_expired_replacement_key(
                &operator.principal.tenant_id,
                value.session_id,
                &value.old_funding_receipt_digest,
            ) {
                Ok(key) => key,
                Err(_) => return conflict(),
            };
            match self
                .store
                .adaptive_work_funding_expired_replacement(&operator.principal.tenant_id, &key)
            {
                Ok(Some(prior))
                    if prior.issuer_principal == operator.principal
                        && submission(&prior.request) == *value =>
                {
                    return receipt(&prior, true, false);
                }
                Ok(Some(_)) => return conflict(),
                Ok(None) => {}
                Err(error) => return workflow_error(error),
            }
        }
        let guard = match self.provider_recovery_fence() {
            Ok(guard) => guard,
            Err(reason) => return json_error(503, "workflow_busy", reason, true),
        };
        let (project, session, old_digest) = if let Some(value) = &supplied {
            (
                value.project_id.clone(),
                value.session_id,
                value.old_funding_receipt_digest.as_str(),
            )
        } else {
            match scope(path) {
                Some(scope) => scope,
                None => {
                    return json_error(
                        400,
                        "invalid_input",
                        "project/session/old funding digest required",
                        false,
                    )
                }
            }
        };
        let now = clock();
        if source_only {
            let metadata = match self.store.adaptive_work_funding_expired_replacement_source(
                &operator.principal,
                &project,
                session,
                old_digest,
                now,
            ) {
                Ok(source) => source,
                Err(error) => return workflow_error(error),
            };
            let key = match metadata.replacement_key() {
                Ok(key) => key,
                Err(error) => return workflow_error(error),
            };
            let (members, calls, historical_inspection) = match self
                .store
                .adaptive_work_funding_expired_replacement(&operator.principal.tenant_id, &key)
            {
                Ok(Some(prior)) => {
                    let mut calls = Vec::with_capacity(prior.review_members.len());
                    for member in &prior.review_members {
                        match self.store.adaptive_leadership_review_call(
                            &operator.principal.tenant_id,
                            member.review_id,
                        ) {
                            Ok(Some(call)) if call.version == member.version => calls.push(call),
                            Ok(_) => return conflict(),
                            Err(error) => return workflow_error(error),
                        }
                    }
                    (prior.review_members, calls, true)
                }
                Ok(None) => {
                    let fresh = match self
                        .store
                        .adaptive_work_funding_expired_replacement_verification_source(
                            &operator.principal,
                            &project,
                            session,
                            old_digest,
                            clock(),
                        ) {
                        Ok(source) => source,
                        Err(error) => return workflow_error(error),
                    };
                    if fresh.source != metadata
                        || self
                            .validate_expired_replacement_current_authority(
                                operator,
                                &fresh.project,
                                &fresh.session,
                                &fresh.calls,
                            )
                            .is_err()
                        || self
                            .validate_expired_replacement_no_io(&fresh, &guard)
                            .is_err()
                    {
                        return conflict();
                    }
                    (fresh.review_members, fresh.calls, false)
                }
                Err(error) => return workflow_error(error),
            };
            let mut retired = Vec::with_capacity(members.len());
            for member in members {
                let Some(call) = calls
                    .iter()
                    .find(|call| call.grant.review_id == member.review_id)
                else {
                    return conflict();
                };
                let evidence = match adaptive_recovery_release::retired_admission_evidence(call) {
                    Ok(evidence) => evidence,
                    Err(_) => return conflict(),
                };
                retired.push(serde_json::json!({"review_id": member.review_id, "member": member, "retired_review": evidence}));
            }
            let digest = match metadata.canonical_digest() {
                Ok(digest) => digest,
                Err(error) => return workflow_error(error),
            };
            let response = json(
                200,
                &serde_json::json!({
                    "schema_version": 1, "authority": "unverified_source_metadata", "source": metadata,
                    "source_digest": digest, "replacement_key": key, "retired_reviews": retired,
                    "authority_issued": false, "model_decision_recorded": false,
                    "issuer": operator.principal,
                    "historical_inspection": historical_inspection,
                    "eligibility": if historical_inspection { "historical_noneligible" } else { "not_asserted" },
                }),
            );
            return if response.body.len() <= MAX_WORKFLOW_BODY_BYTES {
                response
            } else {
                conflict()
            };
        }
        let fresh = match self
            .store
            .adaptive_work_funding_expired_replacement_verification_source(
                &operator.principal,
                &project,
                session,
                old_digest,
                now,
            ) {
            Ok(source) => source,
            Err(error) => return workflow_error(error),
        };
        let (evidence, release) = match verify(&fresh) {
            Ok(value) => value,
            Err(_) => return conflict(),
        };
        if self
            .validate_expired_replacement_current_authority(
                operator,
                &fresh.project,
                &fresh.session,
                &fresh.calls,
            )
            .is_err()
            || self
                .validate_expired_replacement_no_io(&fresh, &guard)
                .is_err()
        {
            return conflict();
        }
        let (operation, reason, limits) = if let Some(value) = &supplied {
            (
                value.operation_id,
                value.reason_ref.clone(),
                value.limits.clone(),
            )
        } else {
            let Some(operation) = query_parameter(path, "operation_id")
                .and_then(|id| Uuid::parse_str(id).ok())
                .filter(|id| !id.is_nil())
            else {
                return json_error(400, "invalid_input", "new operation UUID required", false);
            };
            let (Some(reason), Some(limits)) =
                (query_parameter(path, "reason_ref"), draft_limits(path))
            else {
                return json_error(
                    400,
                    "invalid_input",
                    "explicit replacement reason and limits required",
                    false,
                );
            };
            (operation, reason.to_owned(), limits)
        };
        let draft = AdaptiveWorkFundingExpiredReplacementDraftV1 {
            old_funding_receipt_digest: old_digest.to_owned(),
            operation_id: operation,
            reason_ref: reason,
            limits,
            release,
            proof_digest: evidence.proof_digest,
        };
        let request = match self.store.adaptive_work_funding_expired_replacement_draft(
            &operator.principal,
            &project,
            session,
            &draft,
            clock(),
        ) {
            Ok(request) => request,
            Err(error) => return workflow_error(error),
        };
        if request.source != fresh.source {
            return conflict();
        }
        let public = submission(&request);
        if let Some(value) = supplied {
            if public != value {
                return conflict();
            }
        } else {
            return json(
                200,
                &serde_json::json!({"schema_version": 1, "requires_explicit_submission": true,
                "request": public, "grant_schema_version": 5, "model_decision_recorded": false,
                "developer_window_created": false}),
            );
        }
        let result = self
            .store
            .authorize_adaptive_work_funding_expired_replacement(
                &operator.principal,
                &request,
                clock(),
                |source, expected| {
                    let (mut evidence, release) =
                        verify(source).map_err(|_| expired_replacement_error())?;
                    if release != expected.release
                        || evidence.source_digest != expected.source_digest
                        || evidence.proof_digest != expected.proof_digest
                    {
                        return Err(expired_replacement_error());
                    }
                    self.validate_expired_replacement_current_authority(
                        operator,
                        &source.project,
                        &source.session,
                        &source.calls,
                    )
                    .map_err(|_| expired_replacement_error())?;
                    self.validate_expired_replacement_no_io(source, &guard)
                        .map_err(|_| expired_replacement_error())?;
                    evidence.verified_at_unix_ms = clock();
                    Ok(evidence)
                },
            );
        match result {
            Ok((replayed, value)) => receipt(&value, replayed, false),
            Err(error) => workflow_error(error),
        }
    }

    fn validate_expired_replacement_no_io(
        &self,
        source: &AdaptiveWorkFundingExpiredReplacementVerificationSourceV1,
        guard: &ProviderRecoveryGuard<'_>,
    ) -> Result<(), &'static str> {
        for member in &source.review_members {
            let call = source
                .calls
                .iter()
                .find(|call| call.grant.review_id == member.review_id)
                .ok_or("expired funding member missing")?;
            self.validate_funded_request_no_io(&call.request_id(), guard)?;
        }
        Ok(())
    }

    // Safe in the atomic domain callback: this method never acquires the store.
    fn validate_expired_replacement_current_authority(
        &self,
        operator: &BoundPrincipal,
        project: &sentinel_workflow::ProjectV1,
        session: &AdaptiveSessionV1,
        calls: &[AdaptiveLeadershipReviewCallV1],
    ) -> Result<(), &'static str> {
        self.validate_work_funding_operator(operator)
            .map_err(|_| "expired funding operator changed")?;
        if project.tenant_id != operator.principal.tenant_id
            || session.grant.authority.tenant_id != project.tenant_id
            || session.grant.authority.project_id != project.project_id
        {
            return Err("expired funding scope changed");
        }
        let runtime = self
            .authority
            .as_ref()
            .ok_or("expired funding runtime unavailable")?;
        runtime
            .project_profiles
            .family(&project.governance.project_profile)
            .map_err(|_| "expired funding project profile unavailable")?;
        let expected = &session.grant.authority;
        let current = runtime
            .snapshot_from_validated_project(
                project,
                &project.tenant_id,
                &project.project_id,
                &expected.work_item_id,
                expected.agent_id,
                false,
            )
            .map_err(|_| "expired funding assignment unavailable")?;
        if current != *expected {
            return Err("expired funding assignment changed");
        }
        let work = project
            .work_items
            .get(&current.work_item_id)
            .ok_or("expired funding work unavailable")?;
        let employee = self
            .principals
            .by_principal_id
            .values()
            .find(|bound| {
                bound.principal.tenant_id == project.tenant_id
                    && bound.principal.agent_id == Some(current.agent_id)
                    && bound.principal.kind == CompanyPrincipalKindV1::Agent
                    && bound.principal.role == work.spec.required_role
                    && bound.execution_authority == current.principal
            })
            .ok_or("expired funding assignee changed")?;
        self.validate_company_employee(&employee.principal)?;
        if !project.governance.participants.iter().any(|participant| {
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
        }) {
            return Err("expired funding leader unavailable");
        }
        let (profile, digest) = runtime
            .profile_for_binding(&current.profile_id)
            .map_err(|_| "expired funding profile unavailable")?;
        if digest != current.profile_digest {
            return Err("expired funding profile changed");
        }
        let catalog =
            model_execution::tool_catalog::adaptive_tool_catalog(profile, &current, &work.spec)?;
        if calls
            .iter()
            .any(|call| call.context.tool_catalog != catalog)
        {
            return Err("expired funding catalog changed");
        }
        if let Some(reference) = &session.last_observation {
            let adapter = ReplacementObservationAuthority {
                runtime,
                project,
                session,
            };
            let observation = crate::workbench::read_workbench_private_observation(
                &reference.effect.id.to_string(),
                &adapter,
                profile,
                digest,
            )
            .map_err(|_| "expired funding observation unavailable")?
            .ok_or("expired funding observation missing")?;
            observation
                .validate(
                    &reference.effect.id.to_string(),
                    &reference.effect.request_digest,
                )
                .map_err(|_| "expired funding observation invalid")?;
            if observation.digest() != reference.observation_digest {
                return Err("expired funding observation changed");
            }
        }
        Ok(())
    }
}

fn expired_replacement_error() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::AuthorityConflict,
        false,
        "expired funding proof/authority changed",
    )
}

#[cfg(test)]
#[path = "tests/expired_funding_replacement.rs"]
mod tests;
