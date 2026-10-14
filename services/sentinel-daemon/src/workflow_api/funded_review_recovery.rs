//! Explicit, normally charged retry of one proven undispatched funded review.
use super::*;
use sentinel_workflow::{
    adaptive_leadership_evidence_fingerprint, adaptive_leadership_review_id,
    AdaptiveFundedReviewRecoveryEvidenceV1, AdaptiveFundedReviewRecoveryReceiptV1,
    AdaptiveFundedReviewRecoveryRequestV1, AdaptiveFundedReviewRecoveryVerificationSourceV1,
    AdaptiveLeadershipReviewCallV1, AdaptiveLeadershipReviewContextV1,
    AdaptiveLeadershipReviewGrantV1,
};

fn conflict() -> WorkflowHttpResponse {
    json_error(
        409,
        "funded_review_recovery_conflict",
        "funded review retry rejected",
        false,
    )
}

fn metadata_query_modes(path: &str, method: &str) -> Result<(bool, bool), ()> {
    let Some((_, query)) = path.split_once('?') else {
        return Ok((false, false));
    };
    let mut seen = std::collections::HashSet::new();
    let mut source_only = false;
    let mut receipt_only = false;
    let mut unknown_parameter = false;
    // Match the explicit-query validation used by work-funding supersession.
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if matches!(
            key,
            "mode"
                | "project_id"
                | "session_id"
                | "retired_review_id"
                | "source_only"
                | "receipt_only"
                | "receipt_key"
                | "operation_id"
        ) {
            if value.is_empty() || !seen.insert(key) {
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
        } else {
            unknown_parameter = true;
        }
    }
    if (source_only && receipt_only) || (receipt_only && unknown_parameter) {
        return Err(());
    }
    Ok((source_only, receipt_only))
}

fn receipt(value: &AdaptiveFundedReviewRecoveryReceiptV1, replayed: bool) -> WorkflowHttpResponse {
    receipt_response(value, replayed, false)
}

fn receipt_response(
    value: &AdaptiveFundedReviewRecoveryReceiptV1,
    replayed: bool,
    historical_lookup: bool,
) -> WorkflowHttpResponse {
    let mut public = serde_json::json!({
        "schema_version":1, "receipt_kind":"immutable_issuance", "replayed":replayed,
        "receipt_key":value.receipt_key, "operation_id":value.request.operation_id,
        "review_id":value.grant_template.review_id,
        "retired_review_id":value.request.source.retired_review_id,
        "expires_at_unix_ms":value.request.expires_at_unix_ms,
        "ordinal":value.request.source.ordinal,
        "authority":"one_funded_undispatched_recovery_review",
        "decision_state":"not_asserted_by_issuance_receipt"
    });
    if historical_lookup {
        public["historical_lookup"] = serde_json::json!(true);
    }
    json(200, &public)
}

fn template(
    source: &AdaptiveFundedReviewRecoveryVerificationSourceV1,
    request: &AdaptiveFundedReviewRecoveryRequestV1,
) -> Result<
    (
        AdaptiveLeadershipReviewGrantV1,
        AdaptiveLeadershipReviewContextV1,
    ),
    &'static str,
> {
    let old = source
        .calls
        .iter()
        .find(|call| call.grant.review_id == source.source.retired_review_id)
        .ok_or("funded retired review missing")?;
    let mut grant = old.grant.clone();
    let mut context = old.context.clone();
    let epoch = grant
        .work_funding
        .as_mut()
        .ok_or("funded review epoch missing")?;
    let old_marker = epoch
        .evidence_ref()
        .map_err(|_| "funded old marker invalid")?;
    epoch.binding.ordinal = source.source.ordinal;
    let marker = epoch
        .evidence_ref()
        .map_err(|_| "funded retry marker invalid")?;
    context
        .evidence_refs
        .retain(|reference| reference != &old_marker);
    context.evidence_refs.push(marker);
    context.evidence_refs.push(format!(
        "funded-undispatched-recovery:{}:{}",
        source
            .source
            .canonical_digest()
            .map_err(|_| "funded source digest invalid")?,
        adaptive_recovery_release::json_digest(request)?
    ));
    context.evidence_refs.sort();
    context.evidence_refs.dedup();
    grant.evidence_fingerprint =
        adaptive_leadership_evidence_fingerprint(&context.tool_catalog, &context.evidence_refs)
            .map_err(|_| "funded retry fingerprint invalid")?;
    grant.review_id = adaptive_leadership_review_id(
        source.session.grant.session_id,
        source.session.version,
        &grant.evidence_fingerprint,
    )
    .map_err(|_| "funded retry identity invalid")?;
    grant.expires_at_unix_ms = request.expires_at_unix_ms;
    Ok((grant, context))
}

