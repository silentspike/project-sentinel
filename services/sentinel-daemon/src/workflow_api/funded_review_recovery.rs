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

// Never pass arbitrary verifier/authority text through to production diagnostics.
fn rejection_reason(reason: &str) -> &'static str {
    match reason {
        "provider recovery unavailable" => "recovery_unavailable",
        "workflow recovery busy" => "recovery_busy",
        "provider recovery admission unavailable" => "admission_unavailable",
        "provider admission closed" => "admission_closed",
        "provider ownership unavailable" => "ownership_unavailable",
        "funded recovery EventStore missing" => "event_store_missing",
        "funded completion unavailable" => "completion_unavailable",
        "funded history unavailable" => "history_unavailable",
        "funded review has provider history" => "provider_history",
        "funded retired review missing" => "retired_review_missing",
        "funded leader missing" | "funded leader changed" => "leader_changed",
        "funded runtime missing" => "runtime_missing",
        "funded review epoch missing" | "funded recovery epoch missing" => "epoch_missing",
        "funded old marker invalid" | "funded retry marker invalid" => "funding_marker_invalid",
        "funded source digest invalid" | "funded recovery source invalid" => "source_invalid",
        "funded retry fingerprint invalid" | "funded retry identity invalid" => {
            "retry_identity_invalid"
        }
        "funded project profile unavailable" | "funded profile missing" => "profile_unavailable",
        "funded assignment unavailable" | "funded assignment changed" => "assignment_changed",
        "funded assignee missing" | "funded assignee changed" => "assignee_changed",
        "funded work missing" => "work_missing",
        "funded catalog changed" => "catalog_changed",
        "funded original window invalid" => "original_window_invalid",
        "funded retry lifetime exhausted" => "lifetime_exhausted",
        "funded recovery serving evidence changed" => "serving_evidence_changed",
        "funded release proof clock invalid" => "proof_clock_invalid",
        "funded recovery installed release mismatch" => "installed_release_mismatch",
        "funded recovery attestation invalid" => "attestation_invalid",
        "funded recovery attestation binding invalid" => "attestation_binding_invalid",
        "funded recovery gates invalid" | "funded recovery gate binding invalid" => "gates_invalid",
        "funded historical serving interval unproven" => "historical_interval_unproven",
        "funded historical admission ordering unproven" => "historical_ordering_unproven",
        "funded retired evidence changed" => "retired_evidence_changed",
        "funded request proof invalid" | "funded request proof binding invalid" => {
            "request_proof_invalid"
        }
        "funded never-reserved proof invalid" => "never_reserved_proof_invalid",
        "funded durable-provider-gate proof invalid" => "provider_gate_proof_invalid",
        "funded release proof missing"
        | "funded release evidence invalid"
        | "funded release-before-I/O proof invalid" => "release_proof_invalid",
        "funded recovery reference invalid" | "funded recovery referenced bytes changed" => {
            "reference_invalid"
        }
        "funded recovery retired review missing" | "funded retirement missing" => {
            "retired_review_missing"
        }
        "funded recovery key invalid" => "receipt_key_invalid",
        "funded recovery release invalid" | "funded historical release invalid" => {
            "release_invalid"
        }
        "funded recovery manifest invalid"
        | "funded historical manifest invalid"
        | "funded historical manifest binding invalid" => "manifest_invalid",
        "funded historical deployment invalid" => "historical_deployment_invalid",
        "funded ordering evidence invalid" => "ordering_evidence_invalid",
        "funded retired evidence invalid" => "retired_evidence_invalid",
        "recovery evidence parent not protected"
        | "recovery evidence file not protected"
        | "recovery evidence path invalid" => "evidence_not_protected",
        "recovery evidence parent unavailable"
        | "recovery evidence unavailable"
        | "recovery evidence open failed"
        | "recovery evidence stat failed"
        | "recovery evidence read failed" => "evidence_unavailable",
        "recovery evidence replaced during open"
        | "recovery evidence changed during read"
        | "recovery evidence vanished" => "evidence_changed",
        "recovery evidence oversized" | "recovery gateway oversized" => "evidence_oversized",
        "recovery serving process unavailable"
        | "recovery serving executable unavailable"
        | "recovery serving executable stat failed"
        | "recovery gateway read failed"
        | "recovery serving identity unavailable"
        | "recovery serving PID invalid" => "serving_identity_unavailable",
        "recovery serving process changed"
        | "recovery serving executable changed"
        | "recovery serving executable identity mismatch" => "serving_identity_changed",
        _ => "check_rejected",
    }
}