impl WorkflowApi {
    pub(super) fn funded_review_recovery_http(
        &self,
        operator: &BoundPrincipal,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> WorkflowHttpResponse {
        self.funded_review_recovery_http_with_checks(
            operator,
            method,
            path,
            body,
            now_unix_ms,
            adaptive_recovery_release::verified_current_funded_review_recovery,
        )
    }

    fn funded_review_recovery_http_with_checks(
        &self,
        operator: &BoundPrincipal,
        method: &str,
        path: &str,
        body: &[u8],
        clock: impl Fn() -> u64,
        verify: impl Fn(
            &AdaptiveFundedReviewRecoveryVerificationSourceV1,
        ) -> Result<AdaptiveFundedReviewRecoveryEvidenceV1, &'static str>,
    ) -> WorkflowHttpResponse {
        if self.validate_work_funding_operator(operator).is_err() {
            return json_error(
                403,
                "authority_conflict",
                "operator recovery authority required",
                false,
            );
        }
        let (source_only, receipt_only) = match metadata_query_modes(path, method) {
            Ok(value) => value,
            Err(()) => {
                return json_error(
                    400,
                    "invalid_input",
                    "invalid or ambiguous metadata query",
                    false,
                )
            }
        };
        if receipt_only {
            let key = query_parameter(path, "receipt_key").filter(|key| key.len() <= 128);
            let operation = query_parameter(path, "operation_id")
                .and_then(|id| Uuid::parse_str(id).ok())
                .filter(|id| !id.is_nil());
            let project =
                query_parameter(path, "project_id").and_then(|id| ProjectId::parse(id).ok());
            let session = query_parameter(path, "session_id")
                .and_then(|id| Uuid::parse_str(id).ok())
                .filter(|id| !id.is_nil());
            let retired = query_parameter(path, "retired_review_id")
                .and_then(|id| Uuid::parse_str(id).ok())
                .filter(|id| !id.is_nil());
            let (Some(key), Some(operation), Some(project), Some(session), Some(retired)) =
                (key, operation, project, session, retired)
            else {
                return json_error(
                    400,
                    "invalid_input",
                    "exact receipt identity and original scope required",
                    false,
                );
            };
            // Historical lookup never falls through to draft/issuance, even on absence.
            return match self
                .store
                .adaptive_funded_review_recovery(&operator.principal.tenant_id, key)
            {
                Ok(Some(prior))
                    if prior.issuer_principal == operator.principal
                        && prior.receipt_key == key
                        && prior.request.operation_id == operation
                        && prior.request.source.tenant_id == operator.principal.tenant_id
                        && prior.request.source.project_id == project
                        && prior.request.source.session_id == session
                        && prior.request.source.retired_review_id == retired =>
                {
                    receipt_response(&prior, true, true)
                }
                Ok(_) => conflict(),
                Err(error) => workflow_error(error),
            };
        }
        let supplied = if method == "POST" {
            match serde_json::from_slice::<AdaptiveFundedReviewRecoveryRequestV1>(body) {
                Ok(request) if request.source.tenant_id == operator.principal.tenant_id => {
                    Some(request)
                }
                _ => return conflict(),
            }
        } else if method == "GET" {
            None
        } else {
            return json_error(405, "method_not_allowed", "GET or POST required", false);
        };
        // Immutable replay precedes fresh authority, source, proof and clock checks.
        if let Some(request) = &supplied {
            let key = match request.source.receipt_key() {
                Ok(key) => key,
                Err(_) => return conflict(),
            };
            match self
                .store
                .adaptive_funded_review_recovery(&operator.principal.tenant_id, &key)
            {
                Ok(Some(prior))
                    if prior.request == *request
                        && prior.issuer_principal == operator.principal =>
                {
                    return receipt(&prior, true)
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
        let (project_id, session_id, retired_id) = if let Some(request) = &supplied {
            (
                request.source.project_id.clone(),
                request.source.session_id,
                request.source.retired_review_id,
            )
        } else {
            let project =
                query_parameter(path, "project_id").and_then(|id| ProjectId::parse(id).ok());
            let session =
                query_parameter(path, "session_id").and_then(|id| Uuid::parse_str(id).ok());
            let retired =
                query_parameter(path, "retired_review_id").and_then(|id| Uuid::parse_str(id).ok());
            match (project, session, retired) {
                (Some(project), Some(session), Some(retired))
                    if !session.is_nil() && !retired.is_nil() =>
                {
                    (project, session, retired)
                }
                _ => {
                    return json_error(
                        400,
                        "invalid_input",
                        "project, session and retired review required",
                        false,
                    )
                }
            }
        };
        let now = clock();
        let source = match self
            .store
            .adaptive_funded_review_recovery_verification_source(
                &operator.principal,
                &project_id,
                session_id,
                retired_id,
                now,
            ) {
            Ok(source) => source,
            Err(error) => return workflow_error(error),
        };
        if self
            .validate_funded_recovery_current_authority(&source)
            .is_err()
        {
            return conflict();
        }
        if source_only {
            let old = match source
                .calls
                .iter()
                .find(|call| call.grant.review_id == retired_id)
            {
                Some(old) => old,
                None => return conflict(),
            };
            let retired = match adaptive_recovery_release::retired_admission_evidence(old) {
                Ok(retired) => retired,
                Err(_) => return conflict(),
            };
            let digest = match source.source.canonical_digest() {
                Ok(digest) => digest,
                Err(_) => return conflict(),
            };
            // Store metadata is not historical NoIO evidence or dispatch authority.
            return json(
                200,
                &serde_json::json!({
                    "schema_version": 1,
                    "authority": "unverified_source_metadata",
                    "source": source.source,
                    "source_digest": digest,
                    "receipt_key": match source.source.receipt_key() {
                        Ok(key) => key,
                        Err(error) => return workflow_error(error),
                    },
                    "retired_review": retired,
                    "authority_issued": false,
                    "model_decision_recorded": false
                }),
            );
        }
        let evidence = match verify(&source) {
            Ok(evidence) => evidence,
            Err(_) => return conflict(),
        };
        if self
            .validate_funded_recovery_no_io(&source, &guard)
            .is_err()
        {
            return conflict();
        }
        let request = if let Some(request) = supplied {
            if request.source.as_ref() != &source.source
                || request.source_digest != evidence.source_digest
                || request.release != evidence.release
                || request.proof_digest != evidence.proof_digest
            {
                return conflict();
            }
            request
        } else {
            let old = source
                .calls
                .iter()
                .find(|call| call.grant.review_id == retired_id)
                .unwrap();
            let window = match old
                .grant
                .expires_at_unix_ms
                .checked_sub(old.grant_issued_at_unix_ms)
            {
                Some(window) => window.min(source.funding.request.limits.max_window_ms),
                None => return conflict(),
            };
            let draft_clock = clock();
            let expires = match draft_clock.checked_add(window) {
                Some(expires) => expires.min(source.funding.request.limits.expires_at_unix_ms),
                None => return conflict(),
            };
            AdaptiveFundedReviewRecoveryRequestV1 {
                schema_version: 1,
                operation_id: Uuid::new_v4(),
                source: Box::new(source.source.clone()),
                source_digest: evidence.source_digest,
                release: evidence.release,
                proof_digest: evidence.proof_digest,
                reason_ref: "funded-undispatched-recovery".into(),
                expires_at_unix_ms: expires,
            }
        };
        let (grant, context) = match template(&source, &request) {
            Ok(value) => value,
            Err(_) => return conflict(),
        };
        let retry_id = format!("company-leadership-{}", grant.review_id);
        if self
            .validate_funded_request_no_io(&retry_id, &guard)
            .is_err()
        {
            return conflict();
        }
        if validate_lifetime(&source, &request, clock()).is_err() {
            return conflict();
        }
        if method == "GET" {
            return json(
                200,
                &serde_json::json!({"schema_version":1, "request":request,
                "authority":"not_issued", "grant_schema_version":6,
                "requires_explicit_submission":true, "review_id":grant.review_id,
                "retired_review_id":source.source.retired_review_id,
                "bounds":{"max_call_duration_ms":grant.max_duration_ms,
                    "dispatch_margin_ms":source.funding.request.limits.dispatch_margin_ms,
                    "funding_expires_at_unix_ms":source.funding.request.limits.expires_at_unix_ms}}),
            );
        }
        match self.store.authorize_adaptive_funded_review_recovery(
            &operator.principal,
            &request,
            &grant,
            &context,
            clock(),
            |fresh, request| {
                // The domain connection is held: no WorkflowStore calls in this callback.
                self.validate_funded_recovery_no_io(fresh, &guard)
                    .map_err(|_| workflow_unavailable())?;
                self.validate_funded_request_no_io(&retry_id, &guard)
                    .map_err(|_| workflow_unavailable())?;
                let mut evidence = verify(fresh).map_err(|_| workflow_unavailable())?;
                if evidence.release != request.release
                    || evidence.proof_digest != request.proof_digest
                {
                    return Err(workflow_unavailable());
                }
                let final_clock = clock();
                validate_lifetime(fresh, request, final_clock)
                    .map_err(|_| workflow_unavailable())?;
                evidence.verified_at_unix_ms = final_clock;
                Ok(evidence)
            },
        ) {
            Ok((replayed, value)) => receipt(&value, replayed),
            Err(error) => workflow_error(error),
        }
    }

    fn validate_funded_recovery_no_io(
        &self,
        source: &AdaptiveFundedReviewRecoveryVerificationSourceV1,
        guard: &ProviderRecoveryGuard<'_>,
    ) -> Result<(), &'static str> {
        for call in &source.calls {
            if call
                .grant
                .work_funding
                .as_ref()
                .is_none_or(|epoch| epoch.receipt != *source.funding)
            {
                continue;
            }
            let id = call.request_id();
            self.validate_funded_request_no_io(&id, guard)?;
        }
        Ok(())
    }

    pub(super) fn validate_funded_request_no_io(
        &self,
        id: &str,
        guard: &ProviderRecoveryGuard<'_>,
    ) -> Result<(), &'static str> {
        let events = self
            .event_store
            .as_ref()
            .ok_or("funded recovery EventStore missing")?;
        if guard.request_is_active(id)
            || events
                .get_llm_completion(id)
                .map_err(|_| "funded completion unavailable")?
                .is_some()
        {
            return Err("funded review has provider history");
        }
        for prefix in [
            "llm_usage_",
            "llm_resolution_",
            "llm_provider_diagnostic:",
            "llm_requeue_",
            "llm_local_adoption_",
            "llm_local_adoption_retired_",
        ] {
            if events
                .has_event_operation_id(&format!("{prefix}{id}"))
                .map_err(|_| "funded history unavailable")?
            {
                return Err("funded review has provider history");
            }
        }
        Ok(())
    }

    fn validate_funded_recovery_current_authority(
        &self,
        source: &AdaptiveFundedReviewRecoveryVerificationSourceV1,
    ) -> Result<(), &'static str> {
        let old = source
            .calls
            .iter()
            .find(|call| call.grant.review_id == source.source.retired_review_id)
            .ok_or("funded retired review missing")?;
        let leader = self
            .principals
            .principal(&old.grant.leadership_principal.principal_id)
            .ok_or("funded leader missing")?;
        if leader.principal != old.grant.leadership_principal
            || leader.execution_authority != old.grant.leadership_authority
        {
            return Err("funded leader changed");
        }
        self.validate_company_employee(&leader.principal)?;
        let runtime = self.authority.as_ref().ok_or("funded runtime missing")?;
        runtime
            .project_profiles
            .family(&source.project.governance.project_profile)
            .map_err(|_| "funded project profile unavailable")?;
        let current = runtime
            .snapshot_from_validated_project(
                &source.project,
                &source.source.tenant_id,
                &source.source.project_id,
                &source.session.grant.authority.work_item_id,
                source.session.grant.authority.agent_id,
                false,
            )
            .map_err(|_| "funded assignment unavailable")?;
        if current != source.session.grant.authority {
            return Err("funded assignment changed");
        }
        let employee = self
            .principals
            .principal(&current.principal.principal_id)
            .ok_or("funded assignee missing")?;
        if employee.execution_authority != current.principal {
            return Err("funded assignee changed");
        }
        self.validate_company_employee(&employee.principal)?;
        let work = source
            .project
            .work_items
            .get(&current.work_item_id)
            .ok_or("funded work missing")?;
        let (profile, digest) = runtime
            .profile_for_binding(&current.profile_id)
            .map_err(|_| "funded profile missing")?;
        if digest != current.profile_digest
            || model_execution::tool_catalog::adaptive_tool_catalog(profile, &current, &work.spec)?
                != old.context.tool_catalog
        {
            return Err("funded catalog changed");
        }
        Ok(())
    }

    pub(super) fn verify_funded_review_recovery_membership(
        &self,
        call: &AdaptiveLeadershipReviewCallV1,
    ) -> Result<(), &'static str> {
        #[cfg(test)]
        tests::record_membership_verification();
        let binding = call
            .grant
            .funded_review_recovery
            .as_ref()
            .ok_or("funded retry binding missing")?;
        binding
            .validate_for(&call.grant)
            .map_err(|_| "funded retry binding invalid")?;
        let receipt = self
            .store
            .adaptive_funded_review_recovery(
                &call.grant.leadership_principal.tenant_id,
                &binding.receipt_key,
            )
            .map_err(|_| "funded retry receipt unavailable")?
            .ok_or("funded retry receipt missing")?;
        if receipt
            .bound_grant()
            .map_err(|_| "funded retry receipt invalid")?
            != call.grant
            || receipt.context != call.context
        {
            return Err("funded retry membership changed");
        }
        let issuer = self
            .principals
            .principal(&receipt.issuer_principal.principal_id)
            .ok_or("funded issuer missing")?;
        if issuer.principal != receipt.issuer_principal {
            return Err("funded issuer changed");
        }
        let historical = self
            .store
            .adaptive_funded_review_recovery_historical_verification_source(
                &call.grant.leadership_principal.tenant_id,
                &binding.receipt_key,
            )
            .map_err(|_| "funded historical source unavailable")?
            .ok_or("funded historical source missing")?;
        if historical.source != *receipt.request.source {
            return Err("funded historical source changed");
        }
        let mut evidence =
            adaptive_recovery_release::verified_current_funded_review_recovery(&historical)?;
        // Verification time is an issuance fact, not part of immutable proof identity.
        evidence.verified_at_unix_ms = receipt.evidence.verified_at_unix_ms;
        if evidence != receipt.evidence {
            return Err("funded retry serving proof changed");
        }
        Ok(())
    }
}