fn rejected(stage: &'static str, reason: &'static str) -> WorkflowHttpResponse {
    tracing::warn!(
        stage,
        reason,
        outcome = "rejected",
        "Funded review recovery stage"
    );
    conflict()
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

pub(super) fn template(
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
    // This template seals a new NoIO recovery review, never a retained replay.
    context.presentation_version = Some(2);
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
        let started = std::time::Instant::now();
        let span = tracing::info_span!(
            "funded_review_recovery",
            operation_id = tracing::field::Empty,
            review_id = tracing::field::Empty
        );
        let _entered = span.enter();
        // Fixed call sites bound event volume; starts remain visible during a wait.
        macro_rules! stage {
            ($name:literal, $check:expr, $reason:expr) => {{
                let stage_started = std::time::Instant::now();
                tracing::info!(
                    stage = $name,
                    outcome = "started",
                    request_elapsed_ms =
                        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    "Funded review recovery stage"
                );
                let result = $check;
                let elapsed_ms =
                    u64::try_from(stage_started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let request_elapsed_ms =
                    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                match &result {
                    Ok(_) => tracing::info!(
                        stage = $name,
                        outcome = "completed",
                        elapsed_ms,
                        request_elapsed_ms,
                        "Funded review recovery stage"
                    ),
                    Err(error) => tracing::warn!(
                        stage = $name,
                        outcome = "rejected",
                        reason = ($reason)(error),
                        elapsed_ms,
                        request_elapsed_ms,
                        "Funded review recovery stage"
                    ),
                }
                result
            }};
        }
        if stage!(
            "preflight_operator",
            self.validate_work_funding_operator(operator),
            |error: &WorkflowError| workflow_error_code(error.code)
        )
        .is_err()
        {
            return json_error(
                403,
                "authority_conflict",
                "operator recovery authority required",
                false,
            );
        }
        let (source_only, receipt_only) = match stage!(
            "preflight_query",
            metadata_query_modes(path, method),
            |_: &()| "invalid_metadata_query"
        ) {
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
                tracing::warn!(
                    stage = "preflight_receipt",
                    reason = "invalid_receipt_scope",
                    outcome = "rejected",
                    "Funded review recovery stage"
                );
                return json_error(
                    400,
                    "invalid_input",
                    "exact receipt identity and original scope required",
                    false,
                );
            };
            span.record("operation_id", tracing::field::display(operation));
            span.record("review_id", tracing::field::display(retired));
            // Historical lookup never falls through to draft/issuance, even on absence.
            return match stage!(
                "receipt_lookup",
                self.store
                    .adaptive_funded_review_recovery(&operator.principal.tenant_id, key),
                |error: &WorkflowError| workflow_error_code(error.code)
            ) {
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
                Ok(Some(_)) => rejected("receipt_lookup", "receipt_identity_mismatch"),
                Ok(None) => rejected("receipt_lookup", "receipt_missing"),
                Err(error) => workflow_error(error),
            };
        }
        let supplied = if method == "POST" {
            match stage!(
                "preflight_body",
                serde_json::from_slice::<AdaptiveFundedReviewRecoveryRequestV1>(body),
                |_: &serde_json::Error| "invalid_request"
            ) {
                Ok(request) if request.source.tenant_id == operator.principal.tenant_id => {
                    span.record(
                        "operation_id",
                        tracing::field::display(request.operation_id),
                    );
                    span.record(
                        "review_id",
                        tracing::field::display(request.source.retired_review_id),
                    );
                    Some(request)
                }
                Ok(_) => return rejected("preflight_body", "tenant_mismatch"),
                Err(_) => return conflict(),
            }
        } else if method == "GET" {
            None
        } else {
            tracing::warn!(
                stage = "preflight_method",
                reason = "method_not_allowed",
                outcome = "rejected",
                "Funded review recovery stage"
            );
            return json_error(405, "method_not_allowed", "GET or POST required", false);
        };
        // Immutable replay precedes fresh authority, source, proof and clock checks.
        if let Some(request) = &supplied {
            let key = match stage!(
                "replay_key",
                request.source.receipt_key(),
                |error: &WorkflowError| workflow_error_code(error.code)
            ) {
                Ok(key) => key,
                Err(_) => return conflict(),
            };
            match stage!(
                "replay_lookup",
                self.store
                    .adaptive_funded_review_recovery(&operator.principal.tenant_id, &key),
                |error: &WorkflowError| workflow_error_code(error.code)
            ) {
                Ok(Some(prior))
                    if prior.request == *request
                        && prior.issuer_principal == operator.principal =>
                {
                    tracing::info!(
                        stage = "replay_lookup",
                        reason = "immutable_replay",
                        outcome = "completed",
                        "Funded review recovery stage"
                    );
                    return receipt(&prior, true);
                }
                Ok(Some(_)) => return rejected("replay_lookup", "replay_mismatch"),
                Ok(None) => {}
                Err(error) => return workflow_error(error),
            }
        }
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
                    tracing::warn!(
                        stage = "preflight_scope",
                        reason = "invalid_scope",
                        outcome = "rejected",
                        "Funded review recovery stage"
                    );
                    return json_error(
                        400,
                        "invalid_input",
                        "project, session and retired review required",
                        false,
                    );
                }
            }
        };
        span.record("review_id", tracing::field::display(retired_id));
        let guard = match stage!(
            "guard_wait",
            self.provider_recovery_fence(),
            |reason: &&str| { rejection_reason(reason) }
        ) {
            Ok(guard) => guard,
            Err(reason) => return json_error(503, "workflow_busy", reason, true),
        };
        let now = clock();
        let source = match stage!(
            "source",
            self.store
                .adaptive_funded_review_recovery_verification_source(
                    &operator.principal,
                    &project_id,
                    session_id,
                    retired_id,
                    now,
                ),
            |error: &WorkflowError| workflow_error_code(error.code)
        ) {
            Ok(source) => source,
            Err(error) => return workflow_error(error),
        };
        if stage!(
            "current_authority",
            self.validate_funded_recovery_current_authority(&source),
            |reason: &&str| rejection_reason(reason)
        )
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
                None => return rejected("source_metadata", "retired_review_missing"),
            };
            let retired = match adaptive_recovery_release::retired_admission_evidence(old) {
                Ok(retired) => retired,
                Err(reason) => return rejected("source_metadata", rejection_reason(reason)),
            };
            let digest = match source.source.canonical_digest() {
                Ok(digest) => digest,
                Err(_) => return rejected("source_metadata", "source_digest_invalid"),
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
                        Err(error) => {
                            tracing::warn!(
                                stage = "source_metadata",
                                reason = workflow_error_code(error.code),
                                outcome = "rejected",
                                "Funded review recovery stage"
                            );
                            return workflow_error(error);
                        }
                    },
                    "retired_review": retired,
                    "authority_issued": false,
                    "model_decision_recorded": false
                }),
            );
        }
        let evidence = match stage!("verify", verify(&source), |reason: &&str| {
            rejection_reason(reason)
        }) {
            Ok(evidence) => evidence,
            Err(_) => return conflict(),
        };
        if stage!(
            "source_no_io",
            self.validate_funded_recovery_no_io(&source, &guard),
            |reason: &&str| rejection_reason(reason)
        )
        .is_err()
        {
            return conflict();
        }
        let request = if let Some(request) = supplied {
            let mismatch = if request.source.as_ref() != &source.source {
                Some("source_mismatch")
            } else if request.source_digest != evidence.source_digest {
                Some("source_digest_mismatch")
            } else if request.release != evidence.release {
                Some("release_mismatch")
            } else if request.proof_digest != evidence.proof_digest {
                Some("proof_digest_mismatch")
            } else {
                None
            };
            if let Some(reason) = mismatch {
                return rejected("request_binding", reason);
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
                None => return rejected("draft", "original_window_invalid"),
            };
            let draft_clock = clock();
            let expires = match draft_clock.checked_add(window) {
                Some(expires) => expires.min(source.funding.request.limits.expires_at_unix_ms),
                None => return rejected("draft", "expiry_overflow"),
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
        span.record(
            "operation_id",
            tracing::field::display(request.operation_id),
        );
        let (grant, context) =
            match stage!("template", template(&source, &request), |reason: &&str| {
                rejection_reason(reason)
            }) {
                Ok(value) => value,
                Err(_) => return conflict(),
            };
        let retry_id = format!("company-leadership-{}", grant.review_id);
        if stage!(
            "retry_no_io",
            self.validate_funded_request_no_io(&retry_id, &guard),
            |reason: &&str| rejection_reason(reason)
        )
        .is_err()
        {
            return conflict();
        }
        if stage!(
            "lifetime",
            validate_lifetime(&source, &request, clock()),
            |reason: &&str| rejection_reason(reason)
        )
        .is_err()
        {
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
        let authorize_started = std::time::Instant::now();
        match stage!(
            "authorize_store",
            self.store.authorize_adaptive_funded_review_recovery(
                &operator.principal,
                &request,
                &grant,
                &context,
                clock(),
                |fresh, request| {
                    // Includes store lock/source preparation, not just lock wait.
                    tracing::info!(
                        stage = "authorize_callback",
                        outcome = "started",
                        store_elapsed_ms = u64::try_from(authorize_started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        "Funded review recovery stage"
                    );
                    // The domain connection is held: no WorkflowStore calls in this callback.
                    stage!(
                        "fresh_source_no_io",
                        self.validate_funded_recovery_no_io(fresh, &guard),
                        |reason: &&str| rejection_reason(reason)
                    )
                    .map_err(|_| workflow_unavailable())?;
                    stage!(
                        "fresh_retry_no_io",
                        self.validate_funded_request_no_io(&retry_id, &guard),
                        |reason: &&str| rejection_reason(reason)
                    )
                    .map_err(|_| workflow_unavailable())?;
                    let mut evidence = stage!("fresh_verify", verify(fresh), |reason: &&str| {
                        rejection_reason(reason)
                    })
                    .map_err(|_| workflow_unavailable())?;
                    if evidence.release != request.release
                        || evidence.proof_digest != request.proof_digest
                    {
                        tracing::warn!(
                            stage = "fresh_binding",
                            reason = if evidence.release != request.release {
                                "release_mismatch"
                            } else {
                                "proof_digest_mismatch"
                            },
                            outcome = "rejected",
                            "Funded review recovery stage"
                        );
                        return Err(workflow_unavailable());
                    }
                    let final_clock = clock();
                    stage!(
                        "fresh_lifetime",
                        validate_lifetime(fresh, request, final_clock),
                        |reason: &&str| rejection_reason(reason)
                    )
                    .map_err(|_| workflow_unavailable())?;
                    evidence.verified_at_unix_ms = final_clock;
                    tracing::info!(
                        stage = "authorize_callback",
                        outcome = "completed",
                        store_elapsed_ms = u64::try_from(authorize_started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        "Funded review recovery stage"
                    );
                    Ok(evidence)
                },
            ),
            |error: &WorkflowError| workflow_error_code(error.code)
        ) {
            Ok((replayed, value)) => {
                tracing::info!(
                    stage = "authorize_store",
                    replayed,
                    outcome = "completed",
                    "Funded review recovery stage"
                );
                receipt(&value, replayed)
            }
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
            if source.review_has_prior_adoption(call) {
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
mod input_validation_tests {
    use super::*;

    const GET_SCOPE: &str = "?mode=funded_undispatched_retry&project_id=project-input&session_id=11111111-1111-4111-8111-111111111111&retired_review_id=22222222-2222-4222-8222-222222222222";

    #[test]
    fn funded_diagnostic_reasons_are_allowlisted() {
        for (reason, expected) in [
            ("workflow recovery busy", "recovery_busy"),
            ("funded history unavailable", "history_unavailable"),
            ("funded review has provider history", "provider_history"),
            ("funded assignment changed", "assignment_changed"),
            ("recovery evidence unavailable", "evidence_unavailable"),
            (
                "recovery evidence file not protected",
                "evidence_not_protected",
            ),
            (
                "funded recovery attestation binding invalid",
                "attestation_binding_invalid",
            ),
            ("funded retry lifetime exhausted", "lifetime_exhausted"),
            ("funded release proof clock invalid", "proof_clock_invalid"),
        ] {
            assert_eq!(rejection_reason(reason), expected);
        }
        for reason in [
            "",
            "secret body or authority payload",
            "funded history unavailable: secret detail",
            "recovery evidence unavailable\nsecret detail",
        ] {
            assert_eq!(rejection_reason(reason), "check_rejected");
        }
    }

    #[test]
    fn funded_diagnostic_rejection_keeps_public_conflict_unchanged() {
        assert_eq!(
            rejected("request_binding", "proof_digest_mismatch"),
            json_error(
                409,
                "funded_review_recovery_conflict",
                "funded review retry rejected",
                false,
            )
        );
    }

    #[test]
    fn funded_get_invalid_input_precedes_recovery_intent() {
        let (_temp, api, _) =
            adaptive_resume_policy::tests::unreviewed_policy_root_with_limits(120_000, false, 4, 4);
        let operator = api.principals.principal("operator").unwrap();
        let scope_error = "project, session and retired review required";
        let cases = [
            ("?mode=funded_undispatched_retry".to_owned(), scope_error),
            (
                GET_SCOPE.replace("project-input", "bad/project"),
                scope_error,
            ),
            (
                GET_SCOPE.replace("11111111-1111-4111-8111-111111111111", "bad-uuid"),
                scope_error,
            ),
            (
                GET_SCOPE.replace(
                    "11111111-1111-4111-8111-111111111111",
                    &Uuid::nil().to_string(),
                ),
                scope_error,
            ),
            (
                GET_SCOPE.replace(
                    "22222222-2222-4222-8222-222222222222",
                    &Uuid::nil().to_string(),
                ),
                scope_error,
            ),
            (
                format!(
                    "{}&source_only=true",
                    GET_SCOPE.replace("project-input", "bad/project")
                ),
                scope_error,
            ),
            (
                format!("{GET_SCOPE}&project_id=duplicate"),
                "invalid or ambiguous metadata query",
            ),
            (
                format!("{GET_SCOPE}&receipt_only=true"),
                "exact receipt identity and original scope required",
            ),
        ];
        for pending in [false, true] {
            *api.provider_admission.write().unwrap() = false;
            api.provider_recovery_pending
                .store(pending, Ordering::Release);
            for (path, message) in &cases {
                let response = api.funded_review_recovery_http_with_checks(
                    &operator,
                    "GET",
                    path,
                    b"",
                    || panic!("invalid input must not read the clock"),
                    |_| panic!("invalid input must not verify recovery evidence"),
                );
                assert_eq!(response, json_error(400, "invalid_input", message, false));
                assert_eq!(api.provider_recovery_is_pending(), pending);
            }
        }
    }

    #[test]
    fn funded_get_valid_scope_keeps_source_and_draft_fenced() {
        let (_temp, api, _) =
            adaptive_resume_policy::tests::unreviewed_policy_root_with_limits(120_000, false, 4, 4);
        let operator = api.principals.principal("operator").unwrap();
        api.provider_recovery_pending.store(true, Ordering::Release);
        for suffix in ["", "&source_only=true"] {
            let response = api.funded_review_recovery_http_with_checks(
                &operator,
                "GET",
                &format!("{GET_SCOPE}{suffix}"),
                b"",
                || panic!("busy recovery must not read the clock"),
                |_| panic!("busy recovery must not verify recovery evidence"),
            );
            assert_eq!(
                response,
                json_error(503, "workflow_busy", "workflow recovery busy", true)
            );
            assert!(api.provider_recovery_is_pending());
        }
    }
}

#[cfg(test)]
#[path = "tests/funded_review_recovery.rs"]
pub(crate) mod tests;