fn validate_lifetime(
    source: &AdaptiveFundedReviewRecoveryVerificationSourceV1,
    request: &AdaptiveFundedReviewRecoveryRequestV1,
    now: u64,
) -> Result<(), &'static str> {
    let old = source
        .calls
        .iter()
        .find(|call| call.grant.review_id == source.source.retired_review_id)
        .ok_or("funded retired review missing")?;
    let window = old
        .grant
        .expires_at_unix_ms
        .checked_sub(old.grant_issued_at_unix_ms)
        .ok_or("funded original window invalid")?;
    if request.expires_at_unix_ms <= now
        || request.expires_at_unix_ms > source.funding.request.limits.expires_at_unix_ms
        || request
            .expires_at_unix_ms
            .checked_sub(now)
            .is_none_or(|remaining| {
                remaining > window.min(source.funding.request.limits.max_window_ms)
            })
        || now
            .checked_add(
                old.grant
                    .max_duration_ms
                    .max(source.funding.request.limits.max_call_duration_ms),
            )
            .and_then(|time| time.checked_add(source.funding.request.limits.dispatch_margin_ms))
            .is_none_or(|deadline| deadline > request.expires_at_unix_ms)
    {
        return Err("funded retry lifetime exhausted");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/funded_review_recovery.rs"]
pub(crate) mod tests;
